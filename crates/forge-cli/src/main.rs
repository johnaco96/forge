//! Forge's command-line interface.
//!
//! Forge is CLI-first by design: the execution engine has to be usable and
//! scriptable before any dashboard exists, and the CLI should stay useful after
//! one does.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use clap::{Parser, Subcommand, ValueEnum};
use forge_core::ids::WorldModelSnapshotId;
use forge_core::ids::{
    ExperimentId, HealthSnapshotId, PolicyExperimentId, PolicyId, PolicyProposalId, TaskId,
};
use forge_core::optimization::PolicyExperimentStatus;
use forge_core::run::RunOutcome;
use forge_store::{FailureFilter, HistoryFilter};

mod commands;
mod output;

#[derive(Parser)]
#[command(
    name = "forge",
    version,
    about = "A longitudinal control plane for autonomous software engineering",
    long_about = "Forge runs coding agents in isolated workspaces, evaluates their work \
                  independently, and keeps the results so that future engineering decisions \
                  can be made from evidence.",
    propagate_version = true
)]
struct Cli {
    /// Repository to operate on. Defaults to the current directory.
    #[arg(long, global = true, value_name = "PATH")]
    repo: Option<PathBuf>,

    /// Print internal diagnostics.
    #[arg(long, short, global = true)]
    verbose: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Prepare this repository for Forge.
    Init {
        /// Rewrite configuration that already exists.
        #[arg(long)]
        force: bool,
    },

    /// Run a task through an agent and evaluate the result.
    Run {
        /// Path to a task file (`.yaml`, `.yml`, or `.json`).
        task: PathBuf,

        /// Agent to run. Defaults to `defaults.agent` in the config.
        #[arg(long)]
        agent: Option<String>,

        /// Revision to start from. Defaults to HEAD.
        #[arg(long, value_name = "REV")]
        base: Option<String>,

        /// Wall-clock budget for the agent, overriding the configured timeout.
        #[arg(long, value_name = "SECONDS")]
        timeout_secs: Option<u64>,

        /// Keep the workspace after the run, for inspection.
        #[arg(long)]
        keep_workspace: bool,
    },

    /// Run several agents independently from one base and compare the results.
    Compete {
        /// Path to a task file (`.yaml`, `.yml`, or `.json`).
        task: PathBuf,

        /// Comma-separated agents. At least two, with no duplicates.
        #[arg(long, value_name = "AGENT,AGENT[,...]")]
        agents: String,

        /// Revision every competitor starts from. Defaults to HEAD.
        #[arg(long, value_name = "REV")]
        base: Option<String>,

        /// Wall-clock budget for each agent.
        #[arg(long, value_name = "SECONDS")]
        timeout_secs: Option<u64>,

        /// Keep every participant workspace after the experiment.
        #[arg(long)]
        keep_workspace: bool,
    },

    /// Execute an explicit, validated task DAG as a multi-agent team.
    Team {
        /// Path to a task file containing a `team` plan.
        task: PathBuf,

        /// Revision the root team objective starts from. Defaults to HEAD.
        #[arg(long, value_name = "REV")]
        base: Option<String>,

        /// Wall-clock budget for each agent-backed node.
        #[arg(long, value_name = "SECONDS")]
        timeout_secs: Option<u64>,

        /// Keep node workspaces after their ordinary Forge runs.
        #[arg(long)]
        keep_workspace: bool,
    },

    /// Inspect the agents Forge knows about.
    Agent {
        #[command(subcommand)]
        command: AgentCommand,
    },

    /// Work with task definitions.
    Task {
        #[command(subcommand)]
        command: TaskCommand,
    },

