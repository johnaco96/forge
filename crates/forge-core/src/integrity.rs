//! Whether the evaluation itself is still trustworthy.
//!
//! Forge's evaluation specification is trusted; the agent's workspace is not.
//! The gap between them is that the *inputs* to an evaluation — test files,
//! benchmark harnesses, fixtures — live in the workspace, where the agent can
//! reach them. An agent that deletes a failing test and then reports passing
//! tests is telling the truth about a measurement that no longer means
//! anything.
//!
//! This module answers one question against the run's base commit: did the
//! things that define this evaluation change?
//!
//! It deliberately does not guess which paths those are. Repositories organize
//! validation differently, and a hard-coded `tests/` would be wrong more often
//! than right; the protected set is declared per repository or per task.

use serde::{Deserialize, Serialize};

use crate::patch::{ChangeKind, PatchWarning, WarningKind, WorkspaceDelta};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProtectionError {
    #[error("protected path pattern `{pattern}` is invalid: {reason}")]
    InvalidPattern { pattern: String, reason: String },
}

/// The state of a run's evaluation inputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntegrityStatus {
    /// No protected path changed, or every change to one was explicitly
    /// permitted by the task.
    Clean,
    /// A protected path was changed or added.
    Modified,
    /// A protected path was deleted. Strictly worse than modification: the
    /// evidence is not merely altered, it is gone.
    Missing,
}

impl IntegrityStatus {
    /// Whether a normal pass is still available.
    pub fn is_acceptable(self) -> bool {
        self == Self::Clean
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Clean => "clean",
            Self::Modified => "modified",
            Self::Missing => "missing",
        }
    }

    pub fn describe(self) -> &'static str {
        match self {
            Self::Clean => "clean",
            Self::Modified => "MODIFIED",
            Self::Missing => "MISSING",
        }
    }
}

impl std::fmt::Display for IntegrityStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Which paths a run is not supposed to touch, and which exceptions the task
/// grants.
///
/// Patterns are glob expressions matched against repository-relative paths, as
/// Git reports them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtectionPolicy {
    /// Paths that define the evaluation.
    #[serde(
        default,
        rename = "protected_paths",
        alias = "protected",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub protected: Vec<String>,
    /// Paths the task explicitly permits changing even though they are
    /// protected. A task whose whole purpose is to add tests needs this.
    #[serde(
        default,
        rename = "allowed_protected_paths",
        alias = "allowed",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub allowed: Vec<String>,
}

impl ProtectionPolicy {
    pub fn new(protected: Vec<String>, allowed: Vec<String>) -> Self {
        Self { protected, allowed }
    }

    pub fn is_empty(&self) -> bool {
        self.protected.is_empty()
    }

    /// Rejects patterns that could match outside the repository.
    ///
    /// Git reports repository-relative paths, so an absolute or parent-relative
    /// pattern can never legitimately match. Such a pattern is a mistake or an
    /// attempt to make protection vacuous, and either way must not be accepted
    /// silently.
    pub fn validate(&self) -> Result<(), ProtectionError> {
        for pattern in self.protected.iter().chain(self.allowed.iter()) {
            let invalid = |reason: &str| ProtectionError::InvalidPattern {
                pattern: pattern.clone(),
                reason: reason.to_string(),
            };
            if pattern.trim().is_empty() {
                return Err(invalid("pattern is empty"));
            }
            let normalized = pattern.replace('\\', "/");
            if normalized.starts_with('/')
                || normalized
                    .as_bytes()
                    .get(1)
                    .is_some_and(|byte| *byte == b':')
            {
                return Err(invalid(
                    "pattern is absolute; patterns are relative to the repository root",
                ));
            }
            if pattern.starts_with("~") {
                return Err(invalid("pattern refers to a home directory"));
            }
            if normalized
                .split('/')
                .any(|segment| segment == ".." || segment == "...")
            {
                return Err(invalid(
                    "pattern escapes the repository root with a parent reference",
                ));
            }
        }
        Ok(())
    }

    /// Compiles the policy for matching.
    pub fn compile(&self) -> Result<CompiledProtection, ProtectionError> {
        self.validate()?;
        Ok(CompiledProtection {
            protected: build_set(&self.protected)?,
            allowed: build_set(&self.allowed)?,
        })
    }

    /// Evaluates a workspace delta against this policy.
    pub fn check(&self, delta: &WorkspaceDelta) -> Result<EvaluationIntegrity, ProtectionError> {
        Ok(self.compile()?.check(delta))
    }
}

