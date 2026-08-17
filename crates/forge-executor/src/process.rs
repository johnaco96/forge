//! Subprocess execution with timeouts, output capture, and cleanup.
//!
//! Every command Forge runs — the agent itself, and every evaluation command —
//! goes through here, so that all of them are timed, bounded, logged as events,
//! and cleaned up the same way.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use forge_core::events::{EventPayload, EventSink};
use forge_core::run::InfrastructureFailure;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

use crate::error::{ExecError, ExecResult};
use crate::resource::DiskWatch;
use crate::sandbox::{EnvPolicy, ExecutionSandbox, Redactor, SandboxedInvocation};

/// Cap on captured stdout/stderr per command.
///
/// Output beyond this is discarded, but still read, so a chatty process is
/// never blocked on a full pipe.
pub const DEFAULT_MAX_OUTPUT_BYTES: usize = 4 * 1024 * 1024;

/// How long to keep draining pipes after the process exits.
///
/// A process can leave grandchildren holding the pipe open; without a bound,
/// waiting for EOF would hang the run.
const PIPE_DRAIN_GRACE: Duration = Duration::from_secs(5);

/// How long a killed process gets to exit before it is killed harder.
const TERM_GRACE: Duration = Duration::from_secs(2);

/// One command to run.
#[derive(Debug, Clone)]
pub struct ExecRequest {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub timeout: Option<Duration>,
    /// Extra variables layered on top of the runner's environment policy.
    pub env: BTreeMap<String, String>,
    /// Human-readable form used in events and errors.
    pub label: String,
    /// Emergency disk floor monitored for the lifetime of the process.
    pub disk_watch: Option<DiskWatch>,
}

impl ExecRequest {
    /// Runs a command line through `sh -c`, so pipelines, redirection, and `&&`
    /// behave as written in a task file.
    pub fn shell(command: impl Into<String>, cwd: impl Into<PathBuf>) -> Self {
        let command = command.into();
        Self {
            program: "/bin/sh".to_string(),
            args: vec!["-c".to_string(), command.clone()],
            cwd: cwd.into(),
            timeout: None,
            env: BTreeMap::new(),
            label: command,
            disk_watch: None,
        }
    }

    /// Runs a program directly, with no shell involved.
    pub fn program(
        program: impl Into<String>,
        args: impl IntoIterator<Item = impl Into<String>>,
        cwd: impl Into<PathBuf>,
    ) -> Self {
        let program = program.into();
        let args: Vec<String> = args.into_iter().map(Into::into).collect();
        let label = if args.is_empty() {
            program.clone()
        } else {
            format!("{program} {}", args.join(" "))
        };
        Self {
            program,
            args,
            cwd: cwd.into(),
            timeout: None,
            env: BTreeMap::new(),
            label,
            disk_watch: None,
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    pub fn with_timeout_secs(self, seconds: u64) -> Self {
        self.with_timeout(Duration::from_secs(seconds))
    }

    /// Applies a timeout only if one is not already set.
    pub fn with_default_timeout(mut self, timeout: Option<Duration>) -> Self {
        if self.timeout.is_none() {
            self.timeout = timeout;
        }
        self
    }

    pub fn with_env(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(name.into(), value.into());
        self
    }

    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = label.into();
        self
    }

    pub fn with_disk_watch(mut self, watch: DiskWatch) -> Self {
        self.disk_watch = Some(watch);
        self
    }
}

/// What a command did.
#[derive(Debug, Clone)]
pub struct ExecOutcome {
    pub label: String,
    /// `None` when the process was killed by a signal or a timeout.
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub duration: Duration,
    pub timed_out: bool,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    /// Operational causes for controlled termination. These remain separate
    /// from the subprocess exit and any later candidate evaluation.
    pub infrastructure_failures: Vec<InfrastructureFailure>,
}

impl ExecOutcome {
    pub fn success(&self) -> bool {
        !self.timed_out && self.exit_code == Some(0)
    }

    pub fn duration_ms(&self) -> u64 {
        self.duration.as_millis() as u64
    }

