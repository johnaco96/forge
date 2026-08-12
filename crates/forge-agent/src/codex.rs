//! The OpenAI Codex CLI adapter.
//!
//! Codex-specific transport, configuration, JSONL parsing, and environment
//! handling live here. The adapter receives the same prompt and workspace as
//! every other agent, and reports only process facts plus harness metadata.
//! Forge evaluates the resulting workspace independently.
//!
//! # Security
//!
//! Forge starts Codex in a dedicated Git worktree and explicitly selects the
//! CLI's `workspace-write` sandbox by default. This constrains model-generated
//! commands, but it is not Forge-managed host containment: the Codex CLI still
//! runs as the invoking user. The configured Codex sandbox and approval modes
//! are therefore reported separately from Forge's `host containment: none`.

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
use forge_executor::{EnvPolicy, ExecRequest, ProcessRunner, find_executable};
use serde::Deserialize;

use crate::adapter::{AgentAdapter, RunContext};
use crate::error::{AgentError, AgentResult};
use crate::prompt::build_agent_prompt_with_context;

/// Executable Forge looks for when none is configured.
pub const DEFAULT_EXECUTABLE: &str = "codex";
/// Codex sandbox used for unattended coding runs.
pub const DEFAULT_SANDBOX_MODE: &str = "workspace-write";
/// Approval policy used for unattended coding runs.
///
/// `never` does not remove the sandbox. It prevents a non-interactive run from
/// pausing for an approval Forge cannot answer; denied actions are returned to
/// the model as failures.
pub const DEFAULT_APPROVAL_POLICY: &str = "never";

const SANDBOX_MODES: &[&str] = &["read-only", "workspace-write", "danger-full-access"];
const APPROVAL_POLICIES: &[&str] = &["untrusted", "on-request", "never"];

/// Environment variables supported by Codex authentication in automation.
const CREDENTIAL_VARS: &[&str] = &["CODEX_API_KEY"];

/// Parent-session markers must not attach a child CLI run to Forge's caller.
const NESTED_SESSION_VARS: &[&str] = &[
    "CODEX_CI",
    "CODEX_INTERNAL_ORIGINATOR_OVERRIDE",
    "CODEX_PERMISSION_PROFILE",
    "CODEX_SHELL",
    "CODEX_THREAD_ID",
];

/// These flags would make the recorded workspace, transport, or security
/// posture disagree with the process Forge actually launched. They have typed
/// adapter settings instead and may not be smuggled through `extra_args`.
const RESERVED_EXTRA_ARGS: &[&str] = &[
    "--ask-for-approval",
    "-a",
    "--cd",
    "-C",
    "--dangerously-bypass-approvals-and-sandbox",
    "--experimental-json",
    "--json",
    "--model",
    "-m",
    "--sandbox",
    "-s",
    "--yolo",
];

/// Runs an engineering task through `codex exec`.
#[derive(Debug, Clone)]
pub struct CodexAdapter {
    executable: String,
    model: Option<String>,
    sandbox_mode: String,
    approval_policy: String,
    extra_args: Vec<String>,
}

impl CodexAdapter {
    pub fn new() -> Self {
        Self {
            executable: DEFAULT_EXECUTABLE.to_string(),
            model: None,
            sandbox_mode: DEFAULT_SANDBOX_MODE.to_string(),
            approval_policy: DEFAULT_APPROVAL_POLICY.to_string(),
            extra_args: Vec::new(),
        }
    }

    /// Builds an adapter from the opaque `[agents.codex]` settings.
    pub fn from_config(config: &AgentConfig) -> Self {
        Self {
            executable: config
                .setting("executable")
                .unwrap_or(DEFAULT_EXECUTABLE)
                .to_string(),
            model: config.model.clone(),
            sandbox_mode: config
                .setting("sandbox_mode")
                .unwrap_or(DEFAULT_SANDBOX_MODE)
                .to_string(),
            approval_policy: config
                .setting("approval_policy")
                .unwrap_or(DEFAULT_APPROVAL_POLICY)
                .to_string(),
            extra_args: config
                .setting("extra_args")
                .map(|raw| raw.split_whitespace().map(str::to_string).collect())
                .unwrap_or_default(),
        }
    }

    pub fn with_executable(mut self, executable: impl Into<String>) -> Self {
        self.executable = executable.into();
        self
    }

    pub fn with_sandbox_mode(mut self, mode: impl Into<String>) -> Self {
        self.sandbox_mode = mode.into();
        self
    }

