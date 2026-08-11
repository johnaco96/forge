//! The agents Forge knows about, and whether it can actually run them.
//!
//! The registry is deliberately honest about the difference between "Forge has
//! heard of this agent" and "Forge can run it right now". Phase 0 ships the
//! interface and the catalogue; the adapters land in Phase 0's later steps and
//! Phase 1.

use std::path::PathBuf;

use forge_core::agent::{AdapterStatus, AgentConfig, AgentDescriptor, Capability};
use forge_core::ids::AgentId;
use forge_executor::find_executable;

use crate::adapter::AgentAdapter;
use crate::claude::ClaudeAdapter;
use crate::error::{AgentError, AgentResult};

/// Whether an agent can be run on this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Availability {
    pub adapter_status: AdapterStatus,
    /// Where the agent's CLI was found, if it is installed.
    pub executable_path: Option<PathBuf>,
    /// Executable the agent expects, if any.
    pub executable: Option<String>,
}

impl Availability {
    /// Whether a run could start right now.
    pub fn is_runnable(&self) -> bool {
        self.adapter_status == AdapterStatus::Implemented
            && (self.executable.is_none() || self.executable_path.is_some())
    }

    /// Short human-readable explanation for `forge agent list`.
    pub fn summary(&self) -> String {
        match (self.adapter_status, &self.executable_path) {
            (AdapterStatus::Planned, _) => "adapter not implemented yet".to_string(),
            (AdapterStatus::Implemented, Some(path)) => format!("ready ({})", path.display()),
            (AdapterStatus::Implemented, None) => match &self.executable {
                Some(exe) => format!("`{exe}` not found on PATH"),
                None => "ready".to_string(),
            },
        }
    }
}

/// The catalogue of known agents.
#[derive(Debug, Clone)]
pub struct AgentRegistry {
    descriptors: Vec<AgentDescriptor>,
}

impl AgentRegistry {
    /// The agents Forge ships knowledge of.
    pub fn builtin() -> Self {
        Self {
            descriptors: vec![
                crate::claude::descriptor(),
                descriptor(
                    "codex",
                    "OpenAI Codex",
                    "codex-cli",
                    Some("codex"),
                    vec![
                        Capability::EditFiles,
                        Capability::RunCommands,
                        Capability::ReportsUsage,
                    ],
                ),
                descriptor(
                    "pi",
                    "Pi",
                    "pi",
                    Some("pi"),
                    vec![Capability::EditFiles, Capability::RunCommands],
                ),
            ],
        }
    }

    pub fn descriptors(&self) -> &[AgentDescriptor] {
        &self.descriptors
    }

    pub fn get(&self, agent_id: &str) -> Option<&AgentDescriptor> {
        self.descriptors
            .iter()
            .find(|d| d.agent_id.as_str() == agent_id)
    }

    /// Probes the machine for whether `descriptor` could run.
    pub fn availability(&self, descriptor: &AgentDescriptor) -> Availability {
        Availability {
            adapter_status: descriptor.adapter_status,
            executable_path: descriptor.executable.as_deref().and_then(find_executable),
            executable: descriptor.executable.clone(),
        }
    }

    /// Resolves an agent name to something runnable.
    ///
    /// The one place in Forge that maps an agent id to a concrete
    /// implementation. Agents without an adapter report precisely why they
    /// cannot run, rather than failing later from inside a run.
    pub fn adapter(
        &self,
        agent_id: &str,
        config: &AgentConfig,
    ) -> AgentResult<Box<dyn AgentAdapter>> {
        let descriptor = self
            .get(agent_id)
            .ok_or_else(|| AgentError::UnknownAgent(agent_id.to_string()))?;

        match (descriptor.adapter_status, agent_id) {
            (AdapterStatus::Implemented, "claude") => {
                Ok(Box::new(ClaudeAdapter::from_config(config)))
            }
            (AdapterStatus::Implemented, other) => Err(AgentError::Unavailable {
                agent: other.to_string(),
                reason: "marked implemented but not constructible; this is a Forge bug".to_string(),
            }),
            (AdapterStatus::Planned, other) => Err(AgentError::NotImplemented {
                agent: other.to_string(),
                reason: "only the Claude Code adapter exists so far".to_string(),
            }),
        }
    }
}

