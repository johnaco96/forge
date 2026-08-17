//! Environment isolation and secret filtering.
//!
//! Coding agents run arbitrary shell commands, and so do the evaluation
//! commands a repository declares. Neither should inherit the operator's whole
//! environment by default: a leaked credential in a captured log is permanent,
//! because Forge's entire value proposition is that it keeps run records
//! forever.
//!
//! Environment filtering is a policy layer, not a sandbox. Required mode adds
//! the separate OCI execution boundary below; development mode still relies on
//! filtering alone.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use forge_core::run::{InfrastructureFailure, InfrastructureFailureKind};
use forge_core::security::{ExecutionSandboxConfig, NetworkPolicy};

use crate::error::{ExecError, ExecResult};
use crate::process::ExecRequest;

/// Environment variables passed through by [`EnvPolicy::conservative`].
///
/// Enough for a normal build toolchain to work, and nothing that carries
/// credentials.
const CONSERVATIVE_ALLOW: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "TERM",
    "TMPDIR",
    "TZ",
    // Toolchain locations that are expensive or impossible to rediscover.
    "CARGO_HOME",
    "RUSTUP_HOME",
    "JAVA_HOME",
    "GOPATH",
    "GOROOT",
    "PYENV_ROOT",
    "NVM_DIR",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
];

/// Case-insensitive substrings that mark a variable as sensitive.
const SECRET_MARKERS: &[&str] = &[
    "TOKEN",
    "SECRET",
    "PASSWORD",
    "PASSWD",
    "CREDENTIAL",
    "PRIVATE_KEY",
    "API_KEY",
    "APIKEY",
    "ACCESS_KEY",
    "SESSION_KEY",
    "AUTH",
];

/// Shortest value that will be redacted from captured output.
///
/// Short values produce false positives (`"true"`, a one-character key) that
/// would mangle logs without protecting anything.
const MIN_REDACTABLE_LEN: usize = 8;

/// Which environment a child process receives.
#[derive(Debug, Clone)]
pub struct EnvPolicy {
    inherit_all: bool,
    allow: Vec<String>,
    /// Applied even when `inherit_all` is set.
    deny_markers: Vec<String>,
    /// Exact names removed regardless of everything else.
    deny_exact: Vec<String>,
    extra: BTreeMap<String, String>,
}

impl EnvPolicy {
    /// Passes through a known-safe allowlist only.
    ///
    /// The default for evaluation commands, which Forge runs on code an agent
    /// just wrote.
    pub fn conservative() -> Self {
        Self {
            inherit_all: false,
            allow: CONSERVATIVE_ALLOW.iter().map(|s| s.to_string()).collect(),
            deny_markers: SECRET_MARKERS.iter().map(|s| s.to_string()).collect(),
            deny_exact: Vec::new(),
            extra: BTreeMap::new(),
        }
    }

    /// Inherits everything except variables that look like secrets.
    ///
    /// Agent harnesses need credentials to reach their model provider, so an
    /// adapter will typically start here and allow its own credential
    /// variables back in explicitly with [`Self::allow_var`].
    pub fn inherit_non_secrets() -> Self {
        Self {
            inherit_all: true,
            allow: Vec::new(),
            deny_markers: SECRET_MARKERS.iter().map(|s| s.to_string()).collect(),
            deny_exact: Vec::new(),
            extra: BTreeMap::new(),
        }
    }

    /// Passes nothing through except what is added explicitly.
    pub fn empty() -> Self {
        Self {
            inherit_all: false,
            allow: Vec::new(),
            deny_markers: SECRET_MARKERS.iter().map(|s| s.to_string()).collect(),
            deny_exact: Vec::new(),
            extra: BTreeMap::new(),
        }
    }

    /// Allows a specific variable through, overriding the secret markers.
    ///
    /// Use for credentials an agent genuinely needs. Values allowed this way
    /// are still redacted from captured output.
    pub fn allow_var(mut self, name: impl Into<String>) -> Self {
        self.allow.push(name.into());
        self
    }

