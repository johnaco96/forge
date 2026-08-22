//! Bounded, production-representative live agent qualification.
//!
//! The probe intentionally runs only when an operator asks for it: it reaches
//! a real provider and may consume a small amount of quota. It creates a
//! standalone temporary Git repository, executes the configured adapter under
//! the exact Forge OCI boundary, requires one controlled file mutation, and
//! removes successful evidence automatically. A failure keeps its disposable
//! directory so the already-redacted harness logs can be inspected.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use forge_agent::{AgentError, AgentRegistry, RunContext};
use forge_core::config::{ForgeConfig, Layout};
use forge_core::events::NullSink;
use forge_core::ids::{RunId, TaskId};
use forge_core::integrity::ProtectionPolicy;
use forge_core::run::{
    AgentExecution, AgentExecutionStatus, ExecutionProvenance, InfrastructureFailureKind,
};
use forge_core::security::ExecutionSandboxConfig;
use forge_core::task::{EngineeringTask, EvaluationSpec, TaskClassification, TaskMetadata};
use forge_core::workspace::{Workspace, WorkspaceKind};
use forge_executor::{DiskWatch, DockerSandbox, ExecError};
use forge_git::Repository;
use forge_runner::{RunRequest, Runner};
use forge_store::Store;
use tempfile::TempDir;

const MARKER_NAME: &str = "forge-production-probe.txt";
const MARKER_CONTENT: &str = "forge-agent-probe-ok\n";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeFailureKind {
    ExecutableMissing,
    VersionIncompatible,
    AuthenticationFailure,
    SandboxFailure,
    ProcessFailure,
    Timeout,
    FilesystemFailure,
    ProviderFailure,
}

impl ProbeFailureKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::ExecutableMissing => "executable_missing",
            Self::VersionIncompatible => "version_incompatible",
            Self::AuthenticationFailure => "authentication_failure",
            Self::SandboxFailure => "sandbox_failure",
            Self::ProcessFailure => "process_failure",
            Self::Timeout => "timeout",
            Self::FilesystemFailure => "filesystem_failure",
            Self::ProviderFailure => "provider_failure",
        }
    }
}

#[derive(Debug)]
pub struct LiveProbeSuccess {
    pub agent_id: String,
    pub duration_ms: u64,
}

#[derive(Debug)]
pub struct LiveProbeFailure {
    pub kind: ProbeFailureKind,
    pub detail: String,
    pub evidence_dir: Option<PathBuf>,
}

impl fmt::Display for LiveProbeFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "kind={}; {}", self.kind.as_str(), self.detail)?;
        if let Some(path) = &self.evidence_dir {
            write!(
                formatter,
                "; redacted evidence retained at {}",
                path.display()
            )?;
        }
        Ok(())
    }
}

pub enum LiveProbeOutcome {
    Passed(LiveProbeSuccess),
    Failed(LiveProbeFailure),
}

