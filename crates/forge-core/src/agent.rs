//! Agent identity and configuration.
//!
//! Forge treats agents as interchangeable engineering workers, so the core
//! model describes *what an agent is* without knowing how any particular one is
//! invoked. Adapters live in `forge-agent`; nothing here depends on Claude
//! Code, Codex, or Pi.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::ids::AgentId;
use crate::security::ExecutionSandboxConfig;

/// Whether Forge can actually run this agent yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdapterStatus {
    /// An adapter exists and can execute runs.
    Implemented,
    /// Forge knows about the agent but cannot execute it yet.
    Planned,
}

impl std::fmt::Display for AdapterStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Implemented => f.write_str("implemented"),
            Self::Planned => f.write_str("planned"),
        }
    }
}

/// A coarse capability claim, used later to filter candidates before routing.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    /// Can edit files in a working tree.
    EditFiles,
    /// Can execute shell commands.
    RunCommands,
    /// Reports token usage or cost.
    ReportsUsage,
    /// Emits a machine-readable trajectory Forge can ingest as events.
    StructuredTrajectory,
    /// Anything not yet modeled.
    Other(String),
}

/// What Forge knows about an agent before any run happens.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentDescriptor {
    pub agent_id: AgentId,
    pub display_name: String,
    /// The harness the agent runs under, e.g. `claude-code`.
    pub harness: String,
    /// Executable Forge expects on `PATH`, if the harness is a CLI.
    pub executable: Option<String>,
    pub default_model: Option<String>,
    pub capabilities: Vec<Capability>,
    pub adapter_status: AdapterStatus,
}

/// The exact configuration one run used.
///
/// This is the unit the experience ledger groups outcomes by: "Claude Code"
/// is not a stable thing to compare, but a specific harness/model/tool/context
/// configuration is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentConfig {
    /// Version of the deterministic fingerprint contract. Historical records
    /// predate this field and deserialize as v1; new runs use v2 and therefore
    /// never silently pool with observations whose effective configuration was
    /// only partially known.
    #[serde(default = "legacy_fingerprint_version")]
    pub fingerprint_version: u8,
    pub agent_id: AgentId,
    pub harness: String,
    /// Version reported by the configured harness, when explicitly captured.
    /// Missing historical values remain missing rather than being inferred.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
    /// Fingerprint of the immutable engineering policy that governed this
    /// execution. It binds routing evidence to execution-policy semantics
    /// without duplicating that domain model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_policy_fingerprint: Option<String>,
    /// Actual host-containment and resource boundary used for this run.
    #[serde(default)]
    pub sandbox: ExecutionSandboxConfig,
    /// Harness-specific settings, kept opaque so adding a knob does not require
    /// a schema change.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub settings: BTreeMap<String, String>,
}

