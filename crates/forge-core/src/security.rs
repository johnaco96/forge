//! What Forge is and is not protecting you from.
//!
//! Forge always isolates an agent's *changes* from the primary working tree.
//! Required mode also contains the agent/evaluator *process* in a
//! Docker-compatible OCI boundary; explicit development mode does not. Those
//! are different guarantees, and reports keep them separate.
//!
//! So the posture is modeled, recorded, and printed rather than left implicit.
//! When a run has no containment and an unrestricted agent, Forge says so every
//! time. A Git worktree is not a sandbox, and Forge should never be the reason
//! someone believes otherwise.

use serde::{Deserialize, Serialize};

/// Network access granted to a production container.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkPolicy {
    #[default]
    None,
    /// A pre-created operator-managed Docker network with external filtering.
    Restricted,
    /// Docker's ordinary bridge network. Explicitly not egress-restricted.
    Allowed,
}

impl NetworkPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Restricted => "restricted",
            Self::Allowed => "allowed",
        }
    }
}

/// Explicit execution boundary. Host mode exists for development; container
/// mode is fail-closed and never falls back to host execution.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum ExecutionSandboxConfig {
    #[serde(rename = "none", alias = "host")]
    #[default]
    None,
    #[serde(rename = "required", alias = "container")]
    Required {
        #[serde(default = "default_container_runtime")]
        runtime: String,
        image: String,
        #[serde(default)]
        network: NetworkPolicy,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        restricted_network: Option<String>,
        #[serde(default = "default_cpu_millis")]
        cpu_millis: u32,
        #[serde(default = "default_memory_bytes")]
        memory_bytes: u64,
        #[serde(default = "default_pids_limit")]
        pids_limit: u32,
        #[serde(default = "default_workspace_limit_bytes")]
        workspace_limit_bytes: u64,
        /// Names a contained command may explicitly request. This is an
        /// allowlist, not a sandbox-wide requirement or container-level
        /// environment: each invocation defaults to requesting none.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        credential_env: Vec<String>,
    },
}

fn default_container_runtime() -> String {
    "docker".into()
}

const fn default_cpu_millis() -> u32 {
    2_000
}

const fn default_memory_bytes() -> u64 {
    4 * 1024 * 1024 * 1024
}

const fn default_pids_limit() -> u32 {
    256
}

const fn default_workspace_limit_bytes() -> u64 {
    20 * 1024 * 1024 * 1024
}

/// How an agent's file changes are kept away from the user's work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceIsolation {
    /// A dedicated Git worktree on a dedicated branch.
    Worktree,
    /// The agent works directly in the repository. Forge never does this.
    None,
}

impl WorkspaceIsolation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Worktree => "git worktree",
            Self::None => "none",
        }
    }
}

/// How the agent's *process* is confined.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostContainment {
    /// The agent runs as the invoking user with that user's access to the
    /// machine and the network.
    None,
    /// A fail-closed Docker-compatible OCI execution boundary.
    Container,
}

impl HostContainment {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Container => "Docker-compatible OCI",
        }
    }
}

/// What an adapter can say about how permissive its agent is.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSecurity {
    /// The harness's own permission setting, verbatim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<String>,
    /// Whether the agent may act without asking. An unattended agent usually
    /// must be, which is exactly why it needs saying out loud.
    pub unrestricted: bool,
}

impl AgentSecurity {
    pub fn new(permission_mode: Option<String>, unrestricted: bool) -> Self {
        Self {
            permission_mode,
            unrestricted,
        }
    }

    /// For adapters that cannot report a permission model.
    pub fn unknown() -> Self {
        Self {
            permission_mode: None,
            unrestricted: false,
        }
    }
}

/// The security state of one run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecurityPosture {
    pub workspace_isolation: WorkspaceIsolation,
    pub host_containment: HostContainment,
    #[serde(default)]
    pub agent: AgentSecurity,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network_policy: Option<NetworkPolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_millis: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_limit_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pids_limit: Option<u32>,
    #[serde(default)]
    pub credential_env: Vec<String>,
}

impl SecurityPosture {
    /// Explicit development mode: an isolated worktree and no process boundary.
    pub fn current(agent: AgentSecurity) -> Self {
        Self {
            workspace_isolation: WorkspaceIsolation::Worktree,
            host_containment: HostContainment::None,
            agent,
            network_policy: None,
            cpu_millis: None,
            memory_bytes: None,
            workspace_limit_bytes: None,
            pids_limit: None,
            credential_env: Vec::new(),
        }
    }

    pub fn for_sandbox(agent: AgentSecurity, sandbox: &ExecutionSandboxConfig) -> Self {
        match sandbox {
            ExecutionSandboxConfig::None => Self::current(agent),
            ExecutionSandboxConfig::Required {
                network,
                cpu_millis,
                memory_bytes,
                pids_limit,
                workspace_limit_bytes,
                credential_env,
                ..
            } => Self {
                workspace_isolation: WorkspaceIsolation::Worktree,
                host_containment: HostContainment::Container,
                agent,
                network_policy: Some(*network),
                cpu_millis: Some(*cpu_millis),
                memory_bytes: Some(*memory_bytes),
                workspace_limit_bytes: Some(*workspace_limit_bytes),
                pids_limit: Some(*pids_limit),
                credential_env: credential_env.clone(),
            },
        }
    }