    /// Removes a variable the child must not see, whatever else the policy says.
    ///
    /// Takes precedence over [`Self::allow_var`] and over inheritance.
    pub fn deny_var(mut self, name: impl Into<String>) -> Self {
        self.deny_exact.push(name.into());
        self
    }

    /// Sets a variable regardless of the ambient environment.
    pub fn set(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.extra.insert(name.into(), value.into());
        self
    }

    fn looks_secret(&self, name: &str) -> bool {
        let upper = name.to_ascii_uppercase();
        self.deny_markers.iter().any(|m| upper.contains(m.as_str()))
    }

    fn is_explicitly_allowed(&self, name: &str) -> bool {
        self.allow.iter().any(|a| a == name)
    }

    /// Builds the child environment from the current process environment.
    pub fn build(&self) -> BTreeMap<String, String> {
        self.build_from(std::env::vars())
    }

    /// Testable core of [`Self::build`].
    pub fn build_from<I>(&self, source: I) -> BTreeMap<String, String>
    where
        I: IntoIterator<Item = (String, String)>,
    {
        let mut env = BTreeMap::new();
        for (name, value) in source {
            if self.deny_exact.contains(&name) {
                continue;
            }
            let keep = if self.is_explicitly_allowed(&name) {
                true
            } else if self.looks_secret(&name) {
                false
            } else {
                self.inherit_all
            };
            if keep {
                env.insert(name, value);
            }
        }
        env.extend(self.extra.clone());
        env
    }

    /// Builds a redactor for every secret-looking value in the current
    /// environment, whether or not the policy passes it through.
    ///
    /// A value can reach a log without being in the child's environment — an
    /// agent may print a token it read from a config file — so redaction is
    /// deliberately independent of what was passed through.
    pub fn redactor(&self) -> Redactor {
        self.redactor_from(std::env::vars())
    }

    /// Testable core of [`Self::redactor`].
    pub fn redactor_from<I>(&self, source: I) -> Redactor
    where
        I: IntoIterator<Item = (String, String)>,
    {
        let mut secrets: Vec<String> = source
            .into_iter()
            .filter(|(name, value)| self.looks_secret(name) && value.len() >= MIN_REDACTABLE_LEN)
            .map(|(_, value)| value)
            .collect();
        // Longest first, so an embedded shorter secret cannot leave a fragment
        // of a longer one behind.
        secrets.sort_by_key(|s| std::cmp::Reverse(s.len()));
        secrets.dedup();
        Redactor { secrets }
    }
}

impl Default for EnvPolicy {
    fn default() -> Self {
        Self::conservative()
    }
}

/// Removes known secret values from text before it is stored.
#[derive(Debug, Clone, Default)]
pub struct Redactor {
    secrets: Vec<String>,
}

/// Placeholder written in place of a redacted value.
pub const REDACTED: &str = "[redacted]";

impl Redactor {
    /// A redactor that removes nothing.
    pub fn none() -> Self {
        Self::default()
    }

    pub fn with_secret(mut self, secret: impl Into<String>) -> Self {
        let secret = secret.into();
        if secret.len() >= MIN_REDACTABLE_LEN {
            self.secrets.push(secret);
            self.secrets.sort_by_key(|s| std::cmp::Reverse(s.len()));
        }
        self
    }

    pub fn is_empty(&self) -> bool {
        self.secrets.is_empty()
    }

    pub fn redact(&self, text: &str) -> String {
        let mut out = text.to_string();
        for secret in &self.secrets {
            if out.contains(secret.as_str()) {
                out = out.replace(secret.as_str(), REDACTED);
            }
        }
        out
    }
}

/// Fully resolved process invocation after applying an execution boundary.
#[derive(Debug)]
pub struct SandboxedInvocation {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub env: BTreeMap<String, String>,
}