fn build_set(patterns: &[String]) -> Result<globset::GlobSet, ProtectionError> {
    let mut builder = globset::GlobSetBuilder::new();
    for pattern in patterns {
        let glob =
            globset::Glob::new(pattern).map_err(|source| ProtectionError::InvalidPattern {
                pattern: pattern.clone(),
                reason: source.to_string(),
            })?;
        builder.add(glob);
    }
    builder
        .build()
        .map_err(|source| ProtectionError::InvalidPattern {
            pattern: patterns.join(", "),
            reason: source.to_string(),
        })
}

/// A protection policy ready to match paths.
#[derive(Debug, Clone)]
pub struct CompiledProtection {
    protected: globset::GlobSet,
    allowed: globset::GlobSet,
}

impl CompiledProtection {
    pub fn is_protected(&self, path: &str) -> bool {
        self.protected.is_match(path)
    }

    pub fn is_allowed(&self, path: &str) -> bool {
        self.allowed.is_match(path)
    }

    /// Classifies every change against the protected set.
    pub fn check(&self, delta: &WorkspaceDelta) -> EvaluationIntegrity {
        let mut integrity = EvaluationIntegrity::default();

        for entry in &delta.entries {
            if !self.is_protected(&entry.path) {
                continue;
            }
            if self.is_allowed(&entry.path) {
                integrity.allowed.push(entry.path.clone());
                continue;
            }
            match entry.change {
                ChangeKind::Added => integrity.added.push(entry.path.clone()),
                ChangeKind::Modified => integrity.modified.push(entry.path.clone()),
                ChangeKind::Deleted => integrity.deleted.push(entry.path.clone()),
            }
        }

        integrity.status = if !integrity.deleted.is_empty() {
            IntegrityStatus::Missing
        } else if !integrity.modified.is_empty() || !integrity.added.is_empty() {
            IntegrityStatus::Modified
        } else {
            IntegrityStatus::Clean
        };

        integrity
    }
}

/// What happened to a run's evaluation inputs, relative to its base commit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvaluationIntegrity {
    pub status: IntegrityStatus,
    /// Protected paths changed without permission.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub modified: Vec<String>,
    /// Protected paths added without permission.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub added: Vec<String>,
    /// Protected paths deleted without permission.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deleted: Vec<String>,
    /// Protected paths the task explicitly permitted changing. Recorded even
    /// though they do not affect the status, because "the task allowed this"
    /// is exactly the sort of claim a reader will want to check later.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed: Vec<String>,
}

impl Default for EvaluationIntegrity {
    fn default() -> Self {
        Self {
            status: IntegrityStatus::Clean,
            modified: Vec::new(),
            added: Vec::new(),
            deleted: Vec::new(),
            allowed: Vec::new(),
        }
    }
}

impl EvaluationIntegrity {
    /// The state of a run with no protected paths declared.
    ///
    /// Clean, but only because nothing was being watched — which is worth
    /// distinguishing from "we checked and nothing changed".
    pub fn unchecked() -> Self {
        Self::default()
    }

    pub fn is_acceptable(&self) -> bool {
        self.status.is_acceptable()
    }

    /// Every protected path that changed without permission.
    pub fn violations(&self) -> Vec<&str> {
        self.deleted
            .iter()
            .chain(self.modified.iter())
            .chain(self.added.iter())
            .map(String::as_str)
            .collect()
    }

    /// Structured warnings for the run record.
    pub fn warnings(&self) -> Vec<PatchWarning> {
        let mut warnings = Vec::new();
        for path in &self.deleted {
            warnings.push(PatchWarning::new(
                WarningKind::ProtectedPathDeleted,
                Some(path.clone()),
                "protected evaluation input was deleted",
            ));
        }
        for path in &self.modified {
            warnings.push(PatchWarning::new(
                WarningKind::ProtectedPathModified,
                Some(path.clone()),
                "protected evaluation input was modified",
            ));
        }
        for path in &self.added {
            warnings.push(PatchWarning::new(
                WarningKind::ProtectedPathAdded,
                Some(path.clone()),
                "protected evaluation input was added",
            ));
        }
        for path in &self.allowed {
            warnings.push(PatchWarning::new(
                WarningKind::ProtectedPathAllowed,
                Some(path.clone()),
                "protected path changed, permitted by the task",
            ));
        }
        warnings
    }