    /// Whether this run had an unrestricted agent and no containment.
    pub fn is_unconfined(&self) -> bool {
        self.host_containment == HostContainment::None && self.agent.unrestricted
    }

    /// The warning to print before or after such a run.
    ///
    /// Returns `None` when there is nothing extra to say — either the agent is
    /// restricted, or containment exists.
    pub fn warning(&self) -> Option<String> {
        if !self.is_unconfined() {
            return None;
        }
        Some(format!(
            "This run had no host containment and an unrestricted agent{}.\n\
             The Git worktree isolates ordinary candidate changes; it does not contain\n\
             the process or protect files the process reaches outside that worktree.\n\
             Run Forge only on repositories and tasks you would run by hand.",
            self.agent
                .permission_mode
                .as_ref()
                .map(|mode| format!(" (permission mode: {mode})"))
                .unwrap_or_default()
        ))
    }

    /// Label/value rows for the run report.
    pub fn rows(&self) -> Vec<(&'static str, String)> {
        let mut rows = vec![
            (
                "Workspace isolation",
                self.workspace_isolation.as_str().to_string(),
            ),
            (
                "Host containment",
                self.host_containment.as_str().to_string(),
            ),
            (
                "Agent permission mode",
                self.agent
                    .permission_mode
                    .clone()
                    .unwrap_or_else(|| "unknown".to_string()),
            ),
        ];
        if let Some(network) = self.network_policy {
            rows.push(("Network policy", network.as_str().to_string()));
        }
        if let Some(cpu_millis) = self.cpu_millis {
            rows.push((
                "CPU limit",
                format!("{:.3} CPUs", cpu_millis as f64 / 1000.0),
            ));
        }
        if let Some(memory_bytes) = self.memory_bytes {
            rows.push(("Memory limit", format!("{memory_bytes} bytes")));
        }
        if let Some(workspace_limit_bytes) = self.workspace_limit_bytes {
            rows.push(("Workspace limit", format!("{workspace_limit_bytes} bytes")));
        }
        if let Some(pids_limit) = self.pids_limit {
            rows.push(("Process limit", pids_limit.to_string()));
        }
        if self.host_containment == HostContainment::Container {
            rows.push((
                "Credential policy",
                if self.credential_env.is_empty() {
                    "none injected".into()
                } else {
                    format!("job-scoped allowlist: {}", self.credential_env.join(","))
                },
            ));
        }
        rows
    }
}

impl Default for SecurityPosture {
    fn default() -> Self {
        Self::current(AgentSecurity::unknown())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_current_posture_never_claims_containment() {
        // Until containers exist, claiming anything here would be a lie.
        let posture = SecurityPosture::current(AgentSecurity::unknown());
        assert_eq!(posture.workspace_isolation, WorkspaceIsolation::Worktree);
        assert_eq!(posture.host_containment, HostContainment::None);
    }

    #[test]
    fn an_unrestricted_agent_without_containment_warns() {
        let posture = SecurityPosture::current(AgentSecurity::new(
            Some("bypassPermissions".to_string()),
            true,
        ));
        assert!(posture.is_unconfined());

        let warning = posture.warning().expect("should warn");
        assert!(warning.contains("bypassPermissions"), "{warning}");
        assert!(warning.contains("does not contain"), "{warning}");
    }

    #[test]
    fn a_restricted_agent_does_not_warn() {
        let posture =
            SecurityPosture::current(AgentSecurity::new(Some("acceptEdits".to_string()), false));
        assert!(!posture.is_unconfined());
        assert!(posture.warning().is_none());
    }

    #[test]
    fn containment_would_silence_the_warning() {
        let mut posture = SecurityPosture::current(AgentSecurity::new(
            Some("bypassPermissions".to_string()),
            true,
        ));
        posture.host_containment = HostContainment::Container;
        assert!(!posture.is_unconfined());
        assert!(posture.warning().is_none());
    }

    #[test]
    fn the_report_rows_state_all_three_facts() {
        let posture = SecurityPosture::current(AgentSecurity::new(
            Some("bypassPermissions".to_string()),
            true,
        ));
        let rows = posture.rows();
        assert_eq!(rows[0], ("Workspace isolation", "git worktree".to_string()));
        assert_eq!(rows[1], ("Host containment", "none".to_string()));
        assert_eq!(
            rows[2],
            ("Agent permission mode", "bypassPermissions".to_string())
        );
    }

    #[test]
    fn an_adapter_that_cannot_report_permissions_says_unknown() {
        let posture = SecurityPosture::current(AgentSecurity::unknown());
        assert_eq!(posture.rows()[2].1, "unknown");
        assert!(posture.warning().is_none());
    }

    #[test]
    fn posture_round_trips() {
        let posture = SecurityPosture::current(AgentSecurity::new(Some("x".into()), true));
        let json = serde_json::to_string(&posture).unwrap();
        assert_eq!(
            serde_json::from_str::<SecurityPosture>(&json).unwrap(),
            posture
        );
    }
}
