//! `forge compete` — independent runs from one base, compared dimensionally.

use std::collections::{BTreeSet, HashSet};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Result, bail};
use forge_agent::AgentRegistry;
use forge_core::experiment::ComparisonRelation;
use forge_core::result::Metric;
use forge_core::run::RunOutcome;
use forge_core::task::EngineeringTask;
use forge_runner::{
    Competitor, ExperimentReport, ExperimentRequest, RunReport, RunRequest, Runner,
};
use forge_store::Store;

use crate::commands::run::{format_duration, resolve_repository, short, summarize, thousands};
use crate::output;

pub struct CompeteArgs {
    pub task_path: PathBuf,
    pub repo: Option<PathBuf>,
    pub agents: String,
    pub base: Option<String>,
    pub timeout_secs: Option<u64>,
    pub keep_workspace: bool,
}

pub enum CompeteExit {
    AllPassed,
    SomeNotPassed,
}

pub async fn run(args: CompeteArgs) -> Result<CompeteExit> {
    let (repository, layout, config) = resolve_repository(args.repo.as_deref())?;

    // The task and full participant list are validated before any adapter is
    // prepared or any experiment is created.
    let task = EngineeringTask::load(&args.task_path)?;
    task.validate()?;
    let agent_ids = parse_agents(&args.agents)?;
    let registry = AgentRegistry::builtin();
    for agent_id in &agent_ids {
        if registry.get(agent_id).is_none() {
            bail!("unknown agent `{agent_id}`; run `forge agent list` to see the available agents");
        }
    }

    let store = Store::open(layout.store_path(&config)).await?;
    let runner = Runner::new(repository, config, store);

    // Provider selection ends here. The runner sees only trait objects and
    // drives every one through the same run pipeline.
    let mut adapters = Vec::with_capacity(agent_ids.len());
    for agent_id in &agent_ids {
        let request = RunRequest::new(task.clone(), agent_id);
        let agent_config = runner.agent_config(&request)?;
        adapters.push(registry.adapter(agent_id, &agent_config)?);
    }
    let competitors = agent_ids
        .iter()
        .zip(adapters.iter())
        .map(|(agent_id, adapter)| Competitor::new(agent_id, adapter.as_ref()))
        .collect();

    let mut request = ExperimentRequest::new(task.clone());
    request.base_rev = args.base;
    request.timeout = args.timeout_secs.map(Duration::from_secs);
    if args.keep_workspace {
        request.keep_workspace = Some(true);
    }

    let report = runner.compete(request, competitors).await?;
    print_report(&report, &task);

    Ok(
        if report
            .runs
            .iter()
            .all(|run| run.outcome() == RunOutcome::Passed)
        {
            CompeteExit::AllPassed
        } else {
            CompeteExit::SomeNotPassed
        },
    )
}