    pub fn with_approval_policy(mut self, policy: impl Into<String>) -> Self {
        self.approval_policy = policy.into();
        self
    }

    pub fn with_extra_args(mut self, args: Vec<String>) -> Self {
        self.extra_args = args;
        self
    }

    pub fn executable(&self) -> &str {
        &self.executable
    }

    /// The direct-process argument vector, excluding the executable.
    ///
    /// The shared prompt is one positional argument. No shell sees it, so task
    /// text containing substitutions or quotes remains data.
    pub fn command_args(&self, prompt: &str, workspace: &Path) -> Vec<String> {
        // CLI 0.147 advertises approval as an `exec` option but only accepts it
        // before the subcommand. Keep all inherited global flags there.
        let mut args = vec![
            "--sandbox".to_string(),
            self.sandbox_mode.clone(),
            "--ask-for-approval".to_string(),
            self.approval_policy.clone(),
            "--cd".to_string(),
            workspace.to_string_lossy().into_owned(),
        ];
        if let Some(model) = &self.model {
            args.push("--model".to_string());
            args.push(model.clone());
        }
        args.push("exec".to_string());
        args.extend(self.extra_args.iter().cloned());
        args.push("--json".to_string());
        args.push(prompt.to_string());
        args
    }

    fn env_policy(&self) -> EnvPolicy {
        let mut policy = EnvPolicy::inherit_non_secrets();
        for var in CREDENTIAL_VARS {
            policy = policy.allow_var(*var);
        }
        for var in NESTED_SESSION_VARS {
            policy = policy.deny_var(*var);
        }
        policy
    }

    fn command_label(&self, prompt: &str, workspace: &Path) -> String {
        format!(
            "{} --sandbox {} --ask-for-approval {} --cd {}{} exec --json <prompt: {} chars>",
            self.executable,
            self.sandbox_mode,
            self.approval_policy,
            workspace.display(),
            self.model
                .as_ref()
                .map(|model| format!(" --model {model}"))
                .unwrap_or_default(),
            prompt.len(),
        )
    }

    fn validate_config(&self) -> AgentResult<()> {
        if !SANDBOX_MODES.contains(&self.sandbox_mode.as_str()) {
            return Err(AgentError::Unavailable {
                agent: "codex".to_string(),
                reason: format!(
                    "unknown sandbox_mode `{}`; expected one of {}",
                    self.sandbox_mode,
                    SANDBOX_MODES.join(", ")
                ),
            });
        }
        if !APPROVAL_POLICIES.contains(&self.approval_policy.as_str()) {
            return Err(AgentError::Unavailable {
                agent: "codex".to_string(),
                reason: format!(
                    "unknown approval_policy `{}`; expected one of {}",
                    self.approval_policy,
                    APPROVAL_POLICIES.join(", ")
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
                agent: "codex".to_string(),
                reason: format!(
                    "extra_args contains reserved transport/security flag `{argument}`; use the \
                     matching `[agents.codex]` setting instead"
                ),
            });
        }
        Ok(())
    }

    fn invocation_metadata(&self) -> BTreeMap<String, String> {
        let mut metadata = BTreeMap::from([
            (
                "codex.approval_policy".to_string(),
                self.approval_policy.clone(),
            ),
            ("codex.sandbox_mode".to_string(), self.sandbox_mode.clone()),
        ]);
        if let Some(model) = &self.model {
            metadata.insert("codex.requested_model".to_string(), model.clone());
        }
        metadata
    }
}

impl Default for CodexAdapter {
    fn default() -> Self {
        Self::new()
    }
}

/// The Codex CLI catalogue entry.
pub fn descriptor() -> AgentDescriptor {
    AgentDescriptor {
        agent_id: AgentId::new("codex").expect("valid agent id"),
        display_name: "OpenAI Codex".to_string(),
        harness: "codex-cli".to_string(),
        executable: Some(DEFAULT_EXECUTABLE.to_string()),
        default_model: None,
        capabilities: vec![
            Capability::EditFiles,
            Capability::RunCommands,
            Capability::ReportsUsage,
            Capability::StructuredTrajectory,
        ],
        adapter_status: AdapterStatus::Implemented,
    }
}

#[async_trait]
impl AgentAdapter for CodexAdapter {
    fn descriptor(&self) -> AgentDescriptor {
        let mut descriptor = descriptor();
        descriptor.executable = Some(self.executable.clone());
        descriptor.default_model = self.model.clone();
        descriptor
    }