/// Exercises one real agent/provider without touching the source repository or
/// its durable Forge ledger.
pub async fn run_live_agent_probe(
    repository: &Repository,
    _layout: &Layout,
    config: &ForgeConfig,
    agent_id: &str,
    timeout_secs: u64,
) -> Result<LiveProbeOutcome> {
    if !matches!(config.containment, ExecutionSandboxConfig::Required { .. }) {
        return Ok(LiveProbeOutcome::Failed(LiveProbeFailure {
            kind: ProbeFailureKind::SandboxFailure,
            detail: "a production live probe requires containment.mode=required".into(),
            evidence_dir: None,
        }));
    }

    let fixture = ProbeFixture::create()?;
    let task = probe_task()?;
    let store = Store::open_in_memory()
        .await
        .context("opening the live-probe in-memory ledger")?;
    let runner = Runner::new(repository.clone(), config.clone(), store);
    let mut request = RunRequest::new(task.clone(), agent_id);
    request.timeout = Some(Duration::from_secs(timeout_secs));
    request.execution_provenance = ExecutionProvenance::Live;
    let agent_config = match runner.agent_config(&request) {
        Ok(agent_config) => agent_config,
        Err(error) => {
            return Ok(retained_failure(
                fixture.temp,
                ProbeFailureKind::ProcessFailure,
                format!("could not resolve agent configuration: {error}"),
            ));
        }
    };

    let registry = AgentRegistry::builtin();
    let adapter = match registry.adapter(agent_id, &agent_config) {
        Ok(adapter) => adapter,
        Err(error) => {
            let kind = classify_agent_error(&error);
            return Ok(retained_failure(
                fixture.temp,
                kind,
                compact(&error.to_string()),
            ));
        }
    };
    if let Err(error) = adapter.prepare().await {
        let kind = classify_agent_error(&error);
        return Ok(retained_failure(
            fixture.temp,
            kind,
            compact(&error.to_string()),
        ));
    }

    let sandbox = match DockerSandbox::from_config(
        &config.containment,
        &fixture.git_dir,
        &fixture.workspace,
        &format!("doctor-live-probe-{agent_id}"),
    ) {
        Ok(Some(sandbox)) => sandbox,
        Ok(None) => {
            return Ok(retained_failure(
                fixture.temp,
                ProbeFailureKind::SandboxFailure,
                "required containment unexpectedly produced no execution sandbox".into(),
            ));
        }
        Err(error) => {
            return Ok(retained_failure(
                fixture.temp,
                ProbeFailureKind::SandboxFailure,
                compact(&error.to_string()),
            ));
        }
    };

    let run_id = RunId::new("DOCTOR-PROBE").expect("static probe run ID is valid");
    let workspace = Workspace::new(
        run_id.clone(),
        WorkspaceKind::Worktree,
        fixture.workspace.clone(),
        "forge/doctor-probe",
        fixture.base_commit.clone(),
    );
    let artifacts_dir = fixture.temp.path().join("artifacts");
    fs::create_dir_all(&artifacts_dir).context("creating live-probe artifact directory")?;
    let mut context = RunContext::new(
        &run_id,
        &task,
        &workspace,
        &agent_config,
        &NullSink,
        artifacts_dir,
    )
    .with_timeout(Some(Duration::from_secs(timeout_secs)))
    .with_sandbox(Some(sandbox));
    if let ExecutionSandboxConfig::Required {
        workspace_limit_bytes,
        ..
    } = &config.containment
    {
        context = context.with_disk_watch(
            DiskWatch::new(
                [fixture.workspace.clone()],
                config.resources.emergency_free_bytes,
                Duration::from_secs(config.resources.disk_watch_interval_secs),
            )
            .with_workspace_limit(&fixture.workspace, *workspace_limit_bytes),
        );
    }

    let execution = match adapter.execute(&context).await {
        Ok(execution) => execution,
        Err(error) => {
            let kind = classify_agent_error(&error);
            return Ok(retained_failure(
                fixture.temp,
                kind,
                compact(&format!("agent invocation could not start: {error}")),
            ));
        }
    };
    let diagnostic = execution_diagnostic(&execution);
    if let Some(failure) = execution.infrastructure_failures.first() {
        return Ok(retained_failure(
            fixture.temp,
            classify_infrastructure(failure.kind),
            compact(&format!("{}; {diagnostic}", failure.detail)),
        ));
    }
    if execution.timed_out || execution.status == AgentExecutionStatus::TimedOut {
        return Ok(retained_failure(
            fixture.temp,
            ProbeFailureKind::Timeout,
            compact(&format!(
                "agent exceeded the {timeout_secs}s live-probe budget; {diagnostic}"
            )),
        ));
    }
    if execution.status != AgentExecutionStatus::Completed {
        return Ok(retained_failure(
            fixture.temp,
            classify_diagnostic(&diagnostic),
            compact(&format!(
                "agent process status={} exit={:?}; {diagnostic}",
                execution.status.as_str(),
                execution.exit_code
            )),
        ));
    }
    if let Err(detail) = verify_workspace(&fixture.workspace) {
        let combined = format!("{detail}; {diagnostic}");
        return Ok(retained_failure(
            fixture.temp,
            classify_diagnostic(&combined),
            compact(&combined),
        ));
    }

    Ok(LiveProbeOutcome::Passed(LiveProbeSuccess {
        agent_id: agent_id.to_string(),
        duration_ms: execution.duration_ms,
    }))
}

struct ProbeFixture {
    temp: TempDir,
    workspace: PathBuf,
    git_dir: PathBuf,
    base_commit: String,
}

impl ProbeFixture {
    fn create() -> Result<Self> {
        let temp = tempfile::Builder::new()
            .prefix("forge-agent-probe-")
            .tempdir_in(std::env::temp_dir())
            .context("creating a disposable live-probe directory")?;
        let workspace = temp.path().join("workspace");
        fs::create_dir_all(&workspace).context("creating the disposable probe workspace")?;
        run_git(&workspace, &["init", "--quiet", "--initial-branch=main"])?;
        run_git(
            &workspace,
            &["config", "user.email", "forge-probe@example.invalid"],
        )?;
        run_git(&workspace, &["config", "user.name", "Forge Probe"])?;
        fs::write(
            workspace.join("README.md"),
            "# Forge production agent probe\n",
        )
        .context("writing the disposable probe fixture")?;
        run_git(&workspace, &["add", "README.md"])?;
        run_git(
            &workspace,
            &[
                "-c",
                "commit.gpgsign=false",
                "commit",
                "--quiet",
                "--no-verify",
                "-m",
                "probe base",
            ],
        )?;
        let base_commit = run_git(&workspace, &["rev-parse", "HEAD"])?;
        let git_dir = fs::canonicalize(workspace.join(".git"))
            .context("resolving disposable probe Git metadata")?;
        Ok(Self {
            temp,
            workspace: fs::canonicalize(workspace)
                .context("resolving disposable probe workspace")?,
            git_dir,
            base_commit,
        })
    }
}

