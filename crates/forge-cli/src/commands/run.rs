//! `forge run` — execute one task through one agent.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use chrono::Utc;
use forge_agent::AgentRegistry;
use forge_core::agent::AdapterStatus;
use forge_core::config::{ForgeConfig, Layout};
use forge_core::result::Verdict;
use forge_core::routing::{
    CandidateAgentSet, EvidencePolicyVersion, MinimumRoutingEvidence, RoutingDecision,
    RoutingEvidencePolicy, RoutingRequest,
};
use forge_core::run::{PatchSummary, RunOutcome, SelectionSource};
use forge_core::task::{EngineeringTask, TaskRevision};
use forge_git::Repository;
use forge_router::{
    CandidateAvailability, CandidateRequest, CandidateRequirements, ROUTER_VERSION,
    RoutingContract, resolve_candidates,
};
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
    /// Routing deliberately stopped before any agent execution.
    RoutingStopped,
}

pub async fn run(args: RunArgs) -> Result<RunExit> {
    let (repository, layout, config) = resolve_repository(args.repo.as_deref())?;

    let task = EngineeringTask::load(&args.task_path)?;
    task.validate()?;

    let explicit_agent = args.agent.is_some();
    let store = Store::open(layout.store_path(&config))
        .await
        .with_context(|| format!("opening the ledger at {}", config.store.path))?;
    let runner = Runner::new(repository, config.clone(), store.clone());
    let active_policy = runner.ensure_active_policy().await?;
    let agent_id = match args.agent.clone() {
        Some(agent) => agent,
        None if active_policy.routing.use_learned_routing => "auto".into(),
        None => config.defaults.agent.clone().ok_or_else(|| {
            anyhow!("no agent specified and no `defaults.agent` configured; pass --agent <name>")
        })?,
    };

    let registry = AgentRegistry::builtin();
    if agent_id != "auto" && registry.get(&agent_id).is_none() {
        bail!("unknown agent `{agent_id}`; run `forge agent list` to see the available agents");
    }

    if agent_id == "auto" {
        return run_auto(args, task, registry, runner, store, &layout, &config).await;
    }

    let mut request = RunRequest::new(task.clone(), &agent_id);
    request.execution_provenance = config.execution_provenance_for(&agent_id);
    request.selection_source = SelectionSource::Manual;
    if explicit_agent {
        request.manual_policy_override = Some(format!("agent={agent_id}"));
    }
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

async fn run_auto(
    args: RunArgs,
    task: EngineeringTask,
    registry: AgentRegistry,
    runner: Runner,
    store: Store,
    layout: &Layout,
    config: &ForgeConfig,
) -> Result<RunExit> {
    let active_policy = runner.ensure_active_policy().await?;
    let mut requested = Vec::new();
    for descriptor in registry
        .descriptors()
        .iter()
        .filter(|descriptor| descriptor.adapter_status == AdapterStatus::Implemented)
    {
        let agent_id = descriptor.agent_id.as_str();
        let mut candidate_request = RunRequest::new(task.clone(), agent_id);
        candidate_request.timeout = args.timeout_secs.map(Duration::from_secs);
        let agent_config = runner.agent_config(&candidate_request)?;
        let adapter = registry.adapter(agent_id, &agent_config)?;
        let configured_descriptor = adapter.descriptor();
        if registry.availability(&configured_descriptor).is_runnable() {
            requested.push(CandidateRequest {
                config: agent_config,
                availability: CandidateAvailability::Available,
            });
        }
    }
    if requested.is_empty() {
        bail!(
            "automatic routing found no available implemented agents; configure or install an agent, then run `forge agent list`"
        );
    }
    let candidates: CandidateAgentSet = resolve_candidates(
        registry.descriptors(),
        requested,
        &CandidateRequirements::default(),
    )?;
    let task_revision = TaskRevision::snapshot(task.clone())?;
    let persisted_revision = store.upsert_task(&task).await?;
    if persisted_revision != *task_revision.revision_id() {
        bail!("the persisted task revision differs from the routing snapshot");
    }
    let evidence_policy = RoutingEvidencePolicy {
        version: EvidencePolicyVersion(active_policy.routing.evidence_policy_version.clone()),
        ..RoutingEvidencePolicy::default()
    };
    let request = RoutingRequest::new(
        task_revision,
        candidates,
        evidence_policy,
        MinimumRoutingEvidence {
            total: active_policy.routing.minimum_total_evidence,
            per_agent: active_policy.routing.minimum_agent_evidence,
        },
        active_policy.exploration.policy,
        Utc::now(),
    );
    let resolved_base = runner.resolve_base(args.base.as_deref())?;
    let world_model_snapshot_id = if config.world_model.enabled {
        store
            .world_model_for_commit(&task.repository, resolved_base.as_str())
            .await?
            .map(|snapshot| snapshot.snapshot_id)
    } else {
        None
    };
    let mut routing_config = config.routing.clone();
    routing_config.minimum_total_evidence = active_policy.routing.minimum_total_evidence;
    routing_config.minimum_agent_evidence = active_policy.routing.minimum_agent_evidence;
    routing_config.minimum_score_margin = active_policy.routing.minimum_score_margin;
    routing_config.exploration_policy = active_policy.exploration.policy;
    let record = RoutingContract::new(store.clone())
        .route_with_world_model(&request, &routing_config, world_model_snapshot_id)
        .await?;
    print_routing(&record, &args.task_path);

    let selected = match &record.decision {
        RoutingDecision::Selected { agent, .. } => agent.clone(),
        RoutingDecision::InsufficientEvidence { .. }
        | RoutingDecision::CompeteRecommended { .. } => return Ok(RunExit::RoutingStopped),
    };
    let selected_id = selected.agent_id.to_string();
    let adapter = registry.adapter(&selected_id, &selected.config)?;
    let mut run_request = RunRequest::new(task.clone(), &selected_id);
    run_request.execution_provenance = config.execution_provenance_for(&selected_id);
    run_request.selection_source = SelectionSource::Automatic {
        decision_id: record.decision_id.clone(),
        router_version: ROUTER_VERSION.into(),
        evidence_fingerprint: record.evidence_fingerprint.clone(),
    };
    if args.agent.is_some() {
        run_request.manual_policy_override = Some("agent=auto".into());
    }
    run_request.base_rev = Some(resolved_base.as_str().into());
    run_request.timeout = args.timeout_secs.map(Duration::from_secs);
    if args.keep_workspace {
        run_request.keep_workspace = Some(true);
    }
    let report = runner.execute(run_request, adapter.as_ref()).await?;
    store
        .link_routing_decision_run(&record.decision_id, &report.run.run_id)
        .await?;
    print_report(&report, &task, &selected_id, layout, config);
    Ok(if report.outcome().is_success() {
        RunExit::Passed
    } else {
        RunExit::NotPassed
    })
}

fn print_routing(record: &forge_core::RoutingDecisionRecord, task_path: &Path) {
    println!("Forge routing\n");
    println!("Task\n  {}", record.task_id);
    println!("\nCandidates");
    for candidate in &record.candidates {
        println!(
            "  {}  {}",
            candidate.agent_id,
            &candidate.config_fingerprint[..12]
        );
    }
    let summary = match &record.decision {
        RoutingDecision::Selected {
            evidence_summary, ..
        }
        | RoutingDecision::InsufficientEvidence {
            evidence_summary, ..
        }
        | RoutingDecision::CompeteRecommended {
            evidence_summary, ..
        } => evidence_summary,
    };
    println!(
        "\nHistorical evidence\n  {} eligible runs, {} resolved across {} similar task revisions",
        summary.eligible_runs, summary.resolved_runs, summary.similar_task_revisions
    );
    if !summary.excluded.is_empty() {
        println!("\nExcluded evidence");
        for excluded in &summary.excluded {
            println!("  {:?}: {}", excluded.reason, excluded.count);
        }
    }
    println!("\nPredicted success");
    for score in record.decision.scores() {
        println!(
            "  {:<12} {:.3}  ({} pass / {} fail / {} unresolved)",
            score.agent.agent_id,
            score.predicted_success,
            score.positive_count,
            score.negative_count,
            score.unresolved_count
        );
    }
    match &record.decision {
        RoutingDecision::Selected {
            agent,
            decision_margin,
            ..
        } => {
            println!("\nResult\n  SELECTED {}", agent.agent_id);
            if let Some(margin) = decision_margin {
                println!("  score margin {margin:.3}");
            }
        }
        RoutingDecision::InsufficientEvidence {
            evidence_summary, ..
        } => {
            println!("\nResult\n  INSUFFICIENT EVIDENCE");
            println!(
                "  {} resolved; routing thresholds were not satisfied or only one candidate was available",
                evidence_summary.resolved_runs
            );
        }
        RoutingDecision::CompeteRecommended { .. } => {
            let agents = record
                .candidates
                .iter()
                .map(|candidate| candidate.agent_id.as_str())
                .collect::<Vec<_>>()
                .join(",");
            println!("\nResult\n  COMPETITION RECOMMENDED");
            println!(
                "\nSuggested command\n  forge compete {} --agents {agents}",
                task_path.display()
            );
        }
    }
    println!(
        "\nRouter\n  {}\n  decision {}\n  evidence {}",
        record.router_version,
        record.decision_id,
        &record.evidence_fingerprint[..12]
    );
}

pub(crate) fn resolve_repository(repo: Option<&Path>) -> Result<(Repository, Layout, ForgeConfig)> {
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
            (
                "Selection",
                match &run.selection_source {
                    SelectionSource::Manual => format!("MANUAL → {agent_id}"),
                    SelectionSource::Automatic { decision_id, .. } => {
                        format!("AUTO → {agent_id} ({decision_id})")
                    }
                    SelectionSource::Competition { experiment_id } => {
                        format!("COMPETITION → {agent_id} ({experiment_id})")
                    }
                }
            ),
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
            vec![
                check.name.clone(),
                check.kind.to_string(),
                if check.required {
                    "required"
                } else {
                    "optional"
                }
                .to_string(),
                check.verdict.to_string(),
                check.execution_status.as_str().to_string(),
                check
                    .exit_code
                    .map(|code| format!("exit {code}, {}ms", check.duration_ms))
                    .unwrap_or_else(|| format!("{}ms", check.duration_ms)),
            ]
        })
        .collect();

    println!(
        "{}",
        output::table(
            &["evaluator", "category", "policy", "result", "execution", ""],
            &rows
        )
    );

    let summary = evaluation.summary();
    println!(
        "\n{}",
        output::fields(&[
            ("Required", summary.required_count.to_string()),
            ("Optional", summary.optional_count.to_string()),
            ("Metrics", summary.metric_count.to_string()),
            ("Execution errors", summary.execution_errors.to_string()),
        ])
    );

    for check in &evaluation.checks {
        if check.verdict != Verdict::Pass
            && let Some(detail) = &check.detail
        {
            println!("\n  {} detail:", check.name);
            for line in detail.lines().take(10) {
                println!("    {line}");
            }
        }
        if let Some(path) = &check.output_path {
            println!("  {} artifact: {}", check.name, path.display());
        }
        for warning in &check.warnings {
            println!("  {} warning: {warning}", check.name);
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
        RunOutcome::Inconclusive => "  (evaluation evidence was inconclusive)",
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

pub(crate) fn short(commit: &str) -> String {
    commit.chars().take(7).collect()
}

/// A one-line summary of an objective, short enough not to wrap a terminal.
pub(crate) fn summarize(text: &str) -> String {
    const MAX: usize = 68;
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= MAX {
        return collapsed;
    }
    let truncated: String = collapsed.chars().take(MAX).collect();
    let cut = truncated.rfind(' ').unwrap_or(truncated.len());
    format!("{} …", &truncated[..cut])
}

pub(crate) fn thousands(value: u64) -> String {
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

pub(crate) fn format_duration(delta: Option<chrono::TimeDelta>) -> String {
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
