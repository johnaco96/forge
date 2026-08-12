//! `forge world` — deterministic repository world-model lifecycle.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Context, Result, anyhow, bail};
use forge_core::ids::WorldModelSnapshotId;
use forge_core::world::{
    SnapshotRelation, WorldModelProvenanceSource, WorldModelSnapshot, WorldModelSnapshotStatus,
    WorldQueryKind,
};
use forge_store::{Store, WorldModelQuery};
use forge_world::{WorldModelBuilder, snapshot_relation};

use crate::WorldQueryKindArg;
use crate::commands::run::resolve_repository;
use crate::output;

pub enum WorldBuildExit {
    Complete,
    NotComplete,
}

pub async fn build(repo: Option<PathBuf>) -> Result<WorldBuildExit> {
    let (repository, layout, config) = resolve_repository(repo.as_deref())?;
    if !config.world_model.enabled {
        bail!("world-model extraction is disabled in `.forge/config.toml`");
    }
    let store = Store::open(layout.store_path(&config))
        .await
        .with_context(|| format!("opening the ledger at {}", config.store.path))?;
    let commit = repository.resolve("HEAD")?;
    let snapshot_id = store.next_world_model_snapshot_id().await?;
    let report = WorldModelBuilder::from_config(&config.world_model)?
        .build(
            snapshot_id,
            &repository,
            &config.repository.name,
            &commit,
            &store,
        )
        .await?;
    store.insert_world_model_snapshot(&report.snapshot).await?;
    store.append_world_model_events(&report.events).await?;

    println!("Forge world model {}\n", report.snapshot.snapshot_id);
    print_snapshot(&report.snapshot, SnapshotRelation::Exact, true);
    Ok(
        if report.snapshot.status == WorldModelSnapshotStatus::Complete {
            WorldBuildExit::Complete
        } else {
            WorldBuildExit::NotComplete
        },
    )
}

pub async fn show(repo: Option<PathBuf>, snapshot_id: Option<WorldModelSnapshotId>) -> Result<()> {
    let (repository, layout, config) = resolve_repository(repo.as_deref())?;
    let store = Store::open(layout.store_path(&config)).await?;
    let snapshot = if let Some(snapshot_id) = snapshot_id {
        store
            .load_world_model_snapshot(&snapshot_id)
            .await?
            .ok_or_else(|| anyhow!("world-model snapshot `{snapshot_id}` does not exist"))?
    } else {
        store
            .current_world_model(&config.repository.name)
            .await?
            .ok_or_else(|| anyhow!("no current world model; run `forge world build`"))?
    };
    let head = repository.resolve("HEAD")?;
    let relation = snapshot_relation(&repository, &snapshot.commit, &head);
    let is_current = store
        .current_world_model(&config.repository.name)
        .await?
        .is_some_and(|current| current.snapshot_id == snapshot.snapshot_id);
    println!("Forge world model {}\n", snapshot.snapshot_id);
    print_snapshot(&snapshot, relation, is_current);
    Ok(())
}

pub async fn query(
    repo: Option<PathBuf>,
    snapshot_id: Option<WorldModelSnapshotId>,
    kind: WorldQueryKindArg,
    term: Option<String>,
    limit: u32,
) -> Result<()> {
    let (repository, layout, config) = resolve_repository(repo.as_deref())?;
    let store = Store::open(layout.store_path(&config)).await?;
    let snapshot = if let Some(snapshot_id) = snapshot_id {
        store
            .load_world_model_snapshot(&snapshot_id)
            .await?
            .ok_or_else(|| anyhow!("world-model snapshot `{snapshot_id}` does not exist"))?
    } else {
        store
            .current_world_model(&config.repository.name)
            .await?
            .ok_or_else(|| anyhow!("no current world model; run `forge world build`"))?
    };
    let head = repository.resolve("HEAD")?;
    let relation = snapshot_relation(&repository, &snapshot.commit, &head);
    let mut request = WorldModelQuery::new(snapshot.snapshot_id.clone(), kind.into());
    request.term = term;
    request.limit = limit;
    let facts = store.query_world_model(&request).await?;

    println!("Forge world query\n");
    println!(
        "{}",
        output::fields(&[
            ("Snapshot", snapshot.snapshot_id.to_string()),
            ("Commit", short(&snapshot.commit)),
            ("Relation to HEAD", relation_name(relation).into()),
            ("Matches", facts.len().to_string()),
        ])
    );
    if !facts.is_empty() {
        println!("\nFacts");
        let rows = facts
            .iter()
            .map(|fact| {
                vec![
                    fact.kind().as_str().to_string(),
                    fact.id().to_string(),
                    fact.metadata().confidence.as_str().to_string(),
                    fact.display_name(),
                ]
            })
            .collect::<Vec<_>>();
        println!(
            "{}",
            output::table(&["kind", "id", "evidence", "summary"], &rows)
        );
    }
    Ok(())
}

