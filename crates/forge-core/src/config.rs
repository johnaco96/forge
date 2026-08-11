//! Per-repository Forge configuration and on-disk layout.
//!
//! Everything Forge writes lives under `.forge/` inside the repository, so a
//! repository carries its own task definitions and run history and nothing
//! depends on machine-global state.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::ids::RunId;
use crate::routing::{ExplorationPolicy, MinimumRoutingEvidence};
use crate::run::ExecutionProvenance;

/// Directory Forge owns inside a repository.
pub const FORGE_DIR: &str = ".forge";
/// Config file name inside [`FORGE_DIR`].
pub const CONFIG_FILE: &str = "config.toml";
/// Config schema version this build understands.
pub const CONFIG_VERSION: u32 = 1;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("no Forge configuration at `{0}`; run `forge init` first")]
    NotInitialized(PathBuf),
    #[error("failed to read `{path}`")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse `{path}`")]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("failed to serialize configuration")]
    Serialize(#[from] toml::ser::Error),
    #[error("unsupported config version {found}; this build understands {CONFIG_VERSION}")]
    UnsupportedVersion { found: u32 },
    #[error("invalid configuration: {0}")]
    Invalid(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositoryConfig {
    /// Logical name a task's `repository` field must match.
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspacesConfig {
    /// Where agent worktrees are created, relative to the repository root.
    pub root: String,
    /// Prefix for branches Forge creates. Keeps agent branches identifiable and
    /// separable from human branches.
    pub branch_prefix: String,
    /// Whether to keep worktrees after a run finishes. Useful while debugging a
    /// run; wasteful as a default.
    #[serde(default)]
    pub keep_after_run: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoreConfig {
    /// SQLite database path, relative to the repository root.
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DefaultsConfig {
    /// Agent used when `--agent` is omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// Wall-clock budget for a single agent run.
    pub timeout_secs: u64,
}

/// Conservative settings for the deterministic historical router.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoutingConfig {
    pub minimum_total_evidence: u64,
    pub minimum_agent_evidence: u64,
    #[serde(default)]
    pub exploration_policy: ExplorationPolicy,
    #[serde(default = "default_minimum_score_margin")]
    pub minimum_score_margin: f64,
    #[serde(default)]
    pub baseline: BaselineRoutingConfig,
    #[serde(default = "default_periodic_competition_interval")]
    pub periodic_competition_interval: u64,
}

fn default_minimum_score_margin() -> f64 {
    0.05
}

fn default_periodic_competition_interval() -> u64 {
    10
}

fn default_prior() -> f64 {
    1.0
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BaselineRoutingConfig {
    #[serde(default = "default_prior")]
    pub prior_alpha: f64,
    #[serde(default = "default_prior")]
    pub prior_beta: f64,
}

impl Default for BaselineRoutingConfig {
    fn default() -> Self {
        Self {
            prior_alpha: 1.0,
            prior_beta: 1.0,
        }
    }
}

impl Default for RoutingConfig {
    fn default() -> Self {
        let minimum = MinimumRoutingEvidence::default();
        Self {
            minimum_total_evidence: minimum.total,
            minimum_agent_evidence: minimum.per_agent,
            exploration_policy: ExplorationPolicy::default(),
            minimum_score_margin: default_minimum_score_margin(),
            baseline: BaselineRoutingConfig::default(),
            periodic_competition_interval: default_periodic_competition_interval(),
        }
    }
}

impl RoutingConfig {
    pub fn minimum_evidence(&self) -> MinimumRoutingEvidence {
        MinimumRoutingEvidence {
            total: self.minimum_total_evidence,
            per_agent: self.minimum_agent_evidence,
        }
    }
}

/// Per-agent configuration, keyed by agent id under `[agents.<id>]`.
///
/// `executable`, `model`, `timeout_secs`, `extra_args`, and
/// `execution_provenance` have meaning to Forge. Everything else is collected
/// into [`Self::settings`] and passed to the adapter uninterpreted, so a
/// harness-specific knob never requires a core change.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSettings {
    /// Overrides the executable the adapter looks for on `PATH`. Useful for a
    /// non-standard install location, and for pointing tests at a stub.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executable: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Overrides `defaults.timeout_secs` for this agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
    /// Extra arguments appended to the agent's command line.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra_args: Vec<String>,
    /// Explicit override for deterministic/stub infrastructure executions.
    /// Normal CLI executions default to `live`; Forge never infers this from
    /// the executable path or agent name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_provenance: Option<ExecutionProvenance>,
    /// Harness-specific keys. Forge never interprets these.
    #[serde(default, flatten)]
    pub settings: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForgeConfig {
    pub version: u32,
    pub repository: RepositoryConfig,
    pub workspaces: WorkspacesConfig,
    pub store: StoreConfig,
    pub defaults: DefaultsConfig,
    #[serde(default)]
    pub routing: RoutingConfig,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub agents: BTreeMap<String, AgentSettings>,
}

impl ForgeConfig {
    pub fn default_for(repository_name: impl Into<String>) -> Self {
        Self {
            version: CONFIG_VERSION,
            repository: RepositoryConfig {
                name: repository_name.into(),
            },
            workspaces: WorkspacesConfig {
                root: format!("{FORGE_DIR}/worktrees"),
                branch_prefix: "forge/".to_string(),
                keep_after_run: false,
            },
            store: StoreConfig {
                path: format!("{FORGE_DIR}/forge.db"),
            },
            defaults: DefaultsConfig {
                agent: Some("claude".to_string()),
                timeout_secs: 3600,
            },
            routing: RoutingConfig::default(),
            agents: BTreeMap::new(),
        }
    }

    /// Settings for `agent_id`, or defaults when the config does not mention it.
    pub fn agent(&self, agent_id: &str) -> AgentSettings {
        self.agents.get(agent_id).cloned().unwrap_or_default()
    }

    /// The wall-clock budget for one run of `agent_id`.
    pub fn timeout_secs_for(&self, agent_id: &str) -> u64 {
        self.agent(agent_id)
            .timeout_secs
            .unwrap_or(self.defaults.timeout_secs)
    }

    /// Provenance asserted by the execution caller. Normal configured agents
    /// are live; deterministic test harnesses must opt into `synthetic`.
    pub fn execution_provenance_for(&self, agent_id: &str) -> ExecutionProvenance {
        self.agent(agent_id)
            .execution_provenance
            .unwrap_or(ExecutionProvenance::Live)
    }

    pub fn load(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let path = path.as_ref();
        if !path.exists() {
            return Err(ConfigError::NotInitialized(path.to_path_buf()));
        }
        let raw = fs::read_to_string(path).map_err(|source| ConfigError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let config: ForgeConfig = toml::from_str(&raw).map_err(|source| ConfigError::Parse {
            path: path.to_path_buf(),
            source,
        })?;
        config.validate()?;
        Ok(config)
    }

    pub fn save(&self, path: impl AsRef<Path>) -> Result<(), ConfigError> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|source| ConfigError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let body = toml::to_string_pretty(self)?;
        fs::write(path, body).map_err(|source| ConfigError::Io {
            path: path.to_path_buf(),
            source,
        })
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.version != CONFIG_VERSION {
            return Err(ConfigError::UnsupportedVersion {
                found: self.version,
            });
        }
        if self.repository.name.trim().is_empty() {
            return Err(ConfigError::Invalid("repository.name is empty".into()));
        }
        let root = self.workspaces.root.trim();
        if root.is_empty() || root == "." || root == "/" {
            return Err(ConfigError::Invalid(format!(
                "workspaces.root `{root}` would place agent worktrees at the repository root"
            )));
        }
        if Path::new(root).is_absolute() {
            return Err(ConfigError::Invalid(
                "workspaces.root must be relative to the repository root".into(),
            ));
        }
        if self.workspaces.branch_prefix.trim().is_empty() {
            return Err(ConfigError::Invalid(
                "workspaces.branch_prefix is empty; agent branches must be distinguishable from \
                 human branches"
                    .into(),
            ));
        }
        if self.store.path.trim().is_empty() {
            return Err(ConfigError::Invalid("store.path is empty".into()));
        }
        if self.defaults.timeout_secs == 0 {
            return Err(ConfigError::Invalid(
                "defaults.timeout_secs must be greater than zero".into(),
            ));
        }
        if self.routing.minimum_total_evidence == 0 {
            return Err(ConfigError::Invalid(
                "routing.minimum_total_evidence must be greater than zero".into(),
            ));
        }
        if self.routing.minimum_agent_evidence == 0 {
            return Err(ConfigError::Invalid(
                "routing.minimum_agent_evidence must be greater than zero".into(),
            ));
        }
        if !self.routing.minimum_score_margin.is_finite()
            || !(0.0..=1.0).contains(&self.routing.minimum_score_margin)
        {
            return Err(ConfigError::Invalid(
                "routing.minimum_score_margin must be between zero and one".into(),
            ));
        }
        if !self.routing.baseline.prior_alpha.is_finite()
            || self.routing.baseline.prior_alpha <= 0.0
            || !self.routing.baseline.prior_beta.is_finite()
            || self.routing.baseline.prior_beta <= 0.0
        {
            return Err(ConfigError::Invalid(
                "routing baseline priors must be finite and greater than zero".into(),
            ));
        }
        if self.routing.periodic_competition_interval == 0 {
            return Err(ConfigError::Invalid(
                "routing.periodic_competition_interval must be greater than zero".into(),
            ));
        }
        Ok(())
    }

    /// The commented `config.toml` written by `forge init`.
    ///
    /// Kept next to the struct, and covered by a test asserting it parses back
    /// to [`ForgeConfig::default_for`], so the documented defaults cannot drift
    /// from the real ones.
    pub fn template(repository_name: &str) -> String {
        let default = Self::default_for(repository_name);
        format!(
            "# Forge configuration for this repository.\n\
             # See docs/ for what each setting affects.\n\
             version = {version}\n\
             \n\
             [repository]\n\
             # Tasks must name this repository in their `repository` field.\n\
             name = \"{name}\"\n\
             \n\
             [workspaces]\n\
             # Agents never touch your working tree; each run gets a worktree here.\n\
             root = \"{workspace_root}\"\n\
             branch_prefix = \"{branch_prefix}\"\n\
             # Keep worktrees after a run finishes (useful when debugging a run).\n\
             keep_after_run = {keep}\n\
             \n\
             [store]\n\
             # Experience ledger: runs, events, evaluations, metrics.\n\
             path = \"{store_path}\"\n\
             \n\
             [defaults]\n\
             agent = \"claude\"\n\
             timeout_secs = {timeout}\n\
             \n\
             [routing]\n\
             # Deterministic historical-baseline routing policy.\n\
             minimum_total_evidence = {minimum_total}\n\
             minimum_agent_evidence = {minimum_agent}\n\
             minimum_score_margin = {minimum_margin}\n\
             exploration_policy = \"{exploration}\"\n\
             periodic_competition_interval = {periodic_interval}\n\
             \n\
             [routing.baseline]\n\
             prior_alpha = {prior_alpha}\n\
             prior_beta = {prior_beta}\n\
             \n\
             # Per-agent overrides. Unrecognized keys are passed to the adapter.\n\
             # [agents.claude]\n\
             # executable = \"claude\"\n\
             # model = \"opus\"\n\
             # timeout_secs = 1800\n\
             # execution_provenance = \"synthetic\" # only for deterministic stubs\n\
             # permission_mode = \"acceptEdits\"\n",
            version = default.version,
            name = default.repository.name,
            workspace_root = default.workspaces.root,
            branch_prefix = default.workspaces.branch_prefix,
            keep = default.workspaces.keep_after_run,
            store_path = default.store.path,
            timeout = default.defaults.timeout_secs,
            minimum_total = default.routing.minimum_total_evidence,
            minimum_agent = default.routing.minimum_agent_evidence,
            minimum_margin = default.routing.minimum_score_margin,
            periodic_interval = default.routing.periodic_competition_interval,
            prior_alpha = default.routing.baseline.prior_alpha,
            prior_beta = default.routing.baseline.prior_beta,
            exploration = match default.routing.exploration_policy {
                ExplorationPolicy::None => "none",
                ExplorationPolicy::CompeteWhenUncertain => "compete_when_uncertain",
                ExplorationPolicy::PeriodicCompetition => "periodic_competition",
            },
        )
    }
}

/// Resolves the paths Forge uses inside a repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    root: PathBuf,
}

impl Layout {
    /// `root` should be the repository root.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn forge_dir(&self) -> PathBuf {
        self.root.join(FORGE_DIR)
    }

    pub fn config_path(&self) -> PathBuf {
        self.forge_dir().join(CONFIG_FILE)
    }

    pub fn tasks_dir(&self) -> PathBuf {
        self.forge_dir().join("tasks")
    }

    pub fn runs_dir(&self) -> PathBuf {
        self.forge_dir().join("runs")
    }

    /// Artifact directory for one run: captured output, diffs, trajectory.
    pub fn run_dir(&self, run_id: &RunId) -> PathBuf {
        self.runs_dir().join(run_id.as_str())
    }

    /// Resolves a config-relative path against the repository root.
    pub fn resolve(&self, relative: &str) -> PathBuf {
        let path = Path::new(relative);
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.root.join(path)
        }
    }

    pub fn worktrees_root(&self, config: &ForgeConfig) -> PathBuf {
        self.resolve(&config.workspaces.root)
    }

    pub fn store_path(&self, config: &ForgeConfig) -> PathBuf {
        self.resolve(&config.store.path)
    }

    /// Whether `forge init` has been run here.
    pub fn is_initialized(&self) -> bool {
        self.config_path().exists()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_written_template_matches_the_struct_defaults() {
        let parsed: ForgeConfig = toml::from_str(&ForgeConfig::template("forge")).unwrap();
        assert_eq!(parsed, ForgeConfig::default_for("forge"));
        parsed.validate().unwrap();
    }

    #[test]
    fn config_round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FORGE_DIR).join(CONFIG_FILE);
        let config = ForgeConfig::default_for("distributed-runtime");
        config.save(&path).unwrap();
        assert_eq!(ForgeConfig::load(&path).unwrap(), config);
    }

    #[test]
    fn loading_an_uninitialized_repository_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let err = ForgeConfig::load(dir.path().join("missing.toml")).unwrap_err();
        assert!(matches!(err, ConfigError::NotInitialized(_)), "{err}");
    }

    #[test]
    fn workspaces_root_may_not_be_the_repository_root() {
        // Creating worktrees at the repository root would put agents in the
        // user's working tree, which the design forbids outright.
        for root in [".", "", "/"] {
            let mut config = ForgeConfig::default_for("forge");
            config.workspaces.root = root.to_string();
            assert!(config.validate().is_err(), "accepted root `{root}`");
        }
    }

    #[test]
    fn workspaces_root_must_stay_repository_relative() {
        let mut config = ForgeConfig::default_for("forge");
        config.workspaces.root = "/tmp/forge-worktrees".to_string();
        assert!(config.validate().is_err());
    }

    #[test]
    fn future_config_versions_are_refused_rather_than_guessed_at() {
        let mut config = ForgeConfig::default_for("forge");
        config.version = CONFIG_VERSION + 1;
        assert!(matches!(
            config.validate(),
            Err(ConfigError::UnsupportedVersion { .. })
        ));
    }

    #[test]
    fn routing_defaults_are_backwards_compatible_and_conservative() {
        let raw = r#"
version = 1
[repository]
name = "forge"
[workspaces]
root = ".forge/worktrees"
branch_prefix = "forge/"
keep_after_run = false
[store]
path = ".forge/forge.db"
[defaults]
agent = "claude"
timeout_secs = 3600
"#;
        let config: ForgeConfig = toml::from_str(raw).unwrap();
        assert_eq!(config.routing, RoutingConfig::default());
        assert_eq!(
            config.execution_provenance_for("claude"),
            ExecutionProvenance::Live
        );
    }

    #[test]
    fn stub_provenance_is_an_explicit_typed_override() {
        let mut config = ForgeConfig::default_for("forge");
        config.agents.insert(
            "local".into(),
            AgentSettings {
                execution_provenance: Some(ExecutionProvenance::Synthetic),
                ..Default::default()
            },
        );
        assert_eq!(
            config.execution_provenance_for("local"),
            ExecutionProvenance::Synthetic
        );
    }

    #[test]
    fn routing_thresholds_must_be_nonzero() {
        let mut config = ForgeConfig::default_for("forge");
        config.routing.minimum_agent_evidence = 0;
        assert!(config.validate().is_err());
        config.routing.minimum_agent_evidence = 1;
        config.routing.minimum_total_evidence = 0;
        assert!(config.validate().is_err());
    }

    #[test]
    fn unknown_config_keys_are_rejected() {
        let raw = format!(
            "{}\n[unknown_section]\nkey = 1\n",
            ForgeConfig::template("forge")
        );
        assert!(toml::from_str::<ForgeConfig>(&raw).is_err());
    }

    #[test]
    fn layout_resolves_the_documented_paths() {
        let layout = Layout::new("/repo");
        let config = ForgeConfig::default_for("forge");
        assert_eq!(layout.config_path(), Path::new("/repo/.forge/config.toml"));
        assert_eq!(layout.tasks_dir(), Path::new("/repo/.forge/tasks"));
        assert_eq!(layout.runs_dir(), Path::new("/repo/.forge/runs"));
        assert_eq!(
            layout.run_dir(&RunId::sequential(1)),
            Path::new("/repo/.forge/runs/R-0001")
        );
        assert_eq!(
            layout.worktrees_root(&config),
            Path::new("/repo/.forge/worktrees")
        );
        assert_eq!(
            layout.store_path(&config),
            Path::new("/repo/.forge/forge.db")
        );
    }
}