    /// Exit code for the event stream: a timeout is reported as the shell's
    /// `128 + SIGKILL` convention rather than as a success.
    pub fn event_exit_code(&self) -> i32 {
        self.exit_code
            .unwrap_or(if self.timed_out { 137 } else { -1 })
    }

    /// The tail of the output, for failure messages.
    pub fn tail(&self, max_lines: usize) -> String {
        let combined = if self.stderr.trim().is_empty() {
            &self.stdout
        } else {
            &self.stderr
        };
        let lines: Vec<&str> = combined.lines().collect();
        let start = lines.len().saturating_sub(max_lines);
        lines[start..].join("\n")
    }
}

/// Runs commands under a fixed environment policy.
#[derive(Debug, Clone)]
pub struct ProcessRunner {
    policy: EnvPolicy,
    redactor: Redactor,
    max_output_bytes: usize,
    disk_watch: Option<DiskWatch>,
    sandbox: Option<Arc<dyn ExecutionSandbox>>,
}

impl ProcessRunner {
    pub fn new(policy: EnvPolicy) -> Self {
        let redactor = policy.redactor();
        Self {
            policy,
            redactor,
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
            disk_watch: None,
            sandbox: None,
        }
    }

    /// A runner with the conservative environment policy.
    pub fn conservative() -> Self {
        Self::new(EnvPolicy::conservative())
    }

    pub fn with_max_output_bytes(mut self, max: usize) -> Self {
        self.max_output_bytes = max;
        self
    }

    /// Replaces the redactor derived from the environment policy.
    pub fn with_redactor(mut self, redactor: Redactor) -> Self {
        self.redactor = redactor;
        self
    }

    /// Applies an emergency disk floor to requests which do not override it.
    pub fn with_disk_watch(mut self, watch: DiskWatch) -> Self {
        self.disk_watch = Some(watch);
        self
    }

    pub fn with_sandbox(mut self, sandbox: Arc<dyn ExecutionSandbox>) -> Self {
        self.sandbox = Some(sandbox);
        self
    }

    pub fn policy(&self) -> &EnvPolicy {
        &self.policy
    }

    /// Runs a command to completion, emitting a `CommandExecuted` event.
    ///
    /// A non-zero exit is a normal outcome, not an error: a failing test
    /// command is exactly the kind of evidence Forge exists to record. `Err` is
    /// reserved for Forge being unable to run the command at all.
    pub async fn run(
        &self,
        request: &ExecRequest,
        events: &dyn EventSink,
    ) -> ExecResult<ExecOutcome> {
        let outcome = self.spawn_and_wait(request, events).await?;

        for failure in &outcome.infrastructure_failures {
            events.emit(EventPayload::InfrastructureFailureObserved {
                kind: failure.kind,
                detail: failure.detail.clone(),
            });
        }

        events.emit(EventPayload::CommandExecuted {
            command: self.redactor.redact(&outcome.label),
            exit_code: outcome.event_exit_code(),
            duration_ms: outcome.duration_ms(),
        });

        Ok(outcome)
    }

