//! `forge team` — execute a validated task DAG through ordinary Forge runs.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use forge_core::team::{
    ReviewDecision, TeamComparisonRelation, TeamExecution, TeamOutcome, TeamPlanNode,
};
use forge_store::Store;
use forge_team::{TeamCoordinator, TeamRequest, load_team_task};

use crate::commands::run::{resolve_repository, short};
use crate::output;

pub struct TeamArgs {
    pub task_path: PathBuf,
    pub repo: Option<PathBuf>,
    pub base: Option<String>,
    pub timeout_secs: Option<u64>,
    pub keep_workspace: bool,
}

pub enum TeamExit {
    Passed,
    NotPassed,
}

pub async fn run(args: TeamArgs) -> Result<TeamExit> {
    let (repository, layout, config) = resolve_repository(args.repo.as_deref())?;
    let (task, plan) = load_team_task(&args.task_path)?;
    let store = Store::open(layout.store_path(&config))
        .await
        .with_context(|| format!("opening the ledger at {}", config.store.path))?;
    let coordinator = TeamCoordinator::new(repository, config, store);
    let mut request = TeamRequest::explicit(task, plan);
    request.base_rev = args.base;
    request.timeout = args.timeout_secs.map(Duration::from_secs);
    if args.keep_workspace {
        request.keep_workspace = Some(true);
    }
    let report = coordinator.execute(request).await?;
    print_report(
        &report.team,
        report.execution_strategy,
        report.events_recorded,
    );
    Ok(if report.team.outcome == Some(TeamOutcome::Passed) {
        TeamExit::Passed
    } else {
        TeamExit::NotPassed
    })
}

