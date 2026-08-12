//! `forge health` — longitudinal repository health.
//!
//! Measures and reports. It never changes routing, team planning, or any other
//! execution policy in response to what it finds; acting on a trend is Phase 8.

use std::path::PathBuf;

use anyhow::{Context, Result, anyhow, bail};
use forge_core::health::{
    DimensionStatus, HealthDimensionKind, HealthSnapshotStatus, MaterialityPolicy,
    RepositoryHealthSnapshot, TrendDirection,
};
use forge_core::ids::HealthSnapshotId;
use forge_core::world::SnapshotRelation;
use forge_health::{GitAncestry, RepositoryHealthBuilder};
use forge_store::Store;
use forge_world::snapshot_relation;

use crate::commands::run::resolve_repository;
use crate::output;

/// How many runs of evidence a build considers.
const EVIDENCE_LIMIT: u32 = 2_000;
/// How many snapshots a trend considers.
const TREND_LIMIT: u32 = 500;

pub enum HealthBuildExit {
    Complete,
    NotComplete,
}

/// Builds an immutable health snapshot for the repository's exact current
/// commit.
pub async fn build(repo: Option<PathBuf>) -> Result<HealthBuildExit> {
    let (repository, layout, config) = resolve_repository(repo.as_deref())?;
    let store = Store::open(layout.store_path(&config))
        .await
        .with_context(|| format!("opening the ledger at {}", config.store.path))?;

    let commit = repository.resolve("HEAD")?;
    if !repository.is_clean().unwrap_or(false) {
        // Health describes a commit. A dirty tree is not one.
        bail!(
            "the working tree has uncommitted changes, so `{}` does not describe the \
             files on disk. Commit or stash them first.",
            &commit[..7]
        );
    }

    // The world model must be for exactly this commit, never an ancestor.
    let world = store
        .world_model_for_commit(&config.repository.name, &commit)
        .await?
        .ok_or_else(|| {
            anyhow!(
                "no world model for commit {}; run `forge world build` first",
                &commit[..7]
            )
        })?;

    let evidence = store.health_run_evidence(EVIDENCE_LIMIT).await?;
    let snapshot_id = store.next_health_snapshot_id().await?;
    let report = RepositoryHealthBuilder::new().build(
        snapshot_id,
        &config.repository.name,
        &commit,
        &world,
        &evidence,
        &GitAncestry {
            repository: &repository,
        },
    )?;

    store.insert_health_snapshot(&report.snapshot).await?;
    store.append_health_events(&report.events).await?;
    let promoted = store
        .set_current_health_snapshot(&config.repository.name, &report.snapshot)
        .await?;

    println!(
        "Forge repository health {}\n",
        report.snapshot.health_snapshot_id
    );
    print_header(&report.snapshot);
    println!("\nDimensions");
    println!("{}", dimension_table(&report.snapshot));
    println!("\nStatus\n  {}", report.snapshot.status);

    if !report.excluded.is_empty() {
        println!(
            "\n{}",
            output::section(
                "Excluded evidence",
                output::bullets(
                    report
                        .excluded
                        .iter()
                        .map(|item| format!("{}: {}", item.run_id, item.reason))
                )
            )
        );
    }
    if !promoted {
        println!("\nNote: the current-health pointer was left unchanged.");
    }

    Ok(
        if report.snapshot.status == HealthSnapshotStatus::Complete {
            HealthBuildExit::Complete
        } else {
            HealthBuildExit::NotComplete
        },
    )
}

/// Shows one snapshot's raw measurements and provenance.
pub async fn show(repo: Option<PathBuf>, id: Option<HealthSnapshotId>) -> Result<()> {
    let (_repository, layout, config) = resolve_repository(repo.as_deref())?;
    let store = Store::open(layout.store_path(&config)).await?;
    let snapshot = resolve_snapshot(&store, &config.repository.name, id).await?;

    println!("Forge repository health {}\n", snapshot.health_snapshot_id);
    print_header(&snapshot);

    for kind in HealthDimensionKind::ALL {
        let Some(dimension) = snapshot.dimension(kind) else {
            continue;
        };
        println!("\n{} — {}", kind.label(), dimension.status);
        for note in &dimension.notes {
            println!("  {note}");
        }
        for measurement in &dimension.measurements {
            println!(
                "  {:<34} {:>14}  {}",
                measurement.identity.label(),
                measurement.display_value(),
                measurement.scope.describe()
            );
            println!(
                "  {:<34} {:>14}  {} evidence reference{}",
                "",
                format!("[{}]", measurement.identity.direction),
                measurement.evidence.len(),
                if measurement.evidence.len() == 1 {
                    ""
                } else {
                    "s"
                }
            );
        }
    }

    let missing: Vec<String> = snapshot
        .dimensions
        .iter()
        .filter(|dimension| dimension.status == DimensionStatus::Unavailable)
        .map(|dimension| dimension.kind.label().to_string())
        .collect();
    if !missing.is_empty() {
        println!(
            "\n{}",
            output::section("Not measurable here", output::bullets(&missing))
        );
    }
    Ok(())
}