    async fn spawn_and_wait(
        &self,
        request: &ExecRequest,
        events: &dyn EventSink,
    ) -> ExecResult<ExecOutcome> {
        if !request.cwd.is_dir() {
            return Err(ExecError::MissingWorkingDirectory(request.cwd.clone()));
        }

        let mut child_env = self.policy.build();
        child_env.extend(request.env.clone());
        let invocation = if let Some(sandbox) = &self.sandbox {
            if let Err(error) = sandbox.preflight().await {
                if let ExecError::Infrastructure(failure) = &error {
                    events.emit(EventPayload::InfrastructureFailureObserved {
                        kind: failure.kind,
                        detail: failure.detail.clone(),
                    });
                }
                return Err(error);
            }
            events.emit(EventPayload::SandboxPrepared {
                boundary: "Docker-compatible OCI".into(),
            });
            match sandbox.wrap(request, &child_env) {
                Ok(invocation) => invocation,
                Err(error) => {
                    if let ExecError::Infrastructure(failure) = &error {
                        events.emit(EventPayload::InfrastructureFailureObserved {
                            kind: failure.kind,
                            detail: failure.detail.clone(),
                        });
                    }
                    return Err(error);
                }
            }
        } else {
            SandboxedInvocation {
                program: request.program.clone(),
                args: request.args.clone(),
                cwd: request.cwd.clone(),
                env: child_env,
            }
        };

        let mut command = Command::new(&invocation.program);
        command
            .args(&invocation.args)
            .current_dir(&invocation.cwd)
            .env_clear()
            .envs(&invocation.env)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // If the runner is dropped mid-run (cancellation, panic), the child
            // must not outlive it.
            .kill_on_drop(true);

        // Give the child its own process group so descendants can be cleaned up
        // together. Without this, killing a timed-out `cargo test` leaves the
        // compiler running and the next run inherits a poisoned target dir.
        #[cfg(unix)]
        command.process_group(0);

        tracing::debug!(command = %request.label, cwd = %request.cwd.display(), "exec");

        let started = Instant::now();
        let mut child = command.spawn().map_err(|source| ExecError::Spawn {
            program: invocation.program.clone(),
            source,
        })?;

        let pid = child.id();
        let stdout_buf = SharedBuffer::default();
        let stderr_buf = SharedBuffer::default();
        let mut stdout_task = tokio::spawn(read_capped(
            child.stdout.take().expect("stdout piped"),
            self.max_output_bytes,
            stdout_buf.clone(),
        ));
        let mut stderr_task = tokio::spawn(read_capped(
            child.stderr.take().expect("stderr piped"),
            self.max_output_bytes,
            stderr_buf.clone(),
        ));

        let deadline = request
            .timeout
            .map(|limit| tokio::time::Instant::now() + limit);
        let disk_watch = request.disk_watch.as_ref().or(self.disk_watch.as_ref());
        let mut timed_out = false;
        let mut infrastructure_failures = Vec::new();
        let status = loop {
            let timeout_at = deadline.unwrap_or_else(|| {
                tokio::time::Instant::now() + Duration::from_secs(365 * 24 * 60 * 60)
            });
            let watch_interval = disk_watch
                .map(|watch| watch.interval)
                .unwrap_or_else(|| Duration::from_secs(365 * 24 * 60 * 60));
            tokio::select! {
                status = child.wait() => break Some(status.map_err(ExecError::Wait)?),
                _ = tokio::time::sleep_until(timeout_at), if deadline.is_some() => {
                    timed_out = true;
                    tracing::warn!(command = %request.label, "command timed out; killing");
                    terminate(&mut child, pid).await;
                    break None;
                }
                _ = tokio::time::sleep(watch_interval), if disk_watch.is_some() => {
                    if let Some(watch) = disk_watch
                        && let Err(ExecError::Infrastructure(failure)) = watch.check()
                    {
                        tracing::warn!(command = %request.label, detail = %failure.detail, "disk emergency floor reached; killing");
                        infrastructure_failures.push(failure);
                        terminate(&mut child, pid).await;
                        break None;
                    }
                }
            }
        };

        if let Some(sandbox) = &self.sandbox {
            match sandbox.post_run().await {
                Ok(failures) => infrastructure_failures.extend(failures),
                Err(ExecError::Infrastructure(failure)) => infrastructure_failures.push(failure),
                Err(error) => return Err(error),
            }
            match sandbox.cleanup().await {
                Ok(()) => events.emit(EventPayload::SandboxCleaned),
                Err(ExecError::Infrastructure(failure)) => infrastructure_failures.push(failure),
                Err(error) => return Err(error),
            }
        }

        let (stdout, stdout_truncated) = collect(&mut stdout_task, &stdout_buf).await;
        let (stderr, stderr_truncated) = collect(&mut stderr_task, &stderr_buf).await;

        Ok(ExecOutcome {
            label: request.label.clone(),
            exit_code: status.and_then(|s| s.code()),
            stdout: self.redactor.redact(&stdout),
            stderr: self.redactor.redact(&stderr),
            duration: started.elapsed(),
            timed_out,
            stdout_truncated,
            stderr_truncated,
            infrastructure_failures,
        })
    }
}

impl Default for ProcessRunner {
    fn default() -> Self {
        Self::conservative()
    }
}

/// Output accumulated by a reader task, readable even if that task is still
/// running.
type SharedBuffer = std::sync::Arc<std::sync::Mutex<(Vec<u8>, bool)>>;

/// Takes whatever a reader task has collected.
///
/// The task is given a grace period to finish, but its buffer is read either
/// way: when a grandchild keeps the pipe open, partial output is still far more
/// useful than none.
async fn collect(task: &mut tokio::task::JoinHandle<()>, buffer: &SharedBuffer) -> (String, bool) {
    let finished = tokio::time::timeout(PIPE_DRAIN_GRACE, task).await.is_ok();
    let guard = buffer
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (bytes, truncated) = &*guard;
    (
        String::from_utf8_lossy(bytes).into_owned(),
        *truncated || !finished,
    )
}

/// Reads up to `max` bytes into `buffer`, then keeps draining the pipe so the
/// writer never blocks on a full buffer.
async fn read_capped<R>(mut reader: R, max: usize, buffer: SharedBuffer)
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut chunk = [0u8; 8192];