fn probe_task() -> Result<EngineeringTask> {
    Ok(EngineeringTask {
        task_id: TaskId::new("PROBE").map_err(|error| anyhow!(error))?,
        repository: "forge-production-probe".into(),
        objective: format!(
            "Production qualification probe. In the workspace root, create `{MARKER_NAME}` with exactly the UTF-8 content `forge-agent-probe-ok` followed by one newline. Use a harmless file or shell operation. Do not modify any other file and do not run broad tests."
        ),
        constraints: vec![
            "Modify only the requested marker file in this disposable workspace".into(),
            "Do not inspect or print credentials or environment-variable values".into(),
        ],
        evaluation: EvaluationSpec::default(),
        protection: ProtectionPolicy::default(),
        metadata: TaskMetadata::default(),
        classification: TaskClassification::default(),
        components: Vec::new(),
        tags: vec!["production-probe".into()],
    })
}

fn verify_workspace(workspace: &Path) -> std::result::Result<(), String> {
    let marker = workspace.join(MARKER_NAME);
    let content = fs::read_to_string(&marker).map_err(|error| {
        format!(
            "controlled marker `{MARKER_NAME}` was not readable after a successful process exit: {error}"
        )
    })?;
    if content != MARKER_CONTENT {
        return Err(format!(
            "controlled marker had unexpected content (expected {} bytes, found {} bytes)",
            MARKER_CONTENT.len(),
            content.len()
        ));
    }
    let status = run_git(
        workspace,
        &["status", "--porcelain=v1", "--untracked-files=all"],
    )
    .map_err(|error| format!("could not verify probe workspace changes: {error:#}"))?;
    let expected = format!("?? {MARKER_NAME}");
    if status != expected {
        return Err(format!(
            "workspace mutation was not controlled: expected Git status `{expected}`, found `{status}`"
        ));
    }
    Ok(())
}

fn run_git(workspace: &Path, args: &[&str]) -> Result<String> {
    let mut command = Command::new("git");
    command
        .env_clear()
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0");
    if let Some(path) = std::env::var_os("PATH") {
        command.env("PATH", path);
    }
    let output = command
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.untrackedCache=false",
            "-c",
            "commit.gpgsign=false",
        ])
        .arg("-C")
        .arg(workspace)
        .args(args)
        .output()
        .with_context(|| format!("running git in {}", workspace.display()))?;
    if !output.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn retained_failure(temp: TempDir, kind: ProbeFailureKind, detail: String) -> LiveProbeOutcome {
    LiveProbeOutcome::Failed(LiveProbeFailure {
        kind,
        detail,
        evidence_dir: Some(temp.keep()),
    })
}

fn classify_agent_error(error: &AgentError) -> ProbeFailureKind {
    match error {
        AgentError::ExecutableNotFound { .. } => ProbeFailureKind::ExecutableMissing,
        AgentError::Exec(ExecError::Infrastructure(failure)) => {
            classify_infrastructure(failure.kind)
        }
        AgentError::Exec(ExecError::Spawn { source, .. })
            if source.kind() == std::io::ErrorKind::NotFound =>
        {
            ProbeFailureKind::ExecutableMissing
        }
        AgentError::Exec(ExecError::MissingWorkingDirectory(_))
        | AgentError::Exec(ExecError::Git(_))
        | AgentError::Exec(ExecError::Io { .. }) => ProbeFailureKind::FilesystemFailure,
        AgentError::Unavailable { reason, .. }
            if reason.to_ascii_lowercase().contains("version") =>
        {
            ProbeFailureKind::VersionIncompatible
        }
        other => classify_diagnostic(&other.to_string()),
    }
}

fn classify_infrastructure(kind: InfrastructureFailureKind) -> ProbeFailureKind {
    match kind {
        InfrastructureFailureKind::CredentialUnavailable
        | InfrastructureFailureKind::CredentialPolicyViolation => {
            ProbeFailureKind::AuthenticationFailure
        }
        InfrastructureFailureKind::SandboxUnavailable => ProbeFailureKind::SandboxFailure,
        InfrastructureFailureKind::DiskExhausted
        | InfrastructureFailureKind::WorkspaceCleanupFailed
        | InfrastructureFailureKind::StoreUnavailable => ProbeFailureKind::FilesystemFailure,
        InfrastructureFailureKind::NetworkPolicyViolation => ProbeFailureKind::ProviderFailure,
        InfrastructureFailureKind::MemoryLimitExceeded
        | InfrastructureFailureKind::CpuLimitExceeded
        | InfrastructureFailureKind::EvaluatorToolUnavailable => ProbeFailureKind::ProcessFailure,
    }
}