fn print_snapshot(snapshot: &WorldModelSnapshot, relation: SnapshotRelation, is_current: bool) {
    let summary = snapshot.summary();
    println!(
        "{}",
        output::fields(&[
            ("Repository", snapshot.repository.clone()),
            ("Commit", snapshot.commit.clone()),
            ("Created", snapshot.created_at.to_rfc3339()),
            ("Schema", snapshot.schema_version.clone()),
            ("Status", status_name(snapshot.status).into()),
            ("Current pointer", yes_no(is_current).into()),
            ("Relation to HEAD", relation_name(relation).into()),
        ])
    );
    println!("\nExtractors");
    let extractor_rows = snapshot
        .extractors
        .iter()
        .map(|extractor| {
            vec![
                extractor.identity.name.clone(),
                extractor.identity.version.clone(),
                if extractor.required { "yes" } else { "no" }.into(),
                format!("{:?}", extractor.status).to_ascii_lowercase(),
                extractor.facts_produced.to_string(),
            ]
        })
        .collect::<Vec<_>>();
    println!(
        "{}",
        output::table(
            &["extractor", "version", "required", "status", "facts"],
            &extractor_rows
        )
    );
    println!("\nFacts");
    println!(
        "{}",
        output::fields(&[
            ("Components", summary.components.to_string()),
            ("Modules", summary.modules.to_string()),
            ("Interfaces", summary.interfaces.to_string()),
            ("Contracts", summary.contracts.to_string()),
            ("Invariants", summary.invariants.to_string()),
            ("Dependencies", summary.dependencies.to_string()),
            ("Ownership", summary.ownership.to_string()),
            (
                "Performance constraints",
                summary.performance_constraints.to_string()
            ),
            (
                "Historical decisions",
                summary.historical_decisions.to_string()
            ),
            (
                "Known failure modes",
                summary.known_failure_modes.to_string()
            ),
            ("Total", summary.total().to_string()),
        ])
    );
    let provenance = provenance_summary(snapshot);
    println!("\nEvidence provenance");
    println!(
        "{}",
        output::table(
            &["source", "references"],
            &provenance
                .into_iter()
                .map(|(source, count)| vec![source, count.to_string()])
                .collect::<Vec<_>>()
        )
    );
}

fn provenance_summary(snapshot: &WorldModelSnapshot) -> BTreeMap<String, u64> {
    let mut counts = BTreeMap::new();
    for record in snapshot.facts.records() {
        for provenance in &record.metadata().provenance {
            let source = match &provenance.source {
                WorldModelProvenanceSource::SourceCode { .. } => "source_code",
                WorldModelProvenanceSource::RepositoryDocument { .. } => "repository_document",
                WorldModelProvenanceSource::Configuration { .. } => "configuration",
                WorldModelProvenanceSource::Test { .. } => "test",
                WorldModelProvenanceSource::Evaluator { .. } => "evaluator",
                WorldModelProvenanceSource::HistoricalRun { .. } => "historical_run",
                WorldModelProvenanceSource::CommitHistory { .. } => "commit_history",
                WorldModelProvenanceSource::UserDeclared { .. } => "user_declared",
                WorldModelProvenanceSource::Imported { .. } => "imported",
                WorldModelProvenanceSource::AgentInferred { .. } => "agent_inferred",
            };
            *counts.entry(source.into()).or_insert(0) += 1;
        }
    }
    counts
}

impl From<WorldQueryKindArg> for WorldQueryKind {
    fn from(kind: WorldQueryKindArg) -> Self {
        match kind {
            WorldQueryKindArg::All => Self::All,
            WorldQueryKindArg::Component => Self::Component,
            WorldQueryKindArg::Module => Self::Module,
            WorldQueryKindArg::Interface => Self::Interface,
            WorldQueryKindArg::Contract => Self::Contract,
            WorldQueryKindArg::Invariant => Self::Invariant,
            WorldQueryKindArg::Dependency => Self::Dependency,
            WorldQueryKindArg::Ownership => Self::Ownership,
            WorldQueryKindArg::PerformanceConstraint => Self::PerformanceConstraint,
            WorldQueryKindArg::HistoricalDecision => Self::HistoricalDecision,
            WorldQueryKindArg::KnownFailureMode => Self::KnownFailureMode,
        }
    }
}

fn status_name(status: WorldModelSnapshotStatus) -> &'static str {
    match status {
        WorldModelSnapshotStatus::Complete => "complete",
        WorldModelSnapshotStatus::Partial => "partial",
        WorldModelSnapshotStatus::Failed => "failed",
    }
}

fn relation_name(relation: SnapshotRelation) -> &'static str {
    match relation {
        SnapshotRelation::Exact => "exact",
        SnapshotRelation::Ancestor => "ancestor",
        SnapshotRelation::Stale => "stale",
        SnapshotRelation::UnknownRelation => "unknown_relation",
    }
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

fn short(commit: &str) -> String {
    commit.chars().take(12).collect()
}