impl AgentConfig {
    pub fn new(agent_id: AgentId, harness: impl Into<String>) -> Self {
        Self {
            fingerprint_version: 2,
            agent_id,
            harness: harness.into(),
            harness_version: None,
            model: None,
            tools: Vec::new(),
            timeout_secs: None,
            execution_policy_fingerprint: None,
            sandbox: ExecutionSandboxConfig::default(),
            settings: BTreeMap::new(),
        }
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    pub fn with_timeout_secs(mut self, timeout_secs: u64) -> Self {
        self.timeout_secs = Some(timeout_secs);
        self
    }

    /// A harness-specific setting, if present.
    ///
    /// Core never interprets these values; only the matching adapter does.
    pub fn setting(&self, key: &str) -> Option<&str> {
        self.settings.get(key).map(String::as_str)
    }

    /// A stable digest of this configuration.
    ///
    /// Two runs sharing a fingerprint are comparable observations of the same
    /// configuration, which is what makes historical aggregation meaningful.
    /// Field ordering is fixed by `BTreeMap` and by writing fields explicitly,
    /// so the digest does not depend on serialization details.
    pub fn fingerprint(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(self.agent_id.as_str().as_bytes());
        hasher.update([0]);
        hasher.update(self.harness.as_bytes());
        hasher.update([0]);
        if self.fingerprint_version >= 2 {
            hasher.update([self.fingerprint_version]);
            hasher.update([0]);
            hasher.update(self.harness_version.as_deref().unwrap_or("").as_bytes());
            hasher.update([0]);
        }
        hasher.update(self.model.as_deref().unwrap_or("").as_bytes());
        hasher.update([0]);
        for tool in &self.tools {
            hasher.update(tool.as_bytes());
            hasher.update([0x1f]);
        }
        hasher.update([0]);
        for (key, value) in &self.settings {
            hasher.update(key.as_bytes());
            hasher.update([0x1e]);
            hasher.update(value.as_bytes());
            hasher.update([0x1f]);
        }
        if self.fingerprint_version >= 2 {
            hasher.update([0]);
            hasher.update(
                self.execution_policy_fingerprint
                    .as_deref()
                    .unwrap_or("")
                    .as_bytes(),
            );
            hasher.update([0]);
            hasher.update(
                self.timeout_secs
                    .map(|value| value.to_string())
                    .unwrap_or_default()
                    .as_bytes(),
            );
            hasher.update([0]);
            hasher.update(
                serde_json::to_vec(&self.sandbox)
                    .expect("sandbox configuration serializes deterministically"),
            );
        }
        let digest = hasher.finalize();
        digest[..8].iter().map(|b| format!("{b:02x}")).collect()
    }
}

fn legacy_fingerprint_version() -> u8 {
    1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> AgentConfig {
        AgentConfig::new(AgentId::new("claude").unwrap(), "claude-code").with_model("opus")
    }

    #[test]
    fn fingerprint_is_stable_across_equal_configs() {
        assert_eq!(config().fingerprint(), config().fingerprint());
    }

    #[test]
    fn fingerprint_versions_preserve_legacy_timeout_semantics() {
        // Historical v1 identity ignored timeout. Preserve that contract for
        // old evidence, while v2 treats the effective execution budget as
        // decision-relevant.
        let mut legacy = config();
        legacy.fingerprint_version = 1;
        let with_timeout = legacy.clone().with_timeout_secs(60);
        assert_eq!(legacy.fingerprint(), with_timeout.fingerprint());

        let with_timeout = config().with_timeout_secs(60);
        assert_ne!(config().fingerprint(), with_timeout.fingerprint());

        let other_model = config().with_model("sonnet");
        assert_ne!(config().fingerprint(), other_model.fingerprint());
    }

    #[test]
    fn historical_unknowns_do_not_pool_with_effective_v2_identity() {
        let historical: AgentConfig =
            serde_json::from_str(r#"{"agent_id":"claude","harness":"claude-code","model":"opus"}"#)
                .unwrap();
        assert_eq!(historical.fingerprint_version, 1);

        let mut effective = config();
        effective.harness_version = Some("2.1.223".into());
        effective.execution_policy_fingerprint = Some("policy-config-v2".into());
        assert_ne!(historical.fingerprint(), effective.fingerprint());
    }

    #[test]
    fn fingerprint_separates_tool_sets() {
        let mut a = config();
        a.tools = vec!["cargo".into(), "rr".into()];
        let mut b = config();
        b.tools = vec!["cargo".into()];
        assert_ne!(a.fingerprint(), b.fingerprint());
    }

    #[test]
    fn fingerprint_is_not_confused_by_field_boundaries() {
        // Without separators, ("ab", "c") and ("a", "bc") would collide.
        let mut a = config();
        a.harness = "ab".into();
        a.model = Some("c".into());
        let mut b = config();
        b.harness = "a".into();
        b.model = Some("bc".into());
        assert_ne!(a.fingerprint(), b.fingerprint());
    }
}