fn classify_diagnostic(detail: &str) -> ProbeFailureKind {
    let detail = detail.to_ascii_lowercase();
    if contains_any(
        &detail,
        &[
            "bwrap:",
            "bubblewrap",
            "no permissions to create a new namespace",
            "sandbox unavailable",
            "sandbox_unavailable",
        ],
    ) {
        ProbeFailureKind::SandboxFailure
    } else if contains_any(
        &detail,
        &[
            "authentication failed",
            "authentication_failure",
            "unauthorized",
            "invalid api key",
            "invalid x-api-key",
            "credential unavailable",
            "credential_unavailable",
            "login required",
            "http 401",
        ],
    ) {
        ProbeFailureKind::AuthenticationFailure
    } else if contains_any(
        &detail,
        &["timed out", "timeout", "deadline exceeded", "time limit"],
    ) {
        ProbeFailureKind::Timeout
    } else if contains_any(
        &detail,
        &[
            "read-only file system",
            "permission denied",
            "no space left on device",
            "filesystem failure",
        ],
    ) {
        ProbeFailureKind::FilesystemFailure
    } else if contains_any(
        &detail,
        &[
            "credit balance",
            "rate limit",
            "too many requests",
            "overloaded",
            "service unavailable",
            "failed to connect",
            "connection error",
            "http 429",
            "http 500",
            "http 502",
            "http 503",
            "api error",
            "api_error",
            "billing error",
        ],
    ) {
        ProbeFailureKind::ProviderFailure
    } else if contains_any(
        &detail,
        &[
            "executable_missing",
            "command not found",
            "not found on path",
        ],
    ) {
        ProbeFailureKind::ExecutableMissing
    } else if contains_any(&detail, &["version incompatible", "version_incompatible"]) {
        ProbeFailureKind::VersionIncompatible
    } else {
        ProbeFailureKind::ProcessFailure
    }
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}

fn execution_diagnostic(execution: &AgentExecution) -> String {
    let mut parts = Vec::new();
    if let Some(report) = execution
        .self_report
        .as_deref()
        .filter(|value| !value.is_empty())
    {
        parts.push(report.to_string());
    }
    for path in [&execution.stderr_path, &execution.stdout_path]
        .into_iter()
        .flatten()
    {
        if let Ok(contents) = fs::read_to_string(path)
            && !contents.trim().is_empty()
        {
            parts.push(contents);
        }
    }
    if parts.is_empty() {
        "no harness diagnostic was emitted".into()
    } else {
        compact(&parts.join(" "))
    }
}

fn compact(value: &str) -> String {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = normalized.chars();
    let compact = chars.by_ref().take(600).collect::<String>();
    if chars.next().is_some() {
        format!("{compact}…")
    } else {
        compact
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostics_have_distinct_required_failure_classes() {
        let cases = [
            ("command not found", ProbeFailureKind::ExecutableMissing),
            (
                "version incompatible with configured harness",
                ProbeFailureKind::VersionIncompatible,
            ),
            (
                "HTTP 401 unauthorized",
                ProbeFailureKind::AuthenticationFailure,
            ),
            (
                "bwrap: no permissions to create a new namespace",
                ProbeFailureKind::SandboxFailure,
            ),
            ("child exited non-zero", ProbeFailureKind::ProcessFailure),
            ("deadline exceeded", ProbeFailureKind::Timeout),
            ("read-only file system", ProbeFailureKind::FilesystemFailure),
            (
                "Credit balance is too low",
                ProbeFailureKind::ProviderFailure,
            ),
            (
                "controlled marker was not readable: No such file or directory (os error 2)",
                ProbeFailureKind::ProcessFailure,
            ),
        ];
        for (diagnostic, expected) in cases {
            assert_eq!(classify_diagnostic(diagnostic), expected, "{diagnostic}");
        }
    }

    #[test]
    fn workspace_verification_requires_exactly_one_exact_marker() {
        let fixture = ProbeFixture::create().unwrap();
        fs::write(fixture.workspace.join(MARKER_NAME), MARKER_CONTENT).unwrap();
        verify_workspace(&fixture.workspace).unwrap();

        fs::write(fixture.workspace.join("unexpected.txt"), "not allowed\n").unwrap();
        let error = verify_workspace(&fixture.workspace).unwrap_err();
        assert!(error.contains("not controlled"), "{error}");
    }
}