    loop {
        match reader.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let mut guard = buffer.lock().unwrap_or_else(|p| p.into_inner());
                let (bytes, truncated) = &mut *guard;
                if bytes.len() < max {
                    let take = (max - bytes.len()).min(n);
                    bytes.extend_from_slice(&chunk[..take]);
                    if take < n {
                        *truncated = true;
                    }
                } else {
                    *truncated = true;
                }
            }
        }
    }
}

/// Kills a process and, on Unix, everything in its process group.
async fn terminate(child: &mut tokio::process::Child, pid: Option<u32>) {
    #[cfg(unix)]
    if let Some(pid) = pid {
        // A negative pid targets the whole process group established by
        // `process_group(0)`, so descendants die with their parent.
        let group = -(pid as i32);
        unsafe {
            libc::kill(group, libc::SIGTERM);
        }
        // Let the group shut down cleanly, then make sure it is gone.
        let _ = tokio::time::timeout(TERM_GRACE, child.wait()).await;
        unsafe {
            libc::kill(group, libc::SIGKILL);
        }
    }

    #[cfg(not(unix))]
    let _ = pid;

    let _ = child.kill().await;
    let _ = child.wait().await;
}

/// Finds an executable on `PATH`.
///
/// Used to report whether an agent's CLI is installed without trying to run it.
pub fn find_executable(name: &str) -> Option<PathBuf> {
    let candidate = Path::new(name);
    if candidate.components().count() > 1 {
        return is_executable(candidate).then(|| candidate.to_path_buf());
    }

    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| is_executable(candidate))
}

