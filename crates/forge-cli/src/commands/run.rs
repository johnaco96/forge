//! `forge run` — execute one task through one agent.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use forge_agent::AgentRegistry;
use forge_core::config::{ForgeConfig, Layout};
use forge_core::result::Verdict;
use forge_core::run::{PatchSummary, RunOutcome};
use forge_core::task::EngineeringTask;
use forge_git::Repository;
use forge_runner::{RunReport, RunRequest, Runner};
use forge_store::Store;

use crate::output;

pub struct RunArgs {
    pub task_path: PathBuf,
    pub repo: Option<PathBuf>,
    pub agent: Option<String>,
    pub base: Option<String>,
    pub timeout_secs: Option<u64>,
    pub keep_workspace: bool,
}

/// What the process exits with.
///
/// Distinguishing these matters for scripting: "Forge could not run this" and
/// "Forge ran it and the result did not pass" call for different responses.
pub enum RunExit {
    /// The change was produced and every check passed.
    Passed,
    /// The run completed but the outcome was not a pass.
    NotPassed,
}

pub async fn run(args: RunArgs) -> Result<RunExit> {
    let (repository, layout, config) = resolve_repository(args.repo.as_deref())?;

    let task = EngineeringTask::load(&args.task_path)?;
    task.validate()?;

    let agent_id = args
        .agent
        .clone()
        .or_else(|| config.defaults.agent.clone())
        .ok_or_else(|| {
            anyhow!("no agent specified and no `defaults.agent` configured; pass --agent <name>")
        })?;

    let registry = AgentRegistry::builtin();
    if registry.get(&agent_id).is_none() {
        bail!("unknown agent `{agent_id}`; run `forge agent list` to see the available agents");
    }

    let store = Store::open(layout.store_path(&config))
        .await
        .with_context(|| format!("opening the ledger at {}", config.store.path))?;
    let runner = Runner::new(repository, config.clone(), store);

    let mut request = RunRequest::new(task.clone(), &agent_id);
    request.base_rev = args.base.clone();
    request.timeout = args.timeout_secs.map(Duration::from_secs);
    if args.keep_workspace {
        request.keep_workspace = Some(true);
    }

    // The registry is the only place the CLI touches a specific agent: it hands
    // back an adapter, and the pipeline drives it through the trait.
    let agent_config = runner.agent_config(&request)?;
    let adapter = registry.adapter(&agent_id, &agent_config)?;

    let report = runner.execute(request, adapter.as_ref()).await?;
    print_report(&report, &task, &agent_id, &layout, &config);

    Ok(if report.outcome().is_success() {
        RunExit::Passed
    } else {
        RunExit::NotPassed
    })
}

fn resolve_repository(repo: Option<&Path>) -> Result<(Repository, Layout, ForgeConfig)> {
    let start = match repo {
        Some(path) => path.to_path_buf(),
        None => std::env::current_dir().context("resolving the current directory")?,
    };
    let repository = Repository::discover(&start)
        .map_err(|err| anyhow!("{err}\n\nRun `forge init` inside a Git repository first."))?;
    let layout = Layout::new(repository.root().to_path_buf());
    if !layout.is_initialized() {
        bail!(
            "`{}` has no Forge configuration; run `forge init` first",
            repository.root().display()
        );
    }
    let config = ForgeConfig::load(layout.config_path())?;
    Ok((repository, layout, config))
}

/// Prints the run report.
///
/// The layout keeps agent execution and Forge's evaluation in separate blocks
/// on purpose. They are different claims from different sources, and a reader
/// should never have to guess which one "completed" refers to.
fn print_report(
    report: &RunReport,
    task: &EngineeringTask,
    agent_id: &str,
    layout: &Layout,
    config: &ForgeConfig,
) {
    let run = &report.run;
    println!("Forge run {}\n", run.run_id);

    let workspace = match (&report.workspace_path, report.workspace_kept) {
        (Some(path), true) => format!("{} (kept)", relative(layout, path)),
        (Some(path), false) => format!("{} (removed)", relative(layout, path)),
        (None, _) => "none".to_string(),
    };

    println!(
        "{}",
        output::fields(&[
            (
                "Task",
                format!("{}  {}", task.task_id, summarize(&task.objective))
            ),
            ("Agent", format!("{agent_id} ({})", run.agent.harness)),
            ("Base commit", short(&run.base_commit)),
            (
                "Branch",
                report.branch.clone().unwrap_or_else(|| "none".to_string())
            ),
            ("Workspace", workspace),
        ])
    );

    print_agent_execution(report);
    print_security(report);
    print_patch(run.patch.as_ref(), layout);
    print_integrity(report);
    print_evaluation(report);
    print_warnings(report);

    println!("\nOverall");
    println!("  {}{}", report.outcome().describe(), outcome_note(report));

    if let Some(reason) = &run.failure_reason {
        println!("\nFailure\n  {reason}");
    }

    println!(
        "\n{}",
        output::fields(&[
            ("Duration", format_duration(run.total_duration())),
            (
                "Recorded",
                format!("{} ({} events)", config.store.path, report.events_recorded)
            ),
        ])
    );

    if report.base_was_dirty {
        println!(
            "\nNote: the working tree had uncommitted changes, which the agent did not\n\
             see. It worked from commit {} only.",
            short(&run.base_commit)
        );
    }
}