    /// List historical runs from the experience ledger.
    History {
        #[arg(long)]
        agent: Option<String>,
        #[arg(long, value_parser = parse_outcome)]
        outcome: Option<RunOutcome>,
        #[arg(long)]
        task: Option<TaskId>,
        #[arg(long)]
        repository: Option<String>,
        #[arg(long)]
        experiment: Option<ExperimentId>,
        #[arg(long, value_parser = parse_datetime, value_name = "RFC3339")]
        from: Option<DateTime<Utc>>,
        #[arg(long, value_parser = parse_datetime, value_name = "RFC3339")]
        through: Option<DateTime<Utc>>,
        #[arg(long)]
        category: Option<String>,
        #[arg(long)]
        language: Option<String>,
        #[arg(long)]
        domain: Option<String>,
        #[arg(long)]
        difficulty: Option<String>,
        #[arg(long)]
        component: Option<String>,
        #[arg(long)]
        tag: Option<String>,
        #[arg(long, default_value_t = 20)]
        limit: u32,
    },

    /// Investigate historical non-passing runs.
    Failures {
        #[arg(long)]
        agent: Option<String>,
        #[arg(long)]
        repository: Option<String>,
        #[arg(long)]
        category: Option<String>,
        #[arg(long)]
        component: Option<String>,
        #[arg(long, default_value_t = 20)]
        limit: u32,
    },

    /// Export normalized run evidence for offline analysis.
    Export {
        #[arg(long, value_enum)]
        format: ExportFormat,
    },

    /// Inspect recorded competitive experiments.
    Experiments {
        #[command(subcommand)]
        command: ExperimentsCommand,
    },

    /// Build and inspect immutable repository world models.
    World {
        #[command(subcommand)]
        command: WorldCommand,
    },

    /// Measure and compare repository health over time.
    Health {
        #[command(subcommand)]
        command: HealthCommand,
    },

    /// Inspect and evolve the repository's engineering policy.
    Policy {
        #[command(subcommand)]
        command: PolicyCommand,
    },
}

#[derive(Subcommand)]
enum PolicyCommand {
    /// Show the active immutable policy and its fixed guardrails.
    Show,
    /// Show repository policy lineage and lifecycle status.
    History {
        #[arg(long, default_value_t = 50)]
        limit: u32,
    },
    /// Build and persist one bounded evidence-backed proposal.
    Propose {
        /// Re-evaluate an existing candidate policy.
        #[arg(long)]
        candidate: Option<PolicyId>,
        #[arg(long)]
        max_world_facts: Option<u32>,
        #[arg(long)]
        timeout_secs: Option<u64>,
        #[arg(long)]
        minimum_score_margin: Option<f64>,
        #[arg(long)]
        learned_routing: Option<bool>,
        #[arg(long, value_parser = parse_datetime, value_name = "RFC3339")]
        cutoff: Option<DateTime<Utc>>,
    },
    /// Explain a persisted proposal without collapsing its tradeoffs.
    Compare { proposal: PolicyProposalId },
    /// Create and inspect bounded deterministic policy experiments.
    Experiment {
        #[command(subcommand)]
        command: PolicyExperimentCommand,
    },
    /// Explicitly promote a proposal that passes every gate.
    Promote {
        proposal: PolicyProposalId,
        #[arg(long, default_value = "cli")]
        actor: String,
    },
    /// Return to a prior immutable policy.
    Rollback {
        policy: PolicyId,
        #[arg(long)]
        reason: String,
        #[arg(long, default_value = "cli")]
        actor: String,
    },
}

#[derive(Subcommand)]
enum PolicyExperimentCommand {
    Create {
        proposal: PolicyProposalId,
        #[arg(long, default_value_t = 50)]
        candidate_share_percent: u32,
        #[arg(long, default_value_t = 20)]
        max_tasks: u32,
        #[arg(long, default_value_t = 4)]
        max_extra_runs: u32,
        #[arg(long)]
        max_extra_cost_usd: Option<f64>,
        #[arg(long, value_parser = parse_datetime, value_name = "RFC3339")]
        expires_at: Option<DateTime<Utc>>,
    },
    Show {
        experiment: PolicyExperimentId,
    },
    Status {
        experiment: PolicyExperimentId,
        #[arg(value_parser = parse_policy_experiment_status)]
        status: PolicyExperimentStatus,
    },
}

