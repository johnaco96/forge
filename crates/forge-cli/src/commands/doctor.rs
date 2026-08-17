//! Read-only production preflight diagnostics.

use std::path::PathBuf;

use anyhow::Result;
use forge_agent::AgentRegistry;
use forge_core::security::ExecutionSandboxConfig;
use forge_executor::{
    DiskPreflightPolicy, find_executable, preflight_disk, preflight_sandbox_config,
};
use forge_store::{LATEST_MIGRATION_VERSION, Store};

use crate::commands::run::resolve_repository;
use crate::output;

pub enum DoctorExit {
    Ready,
    NotReady,
}

struct Check {
    name: &'static str,
    status: &'static str,
    detail: String,
}

pub async fn run(repo: Option<PathBuf>) -> Result<DoctorExit> {
    let (repository, layout, config) = resolve_repository(repo.as_deref())?;
    let mut checks = Vec::new();
    let mut blocked = false;

    checks.push(Check {
        name: "repository",
        status: "PASS",
        detail: format!(
            "{} @ {}",
            repository.root().display(),
            repository.short(&repository.head_commit()?)
        ),
    });
    checks.push(Check {
        name: "version",
        status: "PASS",
        detail: format!(
            "forge {} (workspace package source)",
            env!("CARGO_PKG_VERSION")
        ),
    });

    let store_path = layout.store_path(&config);
    match Store::verify_file(&store_path).await {
        Ok(verification)
            if verification.is_usable()
                && verification.migration_version == LATEST_MIGRATION_VERSION =>
        {
            checks.push(Check {
                name: "store",
                status: "PASS",
                detail: format!(
                    "integrity ok, schema {}, {} runs",
                    verification.migration_version, verification.run_count
                ),
            });
        }
        Ok(verification) => {
            blocked = true;
            checks.push(Check {
                name: "store",
                status: "FAIL",
                detail: format!("verification did not meet current schema: {verification:?}"),
            });
        }
        Err(error) => {
            blocked = true;
            checks.push(Check {
                name: "store",
                status: "FAIL",
                detail: error.to_string(),
            });
        }
    }

    match preflight_disk(
        [layout.worktrees_root(&config), store_path],
        DiskPreflightPolicy {
            minimum_free_bytes: config.resources.minimum_free_bytes,
            minimum_free_percent: config.resources.minimum_free_percent,
        },
    ) {
        Ok(observations) => checks.push(Check {
            name: "disk",
            status: "PASS",
            detail: observations
                .into_iter()
                .map(|(path, value)| {
                    format!(
                        "{}: {} bytes ({:.2}%) free",
                        path.display(),
                        value.available_bytes,
                        value.free_percent()
                    )
                })
                .collect::<Vec<_>>()
                .join("; "),
        }),
        Err(error) => {
            blocked = true;
            checks.push(Check {
                name: "disk",
                status: "FAIL",
                detail: error.to_string(),
            });
        }
    }

    match &config.containment {
        ExecutionSandboxConfig::None => {
            blocked = true;
            checks.push(Check {
                name: "containment",
                status: "WARN",
                detail: "mode=none; acceptable for development, not supervised production"
                    .into(),
            });
        }
        ExecutionSandboxConfig::Required {
            runtime,
            image,
            network,
            cpu_millis,
            memory_bytes,
            pids_limit,
            workspace_limit_bytes,
            credential_env,
            ..
        } => match preflight_sandbox_config(&config.containment).await {
            Ok(()) => checks.push(Check {
                name: "containment",
                status: "PASS",
                detail: format!(
                    "{runtime} image={image}, network={}, cpu={:.3}, memory={memory_bytes}, pids={pids_limit}, workspace={workspace_limit_bytes}, credentials={} explicit variable(s)",
                    network.as_str(),
                    *cpu_millis as f64 / 1000.0,
                    credential_env.len(),
                ),
            }),
            Err(error) => {
                blocked = true;
                checks.push(Check {
                    name: "containment",
                    status: "FAIL",
                    detail: error.to_string(),
                });
            }
        },
    }

    if let Some(agent_id) = &config.defaults.agent {
        let registry = AgentRegistry::builtin();
        match registry.get(agent_id) {
            None => {
                blocked = true;
                checks.push(Check {
                    name: "default agent",
                    status: "FAIL",
                    detail: format!("unknown configured agent `{agent_id}`"),
                });
            }
            Some(descriptor) => {
                let executable = config
                    .agent(agent_id)
                    .executable
                    .or_else(|| descriptor.executable.clone());
                let (status, detail) = match (&config.containment, executable) {
                    (ExecutionSandboxConfig::Required { image, .. }, Some(executable)) => (
                        "PASS",
                        format!("`{executable}` is expected inside verified image `{image}`"),
                    ),
                    (_, Some(executable)) => match find_executable(&executable) {
                        Some(path) => ("PASS", format!("{}", path.display())),
                        None => {
                            blocked = true;
                            ("FAIL", format!("`{executable}` not found on host PATH"))
                        }
                    },
                    (_, None) => ("PASS", "adapter needs no external executable".into()),
                };
                checks.push(Check {
                    name: "default agent",
                    status,
                    detail,
                });
            }
        }
    }

    let rows = checks
        .iter()
        .map(|check| {
            vec![
                check.name.to_string(),
                check.status.to_string(),
                check.detail.clone(),
            ]
        })
        .collect::<Vec<_>>();
    println!(
        "Forge doctor\n\n{}",
        output::table(&["check", "status", "detail"], &rows)
    );
    println!(
        "\n{}",
        if blocked {
            "Result: NOT READY for supervised production"
        } else {
            "Result: production preflight passed"
        }
    );
    Ok(if blocked {
        DoctorExit::NotReady
    } else {
        DoctorExit::Ready
    })
}