    fn security(&self) -> AgentSecurity {
        AgentSecurity::new(
            Some(format!(
                "sandbox={}, approval={}",
                self.sandbox_mode, self.approval_policy
            )),
            self.sandbox_mode == "danger-full-access",
        )
    }

    async fn prepare(&self) -> AgentResult<()> {
        self.validate_config()?;
        find_executable(&self.executable).ok_or_else(|| AgentError::ExecutableNotFound {
            agent: "codex".to_string(),
            executable: self.executable.clone(),
        })?;
        Ok(())
    }

    async fn execute(&self, ctx: &RunContext<'_>) -> AgentResult<AgentExecution> {
        let started_at = Utc::now();
        let prompt = build_agent_prompt_with_context(ctx.task, ctx.workspace, ctx.world_model);

        write_artifact(&ctx.artifacts_dir, "prompt.txt", &prompt);
        ctx.events.emit(EventPayload::PromptSubmitted {
            prompt: prompt.clone(),
        });

        let args = self.command_args(&prompt, &ctx.workspace.path);
        let label = self.command_label(&prompt, &ctx.workspace.path);
        ctx.events.emit(EventPayload::AgentStarted {
            command: label.clone(),
        });

        let request = ExecRequest::program(&self.executable, args, &ctx.workspace.path)
            .with_label(label)
            .with_default_timeout(ctx.timeout);
        let outcome = ProcessRunner::new(self.env_policy())
            .run(&request, ctx.events)
            .await?;

        let stdout_path = write_artifact(&ctx.artifacts_dir, "agent.stdout.log", &outcome.stdout);
        let stderr_path = write_artifact(&ctx.artifacts_dir, "agent.stderr.log", &outcome.stderr);
        let stream = CodexEventStream::parse(&outcome.stdout);
        let mut metadata = self.invocation_metadata();
        if let Some(stream) = &stream {
            metadata.extend(stream.metadata());
        } else {
            metadata.insert("codex.jsonl".to_string(), "unparsed".to_string());
        }

        let execution = AgentExecution {
            status: AgentExecution::classify(outcome.exit_code, outcome.timed_out),
            exit_code: outcome.exit_code,
            timed_out: outcome.timed_out,
            started_at,
            finished_at: Utc::now(),
            duration_ms: outcome.duration_ms(),
            stdout_path: stdout_path.clone(),
            stderr_path: stderr_path.clone(),
            usage: stream
                .as_ref()
                .map(CodexEventStream::usage)
                .unwrap_or_default(),
            self_report: stream.and_then(|stream| stream.final_message),
            harness_metadata: metadata,
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

/// One documented `codex exec --json` event.
///
/// The envelope remains deliberately loose so new event and item kinds degrade
/// to less metadata instead of breaking a run.
#[derive(Debug, Deserialize)]
struct CodexEvent {
    #[serde(rename = "type")]
    event_type: String,
    #[serde(default)]
    thread_id: Option<String>,
    #[serde(default)]
    usage: Option<CodexUsage>,
    #[serde(default)]
    item: Option<CodexItem>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct CodexUsage {
    #[serde(default)]
    input_tokens: Option<u64>,
    #[serde(default)]
    cached_input_tokens: Option<u64>,
    #[serde(default)]
    cache_write_input_tokens: Option<u64>,
    #[serde(default)]
    output_tokens: Option<u64>,
    #[serde(default)]
    reasoning_output_tokens: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct CodexItem {
    #[serde(rename = "type")]
    item_type: String,
    #[serde(default)]
    text: Option<String>,
}

#[derive(Debug, Default)]
struct CodexEventStream {
    event_count: u64,
    invalid_lines: u64,
    thread_id: Option<String>,
    usage: CodexUsage,
    final_message: Option<String>,
    terminal_event: Option<String>,
    completed_item_counts: BTreeMap<String, u64>,
}

impl CodexEventStream {
    fn parse(stdout: &str) -> Option<Self> {
        let mut stream = Self::default();
        for line in stdout.lines().filter(|line| !line.trim().is_empty()) {
            let Ok(event) = serde_json::from_str::<CodexEvent>(line) else {
                stream.invalid_lines += 1;
                continue;
            };
            stream.event_count += 1;
            match event.event_type.as_str() {
                "thread.started" => stream.thread_id = event.thread_id,
                "turn.completed" => {
                    stream.terminal_event = Some(event.event_type);
                    if let Some(usage) = event.usage {
                        stream.usage = usage;
                    }
                }
                "turn.failed" | "error" => {
                    stream.terminal_event = Some(event.event_type);
                }
                "item.completed" => {
                    if let Some(item) = event.item {
                        if item.item_type == "agent_message" {
                            if let Some(text) = item.text {
                                stream.final_message = Some(text);
                            }
                        } else if matches!(
                            item.item_type.as_str(),
                            "command_execution"
                                | "file_change"
                                | "mcp_tool_call"
                                | "plan_update"
                                | "web_search"
                        ) {
                            *stream
                                .completed_item_counts
                                .entry(item.item_type)
                                .or_default() += 1;
                        }
                    }
                }
                _ => {}
            }
        }

        (stream.event_count > 0).then_some(stream)
    }

    fn usage(&self) -> Usage {
        Usage {
            input_tokens: self.usage.input_tokens,
            output_tokens: self.usage.output_tokens,
            // Codex CLI 0.147 JSONL does not expose cost.
            cost_usd: None,
        }
    }

    fn metadata(&self) -> BTreeMap<String, String> {
        let mut metadata = BTreeMap::from([
            (
                "codex.event_count".to_string(),
                self.event_count.to_string(),
            ),
            ("codex.jsonl".to_string(), "parsed".to_string()),
        ]);
        let mut put = |key: &str, value: Option<String>| {
            if let Some(value) = value {
                metadata.insert(key.to_string(), value);
            }
        };
        put("codex.thread_id", self.thread_id.clone());
        put("codex.terminal_event", self.terminal_event.clone());
        put(
            "codex.cached_input_tokens",
            self.usage
                .cached_input_tokens
                .map(|value| value.to_string()),
        );
        put(
            "codex.cache_write_input_tokens",
            self.usage
                .cache_write_input_tokens
                .map(|value| value.to_string()),
        );
        put(
            "codex.reasoning_output_tokens",
            self.usage
                .reasoning_output_tokens
                .map(|value| value.to_string()),
        );
        if self.invalid_lines > 0 {
            metadata.insert(
                "codex.invalid_json_lines".to_string(),
                self.invalid_lines.to_string(),
            );
        }
        for (item_type, count) in &self.completed_item_counts {
            metadata.insert(format!("codex.{item_type}_count"), count.to_string());
        }
        metadata
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_core::agent::AgentConfig;
    use forge_core::run::AgentExecutionStatus;

    fn adapter() -> CodexAdapter {
        CodexAdapter::new()
    }

    #[test]
    fn command_uses_non_interactive_jsonl_with_explicit_workspace_security() {
        let args = adapter().command_args("do the thing", Path::new("/tmp/worktree"));
        assert_eq!(
            args,
            vec![
                "--sandbox",
                "workspace-write",
                "--ask-for-approval",
                "never",
                "--cd",
                "/tmp/worktree",
                "exec",
                "--json",
                "do the thing",
            ]
        );
    }

    #[test]
    fn settings_override_executable_model_and_permission_modes() {
        let mut config = AgentConfig::new(AgentId::new("codex").unwrap(), "codex-cli");
        config.model = Some("gpt-5-codex".to_string());
        config.settings.extend([
            ("executable".to_string(), "/opt/codex".to_string()),
            ("sandbox_mode".to_string(), "danger-full-access".to_string()),
            ("approval_policy".to_string(), "on-request".to_string()),
            (
                "extra_args".to_string(),
                "--ephemeral --color never".to_string(),
            ),
        ]);

        let adapter = CodexAdapter::from_config(&config);
        assert_eq!(adapter.executable(), "/opt/codex");
        let args = adapter.command_args("p", Path::new("/work"));
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--model", "gpt-5-codex"])
        );
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--sandbox", "danger-full-access"])
        );
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--ask-for-approval", "on-request"])
        );
        assert!(args.contains(&"--ephemeral".to_string()));
    }

    #[test]
    fn prompt_is_an_argument_and_is_never_interpolated_into_a_shell() {
        let hostile = "objective: `rm -rf /` and $(whoami)";
        let args = adapter().command_args(hostile, Path::new("/work"));
        assert_eq!(args.last().map(String::as_str), Some(hostile));
        assert!(!args.iter().any(|argument| argument.contains("sh -c")));
    }

    #[test]
    fn invalid_or_conflicting_configuration_fails_before_execution() {
        let bad_mode = adapter().with_sandbox_mode("wishful-thinking");
        assert!(bad_mode.validate_config().is_err());

        let reserved = adapter().with_extra_args(vec!["--sandbox=danger-full-access".into()]);
        let error = reserved.validate_config().unwrap_err().to_string();
        assert!(error.contains("reserved"), "{error}");
    }

    #[test]
    fn environment_allows_only_codex_credentials_and_removes_parent_markers() {
        let env = [
            ("CODEX_API_KEY", "codex-secret"),
            ("GITHUB_TOKEN", "github-secret"),
            ("CODEX_THREAD_ID", "parent-thread"),
            ("PATH", "/usr/bin"),
        ]
        .into_iter()
        .map(|(key, value)| (key.to_string(), value.to_string()));

        let built = adapter().env_policy().build_from(env);
        assert!(built.contains_key("CODEX_API_KEY"));
        assert!(built.contains_key("PATH"));
        assert!(!built.contains_key("GITHUB_TOKEN"));
        assert!(!built.contains_key("CODEX_THREAD_ID"));
    }

    /// Shape documented by `codex exec --json` and observed in CLI 0.147.
    const REALISTIC_STREAM: &str = r#"{"type":"thread.started","thread_id":"0199a213-81c0-7800-8aa1-bbab2a035a53"}
{"type":"turn.started"}
{"type":"item.completed","item":{"id":"item_1","type":"command_execution","command":"cargo test","status":"completed"}}
{"type":"item.completed","item":{"id":"item_2","type":"agent_message","text":"Implemented the fix."}}
{"type":"turn.completed","usage":{"input_tokens":24763,"cached_input_tokens":24448,"cache_write_input_tokens":9,"output_tokens":122,"reasoning_output_tokens":17}}"#;

    #[test]
    fn jsonl_exposes_only_documented_usage_and_metadata() {
        let stream = CodexEventStream::parse(REALISTIC_STREAM).expect("parsed stream");
        assert_eq!(
            stream.final_message.as_deref(),
            Some("Implemented the fix.")
        );
        assert_eq!(stream.usage().input_tokens, Some(24_763));
        assert_eq!(stream.usage().output_tokens, Some(122));
        assert_eq!(stream.usage().cost_usd, None);

        let metadata = stream.metadata();
        assert_eq!(
            metadata.get("codex.thread_id").map(String::as_str),
            Some("0199a213-81c0-7800-8aa1-bbab2a035a53")
        );
        assert_eq!(
            metadata
                .get("codex.cached_input_tokens")
                .map(String::as_str),
            Some("24448")
        );
        assert_eq!(
            metadata
                .get("codex.command_execution_count")
                .map(String::as_str),
            Some("1")
        );
        assert_eq!(
            metadata
                .get("codex.cache_write_input_tokens")
                .map(String::as_str),
            Some("9")
        );
    }

    #[test]
    fn malformed_lines_degrade_to_partial_metadata() {
        let noisy = format!("not json\n{REALISTIC_STREAM}\ntruncated {{");
        let stream = CodexEventStream::parse(&noisy).expect("valid events remain");
        assert_eq!(
            stream
                .metadata()
                .get("codex.invalid_json_lines")
                .map(String::as_str),
            Some("2")
        );
        assert!(CodexEventStream::parse("not json").is_none());
    }

    #[test]
    fn codex_self_report_never_determines_execution_status() {
        let stream = CodexEventStream::parse(
            r#"{"type":"item.completed","item":{"type":"agent_message","text":"I failed"}}
{"type":"turn.completed","usage":{}}"#,
        )
        .unwrap();
        assert_eq!(stream.final_message.as_deref(), Some("I failed"));
        assert_eq!(
            AgentExecution::classify(Some(0), false),
            AgentExecutionStatus::Completed
        );
    }

    #[test]
    fn security_reports_codex_modes_without_claiming_forge_containment() {
        let security = AgentAdapter::security(&adapter());
        assert_eq!(
            security.permission_mode.as_deref(),
            Some("sandbox=workspace-write, approval=never")
        );
        assert!(!security.unrestricted);

        let full_access = adapter().with_sandbox_mode("danger-full-access");
        assert!(AgentAdapter::security(&full_access).unrestricted);
    }

    #[tokio::test]
    async fn prepare_fails_when_executable_is_missing() {
        let error = adapter()
            .with_executable("forge-codex-not-installed")
            .prepare()
            .await
            .unwrap_err();
        assert!(matches!(error, AgentError::ExecutableNotFound { .. }));
        assert!(error.to_string().contains("PATH"));
    }

    #[tokio::test]
    async fn prepare_accepts_an_existing_executable() {
        assert!(adapter().with_executable("sh").prepare().await.is_ok());
    }
}
