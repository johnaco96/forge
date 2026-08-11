//! Phase 3's read-only experience-ledger commands.

use std::path::PathBuf;

use anyhow::{Context, Result};
use forge_core::ids::TaskId;
use forge_store::{FailureFilter, HistoryFilter, Store};

use crate::commands::run::{format_duration, resolve_repository, short, thousands};
use crate::output;

async fn open(repo: Option<PathBuf>) -> Result<Store> {
    let (_, layout, config) = resolve_repository(repo.as_deref())?;
    Store::open(layout.store_path(&config))
        .await
        .with_context(|| format!("opening the ledger at {}", config.store.path))
}

pub async fn history(repo: Option<PathBuf>, filter: HistoryFilter) -> Result<()> {
    let entries = open(repo).await?.history(&filter).await?;
    if entries.is_empty() {
        println!("No runs matched the requested history filters.");
        return Ok(());
    }
    let rows = entries
        .iter()
        .map(|entry| {
            vec![
                entry.run_id.to_string(),
                entry.task_id.to_string(),
                short_revision(&entry.task_revision_id),
                entry.agent_id.clone(),
                entry
                    .outcome
                    .map_or_else(|| "unresolved".into(), |value| value.describe().into()),
                entry
                    .classification
                    .category
                    .clone()
                    .unwrap_or_else(|| "unclassified".into()),
                duration_ms(entry.duration_ms),
                entry.repository.clone(),
                timestamp(entry.created_at),
            ]
        })
        .collect::<Vec<_>>();
    println!(
        "{}",
        output::table(
            &[
                "run",
                "task",
                "revision",
                "agent",
                "outcome",
                "category",
                "duration",
                "repository",
                "created",
            ],
            &rows,
        )
    );
    Ok(())
}

pub async fn agent_stats(repo: Option<PathBuf>, agent_id: String) -> Result<()> {
    let stats = open(repo).await?.agent_statistics(&agent_id).await?;
    println!("Agent statistics: {}\n", stats.agent_id);
    println!(
        "{}",
        output::fields(&[
            ("Total runs", stats.total_runs.to_string()),
            ("PASS", stats.passed.to_string()),
            ("FAIL", stats.failed.to_string()),
            ("INCONCLUSIVE", stats.inconclusive.to_string()),
            ("NO CHANGE", stats.no_change.to_string()),
            ("ERROR", stats.errored.to_string()),
            ("Unresolved", stats.unresolved.to_string()),
            (
                "Pass rate",
                format!("{:.1}% (PASS / all runs)", stats.pass_rate * 100.0)
            ),
            (
                "Agent runtime",
                reported_median_ms(
                    stats.median_runtime_ms,
                    stats.runtime_samples,
                    stats.total_runs,
                ),
            ),
            (
                "Provider tokens",
                reported_median_u64(
                    stats.median_provider_reported_tokens,
                    stats.token_samples,
                    stats.total_runs,
                    "tokens",
                ),
            ),
            (
                "Provider cost",
                cost_summary(
                    stats.known_cost_total_usd,
                    stats.median_known_cost_usd,
                    stats.cost_samples,
                    stats.total_runs,
                ),
            ),
            (
                "Patch size",
                reported_median_u64(
                    stats.median_patch_lines,
                    stats.patch_samples,
                    stats.total_runs,
                    "lines",
                ),
            ),
            (
                "Integrity violations",
                stats.integrity_violations.to_string()
            ),
            ("Unclassified runs", stats.unclassified_runs.to_string()),
        ])
    );

    print_cohorts("Category breakdown", &stats.by_category);
    print_cohorts("Component breakdown", &stats.by_component);
    Ok(())
}