/// Host-containment seam shared by agent and evaluator subprocesses.
#[async_trait]
pub trait ExecutionSandbox: std::fmt::Debug + Send + Sync {
    async fn preflight(&self) -> ExecResult<()>;
    fn wrap(
        &self,
        request: &ExecRequest,
        child_env: &BTreeMap<String, String>,
    ) -> ExecResult<SandboxedInvocation>;
    async fn post_run(&self) -> ExecResult<Vec<InfrastructureFailure>>;
    async fn cleanup(&self) -> ExecResult<()>;
}

#[derive(Debug, Clone)]
pub struct DockerSandbox {
    runtime: String,
    image: String,
    network: NetworkPolicy,
    restricted_network: Option<String>,
    cpu_millis: u32,
    memory_bytes: u64,
    pids_limit: u32,
    credential_env: Vec<String>,
    workspace: PathBuf,
    git_dir: PathBuf,
    container_name: String,
}

impl DockerSandbox {
    pub fn from_config(
        config: &ExecutionSandboxConfig,
        git_common_dir: &Path,
        workspace: &Path,
        run_label: &str,
    ) -> ExecResult<Option<Arc<dyn ExecutionSandbox>>> {
        let ExecutionSandboxConfig::Required {
            runtime,
            image,
            network,
            restricted_network,
            cpu_millis,
            memory_bytes,
            pids_limit,
            credential_env,
            ..
        } = config
        else {
            return Ok(None);
        };
        let workspace = canonical_mount(workspace)?;
        let git_dir = canonical_mount(git_common_dir)?;
        for path in [&workspace, &git_dir] {
            if path.to_string_lossy().contains(',') {
                return Err(infrastructure(
                    InfrastructureFailureKind::SandboxUnavailable,
                    format!(
                        "Docker --mount cannot safely encode path `{}` containing a comma",
                        path.display()
                    ),
                ));
            }
        }
        let safe_label = run_label
            .chars()
            .map(|value| {
                if value.is_ascii_alphanumeric() || value == '-' || value == '_' {
                    value.to_ascii_lowercase()
                } else {
                    '-'
                }
            })
            .collect::<String>();
        Ok(Some(Arc::new(Self {
            runtime: runtime.clone(),
            image: image.clone(),
            network: *network,
            restricted_network: restricted_network.clone(),
            cpu_millis: *cpu_millis,
            memory_bytes: *memory_bytes,
            pids_limit: *pids_limit,
            credential_env: credential_env.clone(),
            workspace,
            git_dir,
            container_name: format!("forge-{safe_label}-{}", std::process::id()),
        })))
    }

    fn network_name(&self) -> ExecResult<&str> {
        match self.network {
            NetworkPolicy::None => Ok("none"),
            NetworkPolicy::Allowed => Ok("bridge"),
            NetworkPolicy::Restricted => self.restricted_network.as_deref().ok_or_else(|| {
                infrastructure(
                    InfrastructureFailureKind::SandboxUnavailable,
                    "restricted Docker networking has no configured network",
                )
            }),
        }
    }

    fn docker_environment(&self, child_env: &BTreeMap<String, String>) -> BTreeMap<String, String> {
        let mut environment = BTreeMap::new();
        for name in ["PATH", "HOME", "DOCKER_HOST", "DOCKER_CONTEXT"] {
            if let Ok(value) = std::env::var(name) {
                environment.insert(name.to_string(), value);
            }
        }
        for name in &self.credential_env {
            if let Some(value) = child_env.get(name) {
                environment.insert(name.clone(), value.clone());
            }
        }
        environment
    }

    async fn runtime_status(&self, args: &[&str]) -> ExecResult<std::process::Output> {
        tokio::process::Command::new(&self.runtime)
            .args(args)
            .env_clear()
            .envs(self.docker_environment(&BTreeMap::new()))
            .output()
            .await
            .map_err(|source| {
                infrastructure(
                    InfrastructureFailureKind::SandboxUnavailable,
                    format!(
                        "could not execute container runtime `{}`: {source}",
                        self.runtime
                    ),
                )
            })
    }
}