fn print_agent_execution(report: &RunReport) {
    println!("\nAgent execution");
    let Some(execution) = &report.run.execution else {
        println!("  did not run");
        return;
    };

    let mut rows = vec![
        ("Status", execution.status.describe().to_string()),
        (
            "Duration",
            format_duration(chrono::TimeDelta::try_milliseconds(
                execution.duration_ms as i64,
            )),
        ),
    ];
    if let Some(code) = execution.exit_code {
        rows.push(("Exit code", code.to_string()));
    }
    if let Some(total) = execution.usage.total_tokens() {
        rows.push((
            "Tokens",
            format!(
                "{} ({} in / {} out)",
                thousands(total),
                thousands(execution.usage.input_tokens.unwrap_or(0)),
                thousands(execution.usage.output_tokens.unwrap_or(0))
            ),
        ));
    }
    if let Some(cost) = execution.usage.cost_usd {
        rows.push(("Cost", format!("${cost:.4}")));
    }
    println!("{}", output::fields(&rows));
}

fn print_security(report: &RunReport) {
    println!("\nSecurity posture");
    let Some(posture) = &report.run.security else {
        println!("  not recorded");
        return;
    };
    println!("{}", output::fields(&posture.rows()));
    if let Some(warning) = posture.warning() {
        println!("\n  Warning: {}", warning.replace('\n', "\n  "));
    }
}

fn print_patch(patch: Option<&PatchSummary>, layout: &Layout) {
    println!("\nPatch");
    let Some(patch) = patch else {
        println!("  not captured");
        return;
    };
    if patch.is_empty() {
        println!("  no changes");
        if !patch.excluded.is_empty() {
            println!(
                "  {} workspace change{} excluded by patch policy",
                patch.excluded.len(),
                if patch.excluded.len() == 1 { "" } else { "s" }
            );
        }
        return;
    }
    println!(
        "  {} file{} changed, {} lines (+{} / -{}){}",
        patch.files_changed,
        if patch.files_changed == 1 { "" } else { "s" },
        patch.lines_changed(),
        patch.insertions,
        patch.deletions,
        if patch.binary_files > 0 {
            format!(", {} binary", patch.binary_files)
        } else {
            String::new()
        }
    );
    if let Some(diff) = &patch.diff_path {
        println!("  {}", relative(layout, diff));
    }
    if let Some(commit) = &patch.head_commit {
        println!("  committed as {}", short(commit));
    }
    if !patch.excluded.is_empty() {
        println!(
            "  {} workspace change{} excluded by patch policy",
            patch.excluded.len(),
            if patch.excluded.len() == 1 { "" } else { "s" }
        );
    }
}

fn print_integrity(report: &RunReport) {
    println!("\nEvaluation integrity");
    let Some(integrity) = &report.run.integrity else {
        println!("  not checked");
        return;
    };
    println!("  {}", integrity.summary());
    for path in integrity.violations() {
        println!("  - {path}");
    }
}