pub async fn failures(repo: Option<PathBuf>, filter: FailureFilter) -> Result<()> {
    let summaries = open(repo).await?.failures(&filter).await?;
    if summaries.is_empty() {
        println!("No non-passing runs matched the requested filters.");
        return Ok(());
    }
    for (index, summary) in summaries.iter().enumerate() {
        if index > 0 {
            println!();
        }
        println!(
            "{}  {}  {}  {}",
            summary.run_id,
            summary.outcome.describe(),
            summary.agent_id,
            summary.task_id
        );
        println!(
            "{}",
            output::fields(&[
                ("Repository", summary.repository.clone()),
                ("Task revision", summary.task_revision_id.to_string(),),
                (
                    "Failure reason",
                    summary
                        .failure_reason
                        .clone()
                        .unwrap_or_else(|| "none recorded".into()),
                ),
                (
                    "Category",
                    summary
                        .category
                        .clone()
                        .unwrap_or_else(|| "unclassified".into()),
                ),
                (
                    "Components",
                    if summary.components.is_empty() {
                        "none".into()
                    } else {
                        summary.components.join(", ")
                    },
                ),
                ("Duration", duration_ms(summary.duration_ms)),
                ("Base commit", short(&summary.base_commit)),
                (
                    "Candidate commit",
                    summary
                        .candidate_commit
                        .as_deref()
                        .map(short)
                        .unwrap_or_else(|| "unavailable".into()),
                ),
                (
                    "Integrity",
                    summary
                        .integrity
                        .as_ref()
                        .map(|value| value.status.to_string())
                        .unwrap_or_else(|| "unavailable".into()),
                ),
                (
                    "Integrity violations",
                    summary
                        .integrity
                        .as_ref()
                        .map(|integrity| {
                            let violations = integrity.violations();
                            if violations.is_empty() {
                                "none".into()
                            } else {
                                violations.join(", ")
                            }
                        })
                        .unwrap_or_else(|| "unavailable".into()),
                ),
                (
                    "Warnings",
                    if summary.warnings.is_empty() {
                        "none".into()
                    } else {
                        summary
                            .warnings
                            .iter()
                            .map(|warning| match &warning.path {
                                Some(path) => {
                                    format!("{} ({path}): {}", warning.kind, warning.detail)
                                }
                                None => format!("{}: {}", warning.kind, warning.detail),
                            })
                            .collect::<Vec<_>>()
                            .join("; ")
                    },
                ),
                (
                    "Artifacts",
                    if summary.artifact_paths.is_empty() {
                        "none".into()
                    } else {
                        summary
                            .artifact_paths
                            .iter()
                            .map(|path| path.display().to_string())
                            .collect::<Vec<_>>()
                            .join(", ")
                    },
                ),
                ("Created", timestamp(summary.created_at)),
            ])
        );
        let evaluators = summary
            .failed_evaluators
            .iter()
            .map(|evaluator| {
                vec![
                    evaluator.evaluator_id.clone(),
                    evaluator.kind.to_string(),
                    evaluator.verdict.to_string(),
                    evaluator.execution_status.as_str().to_string(),
                    evaluator
                        .execution_error
                        .clone()
                        .unwrap_or_else(|| "none".into()),
                ]
            })
            .collect::<Vec<_>>();
        if evaluators.is_empty() {
            println!("\n  Failed evaluators: unavailable");
        } else {
            println!(
                "\n{}",
                output::section(
                    "Failed evaluators",
                    output::table(
                        &["evaluator", "kind", "verdict", "status", "error"],
                        &evaluators,
                    ),
                )
            );
        }
    }
    Ok(())
}

pub async fn similar(repo: Option<PathBuf>, task_id: TaskId, limit: u32) -> Result<()> {
    let matches = open(repo).await?.similar_tasks(&task_id, limit).await?;
    println!("Tasks similar to {task_id}");
    if matches.is_empty() {
        println!("\nNo tasks had a positive structured similarity score.");
        return Ok(());
    }
    for candidate in matches {
        println!(
            "\n{}  revision {}  score {:.3}  {}",
            candidate.task.task_id,
            short_revision(&candidate.task.revision_id),
            candidate.score,
            candidate.task.objective.lines().next().unwrap_or_default()
        );
        println!(
            "  Category: {}  Domain: {}  Components: {}",
            candidate
                .task
                .classification
                .category
                .as_deref()
                .unwrap_or("unclassified"),
            candidate
                .task
                .classification
                .domain
                .as_deref()
                .unwrap_or("unclassified"),
            if candidate.task.components.is_empty() {
                "none".into()
            } else {
                candidate.task.components.join(", ")
            },
        );
        println!("  Matched: {}", candidate.matched.join(", "));
        if candidate.historical_outcomes.is_empty() {
            println!("  Historical outcomes: none");
        } else {
            for outcomes in candidate.historical_outcomes {
                println!(
                    "  Historical outcomes: {} — {} runs, {} PASS, {} FAIL, {} INCONCLUSIVE, {} NO CHANGE, {} ERROR",
                    outcomes.agent_id,
                    outcomes.total_runs,
                    outcomes.passed,
                    outcomes.failed,
                    outcomes.inconclusive,
                    outcomes.no_change,
                    outcomes.errored,
                );
            }
        }
    }
    Ok(())
}