fn print_report(team: &TeamExecution, strategy: &str, event_count: usize) {
    println!("Forge team {}\n", team.team_execution_id);
    println!(
        "{}",
        output::fields(&[
            ("Task", team.root_task_id.to_string()),
            ("Task revision", team.root_task_revision_id.to_string()),
            ("Base commit", short(&team.base_commit)),
            ("Plan", format!("{} nodes", team.nodes.len())),
            (
                "Plan fingerprint",
                team.plan.fingerprint.chars().take(12).collect()
            ),
            ("Plan source", format!("{:?}", team.plan_provenance.source)),
            ("Scheduler", strategy.into()),
            ("Events", event_count.to_string()),
        ])
    );

    println!("\nDAG");
    for node_id in &team.plan.topological_order {
        let definition = team.plan.node(node_id).expect("validated plan node");
        let parents = if definition.depends_on.is_empty() {
            "root".into()
        } else {
            definition
                .depends_on
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(",")
        };
        println!("  {parents} -> {}", definition.node_id);
    }

    let rows = team
        .nodes
        .iter()
        .map(|node| {
            let definition = team.plan.node(&node.node_id).expect("validated plan node");
            vec![
                node.node_id.to_string(),
                execution_name(definition).into(),
                node.assignment
                    .as_ref()
                    .map(|assignment| match &assignment.selection_source {
                        forge_core::SelectionSource::Automatic { decision_id, .. } => {
                            format!("{} (auto {decision_id})", assignment.agent.agent_id)
                        }
                        _ => assignment.agent.agent_id.to_string(),
                    })
                    .unwrap_or_else(|| match &definition.assignment {
                        Some(forge_core::TeamAssignmentStrategy::Auto) => node
                            .routing_decision_id
                            .as_ref()
                            .map(|decision| format!("auto ({decision})"))
                            .unwrap_or_else(|| "auto".into()),
                        Some(forge_core::TeamAssignmentStrategy::Explicit { agent }) => {
                            agent.to_string()
                        }
                        None => "deterministic".into(),
                    }),
                node.status.as_str().into(),
                node.run_ids
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(","),
                node.input_commit
                    .as_deref()
                    .map(short)
                    .unwrap_or_else(|| "-".into()),
                node.output_commit
                    .as_deref()
                    .map(short)
                    .unwrap_or_else(|| "-".into()),
                node.output_artifact_ids.len().to_string(),
            ]
        })
        .collect::<Vec<_>>();
    println!(
        "\n{}",
        output::section(
            "Nodes",
            output::table(
                &[
                    "node",
                    "type",
                    "agent",
                    "status",
                    "runs",
                    "input",
                    "output",
                    "artifacts",
                ],
                &rows,
            )
        )
    );

    println!("\nResources");
    println!(
        "{}",
        output::fields(&[
            ("Agent runs", team.resources.agent_run_count.to_string()),
            (
                "Failed attempts",
                team.resources.failed_attempt_count.to_string()
            ),
            ("Warnings", team.resources.warning_count.to_string()),
            (
                "Run duration",
                team.resources
                    .total_run_duration_ms
                    .map(|value| format!("{value}ms"))
                    .unwrap_or_else(|| "unavailable".into())
            ),
            (
                "Tokens",
                team.resources
                    .total_tokens
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "unavailable".into())
            ),
            (
                "Known cost",
                team.resources
                    .known_cost_usd
                    .map(|value| format!("${value:.4}"))
                    .unwrap_or_else(|| "unavailable".into())
            ),
        ])
    );

    let reviews = team
        .nodes
        .iter()
        .filter_map(|node| {
            node.review.as_ref().map(|review| {
                format!(
                    "{}: {} ({} findings)",
                    node.node_id,
                    review_name(review.decision),
                    review.findings.len()
                )
            })
        })
        .collect::<Vec<_>>();
    if !reviews.is_empty() {
        println!("\n{}", output::section("Review", output::bullets(reviews)));
    }

    let failures = team
        .nodes
        .iter()
        .filter_map(|node| {
            node.failure_reason
                .as_ref()
                .map(|reason| format!("{}: {reason}", node.node_id))
        })
        .collect::<Vec<_>>();
    if !failures.is_empty() {
        println!(
            "\n{}",
            output::section("Node failures", output::bullets(failures))
        );
    }

    println!("\nFinal candidate");
    if let Some(candidate) = &team.final_candidate {
        println!(
            "{}",
            output::fields(&[
                ("Commit", short(&candidate.integrated_commit)),
                ("Files changed", candidate.patch.files_changed.to_string()),
                ("Lines changed", candidate.patch.lines_changed().to_string()),
                (
                    "Contributing runs",
                    candidate.contributing_runs.len().to_string()
                ),
            ])
        );
    } else {
        println!("  unavailable");
    }

    println!("\nFinal evaluation");
    if let Some(final_evaluation) = &team.final_evaluation {
        println!("  Integrity  {}", final_evaluation.integrity.status);
        if let Some(evaluation) = &final_evaluation.evaluation {
            for check in &evaluation.checks {
                println!("  {:<10} {}", check.name, check.verdict);
            }
        }
        println!("  Verdict    {}", final_evaluation.verdict);
    } else {
        println!("  unavailable");
    }

    println!("\nTeam outcome");
    println!(
        "  {}",
        team.outcome.map(TeamOutcome::as_str).unwrap_or("error")
    );
    if let Some(reason) = &team.failure_reason {
        println!("  {reason}");
    }

    println!("\nSingle-agent baseline");
    match &team.baseline_comparison {
        Some(comparison) if comparison.baseline_run_id.is_some() => {
            println!("  {}", comparison.baseline_run_id.as_ref().unwrap());
            println!(
                "{}",
                output::fields(&[
                    ("Correctness", relation(comparison.correctness).into()),
                    ("Integrity", relation(comparison.integrity).into()),
                    ("Runtime", relation(comparison.runtime).into()),
                    ("Tokens", relation(comparison.tokens).into()),
                    ("Known cost", relation(comparison.known_cost).into()),
                    ("Patch size", relation(comparison.patch_size).into()),
                    ("Warnings", relation(comparison.warnings).into()),
                ])
            );
        }
        Some(comparison) => println!(
            "  {}",
            comparison
                .note
                .as_deref()
                .unwrap_or("single-agent baseline unavailable")
        ),
        None => println!("  single-agent baseline unavailable"),
    }
}

fn execution_name(node: &TeamPlanNode) -> &'static str {
    match node.execution {
        forge_core::TeamExecutionType::Analysis => "analysis",
        forge_core::TeamExecutionType::Implementation => "implementation",
        forge_core::TeamExecutionType::Review => "review",
        forge_core::TeamExecutionType::Integration => "integration",
        forge_core::TeamExecutionType::Verification => "verification",
    }
}

fn review_name(decision: ReviewDecision) -> &'static str {
    match decision {
        ReviewDecision::Approve => "APPROVE",
        ReviewDecision::RequestChanges => "REQUEST CHANGES",
        ReviewDecision::Inconclusive => "INCONCLUSIVE",
    }
}

fn relation(value: TeamComparisonRelation) -> &'static str {
    match value {
        TeamComparisonRelation::TeamBetter => "team",
        TeamComparisonRelation::BaselineBetter => "baseline",
        TeamComparisonRelation::Tie => "tie",
        TeamComparisonRelation::Incomparable => "not comparable",
        TeamComparisonRelation::Unavailable => "unavailable",
    }
}
