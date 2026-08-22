//! The interface every coding agent is reached through.
//!
//! Forge must never depend on a specific agent implementation. Claude Code,
//! Codex, Pi, and anything local are all just implementations of
//! [`AgentAdapter`] — interchangeable engineering workers.
//!
//! Note what an adapter is *not* allowed to decide: whether the work was any
//! good. An adapter reports what the agent did and what it claimed; the verdict
//! comes from `forge-eval`, on the other side of the trust boundary.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use forge_core::agent::{AgentConfig, AgentDescriptor};
use forge_core::events::EventSink;
use forge_core::ids::RunId;
use forge_core::run::AgentExecution;
use forge_core::security::{AgentSecurity, ExecutionSandboxConfig};
use forge_core::task::EngineeringTask;
use forge_core::workspace::Workspace;
use forge_core::world::WorldModelContext;
use forge_executor::{DiskWatch, ExecutionSandbox};

use crate::error::AgentResult;

/// Provider credential names explicitly approved for this contained agent
/// invocation and understood by its adapter. Evaluators never call this path.
pub(crate) fn contained_agent_credentials(
    config: &ExecutionSandboxConfig,
    supported: &[&str],
) -> Vec<String> {
    contained_agent_credentials_with(config, supported, |name| {
        std::env::var(name).is_ok_and(|value| !value.is_empty())
    })
}

fn contained_agent_credentials_with(
    config: &ExecutionSandboxConfig,
    supported: &[&str],
    is_present: impl Fn(&str) -> bool,
) -> Vec<String> {
    let ExecutionSandboxConfig::Required { credential_env, .. } = config else {
        return Vec::new();
    };

    // Provider credential names are alternatives, not cumulative secrets. A
    // profile may allow both an API key and an OAuth token so different
    // operators can use the same immutable profile. Pass only the provider's
    // first configured, present choice. If none is present, return the first
    // configured choice so ProcessRunner emits a typed CredentialUnavailable
    // failure before a subprocess starts.
    let configured = supported
        .iter()
        .filter(|name| credential_env.iter().any(|allowed| allowed == **name))
        .copied()
        .collect::<Vec<_>>();
    configured
        .iter()
        .find(|name| is_present(name))
        .or_else(|| configured.first())
        .map(|name| vec![(*name).to_string()])
        .unwrap_or_default()
}

/// Everything an adapter needs to execute one run.
pub struct RunContext<'a> {
    pub run_id: &'a RunId,
    pub task: &'a EngineeringTask,
    /// The isolated checkout the agent should modify. An adapter sets this as
    /// the working directory; without host containment it cannot enforce that
    /// the process stays inside it.
    pub workspace: &'a Workspace,
    /// Compact deterministic facts from an exact snapshot of the workspace
    /// base commit. Absence is a supported fallback.
    pub world_model: Option<&'a WorldModelContext>,
    pub config: &'a AgentConfig,
    /// Where the adapter records what happened, as it happens.
    pub events: &'a dyn EventSink,
    /// Wall-clock budget for the whole agent invocation.
    pub timeout: Option<Duration>,
    /// Directory for captured output and harness-native trajectory files.
    pub artifacts_dir: PathBuf,
    pub disk_watch: Option<DiskWatch>,
    pub sandbox: Option<Arc<dyn ExecutionSandbox>>,
}

impl<'a> RunContext<'a> {
    pub fn new(
        run_id: &'a RunId,
        task: &'a EngineeringTask,
        workspace: &'a Workspace,
        config: &'a AgentConfig,
        events: &'a dyn EventSink,
        artifacts_dir: PathBuf,
    ) -> Self {
        Self {
            run_id,
            task,
            workspace,
            world_model: None,
            config,
            events,
            timeout: config.timeout_secs.map(Duration::from_secs),
            artifacts_dir,
            disk_watch: None,
            sandbox: None,
        }
    }

    pub fn with_timeout(mut self, timeout: Option<Duration>) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn with_world_model(mut self, world_model: Option<&'a WorldModelContext>) -> Self {
        self.world_model = world_model;
        self
    }