#[async_trait]
impl ExecutionSandbox for DockerSandbox {
    async fn preflight(&self) -> ExecResult<()> {
        for name in &self.credential_env {
            if std::env::var_os(name).is_none() {
                return Err(infrastructure(
                    InfrastructureFailureKind::CredentialUnavailable,
                    format!("required job credential `{name}` is not present"),
                ));
            }
        }
        let version = self
            .runtime_status(&["version", "--format", "{{.Server.Version}}"])
            .await?;
        if !version.status.success() {
            return Err(infrastructure(
                InfrastructureFailureKind::SandboxUnavailable,
                format!(
                    "container runtime `{}` is unavailable: {}",
                    self.runtime,
                    String::from_utf8_lossy(&version.stderr).trim()
                ),
            ));
        }
        let image = self
            .runtime_status(&["image", "inspect", &self.image])
            .await?;
        if !image.status.success() {
            return Err(infrastructure(
                InfrastructureFailureKind::SandboxUnavailable,
                format!("container image `{}` is not available locally", self.image),
            ));
        }
        if self.network == NetworkPolicy::Restricted {
            let network = self.network_name()?;
            let inspect = self
                .runtime_status(&["network", "inspect", network])
                .await?;
            if !inspect.status.success() {
                return Err(infrastructure(
                    InfrastructureFailureKind::SandboxUnavailable,
                    format!("restricted Docker network `{network}` is not available"),
                ));
            }
        }
        Ok(())
    }

    fn wrap(
        &self,
        request: &ExecRequest,
        child_env: &BTreeMap<String, String>,
    ) -> ExecResult<SandboxedInvocation> {
        let mut args = vec![
            "run".into(),
            "--name".into(),
            self.container_name.clone(),
            "--read-only".into(),
            "--cap-drop=ALL".into(),
            "--security-opt=no-new-privileges".into(),
            "--workdir".into(),
            self.workspace.to_string_lossy().into_owned(),
            "--mount".into(),
            format!(
                "type=bind,src={},dst={}",
                self.workspace.display(),
                self.workspace.display()
            ),
            "--mount".into(),
            format!(
                "type=bind,src={},dst={},readonly",
                self.git_dir.display(),
                self.git_dir.display()
            ),
            "--tmpfs".into(),
            "/tmp:rw,exec,nosuid,nodev,size=1g".into(),
            "--tmpfs".into(),
            "/home/forge:rw,nosuid,nodev,size=256m".into(),
            "--env".into(),
            "HOME=/home/forge".into(),
            "--env".into(),
            "TMPDIR=/tmp".into(),
            "--env".into(),
            "GIT_OPTIONAL_LOCKS=0".into(),
            "--env".into(),
            "GIT_TERMINAL_PROMPT=0".into(),
            "--network".into(),
            self.network_name()?.into(),
            "--cpus".into(),
            format!("{:.3}", self.cpu_millis as f64 / 1000.0),
            "--memory".into(),
            self.memory_bytes.to_string(),
            "--pids-limit".into(),
            self.pids_limit.to_string(),
        ];
        #[cfg(unix)]
        {
            args.push("--user".into());
            args.push(format!("{}:{}", unsafe { libc::geteuid() }, unsafe {
                libc::getegid()
            }));
        }
        for name in ["LANG", "LC_ALL", "LC_CTYPE", "TERM", "TZ"] {
            if let Some(value) = child_env.get(name) {
                args.push("--env".into());
                args.push(format!("{name}={value}"));
            }
        }
        for name in &self.credential_env {
            if !child_env.contains_key(name) {
                return Err(infrastructure(
                    InfrastructureFailureKind::CredentialUnavailable,
                    format!("required job credential `{name}` was filtered before injection"),
                ));
            }
            args.push("--env".into());
            args.push(name.clone());
        }
        args.push(self.image.clone());
        args.push(request.program.clone());
        args.extend(request.args.iter().cloned());
        Ok(SandboxedInvocation {
            program: self.runtime.clone(),
            args,
            cwd: request.cwd.clone(),
            env: self.docker_environment(child_env),
        })
    }

