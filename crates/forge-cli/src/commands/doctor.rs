//! Production preflight diagnostics with an explicit live-provider option.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};
use forge_agent::AgentRegistry;
use forge_core::agent::AdapterStatus;
use forge_core::events::NullSink;
use forge_core::security::ExecutionSandboxConfig;
use forge_core::task::EngineeringTask;
use forge_eval::{EvaluationPlan, EvaluatorPrerequisite};
use forge_executor::{
    DiskPreflightPolicy, DockerSandbox, EnvPolicy, ExecRequest, ProcessRunner, find_executable,
    preflight_disk, preflight_sandbox_config, preflight_sandbox_evaluator_tool,
    preflight_sandbox_executable,
};
use forge_git::Repository;
use forge_store::{LATEST_MIGRATION_VERSION, Store};

use crate::commands::agent_probe::{LiveProbeOutcome, run_live_agent_probe};
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

pub struct DoctorArgs {
    pub repo: Option<PathBuf>,
    pub live_agent_probe: bool,
    pub probe_agent: Option<String>,
    pub probe_timeout_secs: u64,
}

pub async fn run(args: DoctorArgs) -> Result<DoctorExit> {
    let (repository, layout, config) = resolve_repository(args.repo.as_deref())?;
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
                    "{runtime} image={image}, network={}, cpu={:.3}, memory={memory_bytes}, pids={pids_limit}, workspace={workspace_limit_bytes}, credentials={} invocation allowlisted variable(s)",
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

    match load_evaluator_prerequisites(&layout.tasks_dir()) {
        Ok(prerequisites) if prerequisites.is_empty() => {
            if matches!(config.containment, ExecutionSandboxConfig::Required { .. }) {
                blocked = true;
                checks.push(Check {
                    name: "evaluator toolchain",
                    status: "FAIL",
                    detail: "no evaluator executable prerequisites are declared in .forge/tasks"
                        .into(),
                });
            } else {
                checks.push(Check {
                    name: "evaluator toolchain",
                    status: "WARN",
                    detail: "no evaluator executable prerequisites are declared".into(),
                });
            }
        }
        Ok(prerequisites) => {
            let mut reports = Vec::with_capacity(prerequisites.len());
            let mut failure = None;
            for prerequisite in &prerequisites {
                match preflight_sandbox_evaluator_tool(
                    &config.containment,
                    &prerequisite.evaluator_id,
                    &prerequisite.requirement,
                )
                .await
                {
                    Ok(version) => reports.push(format!(
                        "{}:{} ({})",
                        prerequisite.evaluator_id,
                        prerequisite.requirement.executable,
                        version.lines().next().unwrap_or("version not reported")
                    )),
                    Err(error) => {
                        failure = Some(error.to_string());
                        break;
                    }
                }
            }
            if let Some(error) = failure {
                blocked = true;
                checks.push(Check {
                    name: "evaluator toolchain",
                    status: "FAIL",
                    detail: error,
                });
            } else {
                checks.push(Check {
                    name: "evaluator toolchain",
                    status: "PASS",
                    detail: reports.join("; "),
                });
            }
        }
        Err(error) => {
            blocked = true;
            checks.push(Check {
                name: "evaluator toolchain",
                status: "FAIL",
                detail: error.to_string(),
            });
        }
    }

    if let ExecutionSandboxConfig::Required { credential_env, .. } = &config.containment {
        match preflight_invocation_credential_boundary(
            &repository,
            &config.containment,
            credential_env,
        )
        .await
        {
            Ok(detail) => checks.push(Check {
                name: "credential boundary",
                status: "PASS",
                detail,
            }),
            Err(error) => {
                blocked = true;
                checks.push(Check {
                    name: "credential boundary",
                    status: "FAIL",
                    detail: error.to_string(),
                });
            }
        }
    }

    let registry = AgentRegistry::builtin();
    if let Some(agent_id) = &config.defaults.agent {
        match registry.get(agent_id) {
            None => {
                blocked = true;
                checks.push(Check {
                    name: "default agent",
                    status: "FAIL",
                    detail: format!("unknown configured agent `{agent_id}`"),
                });
            }
            Some(descriptor) if descriptor.adapter_status != AdapterStatus::Implemented => {
                blocked = true;
                checks.push(Check {
                    name: "default agent",
                    status: "FAIL",
                    detail: format!("configured agent `{agent_id}` has no implemented adapter"),
                });
            }
            Some(descriptor) if matches!(config.containment, ExecutionSandboxConfig::None) => {
                let executable = config
                    .agent(agent_id)
                    .executable
                    .or_else(|| descriptor.executable.clone());
                let (status, detail) = match executable {
                    Some(executable) => match find_executable(&executable) {
                        Some(path) => ("PASS", format!("{}", path.display())),
                        None => {
                            blocked = true;
                            ("FAIL", format!("`{executable}` not found on host PATH"))
                        }
                    },
                    None => ("PASS", "adapter needs no external executable".into()),
                };
                checks.push(Check {
                    name: "default agent",
                    status,
                    detail,
                });
            }
            Some(_) => {}
        }
    }

    if matches!(config.containment, ExecutionSandboxConfig::Required { .. }) {
        for descriptor in registry
            .descriptors()
            .iter()
            .filter(|descriptor| descriptor.adapter_status == AdapterStatus::Implemented)
        {
            let agent_id = descriptor.agent_id.as_str();
            let settings = config.agent(agent_id);
            let executable = settings
                .executable
                .or_else(|| descriptor.executable.clone());
            let Some(executable) = executable else {
                checks.push(Check {
                    name: agent_check_name(agent_id),
                    status: "PASS",
                    detail: "adapter needs no external executable".into(),
                });
                continue;
            };
            let Some(expected_version) = settings.harness_version else {
                blocked = true;
                checks.push(Check {
                        name: agent_check_name(agent_id),
                        status: "FAIL",
                        detail: format!(
                            "kind=version_incompatible; required containment needs agents.{agent_id}.harness_version for reproducible preflight"
                        ),
                });
                continue;
            };
            match preflight_sandbox_executable(&config.containment, &executable).await {
                Ok(actual_version)
                    if reported_version_matches(&actual_version, &expected_version) =>
                {
                    checks.push(Check {
                        name: agent_check_name(agent_id),
                        status: "PASS",
                        detail: format!(
                            "`{executable}` in pinned image reports {actual_version} (expected {expected_version})"
                        ),
                    });
                }
                Ok(actual_version) => {
                    blocked = true;
                    checks.push(Check {
                        name: agent_check_name(agent_id),
                        status: "FAIL",
                        detail: format!(
                            "kind=version_incompatible; `{executable}` reports {actual_version}; expected harness version {expected_version}"
                        ),
                    });
                }
                Err(error) => {
                    blocked = true;
                    let detail = error.to_string();
                    let kind =
                        if detail.contains("agent executable") && detail.contains("not runnable") {
                            "executable_missing"
                        } else {
                            "sandbox_failure"
                        };
                    checks.push(Check {
                        name: agent_check_name(agent_id),
                        status: "FAIL",
                        detail: format!("kind={kind}; {detail}"),
                    });
                }
            }
        }
    }

    if args.live_agent_probe {
        let agent_id = args
            .probe_agent
            .as_deref()
            .or(config.defaults.agent.as_deref());
        match agent_id {
            None => {
                blocked = true;
                checks.push(Check {
                    name: "agent live probe",
                    status: "FAIL",
                    detail:
                        "kind=process_failure; no probe agent selected and defaults.agent is unset"
                            .into(),
                });
            }
            Some(_) if args.probe_timeout_secs == 0 => {
                blocked = true;
                checks.push(Check {
                    name: "agent live probe",
                    status: "FAIL",
                    detail: "kind=timeout; --probe-timeout-secs must be greater than zero".into(),
                });
            }
            Some(agent_id) => {
                match run_live_agent_probe(
                    &repository,
                    &layout,
                    &config,
                    agent_id,
                    args.probe_timeout_secs,
                )
                .await
                {
                    Ok(LiveProbeOutcome::Passed(report)) => checks.push(Check {
                        name: "agent live probe",
                        status: "PASS",
                        detail: format!(
                            "agent={} completed a controlled disposable-workspace mutation in {} ms; no source repository files or credentials were retained",
                            report.agent_id, report.duration_ms
                        ),
                    }),
                    Ok(LiveProbeOutcome::Failed(failure)) => {
                        blocked = true;
                        checks.push(Check {
                            name: "agent live probe",
                            status: "FAIL",
                            detail: failure.to_string(),
                        });
                    }
                    Err(error) => {
                        blocked = true;
                        checks.push(Check {
                            name: "agent live probe",
                            status: "FAIL",
                            detail: format!("kind=process_failure; {error:#}"),
                        });
                    }
                }
            }
        }
    } else if matches!(config.containment, ExecutionSandboxConfig::Required { .. }) {
        blocked = true;
        checks.push(Check {
            name: "agent live probe",
            status: "WARN",
            detail: "not executed; CLI presence and version are not production proof. Rerun with --live-agent-probe (and optionally --probe-agent).".into(),
        });
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

fn load_evaluator_prerequisites(tasks_dir: &Path) -> Result<Vec<EvaluatorPrerequisite>> {
    if !tasks_dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut paths = std::fs::read_dir(tasks_dir)?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| matches!(extension, "yaml" | "yml" | "json"))
        })
        .collect::<Vec<_>>();
    paths.sort();

    let mut prerequisites = BTreeSet::new();
    for path in paths {
        let task = EngineeringTask::load(&path)?;
        task.validate()?;
        prerequisites.extend(EvaluationPlan::resolve(&task).prerequisites());
    }
    Ok(prerequisites.into_iter().collect())
}

