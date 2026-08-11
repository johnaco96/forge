//! Identifier newtypes.
//!
//! Identifiers are used as filesystem path segments (worktree directories,
//! artifact folders) and as Git branch name components, so they are validated
//! on construction. An identifier that could escape a directory — `..`, an
//! absolute path, a separator — is rejected rather than sanitized, because
//! silently rewriting an id would break the link between a stored record and
//! the directory it names.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// Upper bound on identifier length, chosen to stay well inside filesystem and
/// Git ref limits even when combined with a prefix and suffix.
pub const MAX_ID_LEN: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IdError {
    #[error("identifier must not be empty")]
    Empty,
    #[error("identifier `{0}` is longer than {MAX_ID_LEN} characters")]
    TooLong(String),
    #[error(
        "identifier `{0}` contains characters outside [A-Za-z0-9_-]; \
         identifiers are used as path and branch segments"
    )]
    InvalidCharacters(String),
}

/// Validates that `raw` is safe to use as a path segment and Git ref component.
pub fn validate_id(raw: &str) -> Result<(), IdError> {
    if raw.is_empty() {
        return Err(IdError::Empty);
    }
    if raw.len() > MAX_ID_LEN {
        return Err(IdError::TooLong(raw.to_string()));
    }
    let ok = raw
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if !ok {
        return Err(IdError::InvalidCharacters(raw.to_string()));
    }
    Ok(())
}

macro_rules! define_id {
    ($(#[$meta:meta])* $name:ident, $prefix:literal) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Conventional prefix used by [`Self::sequential`].
            pub const PREFIX: &'static str = $prefix;

            /// Builds a validated identifier from an arbitrary string.
            pub fn new(raw: impl Into<String>) -> Result<Self, IdError> {
                let raw = raw.into();
                validate_id(&raw)?;
                Ok(Self(raw))
            }

            /// Builds the conventional zero-padded identifier for `n`,
            /// e.g. `R-0001`.
            pub fn sequential(n: u64) -> Self {
                Self(format!("{}-{:04}", $prefix, n))
            }

            /// Returns the sequence number if this id has the conventional
            /// `PREFIX-<number>` shape.
            pub fn sequence(&self) -> Option<u64> {
                self.0
                    .strip_prefix(concat!($prefix, "-"))
                    .and_then(|rest| rest.parse().ok())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }

            pub fn into_string(self) -> String {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl FromStr for $name {
            type Err = IdError;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Self::new(s)
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                let raw = String::deserialize(deserializer)?;
                Self::new(raw).map_err(serde::de::Error::custom)
            }
        }
    };
}

define_id!(
    /// Identifies an [`EngineeringTask`](crate::EngineeringTask), e.g. `T-1042`.
    TaskId,
    "T"
);
define_id!(
    /// Identifies one [`AgentRun`](crate::AgentRun), e.g. `R-8821`.
    RunId,
    "R"
);
define_id!(
    /// Identifies a competitive experiment across several runs, e.g. `E-0002`.
    ExperimentId,
    "E"
);
define_id!(
    /// Identifies one persisted automatic-routing decision.
    RoutingDecisionId,
    "RD"
);
define_id!(
    /// Identifies one multi-agent orchestration, e.g. `TE-0001`.
    TeamExecutionId,
    "TE"
);
define_id!(
    /// Identifies one node in a validated team plan.
    TeamNodeId,
    "TN"
);
define_id!(
    /// Identifies one immutable team handoff artifact.
    TeamArtifactId,
    "TA"
);
define_id!(
    /// Identifies an agent as Forge knows it, e.g. `claude` or `codex`.
    AgentId,
    "A"
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sequential_ids_use_the_documented_shape() {
        assert_eq!(RunId::sequential(1).as_str(), "R-0001");
        assert_eq!(TaskId::sequential(1042).as_str(), "T-1042");
        assert_eq!(ExperimentId::sequential(2).as_str(), "E-0002");
        assert_eq!(RoutingDecisionId::sequential(3).as_str(), "RD-0003");
        assert_eq!(TeamExecutionId::sequential(4).as_str(), "TE-0004");
        assert_eq!(TeamNodeId::sequential(5).as_str(), "TN-0005");
        assert_eq!(TeamArtifactId::sequential(6).as_str(), "TA-0006");
    }

    #[test]
    fn sequence_round_trips() {
        assert_eq!(RunId::sequential(8821).sequence(), Some(8821));
        assert_eq!(RunId::new("custom-run").unwrap().sequence(), None);
    }

    #[test]
    fn path_traversal_is_rejected() {
        // Ids become directory names under the worktree root; anything that
        // could climb out of it must not construct.
        for hostile in ["..", "../evil", "/etc/passwd", "a/b", "a\\b", ".", "~"] {
            assert!(
                RunId::new(hostile).is_err(),
                "expected `{hostile}` to be rejected"
            );
        }
    }

    #[test]
    fn empty_and_overlong_ids_are_rejected() {
        assert_eq!(RunId::new(""), Err(IdError::Empty));
        let long = "a".repeat(MAX_ID_LEN + 1);
        assert!(matches!(RunId::new(long), Err(IdError::TooLong(_))));
    }

    #[test]
    fn deserialization_enforces_validation() {
        let err = serde_json::from_str::<RunId>("\"../escape\"").unwrap_err();
        assert!(err.to_string().contains("outside"), "{err}");
    }

    #[test]
    fn ids_serialize_transparently() {
        let id = RunId::sequential(7);
        assert_eq!(serde_json::to_string(&id).unwrap(), "\"R-0007\"");
    }
}