    pub fn with_disk_watch(mut self, disk_watch: DiskWatch) -> Self {
        self.disk_watch = Some(disk_watch);
        self
    }

    pub fn with_sandbox(mut self, sandbox: Option<Arc<dyn ExecutionSandbox>>) -> Self {
        self.sandbox = sandbox;
        self
    }
}

/// A coding agent Forge can run.
#[async_trait]
pub trait AgentAdapter: Send + Sync {
    /// Static description of this agent.
    fn descriptor(&self) -> AgentDescriptor;

    /// The adapter's permission posture for this invocation.
    ///
    /// Unknown is intentionally conservative about facts but does not claim
    /// containment. Adapters with an unrestricted mode must override this.
    fn security(&self) -> AgentSecurity {
        AgentSecurity::unknown()
    }

    /// Checks that the agent can run at all — CLI installed, credentials
    /// present, version supported.
    ///
    /// Called before a workspace is provisioned so a misconfigured agent fails
    /// fast instead of halfway through an experiment.
    async fn prepare(&self) -> AgentResult<()>;

    /// Runs the task in the provided workspace.
    ///
    /// Returning `Ok` means the agent was executed, not that it succeeded: an
    /// agent that exits non-zero or times out is a recorded
    /// [`AgentExecution`], not an error. `Err` is reserved for Forge being
    /// unable to run the agent at all.
    ///
    /// The returned record describes the *process*. It must never encode the
    /// adapter's opinion of the work — that judgment belongs to `forge-eval`,
    /// on the other side of the trust boundary.
    async fn execute(&self, ctx: &RunContext<'_>) -> AgentResult<AgentExecution>;

    /// Stops an in-flight run. Best-effort, and safe to call on a run that has
    /// already finished.
    async fn cancel(&self, _run_id: &RunId) -> AgentResult<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_core::security::NetworkPolicy;

    #[test]
    fn contained_agent_requests_only_configured_supported_credentials() {
        let config = ExecutionSandboxConfig::Required {
            runtime: "docker".into(),
            image: format!("forge@sha256:{}", "a".repeat(64)),
            network: NetworkPolicy::None,
            restricted_network: None,
            cpu_millis: 1_000,
            memory_bytes: 1024,
            pids_limit: 16,
            workspace_limit_bytes: 4096,
            credential_env: vec!["ANTHROPIC_API_KEY".into(), "CODEX_API_KEY".into()],
        };

        assert_eq!(
            contained_agent_credentials_with(
                &config,
                &["ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN"],
                |name| name == "ANTHROPIC_API_KEY"
            ),
            vec!["ANTHROPIC_API_KEY"]
        );
        assert_eq!(
            contained_agent_credentials_with(&config, &["CODEX_API_KEY"], |_| true),
            vec!["CODEX_API_KEY"]
        );
        assert!(
            contained_agent_credentials_with(
                &ExecutionSandboxConfig::None,
                &["CODEX_API_KEY"],
                |_| true
            )
            .is_empty()
        );
    }

    #[test]
    fn contained_agent_selects_one_present_alternative_and_fails_closed_without_one() {
        let config = ExecutionSandboxConfig::Required {
            runtime: "docker".into(),
            image: format!("forge@sha256:{}", "a".repeat(64)),
            network: NetworkPolicy::None,
            restricted_network: None,
            cpu_millis: 1_000,
            memory_bytes: 1024,
            pids_limit: 16,
            workspace_limit_bytes: 4096,
            credential_env: vec!["ANTHROPIC_API_KEY".into(), "ANTHROPIC_AUTH_TOKEN".into()],
        };
        let supported = ["ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN"];

        assert_eq!(
            contained_agent_credentials_with(&config, &supported, |name| {
                name == "ANTHROPIC_AUTH_TOKEN"
            }),
            vec!["ANTHROPIC_AUTH_TOKEN"]
        );
        assert_eq!(
            contained_agent_credentials_with(&config, &supported, |_| false),
            vec!["ANTHROPIC_API_KEY"]
        );
    }
}
