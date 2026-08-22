//! The Claude Code adapter.
//!
//! Everything Claude-specific in Forge lives in this file: the executable name,
//! the command line, the JSON result shape, and the environment variables the
//! harness needs. Nothing outside it knows Claude exists except the registry
//! entry that constructs it.
//!
//! # Security
//!
//! Claude Code runs shell commands. Forge always directs it to a disposable Git
//! worktree and filters credentials. In containment-required configurations
//! the same invocation is wrapped in the OCI boundary; explicit development
//! mode remains an uncontained host process and is reported as such.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use chrono::Utc;
use forge_core::agent::{AdapterStatus, AgentConfig, AgentDescriptor, Capability};
use forge_core::events::EventPayload;
use forge_core::ids::AgentId;
use forge_core::run::{AgentExecution, Usage};
use forge_core::security::AgentSecurity;
use forge_core::security::ExecutionSandboxConfig;
use forge_executor::{EnvPolicy, ExecRequest, ProcessRunner, find_executable};
use serde::Deserialize;

use crate::adapter::{AgentAdapter, RunContext, contained_agent_credentials};
use crate::error::{AgentError, AgentResult};
use crate::prompt::build_agent_prompt_with_context;

/// Executable Forge looks for when none is configured.
pub const DEFAULT_EXECUTABLE: &str = "claude";
/// Credential-isolating wrapper required inside Forge's production OCI image.
pub const CONTAINED_EXECUTABLE: &str = "claude-forge";

/// Fingerprinted adapter protocol selected by the runner for Claude runs.
pub const EXECUTION_PROTOCOL_SETTING: &str = "forge.claude.execution_protocol";
/// Native development execution protocol.
pub const NATIVE_EXECUTION_PROTOCOL: &str = "claude-native-v1";
/// Production protocol: Forge OCI, bare Claude, and a one-shot key helper.
pub const OUTER_OCI_EXECUTION_PROTOCOL: &str = "forge-oci-bare-one-shot-api-key-helper-v1";

/// Permission mode used when none is configured.
///
/// The agent runs unattended, so it cannot answer a permission prompt; anything
/// stricter than this leaves it unable to run the build and test commands the
/// prompt asks it to run. A worktree does not make this mode safe against a
/// malicious process; see the security note on this module. The mode is
/// overridable with `permission_mode` under `[agents.claude]`.
pub const DEFAULT_PERMISSION_MODE: &str = "bypassPermissions";

/// Environment variables Claude Code needs that the secret filter would
/// otherwise strip.
const CREDENTIAL_VARS: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "CLAUDE_CODE_OAUTH_TOKEN",
];
/// The production wrapper deliberately supports only a direct API key. OAuth
/// and bearer-token login cannot use the wrapper's one-shot apiKeyHelper path.
const CONTAINED_CREDENTIAL_VARS: &[&str] = &["ANTHROPIC_API_KEY"];

const PERMISSION_MODES: &[&str] = &[
    "acceptEdits",
    "bypassPermissions",
    "default",
    "delegate",
    "dontAsk",
    "plan",
];

/// Flags whose meaning is part of Forge's recorded transport or containment
/// protocol and therefore cannot be overridden through opaque extra arguments.
const RESERVED_EXTRA_ARGS: &[&str] = &[
    "--add-dir",
    "--allow-dangerously-skip-permissions",
    "--bare",
    "--cwd",
    "--dangerously-skip-permissions",
    "--model",
    "--no-session-persistence",
    "--output-format",
    "--permission-mode",
    "--permission-prompt-tool",
    "--print",
    "--setting-sources",
    "--settings",
    "-p",
];

/// Variables that would tell the child it is nested inside another session.
const NESTED_SESSION_VARS: &[&str] = &[
    "CLAUDECODE",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_SSE_PORT",
];