fn parse_agents(raw: &str) -> Result<Vec<String>> {
    let agents = raw
        .split(',')
        .map(str::trim)
        .filter(|agent| !agent.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    if agents.len() < 2 {
        bail!("--agents requires at least two comma-separated agents");
    }
    let mut seen = HashSet::new();
    for agent in &agents {
        if !seen.insert(agent.clone()) {
            bail!("duplicate agent `{agent}` in --agents");
        }
    }
    Ok(agents)
}

fn print_report(report: &ExperimentReport, task: &EngineeringTask) {
    println!("Experiment {}\n", report.experiment.experiment_id);
    println!(
        "{}",
        output::fields(&[
            (
                "Task",
                format!("{}  {}", task.task_id, summarize(&task.objective)),
            ),
            ("Repository", report.experiment.repository.clone()),
            ("Base commit", short(&report.experiment.base_commit)),
            ("Execution", report.execution_strategy.to_string()),
            (
                "Recorded",
                format!(
                    "{} runs, {} experiment events",
                    report.runs.len(),
                    report.experiment_events_recorded
                ),
            ),
        ])
    );

    let headers = std::iter::once("dimension")
        .chain(
            report
                .runs
                .iter()
                .map(|run| run.run.agent.agent_id.as_str()),
        )
        .collect::<Vec<_>>();
    let rows = result_rows(&report.runs);
    println!("\nResults\n{}", output::table(&headers, &rows));

    println!("\nComparison");
    if let Some(comparison) = &report.experiment.comparison {
        for dimension in &comparison.dimensions {
            let result = comparison_result(dimension, report);
            let result = dimension
                .note
                .as_ref()
                .map(|note| format!("{result} ({note})"))
                .unwrap_or(result);
            println!("{}", output::fields(&[(&dimension.key.label(), result)]));
        }
    }
    println!(
        "{}",
        output::fields(&[("Overall", "No ranking policy configured".into())])
    );
}

fn result_rows(runs: &[RunReport]) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    add_row(&mut rows, "Outcome", runs, |run| {
        run.outcome().describe().to_string()
    });
    add_row(&mut rows, "Agent execution", runs, |run| {
        run.run
            .execution
            .as_ref()
            .map(|execution| execution.status.describe().to_string())
            .unwrap_or_else(|| "did not run".into())
    });
    add_row(&mut rows, "Integrity", runs, |run| {
        run.run
            .integrity
            .as_ref()
            .map(|integrity| integrity.status.describe().to_string())
            .unwrap_or_else(|| "not checked".into())
    });

    let checks: BTreeSet<_> = runs
        .iter()
        .filter_map(|run| run.evaluation.as_ref())
        .flat_map(|evaluation| evaluation.checks.iter().map(|check| check.name.clone()))
        .collect();
    for check_name in checks {
        add_row(&mut rows, &check_name, runs, |run| {
            run.evaluation
                .as_ref()
                .and_then(|evaluation| evaluation.check(&check_name))
                .map(|check| check.verdict.to_string())
                .unwrap_or_else(|| "missing".into())
        });
    }

    let metrics: BTreeSet<_> = runs
        .iter()
        .filter_map(|run| run.evaluation.as_ref())
        .flat_map(|evaluation| evaluation.metrics.iter())
        .filter(|metric| metric.source == "benchmark" && !metric.name.ends_with(".duration_ms"))
        .map(|metric| metric.name.clone())
        .collect();
    for metric_name in metrics {
        add_row(&mut rows, &metric_name, runs, |run| {
            run.evaluation
                .as_ref()
                .and_then(|evaluation| evaluation.metric(&metric_name))
                .map(format_metric)
                .unwrap_or_else(|| "missing".into())
        });
    }

    add_row(&mut rows, "Agent runtime", runs, |run| {
        format_duration(run.run.execution.as_ref().and_then(|execution| {
            chrono::TimeDelta::try_milliseconds(execution.duration_ms as i64)
        }))
    });
    add_row(&mut rows, "Tokens", runs, |run| {
        let usage = run.run.usage();
        match usage.total_tokens() {
            Some(total) => format!(
                "{} ({} in / {} out)",
                thousands(total),
                usage
                    .input_tokens
                    .map(thousands)
                    .unwrap_or_else(|| "?".into()),
                usage
                    .output_tokens
                    .map(thousands)
                    .unwrap_or_else(|| "?".into())
            ),
            None => "unavailable".into(),
        }
    });
    add_row(&mut rows, "Cost", runs, |run| {
        run.run
            .usage()
            .cost_usd
            .map(|cost| format!("${cost:.4}"))
            .unwrap_or_else(|| "unavailable".into())
    });
    add_row(&mut rows, "Patch files", runs, |run| {
        run.run
            .patch
            .as_ref()
            .map(|patch| patch.files_changed.to_string())
            .unwrap_or_else(|| "missing".into())
    });
    add_row(&mut rows, "Patch lines", runs, |run| {
        run.run
            .patch
            .as_ref()
            .map(|patch| format!("+{}/-{}", patch.insertions, patch.deletions))
            .unwrap_or_else(|| "missing".into())
    });
    add_row(&mut rows, "Warnings", runs, |run| {
        if run.run.patch.is_some() {
            run.run.warnings.len().to_string()
        } else {
            "missing".into()
        }
    });
    rows
}

fn add_row(
    rows: &mut Vec<Vec<String>>,
    label: &str,
    runs: &[RunReport],
    value: impl Fn(&RunReport) -> String,
) {
    rows.push(
        std::iter::once(label.to_string())
            .chain(runs.iter().map(value))
            .collect(),
    );
}

fn format_metric(metric: &Metric) -> String {
    format!(
        "{}{} ({})",
        metric.value,
        metric
            .unit
            .as_ref()
            .map(|unit| format!(" {unit}"))
            .unwrap_or_default(),
        metric.direction
    )
}

fn comparison_result(
    dimension: &forge_core::experiment::DimensionComparison,
    report: &ExperimentReport,
) -> String {
    let describe_pair = |pair: &forge_core::experiment::PairwiseComparison| {
        let left = agent_for_run(report, &pair.left_run_id);
        let right = agent_for_run(report, &pair.right_run_id);
        match pair.relation {
            ComparisonRelation::Better => left.to_string(),
            ComparisonRelation::Worse => right.to_string(),
            ComparisonRelation::Equal => "Tie".to_string(),
            ComparisonRelation::NotComparable => "Not comparable".to_string(),
            ComparisonRelation::Missing => "Missing".to_string(),
        }
    };
    if dimension.pairs.len() == 1 {
        return describe_pair(&dimension.pairs[0]);
    }
    dimension
        .pairs
        .iter()
        .map(|pair| {
            format!(
                "{} vs {}: {}",
                agent_for_run(report, &pair.left_run_id),
                agent_for_run(report, &pair.right_run_id),
                describe_pair(pair)
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}

fn agent_for_run<'a>(report: &'a ExperimentReport, run_id: &forge_core::RunId) -> &'a str {
    report
        .runs
        .iter()
        .find(|run| &run.run.run_id == run_id)
        .map(|run| run.run.agent.agent_id.as_str())
        .unwrap_or("unknown")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn participant_validation_accepts_more_than_two_but_rejects_duplicates() {
        assert_eq!(
            parse_agents("claude,codex,pi").unwrap(),
            vec!["claude", "codex", "pi"]
        );
        assert!(parse_agents("claude").is_err());
        assert!(parse_agents("claude,codex,claude").is_err());
        assert!(parse_agents("claude,,").is_err());
    }

    #[test]
    fn no_overall_comparison_key_exists() {
        let keys = [
            forge_core::experiment::ComparisonKey::Outcome,
            forge_core::experiment::ComparisonKey::Integrity,
            forge_core::experiment::ComparisonKey::Runtime,
        ];
        assert!(keys.iter().all(|key| key.label() != "Overall"));
    }
}