impl Default for AgentRegistry {
    fn default() -> Self {
        Self::builtin()
    }
}

fn descriptor(
    id: &str,
    display_name: &str,
    harness: &str,
    executable: Option<&str>,
    capabilities: Vec<Capability>,
) -> AgentDescriptor {
    AgentDescriptor {
        agent_id: AgentId::new(id).expect("built-in agent ids are valid"),
        display_name: display_name.to_string(),
        harness: harness.to_string(),
        executable: executable.map(str::to_string),
        default_model: None,
        capabilities,
        // Every built-in agent is `Planned` until its adapter exists. Flipping
        // one to `Implemented` without an adapter would make `forge agent list`
        // lie about what Forge can do.
        adapter_status: AdapterStatus::Planned,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_builtin_agents_are_the_ones_the_design_names() {
        let registry = AgentRegistry::builtin();
        let ids: Vec<&str> = registry
            .descriptors()
            .iter()
            .map(|d| d.agent_id.as_str())
            .collect();
        assert_eq!(ids, vec!["claude", "codex", "pi"]);
    }

    #[test]
    fn claude_has_an_adapter_and_the_others_do_not() {
        let registry = AgentRegistry::builtin();
        assert_eq!(
            registry.get("claude").unwrap().adapter_status,
            AdapterStatus::Implemented
        );
        for planned in ["codex", "pi"] {
            assert_eq!(
                registry.get(planned).unwrap().adapter_status,
                AdapterStatus::Planned,
                "{planned}"
            );
        }
        assert!(registry.adapter("claude", &config("claude")).is_ok());
    }

    #[test]
    fn availability_separates_installed_from_implemented() {
        let registry = AgentRegistry::builtin();
        let codex = registry.get("codex").unwrap();
        let availability = registry.availability(codex);

        // Whether the CLI is installed is independent of whether Forge can
        // drive it, and the summary must not conflate the two.
        assert_eq!(availability.adapter_status, AdapterStatus::Planned);
        assert!(!availability.is_runnable());
        assert_eq!(availability.summary(), "adapter not implemented yet");
    }

    #[test]
    fn an_installed_cli_is_located_on_path() {
        // `sh` stands in for an agent CLI so the test does not depend on which
        // agents happen to be installed.
        let registry = AgentRegistry::builtin();
        let mut descriptor = registry.get("claude").unwrap().clone();
        descriptor.executable = Some("sh".to_string());

        let availability = registry.availability(&descriptor);
        assert!(availability.executable_path.is_some());
        assert!(availability.is_runnable());
        assert!(availability.summary().starts_with("ready ("));
    }

    #[test]
    fn a_missing_cli_is_reported_rather_than_assumed() {
        let registry = AgentRegistry::builtin();
        let mut descriptor = registry.get("codex").unwrap().clone();
        descriptor.executable = Some("forge-not-installed".to_string());
        descriptor.adapter_status = AdapterStatus::Implemented;

        let availability = registry.availability(&descriptor);
        assert!(!availability.is_runnable());
        assert!(availability.summary().contains("not found on PATH"));
    }

    fn config(agent_id: &str) -> AgentConfig {
        AgentConfig::new(AgentId::new(agent_id).unwrap(), "test")
    }

    /// `Box<dyn AgentAdapter>` is not `Debug`, so `unwrap_err` is unavailable.
    fn adapter_error(agent_id: &str) -> AgentError {
        match AgentRegistry::builtin().adapter(agent_id, &config("claude")) {
            Ok(_) => panic!("expected no adapter for `{agent_id}`"),
            Err(err) => err,
        }
    }

    #[test]
    fn unknown_agents_are_named_in_the_error() {
        let err = adapter_error("gpt-9");
        assert!(matches!(err, AgentError::UnknownAgent(_)), "{err}");
        assert!(err.to_string().contains("forge agent list"));
    }

    #[test]
    fn requesting_a_known_but_unimplemented_agent_explains_why() {
        let err = adapter_error("codex");
        assert!(matches!(err, AgentError::NotImplemented { .. }), "{err}");
    }
}