async fn preflight_invocation_credential_boundary(
    repository: &Repository,
    config: &ExecutionSandboxConfig,
    credential_env: &[String],
) -> Result<String> {
    if credential_env.is_empty() {
        return Err(anyhow!(
            "required containment has no job credential allowlist to probe"
        ));
    }
    let selected_credential = credential_env
        .iter()
        .find(|name| std::env::var(name).is_ok_and(|value| !value.is_empty()))
        .unwrap_or(&credential_env[0]);
    let git_common_dir = repository.git_common_dir()?;
    let sandbox = DockerSandbox::from_config(
        config,
        &git_common_dir,
        repository.root(),
        "doctor-credential-boundary",
    )?
    .ok_or_else(|| anyhow!("credential boundary probe requires containment mode `required`"))?;

    let mut agent_policy = EnvPolicy::conservative();
    for name in credential_env {
        agent_policy = agent_policy.allow_var(name);
    }
    let agent_args = vec![
        "-c".to_string(),
        "test -n \"$(printenv \"$1\")\" || exit 41; printf agent-credential-ok".to_string(),
        "forge-doctor-agent".to_string(),
        selected_credential.clone(),
    ];
    let agent = ProcessRunner::new(agent_policy)
        .with_sandbox(sandbox.clone())
        .run(
            &ExecRequest::program("/bin/sh", agent_args, repository.root())
                .with_label("doctor credentialed agent-like contained command")
                .with_timeout_secs(30)
                .with_required_credential(selected_credential.clone()),
            &NullSink,
        )
        .await?;
    if !agent.success() || agent.stdout != "agent-credential-ok" {
        return Err(anyhow!(
            "credentialed contained probe did not complete successfully: {}",
            agent.tail(5)
        ));
    }

    let mut evaluator_args = vec![
        "-c".to_string(),
        "for name do if printenv \"$name\" >/dev/null 2>&1; then exit 42; fi; done; printf evaluator-credential-isolated"
            .to_string(),
        "forge-doctor-evaluator".to_string(),
    ];
    evaluator_args.extend(credential_env.iter().cloned());
    let evaluator = ProcessRunner::conservative()
        .with_sandbox(sandbox)
        .run(
            &ExecRequest::program("/bin/sh", evaluator_args, repository.root())
                .with_label("doctor credential-free evaluator-like contained command")
                .with_timeout_secs(30),
            &NullSink,
        )
        .await?;
    if !evaluator.success() || evaluator.stdout != "evaluator-credential-isolated" {
        return Err(anyhow!(
            "credential-free evaluator probe did not complete in an isolated environment: {}",
            evaluator.tail(5)
        ));
    }

    Ok(format!(
        "credentialed command received exactly one selected allowlisted variable (`{selected_credential}`); subsequent evaluator command ran with none and observed none"
    ))
}