/// Runs an engineering task through Claude Code.
#[derive(Debug, Clone)]
pub struct ClaudeAdapter {
    executable: String,
    model: Option<String>,
    permission_mode: String,
    extra_args: Vec<String>,
    containerized: bool,
    contained_credential_configured: bool,
}

impl ClaudeAdapter {
    pub fn new() -> Self {
        Self {
            executable: DEFAULT_EXECUTABLE.to_string(),
            model: None,
            permission_mode: DEFAULT_PERMISSION_MODE.to_string(),
            extra_args: Vec::new(),
            containerized: false,
            contained_credential_configured: false,
        }
    }

    /// Builds an adapter from the run's agent configuration.
    ///
    /// Reads `executable`, `permission_mode`, and `extra_args` out of the
    /// opaque settings map that `[agents.claude]` populates. Forge core never
    /// interprets these keys; this is the only place they mean anything.
    pub fn from_config(config: &AgentConfig) -> Self {
        Self {
            executable: config
                .setting("executable")
                .unwrap_or(DEFAULT_EXECUTABLE)
                .to_string(),
            model: config.model.clone(),
            permission_mode: config
                .setting("permission_mode")
                .unwrap_or(DEFAULT_PERMISSION_MODE)
                .to_string(),
            extra_args: config
                .setting("extra_args")
                .map(|raw| {
                    raw.split_whitespace()
                        .map(str::to_string)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default(),
            containerized: matches!(config.sandbox, ExecutionSandboxConfig::Required { .. }),
            contained_credential_configured: matches!(
                &config.sandbox,
                ExecutionSandboxConfig::Required { credential_env, .. }
                    if CONTAINED_CREDENTIAL_VARS
                        .iter()
                        .any(|name| credential_env.iter().any(|allowed| allowed == name))
            ),
        }
    }

    pub fn with_executable(mut self, executable: impl Into<String>) -> Self {
        self.executable = executable.into();
        self
    }

    pub fn with_permission_mode(mut self, mode: impl Into<String>) -> Self {
        self.permission_mode = mode.into();
        self
    }

    pub fn with_extra_args(mut self, args: Vec<String>) -> Self {
        self.extra_args = args;
        self
    }

    pub fn executable(&self) -> &str {
        &self.executable
    }

    /// The command line, excluding the executable itself.
    ///
    /// Separated from execution so it can be asserted on without spawning
    /// anything.
    pub fn command_args(&self, prompt: &str) -> Vec<String> {
        let mut args = vec![
            // Non-interactive: print the result and exit.
            "--print".to_string(),
            prompt.to_string(),
            // Machine-readable result, which is where usage and cost come from.
            "--output-format".to_string(),
            "json".to_string(),
            "--permission-mode".to_string(),
            self.permission_mode.clone(),
        ];
        if let Some(model) = &self.model {
            args.push("--model".to_string());
            args.push(model.clone());
        }
        args.extend(self.extra_args.iter().cloned());
        args
    }

    /// The environment Claude Code runs in.
    ///
    /// Starts from the operator's environment minus anything that looks like a
    /// secret, then allows back exactly the credentials this harness needs, and
    /// removes the markers that would tell it that it is nested inside another
    /// Claude Code session.
    fn env_policy(&self) -> EnvPolicy {
        let mut policy = EnvPolicy::inherit_non_secrets();
        let credential_vars = if self.containerized {
            CONTAINED_CREDENTIAL_VARS
        } else {
            CREDENTIAL_VARS
        };
        for var in credential_vars {
            policy = policy.allow_var(*var);
        }
        for var in NESTED_SESSION_VARS {
            policy = policy.deny_var(*var);
        }
        policy
    }

    fn validate_config(&self) -> AgentResult<()> {
        if !PERMISSION_MODES.contains(&self.permission_mode.as_str()) {
            return Err(AgentError::Unavailable {
                agent: "claude".into(),
                reason: format!(
                    "unknown permission_mode `{}`; expected one of {}",
                    self.permission_mode,
                    PERMISSION_MODES.join(", ")
                ),
            });
        }
        if let Some(argument) = self.extra_args.iter().find(|argument| {
            RESERVED_EXTRA_ARGS.iter().any(|reserved| {
                argument.as_str() == *reserved
                    || argument
                        .strip_prefix(reserved)
                        .is_some_and(|suffix| suffix.starts_with('='))
            })
        }) {
            return Err(AgentError::Unavailable {
                agent: "claude".into(),
                reason: format!(
                    "extra_args contains reserved transport/security flag `{argument}`; use the matching `[agents.claude]` setting instead"
                ),
            });
        }
        if self.containerized && self.executable != CONTAINED_EXECUTABLE {
            return Err(AgentError::Unavailable {
                agent: "claude".into(),
                reason: format!(
                    "contained production execution requires credential-isolating executable `{CONTAINED_EXECUTABLE}`"
                ),
            });
        }
        if self.containerized && !self.contained_credential_configured {
            return Err(AgentError::Unavailable {
                agent: "claude".into(),
                reason: "contained production execution requires ANTHROPIC_API_KEY in containment.credential_env; OAuth and bearer-token login are not accepted by the one-shot helper protocol".into(),
            });
        }
        Ok(())
    }

    fn invocation_metadata(&self) -> BTreeMap<String, String> {
        BTreeMap::from([
            (
                "claude.execution_boundary".into(),
                if self.containerized {
                    OUTER_OCI_EXECUTION_PROTOCOL
                } else {
                    NATIVE_EXECUTION_PROTOCOL
                }
                .into(),
            ),
            (
                "claude.customization_mode".into(),
                if self.containerized { "bare" } else { "native" }.into(),
            ),
            (
                "claude.credential_isolation".into(),
                if self.containerized {
                    "one-shot-api-key-helper-v1"
                } else {
                    "native-environment"
                }
                .into(),
            ),
        ])
    }

    /// A short description of the invocation, for the event stream.
    ///
    /// The prompt is summarized rather than included: it is already recorded in
    /// full by `PromptSubmitted` and as a run artifact.
    fn command_label(&self, prompt: &str) -> String {
        format!(
            "{} --print <prompt: {} chars> --output-format json --permission-mode {}{}",
            self.executable,
            prompt.len(),
            self.permission_mode,
            self.model
                .as_ref()
                .map(|m| format!(" --model {m}"))
                .unwrap_or_default()
        )
    }
}

impl Default for ClaudeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

/// The Claude Code catalogue entry.
pub fn descriptor() -> AgentDescriptor {
    AgentDescriptor {
        agent_id: AgentId::new("claude").expect("valid agent id"),
        display_name: "Claude Code".to_string(),
        harness: "claude-code".to_string(),
        executable: Some(DEFAULT_EXECUTABLE.to_string()),
        default_model: None,
        capabilities: vec![
            Capability::EditFiles,
            Capability::RunCommands,
            Capability::ReportsUsage,
        ],
        adapter_status: AdapterStatus::Implemented,
    }
}

#[async_trait]
impl AgentAdapter for ClaudeAdapter {
    fn descriptor(&self) -> AgentDescriptor {
        let mut descriptor = descriptor();
        descriptor.executable = Some(self.executable.clone());
        descriptor.default_model = self.model.clone();
        descriptor
    }

    fn security(&self) -> AgentSecurity {
        AgentSecurity::new(
            Some(self.permission_mode.clone()),
            self.permission_mode == "bypassPermissions",
        )
    }

    async fn prepare(&self) -> AgentResult<()> {
        self.validate_config()?;
        if self.containerized {
            return Ok(());
        }
        find_executable(&self.executable).ok_or_else(|| AgentError::ExecutableNotFound {
            agent: "claude".to_string(),
            executable: self.executable.clone(),
        })?;
        Ok(())
    }

    async fn execute(&self, ctx: &RunContext<'_>) -> AgentResult<AgentExecution> {
        self.validate_config()?;
        let started_at = Utc::now();
        let prompt = build_agent_prompt_with_context(ctx.task, ctx.workspace, ctx.world_model);

        // Recorded before the agent runs, so an interrupted run still shows
        // exactly what was asked.
        write_artifact(&ctx.artifacts_dir, "prompt.txt", &prompt);
        ctx.events.emit(EventPayload::PromptSubmitted {
            prompt: prompt.clone(),
        });

        let args = self.command_args(&prompt);
        let label = self.command_label(&prompt);
        ctx.events.emit(EventPayload::AgentStarted {
            command: label.clone(),
        });

        let mut runner = ProcessRunner::new(self.env_policy());
        if let Some(sandbox) = &ctx.sandbox {
            runner = runner.with_sandbox(sandbox.clone());
        }
        let mut request = ExecRequest::program(&self.executable, args, &ctx.workspace.path)
            .with_label(label)
            .with_default_timeout(ctx.timeout);
        request = request.with_required_credentials(contained_agent_credentials(
            &ctx.config.sandbox,
            if self.containerized {
                CONTAINED_CREDENTIAL_VARS
            } else {
                CREDENTIAL_VARS
            },
        ));
        if let Some(watch) = &ctx.disk_watch {
            request = request.with_disk_watch(watch.clone());
        }

        // The event this emits is the agent invocation itself; the commands
        // Claude runs internally are not visible to Forge.
        let outcome = runner.run(&request, ctx.events).await?;

        let stdout_path = write_artifact(&ctx.artifacts_dir, "agent.stdout.log", &outcome.stdout);
        let stderr_path = write_artifact(&ctx.artifacts_dir, "agent.stderr.log", &outcome.stderr);

        let status = if outcome.cancelled {
            forge_core::run::AgentExecutionStatus::Cancelled
        } else if outcome.infrastructure_failures.is_empty() {
            AgentExecution::classify(outcome.exit_code, outcome.timed_out)
        } else {
            forge_core::run::AgentExecutionStatus::Cancelled
        };
        let report = ClaudeResult::parse(&outcome.stdout);

        let mut metadata = self.invocation_metadata();
        if let Some(report) = &report {
            metadata.extend(report.metadata());
        } else {
            metadata.insert("claude.result_json".to_string(), "unparsed".to_string());
        }
        let execution = AgentExecution {
            status,
            exit_code: outcome.exit_code,
            timed_out: outcome.timed_out,
            started_at,
            finished_at: Utc::now(),
            duration_ms: outcome.duration_ms(),
            stdout_path: stdout_path.clone(),
            stderr_path: stderr_path.clone(),
            usage: report.as_ref().map(ClaudeResult::usage).unwrap_or_default(),
            // Recorded as trajectory data. Nothing downstream reads it.
            self_report: report.as_ref().and_then(|r| r.result.clone()),
            harness_metadata: metadata,
            infrastructure_failures: outcome.infrastructure_failures.clone(),
        };

        ctx.events.emit(EventPayload::AgentFinished {
            status: execution.status,
            exit_code: execution.exit_code,
            timed_out: execution.timed_out,
            duration_ms: execution.duration_ms,
            stdout_path,
            stderr_path,
        });

        Ok(execution)
    }
}

/// Writes a run artifact, returning where it landed.
///
/// Losing an artifact is worth a warning, never a failed run: the agent's work
/// is in the worktree either way.
fn write_artifact(dir: &Path, name: &str, contents: &str) -> Option<PathBuf> {
    if contents.is_empty() {
        return None;
    }
    if let Err(err) = fs::create_dir_all(dir) {
        tracing::warn!(dir = %dir.display(), %err, "could not create artifact directory");
        return None;
    }
    let path = dir.join(name);
    match fs::write(&path, contents) {
        Ok(()) => Some(path),
        Err(err) => {
            tracing::warn!(path = %path.display(), %err, "could not write artifact");
            None
        }
    }
}

/// The `--output-format json` result envelope.
///
/// Every field is optional: an unexpected or evolving shape must degrade to
/// less metadata, never to a failed run.
#[derive(Debug, Clone, Default, Deserialize)]
struct ClaudeResult {
    #[serde(default)]
    is_error: Option<bool>,
    #[serde(default)]
    subtype: Option<String>,
    #[serde(default)]
    result: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    total_cost_usd: Option<f64>,
    #[serde(default)]
    num_turns: Option<u64>,
    #[serde(default)]
    duration_ms: Option<u64>,
    #[serde(default)]
    terminal_reason: Option<String>,
    #[serde(default)]
    usage: Option<ClaudeUsage>,
    #[serde(default)]
    permission_denials: Option<Vec<serde_json::Value>>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct ClaudeUsage {
    #[serde(default)]
    input_tokens: Option<u64>,
    #[serde(default)]
    output_tokens: Option<u64>,
    #[serde(default)]
    cache_creation_input_tokens: Option<u64>,
    #[serde(default)]
    cache_read_input_tokens: Option<u64>,
}

impl ClaudeResult {
    /// Extracts the result envelope from captured stdout.
    ///
    /// Tolerates leading noise by falling back to scanning lines from the end,
    /// and gives up quietly rather than failing the run.
    fn parse(stdout: &str) -> Option<Self> {
        let trimmed = stdout.trim();
        if trimmed.is_empty() {
            return None;
        }
        if let Ok(parsed) = serde_json::from_str::<Self>(trimmed) {
            return Some(parsed);
        }
        trimmed
            .lines()
            .rev()
            .find_map(|line| serde_json::from_str::<Self>(line.trim()).ok())
    }

    /// Token and cost accounting.
    ///
    /// `input_tokens` is the total volume sent, including cache creation and
    /// cache reads; the breakdown is preserved in [`Self::metadata`].
    fn usage(&self) -> Usage {
        let usage = self.usage.clone().unwrap_or_default();
        let input = [
            usage.input_tokens,
            usage.cache_creation_input_tokens,
            usage.cache_read_input_tokens,
        ]
        .into_iter()
        .flatten()
        .reduce(|a, b| a + b);

        Usage {
            input_tokens: input,
            output_tokens: usage.output_tokens,
            cost_usd: self.total_cost_usd,
        }
    }

    /// Harness details worth keeping, none of which affect any verdict.
    ///
    /// `claude.is_error` in particular is the agent's own view of whether it
    /// succeeded. It is stored because it is interesting, and read by nothing.
    fn metadata(&self) -> BTreeMap<String, String> {
        let mut meta = BTreeMap::new();
        let mut put = |key: &str, value: Option<String>| {
            if let Some(value) = value {
                meta.insert(key.to_string(), value);
            }
        };
        put("claude.session_id", self.session_id.clone());
        put("claude.subtype", self.subtype.clone());
        put("claude.is_error", self.is_error.map(|v| v.to_string()));
        put("claude.num_turns", self.num_turns.map(|v| v.to_string()));
        put(
            "claude.duration_ms",
            self.duration_ms.map(|v| v.to_string()),
        );
        put("claude.terminal_reason", self.terminal_reason.clone());
        put(
            "claude.permission_denials",
            self.permission_denials
                .as_ref()
                .map(|d| d.len().to_string()),
        );
        if let Some(usage) = &self.usage {
            put(
                "claude.cache_read_input_tokens",
                usage.cache_read_input_tokens.map(|v| v.to_string()),
            );
            put(
                "claude.cache_creation_input_tokens",
                usage.cache_creation_input_tokens.map(|v| v.to_string()),
            );
            put(
                "claude.uncached_input_tokens",
                usage.input_tokens.map(|v| v.to_string()),
            );
        }
        meta
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_core::agent::AgentConfig;
    use forge_core::run::AgentExecutionStatus;
    use forge_core::security::NetworkPolicy;

    fn adapter() -> ClaudeAdapter {
        ClaudeAdapter::new()
    }

    fn required_containment() -> ExecutionSandboxConfig {
        ExecutionSandboxConfig::Required {
            runtime: "docker".into(),
            image: format!("forge@sha256:{}", "a".repeat(64)),
            network: NetworkPolicy::Allowed,
            restricted_network: None,
            cpu_millis: 2_000,
            memory_bytes: 1024 * 1024 * 1024,
            pids_limit: 128,
            workspace_limit_bytes: 4 * 1024 * 1024 * 1024,
            credential_env: vec!["ANTHROPIC_API_KEY".into()],
        }
    }

    #[test]
    fn the_command_runs_claude_non_interactively_with_machine_readable_output() {
        let args = adapter().command_args("do the thing");
        assert_eq!(
            args,
            vec![
                "--print",
                "do the thing",
                "--output-format",
                "json",
                "--permission-mode",
                "bypassPermissions",
            ]
        );
    }

    #[test]
    fn a_configured_model_reaches_the_command_line() {
        let mut config = AgentConfig::new(AgentId::new("claude").unwrap(), "claude-code");
        config.model = Some("opus".to_string());
        let args = ClaudeAdapter::from_config(&config).command_args("p");
        assert!(args.windows(2).any(|w| w == ["--model", "opus"]));
    }

    #[test]
    fn no_model_means_no_model_flag() {
        let args = adapter().command_args("p");
        assert!(!args.iter().any(|a| a == "--model"));
    }

    #[test]
    fn settings_override_the_executable_and_permission_mode() {
        let mut config = AgentConfig::new(AgentId::new("claude").unwrap(), "claude-code");
        config
            .settings
            .insert("executable".into(), "/opt/claude".into());
        config
            .settings
            .insert("permission_mode".into(), "acceptEdits".into());
        config
            .settings
            .insert("extra_args".into(), "--verbose --debug".into());

        let adapter = ClaudeAdapter::from_config(&config);
        assert_eq!(adapter.executable(), "/opt/claude");

        let args = adapter.command_args("p");
        assert!(
            args.windows(2)
                .any(|w| w == ["--permission-mode", "acceptEdits"])
        );
        assert!(args.contains(&"--verbose".to_string()));
        assert!(args.contains(&"--debug".to_string()));
    }

    #[test]
    fn contained_execution_requires_the_bare_one_shot_wrapper_and_api_key() {
        let mut config = AgentConfig::new(AgentId::new("claude").unwrap(), "claude-code");
        config.sandbox = required_containment();

        let direct = ClaudeAdapter::from_config(&config);
        let error = direct.validate_config().unwrap_err().to_string();
        assert!(error.contains(CONTAINED_EXECUTABLE), "{error}");

        config
            .settings
            .insert("executable".into(), CONTAINED_EXECUTABLE.into());
        let wrapped = ClaudeAdapter::from_config(&config);
        assert!(wrapped.validate_config().is_ok());
        let metadata = wrapped.invocation_metadata();
        assert_eq!(
            metadata
                .get("claude.credential_isolation")
                .map(String::as_str),
            Some("one-shot-api-key-helper-v1")
        );

        if let ExecutionSandboxConfig::Required { credential_env, .. } = &mut config.sandbox {
            *credential_env = vec!["CLAUDE_CODE_OAUTH_TOKEN".into()];
        }
        let error = ClaudeAdapter::from_config(&config)
            .validate_config()
            .unwrap_err()
            .to_string();
        assert!(error.contains("ANTHROPIC_API_KEY"), "{error}");
    }

    #[test]
    fn reserved_or_unknown_security_configuration_is_rejected() {
        let error = adapter()
            .with_extra_args(vec!["--settings=/repo/untrusted.json".into()])
            .validate_config()
            .unwrap_err()
            .to_string();
        assert!(error.contains("reserved"), "{error}");

        let error = adapter()
            .with_permission_mode("invented")
            .validate_config()
            .unwrap_err()
            .to_string();
        assert!(error.contains("unknown permission_mode"), "{error}");
    }

    #[test]
    fn the_prompt_is_passed_as_an_argument_not_interpolated_into_a_shell() {
        // Command construction must not be shell-quoted: a prompt containing
        // backticks or `$(...)` is data, never something to evaluate.
        let hostile = "objective: `rm -rf /` and $(whoami)";
        let args = adapter().command_args(hostile);
        assert!(args.contains(&hostile.to_string()));
        assert!(!args.iter().any(|a| a.contains("sh -c")));
    }

    #[test]
    fn the_command_label_summarizes_the_prompt_rather_than_repeating_it() {
        let label = adapter().command_label(&"x".repeat(5000));
        assert!(label.contains("<prompt: 5000 chars>"));
        assert!(!label.contains("xxxx"));
    }

    #[test]
    fn credentials_are_allowed_through_but_other_secrets_are_not() {
        let env = vec![
            ("ANTHROPIC_API_KEY", "sk-ant-secret-value"),
            ("GITHUB_TOKEN", "ghp_should_not_pass"),
            ("PATH", "/usr/bin"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect::<Vec<_>>();

        let built = adapter().env_policy().build_from(env);
        assert!(built.contains_key("ANTHROPIC_API_KEY"));
        assert!(built.contains_key("PATH"));
        assert!(!built.contains_key("GITHUB_TOKEN"));
    }

    #[test]
    fn contained_environment_excludes_oauth_and_bearer_credentials() {
        let mut config = AgentConfig::new(AgentId::new("claude").unwrap(), "claude-code");
        config.sandbox = required_containment();
        config
            .settings
            .insert("executable".into(), CONTAINED_EXECUTABLE.into());
        let contained = ClaudeAdapter::from_config(&config);
        let env = [
            ("ANTHROPIC_API_KEY", "api-key"),
            ("ANTHROPIC_AUTH_TOKEN", "bearer"),
            ("CLAUDE_CODE_OAUTH_TOKEN", "oauth"),
        ]
        .into_iter()
        .map(|(key, value)| (key.to_string(), value.to_string()));
        let built = contained.env_policy().build_from(env);

        assert!(built.contains_key("ANTHROPIC_API_KEY"));
        assert!(!built.contains_key("ANTHROPIC_AUTH_TOKEN"));
        assert!(!built.contains_key("CLAUDE_CODE_OAUTH_TOKEN"));
    }

    #[test]
    fn the_child_is_not_told_it_is_nested_inside_another_session() {
        let env = vec![
            ("CLAUDECODE", "1"),
            ("CLAUDE_CODE_ENTRYPOINT", "cli"),
            ("EDITOR", "vim"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect::<Vec<_>>();

        let built = adapter().env_policy().build_from(env);
        assert!(!built.contains_key("CLAUDECODE"));
        assert!(!built.contains_key("CLAUDE_CODE_ENTRYPOINT"));
        assert!(built.contains_key("EDITOR"));
    }

    /// Captured from a real `claude -p --output-format json` invocation.
    const REAL_RESULT: &str = r#"{"is_error":false,"duration_api_ms":3012,"num_turns":1,"stop_reason":"end_turn","session_id":"4f5e8ab3-30d0-4593-9869-bc7bdb8c1973","total_cost_usd":0.0633946,"usage":{"input_tokens":2,"cache_creation_input_tokens":9458,"cache_read_input_tokens":19982,"output_tokens":4,"service_tier":"standard"},"permission_denials":[],"terminal_reason":"completed","subtype":"success","result":"OK","type":"result","duration_ms":2804}"#;

    #[test]
    fn the_real_result_envelope_parses() {
        let parsed = ClaudeResult::parse(REAL_RESULT).expect("parsed");
        assert_eq!(parsed.result.as_deref(), Some("OK"));

        let usage = parsed.usage();
        // Input is the full volume sent, cache included.
        assert_eq!(usage.input_tokens, Some(2 + 9458 + 19982));
        assert_eq!(usage.output_tokens, Some(4));
        assert_eq!(usage.cost_usd, Some(0.0633946));

        let meta = parsed.metadata();
        assert_eq!(
            meta.get("claude.session_id").map(String::as_str),
            Some("4f5e8ab3-30d0-4593-9869-bc7bdb8c1973")
        );
        assert_eq!(
            meta.get("claude.permission_denials").map(String::as_str),
            Some("0")
        );
        assert_eq!(
            meta.get("claude.uncached_input_tokens").map(String::as_str),
            Some("2")
        );
    }

    #[test]
    fn the_envelope_is_found_even_behind_leading_output() {
        let noisy = format!("warning: something\nanother line\n{REAL_RESULT}");
        let parsed = ClaudeResult::parse(&noisy).expect("parsed");
        assert_eq!(parsed.result.as_deref(), Some("OK"));
    }

    #[test]
    fn unparseable_output_degrades_to_less_metadata_not_an_error() {
        assert!(ClaudeResult::parse("").is_none());
        assert!(ClaudeResult::parse("not json at all").is_none());
        // A JSON document of an unexpected shape still parses, just emptily.
        let sparse = ClaudeResult::parse("{}").expect("empty object parses");
        assert!(sparse.usage().is_empty());
        assert!(sparse.result.is_none());
    }

    /// The trust boundary, at the adapter level.
    #[test]
    fn claudes_own_error_flag_is_recorded_but_never_becomes_a_status() {
        let claimed_failure =
            r#"{"is_error":true,"subtype":"error_max_turns","result":"I could not finish"}"#;
        let parsed = ClaudeResult::parse(claimed_failure).unwrap();

        // It is kept as data...
        assert_eq!(
            parsed.metadata().get("claude.is_error").map(String::as_str),
            Some("true")
        );
        assert_eq!(parsed.result.as_deref(), Some("I could not finish"));

        // ...and the execution status still comes from the process alone.
        assert_eq!(
            AgentExecution::classify(Some(0), false),
            AgentExecutionStatus::Completed
        );
    }

    #[test]
    fn the_descriptor_reflects_the_configured_executable() {
        let adapter = adapter().with_executable("/custom/claude");
        let descriptor = AgentAdapter::descriptor(&adapter);
        assert_eq!(descriptor.executable.as_deref(), Some("/custom/claude"));
        assert_eq!(descriptor.adapter_status, AdapterStatus::Implemented);
    }

    #[test]
    fn permission_mode_is_reported_as_security_posture_not_hidden_in_cli_args() {
        let unrestricted = AgentAdapter::security(&adapter());
        assert_eq!(
            unrestricted.permission_mode.as_deref(),
            Some("bypassPermissions")
        );
        assert!(unrestricted.unrestricted);

        let restricted = AgentAdapter::security(&adapter().with_permission_mode("acceptEdits"));
        assert_eq!(restricted.permission_mode.as_deref(), Some("acceptEdits"));
        assert!(!restricted.unrestricted);
    }

    #[tokio::test]
    async fn prepare_fails_when_the_executable_is_missing() {
        let adapter = adapter().with_executable("forge-claude-not-installed");
        let err = adapter.prepare().await.unwrap_err();
        assert!(
            matches!(err, AgentError::ExecutableNotFound { .. }),
            "{err}"
        );
        assert!(err.to_string().contains("PATH"));
    }

    #[tokio::test]
    async fn prepare_succeeds_when_the_executable_exists() {
        // `sh` stands in for the CLI so the test does not depend on Claude Code
        // being installed.
        assert!(adapter().with_executable("sh").prepare().await.is_ok());
    }
}