fn is_executable(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_core::events::{NullSink, RecordingSink};
    use forge_core::ids::RunId;
    use forge_core::run::InfrastructureFailureKind;

    #[derive(Debug)]
    struct ObservableSandbox;

    #[async_trait::async_trait]
    impl ExecutionSandbox for ObservableSandbox {
        async fn preflight(&self) -> ExecResult<()> {
            Ok(())
        }

        fn wrap(
            &self,
            request: &ExecRequest,
            child_env: &BTreeMap<String, String>,
        ) -> ExecResult<SandboxedInvocation> {
            Ok(SandboxedInvocation {
                program: request.program.clone(),
                args: request.args.clone(),
                cwd: request.cwd.clone(),
                env: child_env.clone(),
            })
        }

        async fn post_run(&self) -> ExecResult<Vec<InfrastructureFailure>> {
            Ok(vec![InfrastructureFailure::new(
                InfrastructureFailureKind::MemoryLimitExceeded,
                "fixture memory limit",
            )])
        }

        async fn cleanup(&self) -> ExecResult<()> {
            Ok(())
        }
    }

    fn runner() -> ProcessRunner {
        ProcessRunner::conservative()
    }

    fn cwd() -> PathBuf {
        std::env::temp_dir()
    }

    #[tokio::test]
    async fn captures_stdout_and_exit_code() {
        let outcome = runner()
            .run(&ExecRequest::shell("echo hello", cwd()), &NullSink)
            .await
            .unwrap();

        assert!(outcome.success());
        assert_eq!(outcome.exit_code, Some(0));
        assert_eq!(outcome.stdout.trim(), "hello");
    }

    #[tokio::test]
    async fn a_failing_command_is_an_outcome_not_an_error() {
        // Forge exists to record evidence, including failures. A non-zero exit
        // must not be surfaced as an execution error.
        let outcome = runner()
            .run(
                &ExecRequest::shell("echo oops >&2; exit 3", cwd()),
                &NullSink,
            )
            .await
            .unwrap();

        assert!(!outcome.success());
        assert_eq!(outcome.exit_code, Some(3));
        assert_eq!(outcome.stderr.trim(), "oops");
    }

    #[tokio::test]
    async fn shell_syntax_is_available_to_task_commands() {
        let outcome = runner()
            .run(
                &ExecRequest::shell("echo one && echo two | tr a-z A-Z", cwd()),
                &NullSink,
            )
            .await
            .unwrap();
        assert_eq!(outcome.stdout.trim(), "one\nTWO");
    }

    #[tokio::test]
    async fn every_command_is_recorded_as_an_event() {
        let sink = RecordingSink::new(RunId::sequential(1));
        runner()
            .run(&ExecRequest::shell("exit 2", cwd()), &sink)
            .await
            .unwrap();

        let events = sink.events();
        assert_eq!(events.len(), 1);
        assert!(matches!(
            &events[0].payload,
            EventPayload::CommandExecuted { exit_code: 2, .. }
        ));
    }

    #[tokio::test]
    async fn sandbox_lifecycle_and_resource_failure_are_recorded_in_order() {
        let sink = RecordingSink::new(RunId::sequential(2));
        let outcome = runner()
            .with_sandbox(Arc::new(ObservableSandbox))
            .run(&ExecRequest::shell("true", cwd()), &sink)
            .await
            .unwrap();

        assert!(matches!(
            outcome.infrastructure_failures.as_slice(),
            [InfrastructureFailure {
                kind: InfrastructureFailureKind::MemoryLimitExceeded,
                ..
            }]
        ));
        let events = sink.events();
        assert!(matches!(
            events.as_slice(),
            [
                forge_core::events::Event {
                    payload: EventPayload::SandboxPrepared { .. },
                    ..
                },
                forge_core::events::Event {
                    payload: EventPayload::SandboxCleaned,
                    ..
                },
                forge_core::events::Event {
                    payload: EventPayload::InfrastructureFailureObserved {
                        kind: InfrastructureFailureKind::MemoryLimitExceeded,
                        ..
                    },
                    ..
                },
                forge_core::events::Event {
                    payload: EventPayload::CommandExecuted { exit_code: 0, .. },
                    ..
                },
            ]
        ));
    }

    #[tokio::test]
    async fn a_hung_command_is_killed_at_its_timeout() {
        let started = Instant::now();
        let outcome = runner()
            .run(
                &ExecRequest::shell("sleep 30", cwd()).with_timeout(Duration::from_millis(300)),
                &NullSink,
            )
            .await
            .unwrap();

        assert!(outcome.timed_out);
        assert!(!outcome.success());
        assert_eq!(outcome.event_exit_code(), 137);
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "timeout did not stop the command promptly"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn killing_a_timed_out_command_also_kills_its_children() {
        // The shell exits immediately, leaving `sleep` orphaned. Without a
        // process-group kill, the descendant would survive the run.
        let marker = cwd().join(format!("forge-orphan-{}", std::process::id()));
        let _ = std::fs::remove_file(&marker);
        let script = format!(
            "sh -c 'sleep 2; touch {}' & sleep 30",
            marker.to_string_lossy()
        );

        let outcome = runner()
            .run(
                &ExecRequest::shell(script, cwd()).with_timeout(Duration::from_millis(200)),
                &NullSink,
            )
            .await
            .unwrap();
        assert!(outcome.timed_out);

        tokio::time::sleep(Duration::from_secs(3)).await;
        let survived = marker.exists();
        let _ = std::fs::remove_file(&marker);
        assert!(!survived, "a descendant outlived the timed-out command");
    }

    #[tokio::test]
    async fn output_is_capped_and_reported_as_truncated() {
        let outcome = runner()
            .with_max_output_bytes(64)
            .run(
                &ExecRequest::shell("head -c 100000 /dev/zero | tr '\\0' 'a'", cwd()),
                &NullSink,
            )
            .await
            .unwrap();

        assert!(outcome.stdout_truncated);
        assert!(outcome.stdout.len() <= 64);
    }

    #[tokio::test]
    async fn secrets_are_stripped_from_captured_output() {
        let runner = ProcessRunner::new(EnvPolicy::conservative())
            .with_redactor(Redactor::none().with_secret("sk-ant-super-secret"));

        let outcome = runner
            .run(
                &ExecRequest::shell("echo using sk-ant-super-secret now", cwd()),
                &NullSink,
            )
            .await
            .unwrap();

        assert!(
            !outcome.stdout.contains("sk-ant-super-secret"),
            "{}",
            outcome.stdout
        );
        assert!(outcome.stdout.contains(crate::sandbox::REDACTED));
    }

    #[tokio::test]
    async fn the_environment_policy_reaches_the_child() {
        let outcome = runner()
            .run(
                &ExecRequest::shell("echo \"[$FORGE_TEST_SECRET_TOKEN]\"", cwd()),
                &NullSink,
            )
            .await
            .unwrap();
        // Not inherited, so the variable is empty in the child.
        assert_eq!(outcome.stdout.trim(), "[]");

        let outcome = runner()
            .run(
                &ExecRequest::shell("echo \"[$FORGE_MARKER]\"", cwd())
                    .with_env("FORGE_MARKER", "on"),
                &NullSink,
            )
            .await
            .unwrap();
        assert_eq!(outcome.stdout.trim(), "[on]");
    }

    #[tokio::test]
    async fn a_missing_working_directory_is_an_execution_error() {
        let err = runner()
            .run(
                &ExecRequest::shell("true", cwd().join("definitely-not-here")),
                &NullSink,
            )
            .await
            .unwrap_err();
        assert!(
            matches!(err, ExecError::MissingWorkingDirectory(_)),
            "{err}"
        );
    }

    #[tokio::test]
    async fn a_missing_program_is_an_execution_error() {
        let err = runner()
            .run(
                &ExecRequest::program("forge-no-such-program", ["--version"], cwd()),
                &NullSink,
            )
            .await
            .unwrap_err();
        assert!(matches!(err, ExecError::Spawn { .. }), "{err}");
    }

    #[test]
    fn executables_are_found_on_path() {
        assert!(find_executable("sh").is_some());
        assert!(find_executable("forge-definitely-not-installed").is_none());
    }

    #[test]
    fn tail_prefers_stderr_and_bounds_length() {
        let outcome = ExecOutcome {
            label: "x".into(),
            exit_code: Some(1),
            stdout: "out".into(),
            stderr: (1..=10).map(|i| format!("line {i}\n")).collect(),
            duration: Duration::ZERO,
            timed_out: false,
            stdout_truncated: false,
            stderr_truncated: false,
            infrastructure_failures: Vec::new(),
        };
        assert_eq!(outcome.tail(2), "line 9\nline 10");
    }

    #[tokio::test]
    async fn disk_watchdog_terminates_before_enospc_and_classifies_the_cause() {
        let outcome = runner()
            .run(
                &ExecRequest::shell("sleep 30", cwd()).with_disk_watch(DiskWatch::new(
                    [cwd()],
                    u64::MAX,
                    Duration::from_millis(5),
                )),
                &NullSink,
            )
            .await
            .unwrap();
        assert_eq!(outcome.exit_code, None);
        assert!(!outcome.timed_out);
        assert!(matches!(
            outcome.infrastructure_failures.as_slice(),
            [InfrastructureFailure {
                kind: forge_core::run::InfrastructureFailureKind::DiskExhausted,
                ..
            }]
        ));
    }
}
