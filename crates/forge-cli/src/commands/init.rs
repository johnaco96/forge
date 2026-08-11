//! `forge init` — prepare a repository for Forge.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use forge_agent::AgentRegistry;
use forge_core::config::{ForgeConfig, Layout};
use forge_git::Repository;
use forge_store::Store;

use crate::output;

/// Contents of `.forge/.gitignore`.
///
/// Configuration and task definitions belong in version control — they are the
/// experiment definition. Everything an agent or a run produces does not.
const FORGE_GITIGNORE: &str = "\
# Written by `forge init`.
# Configuration and task definitions are tracked; run output is not.
worktrees/
runs/
forge.db
forge.db-wal
forge.db-shm
";

const EXAMPLE_TASK: &str = r#"# An example Forge task. Copy this file and edit it.
#
# A task separates three things:
#   - objective:   what the agent should achieve, in prose
#   - constraints: invariants the change must not violate
#   - evaluation:  commands Forge runs itself to judge the result
#
# Forge never asks the agent whether it succeeded.

task_id: T-0001
repository: REPOSITORY_NAME
objective: >-
  Describe the engineering outcome you want, not the implementation steps.
  State what "better" means here so the result can be measured.

constraints:
  - All existing tests must pass
  - Public API must remain source-compatible

evaluation:
  tests:
    command: cargo test --workspace
  lint:
    command: cargo clippy --workspace --all-targets -- -D warnings
  # benchmark:
  #   command: ./bench/run.sh
  #   metrics_file: .forge-metrics.json
  #   timeout_secs: 1800
  # security:
  #   command: ./scripts/security-check.sh
  #   required: false
  # complexity:
  #   command: ./scripts/complexity.sh
  #   metrics_file: .forge-complexity.json
  # custom:
  #   - id: api_contract
  #     command: ./scripts/api-contract.sh

# Evaluation inputs are compared with the base commit after the agent runs.
protected_paths:
  - tests/**
  - benches/**

# Optional, repository-defined historical-analysis fields. Forge does not ask
# an agent or model to infer these values.
classification:
  category: refactor
  language: rust
  domain: core
  difficulty: medium
components:
  - forge-core
tags:
  - maintainability
"#;

pub struct InitArgs {
    pub repo: Option<PathBuf>,
    pub force: bool,
}

pub async fn run(args: InitArgs) -> Result<()> {
    let start = args
        .repo
        .clone()
        .unwrap_or(std::env::current_dir().context("resolving the current directory")?);

    let repository = Repository::discover(&start).map_err(|err| {
        anyhow!(
            "{err}\n\n\
             Forge works against repository history, so it needs a Git repository.\n\
             Run `git init` here, or point Forge elsewhere with --repo <path>."
        )
    })?;

    let layout = Layout::new(repository.root().to_path_buf());
    if layout.is_initialized() && !args.force {
        return report_existing(&repository, &layout);
    }

    // Sampled before anything is written: the files `init` creates are
    // untracked by definition, and would otherwise look like pre-existing
    // uncommitted work.
    let was_dirty = repository.is_clean().map(|clean| !clean).unwrap_or(false);

    let config = ForgeConfig::default_for(repository.name());
    let mut created: Vec<String> = Vec::new();

    for dir in [
        layout.forge_dir(),
        layout.tasks_dir(),
        layout.runs_dir(),
        layout.worktrees_root(&config),
    ] {
        fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    }

    write_if_absent(
        &layout.forge_dir().join(".gitignore"),
        FORGE_GITIGNORE,
        args.force,
        &layout,
        &mut created,
    )?;

    let config_path = layout.config_path();
    if !config_path.exists() || args.force {
        fs::write(&config_path, ForgeConfig::template(&repository.name()))
            .with_context(|| format!("writing {}", config_path.display()))?;
        created.push(relative(&layout, &config_path));
    }

    write_if_absent(
        &layout.tasks_dir().join("example.yaml"),
        &EXAMPLE_TASK.replace("REPOSITORY_NAME", &repository.name()),
        false,
        &layout,
        &mut created,
    )?;

    // Opening the ledger creates it and applies migrations, so a fresh
    // repository is immediately ready to record runs.
    let store_path = layout.store_path(&config);
    let existed = store_path.exists();
    let store = Store::open(&store_path)
        .await
        .with_context(|| format!("creating the ledger at {}", store_path.display()))?;
    store
        .record_repository(&config.repository.name, repository.root())
        .await?;
    store
        .record_agents(AgentRegistry::builtin().descriptors())
        .await?;
    store.close().await;
    if !existed {
        created.push(relative(&layout, &store_path));
    }

    print_summary(&repository, &layout, &config, &created, was_dirty);
    Ok(())
}

fn write_if_absent(
    path: &Path,
    contents: &str,
    force: bool,
    layout: &Layout,
    created: &mut Vec<String>,
) -> Result<()> {
    if path.exists() && !force {
        return Ok(());
    }
    fs::write(path, contents).with_context(|| format!("writing {}", path.display()))?;
    created.push(relative(layout, path));
    Ok(())
}

fn relative(layout: &Layout, path: &Path) -> String {
    path.strip_prefix(layout.root())
        .unwrap_or(path)
        .display()
        .to_string()
}

fn report_existing(repository: &Repository, layout: &Layout) -> Result<()> {
    let config = ForgeConfig::load(layout.config_path()).with_context(|| {
        format!(
            "`{}` exists but could not be read; fix it or re-run with --force",
            layout.config_path().display()
        )
    })?;

    println!("Already initialized\n");
    println!(
        "{}",
        output::fields(&[
            ("Repository", config.repository.name.clone()),
            ("Root", repository.root().display().to_string()),
            ("Config", relative(layout, &layout.config_path())),
        ])
    );
    println!("\nRe-run with --force to rewrite the configuration.");
    Ok(())
}

fn print_summary(
    repository: &Repository,
    layout: &Layout,
    config: &ForgeConfig,
    created: &[String],
    was_dirty: bool,
) {
    let head = repository
        .head_commit()
        .map(|c| repository.short(&c))
        .unwrap_or_else(|_| "none yet".to_string());

    println!("Forge initialized\n");
    println!(
        "{}",
        output::fields(&[
            ("Repository", config.repository.name.clone()),
            ("Root", repository.root().display().to_string()),
            ("Base commit", head),
            ("Ledger", config.store.path.clone()),
            ("Workspaces", config.workspaces.root.clone()),
        ])
    );

    if !created.is_empty() {
        println!("\n{}", output::section("Created", output::bullets(created)));
    }

    println!(
        "\n{}",
        output::section(
            "Next",
            output::bullets([
                "forge agent list".to_string(),
                format!(
                    "edit {}",
                    relative(layout, &layout.tasks_dir().join("example.yaml"))
                ),
                "forge task validate .forge/tasks/example.yaml".to_string(),
            ])
        )
    );

    if was_dirty {
        println!(
            "\nNote: the working tree has uncommitted changes. Agents run from committed\n\
             state, so anything uncommitted will not be visible to them."
        );
    }
}