/// Compares two snapshots, defaulting to the nearest prior ancestor.
pub async fn diff(
    repo: Option<PathBuf>,
    from: Option<HealthSnapshotId>,
    to: Option<HealthSnapshotId>,
) -> Result<()> {
    let (repository, layout, config) = resolve_repository(repo.as_deref())?;
    let store = Store::open(layout.store_path(&config)).await?;

    let target = resolve_snapshot(&store, &config.repository.name, to).await?;
    let baseline = match from {
        Some(id) => store
            .load_health_snapshot(&id)
            .await?
            .ok_or_else(|| anyhow!("no health snapshot {id}"))?,
        None => {
            // Default: nearest earlier snapshot on the same ancestry chain.
            let history = store
                .health_snapshots(&config.repository.name, TREND_LIMIT)
                .await?;
            let candidates: Vec<(RepositoryHealthSnapshot, SnapshotRelation)> = history
                .into_iter()
                .map(|snapshot| {
                    let relation = snapshot_relation(&repository, &snapshot.commit, &target.commit);
                    (snapshot, relation)
                })
                .collect();
            forge_health::nearest_ancestor_baseline(&target, &candidates)
                .cloned()
                .ok_or_else(|| {
                    anyhow!(
                        "no earlier health snapshot on the same ancestry chain as {}; \
                         pass two snapshot ids explicitly to compare across branches",
                        target.health_snapshot_id
                    )
                })?
        }
    };

    let relation = snapshot_relation(&repository, &baseline.commit, &target.commit);
    let materiality = MaterialityPolicy::default();
    let diff = forge_health::diff(&baseline, &target, relation, &materiality);

    println!(
        "Forge health diff {} → {}\n",
        diff.from_snapshot_id, diff.to_snapshot_id
    );
    println!(
        "{}",
        output::fields(&[
            (
                "From",
                format!("{} @ {}", diff.from_snapshot_id, short(&diff.from_commit))
            ),
            (
                "To",
                format!("{} @ {}", diff.to_snapshot_id, short(&diff.to_commit))
            ),
            ("Ancestry", describe_relation(diff.relation).to_string()),
            ("Algorithm", diff.algorithm_version.clone()),
        ])
    );

    if !diff.is_chronological() {
        println!(
            "\nNote: these commits are not on one ancestry chain. The comparison is\n\
             structural; it does not describe an evolution over time."
        );
    }

    for (title, changes) in [
        ("Improvements", &diff.improvements),
        ("Regressions", &diff.regressions),
        ("Changes", &diff.neutral_changes),
        ("Newly available", &diff.newly_available),
        ("No longer available", &diff.no_longer_available),
    ] {
        if changes.is_empty() {
            continue;
        }
        println!("\n{title}");
        for change in changes {
            let material = if change.material { "  [material]" } else { "" };
            println!(
                "  {:<34} {:<28} {}{material}",
                change.identity.label(),
                change.describe(),
                change.classification
            );
        }
    }

    if diff.is_empty() {
        println!("\nNo comparable measurements changed.");
    }
    Ok(())
}