fn agent_check_name(agent_id: &str) -> &'static str {
    match agent_id {
        "claude" => "agent claude",
        "codex" => "agent codex",
        _ => "agent harness",
    }
}

fn reported_version_matches(actual: &str, expected: &str) -> bool {
    actual.split_whitespace().any(|part| {
        part.trim_matches(|character: char| matches!(character, '(' | ')' | ',' | ';')) == expected
    })
}

#[cfg(test)]
mod tests {
    use super::{load_evaluator_prerequisites, reported_version_matches};

    #[test]
    fn harness_versions_match_complete_tokens_not_substrings() {
        assert!(reported_version_matches("2.1.223 (Claude Code)", "2.1.223"));
        assert!(reported_version_matches("codex-cli 0.147.0", "0.147.0"));
        assert!(!reported_version_matches("codex-cli 0.147.0", "0.14"));
    }

    #[test]
    fn doctor_collects_declared_tools_from_frozen_tasks() {
        let fixture = tempfile::tempdir().unwrap();
        std::fs::write(
            fixture.path().join("task.yaml"),
            r#"
task_id: T-1
repository: forge
objective: Verify the frozen evaluator toolchain before agent execution
evaluation:
  lint:
    command: cargo fmt --check
    required_tools:
      - executable: cargo
        version_contains: "cargo 1.93."
      - executable: rustfmt
        version_contains: "rustfmt 1."
"#,
        )
        .unwrap();

        let prerequisites = load_evaluator_prerequisites(fixture.path()).unwrap();
        assert_eq!(prerequisites.len(), 2);
        assert_eq!(prerequisites[0].evaluator_id, "lint");
        assert_eq!(prerequisites[0].requirement.executable, "cargo");
        assert_eq!(prerequisites[1].requirement.executable, "rustfmt");
    }
}