pub async fn experiments(repo: Option<PathBuf>, limit: u32) -> Result<()> {
    let entries = open(repo).await?.experiment_history(limit).await?;
    if entries.is_empty() {
        println!("No experiments are recorded in this ledger.");
        return Ok(());
    }
    let rows = entries
        .iter()
        .map(|entry| {
            vec![
                entry.experiment_id.to_string(),
                entry.task_id.to_string(),
                entry.status.as_str().into(),
                entry.participants.join(","),
                entry
                    .runs
                    .iter()
                    .map(|run| {
                        format!(
                            "{}:{}",
                            run.agent_id,
                            run.outcome.map_or("unresolved", |value| value.describe())
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(","),
                duration_ms(entry.duration_ms),
                short(&entry.base_commit),
                timestamp(entry.created_at),
            ]
        })
        .collect::<Vec<_>>();
    println!(
        "{}",
        output::table(
            &[
                "experiment",
                "task",
                "status",
                "agents",
                "outcomes",
                "duration",
                "base",
                "created",
            ],
            &rows,
        )
    );
    Ok(())
}

pub async fn export_jsonl(repo: Option<PathBuf>) -> Result<()> {
    for record in open(repo).await?.export_records().await? {
        println!("{}", serde_json::to_string(&record)?);
    }
    Ok(())
}

fn print_cohorts(title: &str, cohorts: &[forge_store::CohortStatistics]) {
    if cohorts.is_empty() {
        return;
    }
    let rows = cohorts
        .iter()
        .map(|cohort| {
            vec![
                cohort.value.clone(),
                cohort.total_runs.to_string(),
                cohort.passed.to_string(),
                format!("{:.1}%", cohort.pass_rate * 100.0),
            ]
        })
        .collect::<Vec<_>>();
    println!(
        "\n{}",
        output::section(
            title,
            output::table(&["value", "runs", "pass", "pass rate"], &rows),
        )
    );
}

fn reported_median_ms(value: Option<u64>, samples: u64, total: u64) -> String {
    match value {
        Some(value) => format!(
            "{} median ({samples} of {total} runs reported)",
            duration_ms(Some(value))
        ),
        None => format!("unavailable (0 of {total} runs reported)"),
    }
}

fn reported_median_u64(value: Option<u64>, samples: u64, total: u64, unit: &str) -> String {
    match value {
        Some(value) => format!(
            "{} {unit} median ({samples} of {total} runs reported)",
            thousands(value)
        ),
        None => format!("unavailable (0 of {total} runs reported)"),
    }
}

fn cost_summary(total: Option<f64>, median: Option<f64>, samples: u64, runs: u64) -> String {
    match (total, median) {
        (Some(total), Some(median)) => format!(
            "known total ${total:.4}; known median ${median:.4} ({samples} of {runs} runs reported)"
        ),
        _ => format!("unavailable (0 of {runs} runs reported)"),
    }
}

fn duration_ms(value: Option<u64>) -> String {
    format_duration(
        value
            .and_then(|value| i64::try_from(value).ok())
            .and_then(chrono::TimeDelta::try_milliseconds),
    )
}

fn timestamp(value: chrono::DateTime<chrono::Utc>) -> String {
    value.format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

fn short_revision(revision_id: &forge_store::TaskRevisionId) -> String {
    revision_id.as_str().chars().take(15).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_measurements_are_never_rendered_as_zero() {
        assert_eq!(
            reported_median_u64(None, 0, 4, "tokens"),
            "unavailable (0 of 4 runs reported)"
        );
        assert_eq!(
            cost_summary(None, None, 0, 4),
            "unavailable (0 of 4 runs reported)"
        );
    }
}