/// Reports per-dimension trends across the repository's snapshot history.
pub async fn trend(repo: Option<PathBuf>) -> Result<()> {
    let (repository, layout, config) = resolve_repository(repo.as_deref())?;
    let store = Store::open(layout.store_path(&config)).await?;

    let all = store
        .health_snapshots(&config.repository.name, TREND_LIMIT)
        .await?;
    if all.is_empty() {
        println!("No health snapshots recorded. Run `forge health build` first.");
        return Ok(());
    }

    // Only snapshots on the current commit's ancestry chain form a chronology.
    let head = repository.resolve("HEAD")?;
    let series: Vec<RepositoryHealthSnapshot> = all
        .into_iter()
        .filter(|snapshot| {
            matches!(
                snapshot_relation(&repository, &snapshot.commit, &head),
                SnapshotRelation::Exact | SnapshotRelation::Ancestor
            )
        })
        .collect();

    let trends = forge_health::trends(
        &config.repository.name,
        &series,
        &MaterialityPolicy::default(),
    );

    println!("Repository trend\n");
    if trends.dimensions.is_empty() {
        println!("  no comparable measurements yet");
    } else {
        let rows: Vec<Vec<String>> = trends
            .dimensions
            .iter()
            .map(|(kind, direction)| vec![kind.label().to_string(), direction.to_string()])
            .collect();
        println!("{}", output::table(&["dimension", "trend"], &rows));
    }

    println!("\nOverall\n  {}", trends.overall);
    if trends.overall == TrendDirection::InsufficientData {
        println!("  (fewer than the minimum comparable measurements per series)");
    }

    println!(
        "\n{}",
        output::fields(&[
            (
                "Evidence window",
                format!(
                    "{} health snapshot{}",
                    trends.snapshots_considered,
                    if trends.snapshots_considered == 1 {
                        ""
                    } else {
                        "s"
                    }
                )
            ),
            ("Algorithm", trends.algorithm_version.clone()),
        ])
    );

    // Per-series evidence, so no direction is asserted without its basis.
    for series in &trends.trends {
        println!("\n{}  {}", series.identity.label(), series.direction);
        if let Some(percent) = series.percent_change {
            println!(
                "  net change {}{:.1}%",
                if percent >= 0.0 { "+" } else { "" },
                percent
            );
        }
        println!("  {}", series.evidence);
        for point in &series.points {
            println!(
                "    {}  {:>12}  {}",
                point.health_snapshot_id,
                forge_core::health::format_number(point.value),
                short(&point.commit)
            );
        }
    }
    Ok(())
}

async fn resolve_snapshot(
    store: &Store,
    repository: &str,
    id: Option<HealthSnapshotId>,
) -> Result<RepositoryHealthSnapshot> {
    match id {
        Some(id) => store
            .load_health_snapshot(&id)
            .await?
            .ok_or_else(|| anyhow!("no health snapshot {id}")),
        None => store
            .current_health_snapshot(repository)
            .await?
            .ok_or_else(|| anyhow!("no health snapshot recorded; run `forge health build` first")),
    }
}

fn print_header(snapshot: &RepositoryHealthSnapshot) {
    println!(
        "{}",
        output::fields(&[
            ("Repository", snapshot.repository.clone()),
            ("Commit", short(&snapshot.commit)),
            ("World model", snapshot.world_model_snapshot_id.to_string()),
            (
                "World model status",
                snapshot.provenance.world_model_status_label()
            ),
            ("Status", snapshot.status.to_string()),
            ("Builder", snapshot.provenance.builder_version.clone()),
            (
                "Runs considered",
                snapshot.provenance.runs_considered.to_string()
            ),
        ])
    );
}

fn dimension_table(snapshot: &RepositoryHealthSnapshot) -> String {
    let rows: Vec<Vec<String>> = HealthDimensionKind::ALL
        .iter()
        .filter_map(|kind| snapshot.dimension(*kind))
        .map(|dimension| {
            vec![
                dimension.kind.label().to_string(),
                dimension.status.to_string(),
                if dimension.measurements.is_empty() {
                    dimension.notes.first().cloned().unwrap_or_default()
                } else {
                    format!(
                        "{} measurement{}",
                        dimension.measurements.len(),
                        if dimension.measurements.len() == 1 {
                            ""
                        } else {
                            "s"
                        }
                    )
                },
            ]
        })
        .collect();
    output::table(&["dimension", "availability", ""], &rows)
}

fn describe_relation(relation: SnapshotRelation) -> &'static str {
    match relation {
        SnapshotRelation::Exact => "same commit",
        SnapshotRelation::Ancestor => "ancestor → descendant",
        SnapshotRelation::Stale => "diverged",
        SnapshotRelation::UnknownRelation => "unknown",
    }
}

fn short(commit: &str) -> String {
    commit.chars().take(7).collect()
}