    /// A one-line explanation for the run report.
    pub fn summary(&self) -> String {
        match self.status {
            IntegrityStatus::Clean if self.allowed.is_empty() => "clean".to_string(),
            IntegrityStatus::Clean => format!(
                "clean ({} permitted change{})",
                self.allowed.len(),
                if self.allowed.len() == 1 { "" } else { "s" }
            ),
            IntegrityStatus::Modified => format!(
                "MODIFIED — {} protected path{} changed",
                self.modified.len() + self.added.len(),
                if self.modified.len() + self.added.len() == 1 {
                    ""
                } else {
                    "s"
                }
            ),
            IntegrityStatus::Missing => format!(
                "MISSING — {} protected path{} deleted",
                self.deleted.len(),
                if self.deleted.len() == 1 { "" } else { "s" }
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::patch::DeltaEntry;

    fn delta(changes: &[(&str, ChangeKind)]) -> WorkspaceDelta {
        WorkspaceDelta::new(
            changes
                .iter()
                .map(|(path, change)| DeltaEntry::new(*path, *change))
                .collect(),
        )
    }

    fn policy() -> ProtectionPolicy {
        ProtectionPolicy::new(vec!["tests/**".into(), "benches/**".into()], vec![])
    }

    #[test]
    fn a_source_only_change_leaves_the_evaluation_clean() {
        let integrity = policy()
            .check(&delta(&[
                ("src/lib.rs", ChangeKind::Modified),
                ("src/store.rs", ChangeKind::Added),
            ]))
            .unwrap();

        assert_eq!(integrity.status, IntegrityStatus::Clean);
        assert!(integrity.is_acceptable());
        assert!(integrity.violations().is_empty());
        assert_eq!(integrity.summary(), "clean");
    }

    #[test]
    fn deleting_a_protected_test_reports_missing() {
        let integrity = policy()
            .check(&delta(&[
                ("src/lib.rs", ChangeKind::Modified),
                ("tests/median.rs", ChangeKind::Deleted),
            ]))
            .unwrap();

        assert_eq!(integrity.status, IntegrityStatus::Missing);
        assert!(!integrity.is_acceptable());
        assert_eq!(integrity.deleted, vec!["tests/median.rs"]);
        assert!(integrity.summary().contains("MISSING"));
    }

    #[test]
    fn rewriting_a_protected_test_reports_modified() {
        let integrity = policy()
            .check(&delta(&[("tests/median.rs", ChangeKind::Modified)]))
            .unwrap();

        assert_eq!(integrity.status, IntegrityStatus::Modified);
        assert!(!integrity.is_acceptable());
        assert_eq!(integrity.modified, vec!["tests/median.rs"]);
    }

    #[test]
    fn adding_a_protected_file_is_also_a_modification() {
        // Adding a trivially-passing test alongside a failing one is the same
        // manipulation as editing it.
        let integrity = policy()
            .check(&delta(&[("tests/always_passes.rs", ChangeKind::Added)]))
            .unwrap();

        assert_eq!(integrity.status, IntegrityStatus::Modified);
        assert_eq!(integrity.added, vec!["tests/always_passes.rs"]);
    }

    #[test]
    fn deletion_outranks_modification() {
        let integrity = policy()
            .check(&delta(&[
                ("tests/a.rs", ChangeKind::Modified),
                ("tests/b.rs", ChangeKind::Deleted),
            ]))
            .unwrap();
        assert_eq!(integrity.status, IntegrityStatus::Missing);
        assert_eq!(integrity.violations().len(), 2);
    }

    #[test]
    fn a_task_can_explicitly_permit_changing_a_protected_path() {
        // A task whose purpose is to add coverage must be able to say so.
        let policy =
            ProtectionPolicy::new(vec!["tests/**".into()], vec!["tests/new_feature.rs".into()]);
        let integrity = policy
            .check(&delta(&[
                ("src/lib.rs", ChangeKind::Modified),
                ("tests/new_feature.rs", ChangeKind::Added),
            ]))
            .unwrap();

        assert_eq!(integrity.status, IntegrityStatus::Clean);
        assert!(integrity.is_acceptable());
        assert_eq!(integrity.allowed, vec!["tests/new_feature.rs"]);
        assert!(integrity.summary().contains("1 permitted change"));
    }

    #[test]
    fn permission_is_per_path_not_blanket() {
        let policy =
            ProtectionPolicy::new(vec!["tests/**".into()], vec!["tests/new_feature.rs".into()]);
        let integrity = policy
            .check(&delta(&[
                ("tests/new_feature.rs", ChangeKind::Added),
                ("tests/median.rs", ChangeKind::Deleted),
            ]))
            .unwrap();

        assert_eq!(integrity.status, IntegrityStatus::Missing);
        assert_eq!(integrity.allowed, vec!["tests/new_feature.rs"]);
        assert_eq!(integrity.deleted, vec!["tests/median.rs"]);
    }

    #[test]
    fn nothing_is_protected_unless_declared() {
        // `tests/` is not universally meaningful; repositories organize
        // validation differently and Forge must not assume.
        let integrity = ProtectionPolicy::default()
            .check(&delta(&[("tests/median.rs", ChangeKind::Deleted)]))
            .unwrap();
        assert_eq!(integrity.status, IntegrityStatus::Clean);
    }

    #[test]
    fn glob_patterns_match_nested_paths() {
        let policy = ProtectionPolicy::new(vec!["tests/**".into()], vec![]);
        let compiled = policy.compile().unwrap();
        assert!(compiled.is_protected("tests/median.rs"));
        assert!(compiled.is_protected("tests/deep/nested/case.rs"));
        assert!(!compiled.is_protected("src/tests.rs"));
        assert!(!compiled.is_protected("crates/x/tests/a.rs"));

        let recursive = ProtectionPolicy::new(vec!["**/tests/**".into()], vec![]);
        let compiled = recursive.compile().unwrap();
        assert!(compiled.is_protected("crates/x/tests/a.rs"));
    }

    #[test]
    fn suffix_patterns_work_for_repositories_that_colocate_tests() {
        let policy = ProtectionPolicy::new(vec!["**/*_test.go".into()], vec![]);
        let compiled = policy.compile().unwrap();
        assert!(compiled.is_protected("internal/store/store_test.go"));
        assert!(!compiled.is_protected("internal/store/store.go"));
    }

    /// Patterns are matched against repository-relative paths, so a pattern
    /// that reaches outside cannot ever match legitimately. Accepting one
    /// silently would make protection look configured while doing nothing.
    #[test]
    fn patterns_cannot_escape_the_repository_root() {
        for hostile in [
            "/etc/**",
            "../outside/**",
            "tests/../../escape/**",
            "~/secrets/**",
            "..\\outside\\**",
            "C:\\Windows\\**",
            "   ",
        ] {
            let policy = ProtectionPolicy::new(vec![hostile.to_string()], vec![]);
            let err = policy.validate().unwrap_err();
            assert!(
                matches!(err, ProtectionError::InvalidPattern { .. }),
                "accepted `{hostile}`"
            );
            assert!(policy.compile().is_err(), "compiled `{hostile}`");
        }
    }

    #[test]
    fn an_escaping_allow_pattern_is_rejected_too() {
        // Otherwise an allow-list entry could be used to neuter protection.
        let policy = ProtectionPolicy::new(vec!["tests/**".into()], vec!["../**".into()]);
        assert!(policy.validate().is_err());
    }

    #[test]
    fn malformed_globs_are_reported_rather_than_ignored() {
        let policy = ProtectionPolicy::new(vec!["tests/[".into()], vec![]);
        assert!(policy.compile().is_err());
    }

    #[test]
    fn warnings_name_every_violated_path() {
        let integrity = policy()
            .check(&delta(&[
                ("tests/a.rs", ChangeKind::Deleted),
                ("benches/b.rs", ChangeKind::Modified),
            ]))
            .unwrap();

        let warnings = integrity.warnings();
        assert_eq!(warnings.len(), 2);
        assert!(warnings.iter().all(|w| w.kind.concerns_integrity()));
        assert!(
            warnings
                .iter()
                .any(|w| w.kind == WarningKind::ProtectedPathDeleted
                    && w.path.as_deref() == Some("tests/a.rs"))
        );
        assert!(
            warnings
                .iter()
                .any(|w| w.kind == WarningKind::ProtectedPathModified
                    && w.path.as_deref() == Some("benches/b.rs"))
        );
    }

    #[test]
    fn a_permitted_change_is_still_recorded_as_a_warning() {
        let policy = ProtectionPolicy::new(vec!["tests/**".into()], vec!["tests/new.rs".into()]);
        let integrity = policy
            .check(&delta(&[("tests/new.rs", ChangeKind::Added)]))
            .unwrap();

        let warnings = integrity.warnings();
        assert_eq!(warnings[0].kind, WarningKind::ProtectedPathAllowed);
        // Permitted changes do not count as integrity violations.
        assert!(!warnings[0].kind.concerns_integrity());
    }

    #[test]
    fn integrity_round_trips() {
        let integrity = policy()
            .check(&delta(&[("tests/a.rs", ChangeKind::Deleted)]))
            .unwrap();
        let json = serde_json::to_string(&integrity).unwrap();
        let back: EvaluationIntegrity = serde_json::from_str(&json).unwrap();
        assert_eq!(back, integrity);
    }
}