    async fn post_run(&self) -> ExecResult<Vec<InfrastructureFailure>> {
        let inspect = self
            .runtime_status(&[
                "inspect",
                "--format",
                "{{json .State}}",
                &self.container_name,
            ])
            .await?;
        if !inspect.status.success() {
            return Err(infrastructure(
                InfrastructureFailureKind::SandboxUnavailable,
                format!(
                    "could not inspect completed container `{}`",
                    self.container_name
                ),
            ));
        }
        let state: serde_json::Value =
            serde_json::from_slice(&inspect.stdout).map_err(|error| {
                infrastructure(
                    InfrastructureFailureKind::SandboxUnavailable,
                    format!("container state was not valid JSON: {error}"),
                )
            })?;
        let mut failures = Vec::new();
        if let Some(error) = state["Error"].as_str().filter(|value| !value.is_empty()) {
            failures.push(InfrastructureFailure::new(
                InfrastructureFailureKind::SandboxUnavailable,
                format!(
                    "container `{}` could not start its configured command: {error}",
                    self.container_name
                ),
            ));
        }
        if state["OOMKilled"].as_bool() == Some(true) {
            failures.push(InfrastructureFailure::new(
                InfrastructureFailureKind::MemoryLimitExceeded,
                format!(
                    "container `{}` exceeded its {} byte memory limit",
                    self.container_name, self.memory_bytes
                ),
            ));
        }
        Ok(failures)
    }

    async fn cleanup(&self) -> ExecResult<()> {
        let output = self
            .runtime_status(&["rm", "--force", &self.container_name])
            .await?;
        if output.status.success()
            || String::from_utf8_lossy(&output.stderr).contains("No such container")
        {
            Ok(())
        } else {
            Err(infrastructure(
                InfrastructureFailureKind::SandboxUnavailable,
                format!(
                    "could not remove container `{}`: {}",
                    self.container_name,
                    String::from_utf8_lossy(&output.stderr).trim()
                ),
            ))
        }
    }
}

pub async fn preflight_sandbox_config(config: &ExecutionSandboxConfig) -> ExecResult<()> {
    let ExecutionSandboxConfig::Required {
        runtime,
        image,
        network,
        restricted_network,
        credential_env,
        ..
    } = config
    else {
        return Ok(());
    };
    for name in credential_env {
        if std::env::var_os(name).is_none() {
            return Err(infrastructure(
                InfrastructureFailureKind::CredentialUnavailable,
                format!("required job credential `{name}` is not present"),
            ));
        }
    }
    if !sandbox_runtime_status(runtime, &["version", "--format", "{{.Server.Version}}"])
        .await?
        .status
        .success()
    {
        return Err(infrastructure(
            InfrastructureFailureKind::SandboxUnavailable,
            format!("container runtime `{runtime}` is unavailable"),
        ));
    }
    if !sandbox_runtime_status(runtime, &["image", "inspect", image])
        .await?
        .status
        .success()
    {
        return Err(infrastructure(
            InfrastructureFailureKind::SandboxUnavailable,
            format!("container image `{image}` is not available locally"),
        ));
    }
    if *network == NetworkPolicy::Restricted {
        let name = restricted_network.as_deref().ok_or_else(|| {
            infrastructure(
                InfrastructureFailureKind::SandboxUnavailable,
                "restricted network name is missing",
            )
        })?;
        if !sandbox_runtime_status(runtime, &["network", "inspect", name])
            .await?
            .status
            .success()
        {
            return Err(infrastructure(
                InfrastructureFailureKind::SandboxUnavailable,
                format!("restricted Docker network `{name}` is unavailable"),
            ));
        }
    }
    Ok(())
}

async fn sandbox_runtime_status(runtime: &str, args: &[&str]) -> ExecResult<std::process::Output> {
    let mut command = tokio::process::Command::new(runtime);
    command.args(args).env_clear();
    for name in ["PATH", "HOME", "DOCKER_HOST", "DOCKER_CONTEXT"] {
        if let Ok(value) = std::env::var(name) {
            command.env(name, value);
        }
    }
    command.output().await.map_err(|error| {
        infrastructure(
            InfrastructureFailureKind::SandboxUnavailable,
            format!("could not execute container runtime `{runtime}`: {error}"),
        )
    })
}

