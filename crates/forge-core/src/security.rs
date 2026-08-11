//! What Forge is and is not protecting you from.
//!
//! Forge isolates an agent's *changes* from your working tree. It does not
//! contain the agent's *process*. Those are different guarantees, and the gap
//! between them is easy to misread in Forge's favour — a run that reports
//! "workspace: isolated" invites the conclusion that the thing is sandboxed.
//!
//! So the posture is modeled, recorded, and printed rather than left implicit.
//! When a run has no containment and an unrestricted agent, Forge says so every
//! time. A Git worktree is not a sandbox, and Forge should never be the reason
//! someone believes otherwise.

use serde::{Deserialize, Serialize};

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
    /// Reserved for container isolation, which does not exist yet.
    Container,
}

impl HostContainment {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Container => "container",
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
}

impl SecurityPosture {
    /// What Forge can offer today: an isolated worktree and nothing more.
    pub fn current(agent: AgentSecurity) -> Self {
        Self {
            workspace_isolation: WorkspaceIsolation::Worktree,
            host_containment: HostContainment::None,
            agent,
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
        vec![
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
        ]
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
