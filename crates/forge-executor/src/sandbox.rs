//! Environment isolation and secret filtering.
//!
//! Coding agents run arbitrary shell commands, and so do the evaluation
//! commands a repository declares. Neither should inherit the operator's whole
//! environment by default: a leaked credential in a captured log is permanent,
//! because Forge's entire value proposition is that it keeps run records
//! forever.
//!
//! This is a policy layer, not a sandbox. It does not contain a hostile
//! process — that is what containers are for, later. It reduces incidental
//! exposure today.

use std::collections::BTreeMap;

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

#[cfg(test)]
mod tests {
    use super::*;

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