#[derive(Subcommand)]
enum HealthCommand {
    /// Build an immutable health snapshot for the current exact commit.
    Build,
    /// Show a health snapshot's raw measurements and provenance.
    Show {
        /// Snapshot id, e.g. `H-0012`. Defaults to the current snapshot.
        id: Option<HealthSnapshotId>,
    },
    /// Compare two health snapshots.
    Diff {
        /// Baseline. Defaults to the nearest prior snapshot on the same
        /// ancestry chain.
        from: Option<HealthSnapshotId>,
        /// Target. Defaults to the current snapshot.
        to: Option<HealthSnapshotId>,
    },
    /// Report per-dimension trends across recorded history.
    Trend,
}

#[derive(Subcommand)]
enum WorldCommand {
    /// Build a snapshot for the repository's current commit.
    Build,
    /// Show the current or a named immutable snapshot.
    Show {
        snapshot: Option<WorldModelSnapshotId>,
    },
    /// Query typed facts in the current or a named snapshot.
    Query {
        #[arg(value_enum)]
        kind: WorldQueryKindArg,
        term: Option<String>,
        #[arg(long)]
        snapshot: Option<WorldModelSnapshotId>,
        #[arg(long, default_value_t = 50)]
        limit: u32,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum WorldQueryKindArg {
    All,
    #[value(alias = "components")]
    Component,
    #[value(alias = "modules")]
    Module,
    #[value(alias = "interfaces")]
    Interface,
    #[value(alias = "contracts")]
    Contract,
    #[value(alias = "invariants")]
    Invariant,
    #[value(alias = "dependencies")]
    Dependency,
    Ownership,
    #[value(alias = "performance", alias = "performance-constraints")]
    PerformanceConstraint,
    #[value(alias = "decisions")]
    HistoricalDecision,
    #[value(alias = "failure", alias = "failures")]
    KnownFailureMode,
}

#[derive(Subcommand)]
enum AgentCommand {
    /// List known agents and whether Forge can run them.
    List,

    /// Summarize historical outcomes and reported measurements for an agent.
    Stats {
        /// Agent identifier, for example `codex`.
        agent: String,
    },
}

#[derive(Subcommand)]
enum TaskCommand {
    /// Check a task file for problems before running it.
    Validate {
        /// Path to a task file (`.yaml`, `.yml`, or `.json`).
        path: PathBuf,
    },

    /// Find prior tasks using deterministic structured similarity.
    Similar {
        task: TaskId,
        #[arg(long, default_value_t = 10)]
        limit: u32,
    },
}

#[derive(Subcommand)]
enum ExperimentsCommand {
    /// List historical experiments and their linked runs.
    List {
        #[arg(long, default_value_t = 20)]
        limit: u32,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ExportFormat {
    Jsonl,
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    init_tracing(cli.verbose);

    match dispatch(cli).await {
        Ok(code) => code,
        Err(err) => {
            // `{:#}` renders the whole context chain, which is where the
            // actionable part of a failure usually lives.
            eprintln!("error: {err:#}");
            EXIT_FORGE_ERROR
        }
    }
}

/// Forge could not do what was asked.
const EXIT_FORGE_ERROR: std::process::ExitCode = std::process::ExitCode::FAILURE;

/// A run completed, but its outcome was not a pass. Distinct from a Forge
/// error so a script can tell "the tool broke" from "the change did not work".
const EXIT_RUN_NOT_PASSED: u8 = 2;
const EXIT_ROUTING_STOPPED: u8 = 3;

async fn dispatch(cli: Cli) -> anyhow::Result<std::process::ExitCode> {
    match cli.command {
        Command::Init { force } => {
            commands::init::run(commands::init::InitArgs {
                repo: cli.repo,
                force,
            })
            .await?;
            Ok(std::process::ExitCode::SUCCESS)
        }
        Command::Run {
            task,
            agent,
            base,
            timeout_secs,
            keep_workspace,
        } => {
            let exit = commands::run::run(commands::run::RunArgs {
                task_path: task,
                repo: cli.repo,
                agent,
                base,
                timeout_secs,
                keep_workspace,
            })
            .await?;
            Ok(match exit {
                commands::run::RunExit::Passed => std::process::ExitCode::SUCCESS,
                commands::run::RunExit::NotPassed => {
                    std::process::ExitCode::from(EXIT_RUN_NOT_PASSED)
                }
                commands::run::RunExit::RoutingStopped => {
                    std::process::ExitCode::from(EXIT_ROUTING_STOPPED)
                }
            })
        }
        Command::Compete {
            task,
            agents,
            base,
            timeout_secs,
            keep_workspace,
        } => {
            let exit = commands::compete::run(commands::compete::CompeteArgs {
                task_path: task,
                repo: cli.repo,
                agents,
                base,
                timeout_secs,
                keep_workspace,
            })
            .await?;
            Ok(match exit {
                commands::compete::CompeteExit::AllPassed => std::process::ExitCode::SUCCESS,
                commands::compete::CompeteExit::SomeNotPassed => {
                    std::process::ExitCode::from(EXIT_RUN_NOT_PASSED)
                }
            })
        }
        Command::Team {
            task,
            base,
            timeout_secs,
            keep_workspace,
        } => {
            let exit = commands::team::run(commands::team::TeamArgs {
                task_path: task,
                repo: cli.repo,
                base,
                timeout_secs,
                keep_workspace,
            })
            .await?;
            Ok(match exit {
                commands::team::TeamExit::Passed => std::process::ExitCode::SUCCESS,
                commands::team::TeamExit::NotPassed => {
                    std::process::ExitCode::from(EXIT_RUN_NOT_PASSED)
                }
            })
        }
        Command::Agent { command } => match command {
            AgentCommand::List => {
                commands::agent::list()?;
                Ok(std::process::ExitCode::SUCCESS)
            }
            AgentCommand::Stats { agent } => {
                commands::experience::agent_stats(cli.repo, agent).await?;
                Ok(std::process::ExitCode::SUCCESS)
            }
        },
        Command::Task { command } => match command {
            TaskCommand::Validate { path } => {
                commands::task::validate(path)?;
                Ok(std::process::ExitCode::SUCCESS)
            }
            TaskCommand::Similar { task, limit } => {
                commands::experience::similar(cli.repo, task, limit).await?;
                Ok(std::process::ExitCode::SUCCESS)
            }
        },
        Command::History {
            agent,
            outcome,
            task,
            repository,
            experiment,
            from,
            through,
            category,
            language,
            domain,
            difficulty,
            component,
            tag,
            limit,
        } => {
            commands::experience::history(
                cli.repo,
                HistoryFilter {
                    agent_id: agent,
                    outcome,
                    task_id: task,
                    repository,
                    experiment_id: experiment,
                    created_from: from,
                    created_through: through,
                    category,
                    language,
                    domain,
                    difficulty,
                    component,
                    tag,
                    limit,
                },
            )
            .await?;
            Ok(std::process::ExitCode::SUCCESS)
        }
        Command::Failures {
            agent,
            repository,
            category,
            component,
            limit,
        } => {
            commands::experience::failures(
                cli.repo,
                FailureFilter {
                    agent_id: agent,
                    repository,
                    category,
                    component,
                    limit,
                },
            )
            .await?;
            Ok(std::process::ExitCode::SUCCESS)
        }
        Command::Export { format } => {
            match format {
                ExportFormat::Jsonl => commands::experience::export_jsonl(cli.repo).await?,
            }
            Ok(std::process::ExitCode::SUCCESS)
        }
        Command::Experiments { command } => match command {
            ExperimentsCommand::List { limit } => {
                commands::experience::experiments(cli.repo, limit).await?;
                Ok(std::process::ExitCode::SUCCESS)
            }
        },
        Command::Health { command } => match command {
            HealthCommand::Build => {
                let exit = commands::health::build(cli.repo).await?;
                Ok(match exit {
                    commands::health::HealthBuildExit::Complete => std::process::ExitCode::SUCCESS,
                    // Partial health is a real, reportable outcome, not a
                    // tool failure; it exits distinctly so scripts can tell.
                    commands::health::HealthBuildExit::NotComplete => {
                        std::process::ExitCode::from(EXIT_RUN_NOT_PASSED)
                    }
                })
            }
            HealthCommand::Show { id } => {
                commands::health::show(cli.repo, id).await?;
                Ok(std::process::ExitCode::SUCCESS)
            }
            HealthCommand::Diff { from, to } => {
                commands::health::diff(cli.repo, from, to).await?;
                Ok(std::process::ExitCode::SUCCESS)
            }
            HealthCommand::Trend => {
                commands::health::trend(cli.repo).await?;
                Ok(std::process::ExitCode::SUCCESS)
            }
        },
        Command::World { command } => match command {
            WorldCommand::Build => {
                let exit = commands::world::build(cli.repo).await?;
                Ok(match exit {
                    commands::world::WorldBuildExit::Complete => std::process::ExitCode::SUCCESS,
                    commands::world::WorldBuildExit::NotComplete => {
                        std::process::ExitCode::from(EXIT_RUN_NOT_PASSED)
                    }
                })
            }
            WorldCommand::Show { snapshot } => {
                commands::world::show(cli.repo, snapshot).await?;
                Ok(std::process::ExitCode::SUCCESS)
            }
            WorldCommand::Query {
                kind,
                term,
                snapshot,
                limit,
            } => {
                commands::world::query(cli.repo, snapshot, kind, term, limit).await?;
                Ok(std::process::ExitCode::SUCCESS)
            }
        },
        Command::Policy { command } => {
            match command {
                PolicyCommand::Show => commands::policy::show(cli.repo).await?,
                PolicyCommand::History { limit } => {
                    commands::policy::history(cli.repo, limit).await?
                }
                PolicyCommand::Propose {
                    candidate,
                    max_world_facts,
                    timeout_secs,
                    minimum_score_margin,
                    learned_routing,
                    cutoff,
                } => {
                    commands::policy::propose(
                        cli.repo,
                        commands::policy::ProposeArgs {
                            candidate,
                            max_world_facts,
                            timeout_secs,
                            minimum_score_margin,
                            learned_routing,
                            cutoff,
                        },
                    )
                    .await?
                }
                PolicyCommand::Compare { proposal } => {
                    commands::policy::compare(cli.repo, proposal).await?
                }
                PolicyCommand::Experiment { command } => match command {
                    PolicyExperimentCommand::Create {
                        proposal,
                        candidate_share_percent,
                        max_tasks,
                        max_extra_runs,
                        max_extra_cost_usd,
                        expires_at,
                    } => {
                        commands::policy::experiment_create(
                            cli.repo,
                            proposal,
                            candidate_share_percent,
                            forge_core::ExperimentBudget {
                                max_tasks,
                                max_extra_runs,
                                max_extra_cost_usd,
                                expires_at,
                            },
                        )
                        .await?
                    }
                    PolicyExperimentCommand::Show { experiment } => {
                        commands::policy::experiment_show(cli.repo, experiment).await?
                    }
                    PolicyExperimentCommand::Status { experiment, status } => {
                        commands::policy::experiment_status(cli.repo, experiment, status).await?
                    }
                },
                PolicyCommand::Promote { proposal, actor } => {
                    commands::policy::promote(cli.repo, proposal, actor).await?
                }
                PolicyCommand::Rollback {
                    policy,
                    reason,
                    actor,
                } => commands::policy::rollback(cli.repo, policy, reason, actor).await?,
            }
            Ok(std::process::ExitCode::SUCCESS)
        }
    }
}

fn parse_policy_experiment_status(raw: &str) -> Result<PolicyExperimentStatus, String> {
    match raw.to_ascii_lowercase().replace('-', "_").as_str() {
        "running" => Ok(PolicyExperimentStatus::Running),
        "execution_complete" => Ok(PolicyExperimentStatus::ExecutionComplete),
        "concluded" => Ok(PolicyExperimentStatus::Concluded),
        "cancelled" | "canceled" => Ok(PolicyExperimentStatus::Cancelled),
        _ => Err("expected running, execution-complete, concluded, or cancelled".into()),
    }
}

fn parse_outcome(raw: &str) -> Result<RunOutcome, String> {
    match raw.to_ascii_lowercase().replace('-', "_").as_str() {
        "pass" | "passed" => Ok(RunOutcome::Passed),
        "fail" | "failed" => Ok(RunOutcome::Failed),
        "inconclusive" => Ok(RunOutcome::Inconclusive),
        "no_change" => Ok(RunOutcome::NoChange),
        "error" | "errored" => Ok(RunOutcome::Errored),
        _ => Err("expected pass, fail, inconclusive, no-change, or error".into()),
    }
}

fn parse_datetime(raw: &str) -> Result<DateTime<Utc>, String> {
    DateTime::parse_from_rfc3339(raw)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|error| format!("expected an RFC3339 timestamp: {error}"))
}

fn init_tracing(verbose: bool) {
    let level = if verbose {
        tracing::Level::DEBUG
    } else {
        tracing::Level::WARN
    };
    tracing_subscriber::fmt()
        .with_max_level(level)
        .with_target(false)
        .without_time()
        .with_writer(std::io::stderr)
        .init();
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn the_cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn the_documented_commands_parse() {
        for args in [
            vec!["forge", "init"],
            vec!["forge", "init", "--force"],
            vec!["forge", "agent", "list"],
            vec!["forge", "task", "validate", "task.yaml"],
            vec!["forge", "run", "task.yaml", "--agent", "claude"],
            vec!["forge", "compete", "task.yaml", "--agents", "claude,codex"],
            vec!["forge", "team", "task.yaml"],
            vec!["forge", "history", "--agent", "codex", "--limit", "10"],
            vec!["forge", "agent", "stats", "codex"],
            vec!["forge", "failures", "--component", "runner"],
            vec!["forge", "task", "similar", "T-1042"],
            vec!["forge", "experiments", "list"],
            vec!["forge", "export", "--format", "jsonl"],
            vec!["forge", "world", "build"],
            vec!["forge", "world", "show"],
            vec!["forge", "world", "show", "WM-0001"],
            vec!["forge", "world", "query", "component", "storage"],
            vec!["forge", "world", "query", "dependencies", "storage"],
            vec!["forge", "world", "query", "failures", "scheduler"],
            vec!["forge", "policy", "show"],
            vec!["forge", "policy", "history"],
            vec!["forge", "policy", "propose", "--max-world-facts", "8"],
            vec!["forge", "policy", "compare", "PP-0001"],
            vec!["forge", "policy", "experiment", "create", "PP-0001"],
            vec![
                "forge",
                "policy",
                "experiment",
                "status",
                "PX-0001",
                "concluded",
            ],
            vec!["forge", "policy", "promote", "PP-0001"],
            vec![
                "forge",
                "policy",
                "rollback",
                "P-0001",
                "--reason",
                "hard constraint regressed",
            ],
            vec!["forge", "--repo", "/tmp/repo", "agent", "list"],
        ] {
            Cli::try_parse_from(&args).unwrap_or_else(|e| panic!("{args:?}: {e}"));
        }
    }

    #[test]
    fn the_run_command_parses_with_its_options() {
        for args in [
            vec!["forge", "run", "task.yaml"],
            vec!["forge", "run", "task.yaml", "--agent", "claude"],
            vec!["forge", "run", "task.yaml", "--base", "main~1"],
            vec!["forge", "run", "task.yaml", "--timeout-secs", "600"],
            vec!["forge", "run", "task.yaml", "--keep-workspace"],
        ] {
            Cli::try_parse_from(&args).unwrap_or_else(|e| panic!("{args:?}: {e}"));
        }
    }

    #[test]
    fn compete_requires_the_agents_option() {
        assert!(Cli::try_parse_from(["forge", "compete", "task.yaml"]).is_err());
    }

    #[test]
    fn the_team_command_parses_with_its_options() {
        for args in [
            vec!["forge", "team", "task.yaml"],
            vec!["forge", "team", "task.yaml", "--base", "main~1"],
            vec!["forge", "team", "task.yaml", "--timeout-secs", "600"],
            vec!["forge", "team", "task.yaml", "--keep-workspace"],
        ] {
            Cli::try_parse_from(&args).unwrap_or_else(|e| panic!("{args:?}: {e}"));
        }
    }
}