fn print_evaluation(report: &RunReport) {
    println!("\nEvaluation (run by Forge, not by the agent)");
    let Some(evaluation) = &report.evaluation else {
        println!("  no checks configured for this task");
        return;
    };

    let rows: Vec<Vec<String>> = evaluation
        .checks
        .iter()
        .map(|check| {
            let detail = match (check.verdict, check.exit_code) {
                (Verdict::Pass, _) => format!("{}ms", check.duration_ms),
                (_, Some(code)) => format!("exit {code}, {}ms", check.duration_ms),
                (_, None) => format!("{}ms", check.duration_ms),
            };
            vec![check.name.clone(), check.verdict.to_string(), detail]
        })
        .collect();

    println!("{}", output::table(&["check", "result", ""], &rows));

    for check in &evaluation.checks {
        if check.verdict != Verdict::Pass
            && let Some(detail) = &check.detail
        {
            println!("\n  {} failed:", check.name);
            for line in detail.lines().take(10) {
                println!("    {line}");
            }
        }
        for metric in &check.metrics {
            if !metric.name.ends_with(".duration_ms") {
                println!(
                    "  {}: {}{} ({})",
                    metric.name,
                    metric.value,
                    metric
                        .unit
                        .as_ref()
                        .map(|unit| format!(" {unit}"))
                        .unwrap_or_default(),
                    metric.direction
                );
            }
        }
    }
}

fn print_warnings(report: &RunReport) {
    if report.run.warnings.is_empty() {
        return;
    }
    println!("\nPatch warnings");
    for warning in &report.run.warnings {
        let path = warning
            .path
            .as_ref()
            .map(|path| format!(" [{path}]"))
            .unwrap_or_default();
        println!("  - {}{}: {}", warning.kind, path, warning.detail);
    }
}

/// A short explanation for outcomes a reader could otherwise misread.
fn outcome_note(report: &RunReport) -> &'static str {
    match report.outcome() {
        RunOutcome::NoChange => "  (the agent left the workspace unchanged)",
        RunOutcome::Inconclusive if report.evaluation.is_none() => {
            "  (nothing was measured; add an evaluation command to the task)"
        }
        RunOutcome::Inconclusive
            if report
                .run
                .integrity
                .as_ref()
                .is_some_and(|integrity| !integrity.is_acceptable()) =>
        {
            "  (evaluation integrity was compromised)"
        }
        RunOutcome::Inconclusive => "  (a check could not be executed)",
        RunOutcome::Errored => "  (Forge could not complete the run)",
        _ => "",
    }
}

fn relative(layout: &Layout, path: &Path) -> String {
    path.strip_prefix(layout.root())
        .unwrap_or(path)
        .display()
        .to_string()
}

fn short(commit: &str) -> String {
    commit.chars().take(7).collect()
}

/// A one-line summary of an objective, short enough not to wrap a terminal.
fn summarize(text: &str) -> String {
    const MAX: usize = 68;
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= MAX {
        return collapsed;
    }
    let truncated: String = collapsed.chars().take(MAX).collect();
    let cut = truncated.rfind(' ').unwrap_or(truncated.len());
    format!("{} …", &truncated[..cut])
}

fn thousands(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn format_duration(delta: Option<chrono::TimeDelta>) -> String {
    let Some(delta) = delta else {
        return "unknown".to_string();
    };
    let total = delta.num_seconds().max(0);
    let (minutes, seconds) = (total / 60, total % 60);
    if minutes >= 60 {
        format!("{}h {}m {}s", minutes / 60, minutes % 60, seconds)
    } else if minutes > 0 {
        format!("{minutes}m {seconds}s")
    } else if total > 0 {
        format!("{total}s")
    } else {
        format!("{}ms", delta.num_milliseconds().max(0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_read_naturally_at_every_scale() {
        let ms = |n: i64| chrono::TimeDelta::try_milliseconds(n);
        assert_eq!(format_duration(ms(450)), "450ms");
        assert_eq!(format_duration(ms(9_000)), "9s");
        assert_eq!(format_duration(ms(702_000)), "11m 42s");
        assert_eq!(format_duration(ms(3_930_000)), "1h 5m 30s");
        assert_eq!(format_duration(None), "unknown");
    }

    #[test]
    fn token_counts_are_grouped() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(942), "942");
        assert_eq!(thousands(94_201), "94,201");
        assert_eq!(thousands(1_234_567), "1,234,567");
    }

    #[test]
    fn long_objectives_are_summarized_to_one_short_line() {
        assert_eq!(summarize("Improve throughput"), "Improve throughput");
        assert_eq!(summarize("  spaced\n  out  "), "spaced out");

        let long = "Implement the median function in src/lib.rs so that it returns the median of the given values";
        let summary = summarize(long);
        assert!(summary.chars().count() <= 70, "{summary}");
        assert!(summary.ends_with('…'), "{summary}");
        assert!(summary.starts_with("Implement the median"));
    }

    #[test]
    fn commits_are_abbreviated_for_display() {
        assert_eq!(short("a73cf2100000000000000000000000000000000"), "a73cf21");
    }
}