fn canonical_mount(path: &Path) -> ExecResult<PathBuf> {
    std::fs::canonicalize(path).map_err(|error| {
        infrastructure(
            InfrastructureFailureKind::SandboxUnavailable,
            format!("cannot mount `{}`: {error}", path.display()),
        )
    })
}

fn infrastructure(kind: InfrastructureFailureKind, detail: impl Into<String>) -> ExecError {
    ExecError::Infrastructure(InfrastructureFailure::new(kind, detail))
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_core::events::NullSink;
    use forge_core::security::ExecutionSandboxConfig;

    fn env() -> Vec<(String, String)> {
        [
            ("PATH", "/usr/bin"),
            ("HOME", "/Users/dev"),
            ("ANTHROPIC_API_KEY", "sk-ant-super-secret-value"),
            ("AWS_SECRET_ACCESS_KEY", "wJalrXUtnFEMI/K7MDENG"),
            ("GITHUB_TOKEN", "ghp_0123456789abcdef"),
            ("EDITOR", "vim"),
            ("RUSTFLAGS", "-C debuginfo=0"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
    }

    #[test]
    fn the_conservative_policy_passes_only_the_allowlist() {
        let built = EnvPolicy::conservative().build_from(env());
        assert_eq!(built.get("PATH").map(String::as_str), Some("/usr/bin"));
        assert_eq!(built.get("HOME").map(String::as_str), Some("/Users/dev"));
        // Not a secret, but not on the allowlist either.
        assert!(!built.contains_key("EDITOR"));
        assert!(!built.contains_key("ANTHROPIC_API_KEY"));
    }

    #[test]
    fn inheriting_still_drops_anything_that_looks_like_a_secret() {
        let built = EnvPolicy::inherit_non_secrets().build_from(env());
        assert!(built.contains_key("EDITOR"));
        assert!(built.contains_key("RUSTFLAGS"));
        for secret in ["ANTHROPIC_API_KEY", "AWS_SECRET_ACCESS_KEY", "GITHUB_TOKEN"] {
            assert!(
                !built.contains_key(secret),
                "{secret} leaked into the child"
            );
        }
    }

    #[test]
    fn an_agents_credential_can_be_allowed_back_in_explicitly() {
        let built = EnvPolicy::inherit_non_secrets()
            .allow_var("ANTHROPIC_API_KEY")
            .build_from(env());
        assert!(built.contains_key("ANTHROPIC_API_KEY"));
        // Allowing one credential must not allow the rest.
        assert!(!built.contains_key("GITHUB_TOKEN"));
    }

    #[test]
    fn explicit_values_override_the_ambient_environment() {
        let built = EnvPolicy::conservative()
            .set("PATH", "/forge/bin")
            .build_from(env());
        assert_eq!(built.get("PATH").map(String::as_str), Some("/forge/bin"));
    }

    #[test]
    fn secrets_are_redacted_from_captured_output_even_when_allowed_through() {
        let redactor = EnvPolicy::inherit_non_secrets()
            .allow_var("ANTHROPIC_API_KEY")
            .redactor_from(env());

        let log = "calling api with key sk-ant-super-secret-value and ghp_0123456789abcdef";
        let redacted = redactor.redact(log);
        assert!(
            !redacted.contains("sk-ant-super-secret-value"),
            "{redacted}"
        );
        assert!(!redacted.contains("ghp_0123456789abcdef"), "{redacted}");
        assert!(redacted.contains(REDACTED));
    }

    #[test]
    fn short_values_are_not_redacted_so_logs_stay_readable() {
        let redactor = EnvPolicy::conservative()
            .redactor_from(vec![("MY_TOKEN".to_string(), "yes".to_string())]);
        assert!(redactor.is_empty());
        assert_eq!(redactor.redact("yes it works"), "yes it works");
    }

    #[test]
    fn overlapping_secrets_are_fully_removed() {
        // A short secret contained in a longer one must not leave a fragment.
        let redactor = Redactor::none()
            .with_secret("abcdefgh")
            .with_secret("abcdefghijklmno");
        let redacted = redactor.redact("value=abcdefghijklmno");
        assert_eq!(redacted, format!("value={REDACTED}"));
    }

    fn required_container(runtime: &str) -> ExecutionSandboxConfig {
        ExecutionSandboxConfig::Required {
            runtime: runtime.into(),
            image: "forge-runtime@sha256:test".into(),
            network: NetworkPolicy::None,
            restricted_network: None,
            cpu_millis: 1_500,
            memory_bytes: 512 * 1024 * 1024,
            pids_limit: 64,
            workspace_limit_bytes: 1024 * 1024 * 1024,
            credential_env: Vec::new(),
        }
    }

    #[test]
    fn docker_boundary_mounts_only_workspace_and_git_and_enforces_limits() {
        let fixture = tempfile::tempdir().unwrap();
        let workspace = fixture.path().join("workspace");
        let git = fixture.path().join("git-common");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&git).unwrap();
        let sandbox =
            DockerSandbox::from_config(&required_container("docker"), &git, &workspace, "R-0001")
                .unwrap()
                .unwrap();
        let request = ExecRequest::program("/bin/sh", ["-c", "true"], &workspace);
        let invocation = sandbox.wrap(&request, &BTreeMap::new()).unwrap();
        let joined = invocation.args.join(" ");

        assert_eq!(invocation.program, "docker");
        assert!(joined.contains("--read-only"));
        assert!(joined.contains("--cap-drop=ALL"));
        assert!(joined.contains("--security-opt=no-new-privileges"));
        assert!(joined.contains("--network none"));
        assert!(joined.contains("--cpus 1.500"));
        assert!(joined.contains("--memory 536870912"));
        assert!(joined.contains("--pids-limit 64"));
        assert!(joined.contains("HOME=/home/forge"));
        assert!(joined.contains(&workspace.to_string_lossy().into_owned()));
        assert!(joined.contains(&git.to_string_lossy().into_owned()));
        for forbidden in ["/.ssh", "/.aws", "/.config", "/.codex", "/Users/"] {
            assert!(
                !joined.contains(forbidden),
                "leaked mount or path: {joined}"
            );
        }
    }

    #[tokio::test]
    async fn required_containment_fails_closed_when_runtime_is_missing() {
        let error = preflight_sandbox_config(&required_container(
            "forge-container-runtime-that-does-not-exist",
        ))
        .await
        .unwrap_err();
        assert!(matches!(
            error,
            ExecError::Infrastructure(InfrastructureFailure {
                kind: InfrastructureFailureKind::SandboxUnavailable,
                ..
            })
        ));
    }

    /// Runs only in the dedicated CI sandbox job or by an operator with the
    /// pinned fixture image already present. It never pulls an image or calls
    /// a provider.
    #[tokio::test]
    #[ignore = "requires Docker and a local alpine:3.20 fixture image"]
    async fn docker_adversarial_boundary_blocks_host_secret_escape_and_network() {
        let fixture = tempfile::tempdir().unwrap();
        let workspace = fixture.path().join("workspace");
        let git = fixture.path().join("git-common");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&git).unwrap();
        let secret = fixture.path().join("host-secret");
        let sentinel = fixture.path().join("host-sentinel");
        std::fs::write(&secret, "not-for-the-container").unwrap();

        let mut config = required_container("docker");
        let ExecutionSandboxConfig::Required {
            image,
            memory_bytes,
            ..
        } = &mut config
        else {
            unreachable!()
        };
        *image = "alpine:3.20".into();
        *memory_bytes = 128 * 1024 * 1024;
        let sandbox = DockerSandbox::from_config(&config, &git, &workspace, "adversarial-boundary")
            .unwrap()
            .unwrap();
        let command = format!(
            "test ! -e '{}' && ! touch '{}' && test \"$HOME\" = /home/forge && test -z \"$FORGE_FAKE_SECRET\" && ! wget -T 1 -q -O /dev/null http://1.1.1.1",
            secret.display(),
            sentinel.display(),
        );
        let outcome = crate::ProcessRunner::conservative()
            .with_sandbox(sandbox)
            .run(
                &ExecRequest::program("/bin/sh", ["-c", &command], &workspace)
                    .with_env("FORGE_FAKE_SECRET", "host-only-secret")
                    .with_timeout(std::time::Duration::from_secs(10)),
                &NullSink,
            )
            .await
            .unwrap();
        assert!(outcome.success(), "{}", outcome.stderr);
        assert!(!sentinel.exists());
    }

    #[tokio::test]
    #[ignore = "requires Docker and a local alpine:3.20 fixture image"]
    async fn docker_adversarial_timeout_cleans_up_descendants() {
        let fixture = tempfile::tempdir().unwrap();
        let workspace = fixture.path().join("workspace");
        let git = fixture.path().join("git-common");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&git).unwrap();
        let marker = workspace.join("orphan-marker");
        let mut config = required_container("docker");
        let ExecutionSandboxConfig::Required { image, .. } = &mut config else {
            unreachable!()
        };
        *image = "alpine:3.20".into();
        let sandbox = DockerSandbox::from_config(&config, &git, &workspace, "adversarial-timeout")
            .unwrap()
            .unwrap();
        let script = format!("(sleep 2; touch '{}') & sleep 30", marker.display());
        let outcome = crate::ProcessRunner::conservative()
            .with_sandbox(sandbox)
            .run(
                &ExecRequest::program("/bin/sh", ["-c", &script], &workspace)
                    .with_timeout(std::time::Duration::from_millis(500)),
                &NullSink,
            )
            .await
            .unwrap();
        assert!(outcome.timed_out);
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        assert!(!marker.exists(), "container descendant survived cleanup");
    }

    #[tokio::test]
    #[ignore = "requires Docker and a local alpine:3.20 fixture image"]
    async fn docker_adversarial_memory_limit_is_typed() {
        let fixture = tempfile::tempdir().unwrap();
        let workspace = fixture.path().join("workspace");
        let git = fixture.path().join("git-common");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&git).unwrap();
        let mut config = required_container("docker");
        let ExecutionSandboxConfig::Required {
            image,
            memory_bytes,
            ..
        } = &mut config
        else {
            unreachable!()
        };
        *image = "alpine:3.20".into();
        *memory_bytes = 32 * 1024 * 1024;
        let sandbox = DockerSandbox::from_config(&config, &git, &workspace, "adversarial-memory")
            .unwrap()
            .unwrap();
        let outcome = crate::ProcessRunner::conservative()
            .with_sandbox(sandbox)
            .run(
                &ExecRequest::program(
                    "/bin/sh",
                    [
                        "-c",
                        "awk 'BEGIN { x=\"x\"; for (i=0; i<28; i++) x=x x; print length(x) }'",
                    ],
                    &workspace,
                )
                .with_timeout(std::time::Duration::from_secs(15)),
                &NullSink,
            )
            .await
            .unwrap();
        assert!(
            outcome
                .infrastructure_failures
                .iter()
                .any(|failure| { failure.kind == InfrastructureFailureKind::MemoryLimitExceeded }),
            "{outcome:?}"
        );
    }

    #[test]
    fn an_explicitly_denied_variable_is_removed_even_if_allowed() {
        let built = EnvPolicy::inherit_non_secrets()
            .allow_var("MARKER")
            .deny_var("MARKER")
            .build_from(vec![("MARKER".to_string(), "value".to_string())]);
        assert!(built.is_empty(), "{built:?}");
    }

    #[test]
    fn secret_detection_is_case_insensitive() {
        let policy = EnvPolicy::inherit_non_secrets();
        let built = policy.build_from(vec![
            ("my_api_key".to_string(), "0123456789".to_string()),
            ("Service_Token".to_string(), "0123456789".to_string()),
        ]);
        assert!(built.is_empty(), "{built:?}");
    }
}
