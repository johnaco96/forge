//! Forge's command-line interface.
//!
//! Forge is CLI-first by design: the execution engine has to be usable and
//! scriptable before any dashboard exists, and the CLI should stay useful after
//! one does.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

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
}

#[derive(Subcommand)]
enum AgentCommand {
    /// List known agents and whether Forge can run them.
    List,
}

#[derive(Subcommand)]
enum TaskCommand {
    /// Check a task file for problems before running it.
    Validate {
        /// Path to a task file (`.yaml`, `.yml`, or `.json`).
        path: PathBuf,
    },
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
        Command::Agent { command } => match command {
            AgentCommand::List => {
                commands::agent::list()?;
                Ok(std::process::ExitCode::SUCCESS)
            }
        },
        Command::Task { command } => match command {
            TaskCommand::Validate { path } => {
                commands::task::validate(path)?;
                Ok(std::process::ExitCode::SUCCESS)
            }
        },
    }
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
}
