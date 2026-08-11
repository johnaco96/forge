//! From what an agent did to what counts as its result.
//!
//! ```text
//! WorkspaceDelta   everything that changed in the workspace
//!       ↓
//! PatchPolicy      what Forge is willing to call a result
//!       ↓
//! CandidatePatch   the change under evaluation, plus what was left out and why
//! ```
//!
//! The layer exists because `git add -A` is a convenient way to collect changes
//! and a terrible definition of an engineering result. A run that swept a
//! `target/` directory into its patch has not produced a 300-file change; it has
//! produced a small change and a mess. Keeping the distinction explicit means
//! the exclusions are recorded evidence rather than a silent filter.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Default ceiling on a single file in a candidate patch.
///
/// Large enough for any plausible source file or fixture, small enough that a
/// stray binary or dataset is caught.
pub const DEFAULT_MAX_FILE_BYTES: u64 = 5 * 1024 * 1024;

/// Path prefixes never eligible for a candidate patch.
///
/// `.forge/` is Forge's own runtime state — ledger, worktrees, run artifacts —
/// and an agent editing it is changing the instruments, not the code. `.git/`
/// should be unreachable through Git's own plumbing, and is listed anyway
/// because the cost of being wrong about that is unbounded.
pub const ALWAYS_EXCLUDED_PREFIXES: &[&str] = &[".forge/", ".git/"];

/// How a path changed relative to the base commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
}

impl ChangeKind {
    /// Parses a `git diff --name-status` letter.
    ///
    /// Renames are read with `--no-renames`, so only A/M/D appear; anything
    /// else is treated as a modification rather than silently dropped.
    pub fn from_status(letter: char) -> Self {
        match letter {
            'A' | 'C' => Self::Added,
            'D' => Self::Deleted,
            _ => Self::Modified,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Added => "added",
            Self::Modified => "modified",
            Self::Deleted => "deleted",
        }
    }
}

impl std::fmt::Display for ChangeKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One changed path.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeltaEntry {
    /// Repository-relative, as Git reports it.
    pub path: String,
    pub change: ChangeKind,
    pub insertions: u64,
    pub deletions: u64,
    /// Git reported no line counts for this file.
    pub is_binary: bool,
    /// Size in the workspace. `None` for deletions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    /// Git's ignore rules matched this untracked path.
    #[serde(default)]
    pub is_ignored: bool,
}

impl DeltaEntry {
    pub fn new(path: impl Into<String>, change: ChangeKind) -> Self {
        Self {
            path: path.into(),
            change,
            insertions: 0,
            deletions: 0,
            is_binary: false,
            size_bytes: None,
            is_ignored: false,
        }
    }

    pub fn lines_changed(&self) -> u64 {
        self.insertions + self.deletions
    }
}

/// Everything that changed in a workspace, before any policy is applied.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceDelta {
    pub entries: Vec<DeltaEntry>,
}

impl WorkspaceDelta {
    pub fn new(entries: Vec<DeltaEntry>) -> Self {
        Self { entries }
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn paths(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|e| e.path.as_str())
    }

    /// Paths matching a change kind.
    pub fn paths_with(&self, change: ChangeKind) -> Vec<String> {
        self.entries
            .iter()
            .filter(|e| e.change == change)
            .map(|e| e.path.clone())
            .collect()
    }
}

/// Why a change was left out of the candidate patch.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "reason")]
pub enum ExclusionReason {
    /// Forge's own runtime state.
    ForgeArtifact,
    /// Git internals.
    GitInternal,
    /// An untracked file excluded by repository `.gitignore` rules.
    GitIgnored,
    /// Larger than the policy allows.
    TooLarge { bytes: u64, limit: u64 },
}

impl ExclusionReason {
    pub fn describe(&self) -> String {
        match self {
            Self::ForgeArtifact => "Forge runtime artifact".to_string(),
            Self::GitInternal => "Git internal".to_string(),
            Self::GitIgnored => "ignored by repository Git rules".to_string(),
            Self::TooLarge { bytes, limit } => {
                format!(
                    "{} exceeds the {} limit",
                    human_bytes(*bytes),
                    human_bytes(*limit)
                )
            }
        }
    }
}

/// A change and the reason it is not part of the result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExcludedEntry {
    pub path: String,
    pub change: ChangeKind,
    #[serde(flatten)]
    pub reason: ExclusionReason,
}

/// A condition worth surfacing about a patch.
///
/// Structured rather than printed, so the ledger can answer "which runs
/// modified protected paths" without grepping terminal output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WarningKind {
    /// A binary file was added or modified.
    BinaryFile,
    /// A file was left out for exceeding the size limit.
    LargeFileExcluded,
    /// Forge runtime state was touched.
    ForgeArtifactExcluded,
    /// Git internals were touched.
    GitInternalExcluded,
    /// Repository ignore rules kept generated output out of the patch.
    GitIgnoredExcluded,
    /// A protected evaluation input was modified.
    ProtectedPathModified,
    /// A protected evaluation input was deleted.
    ProtectedPathDeleted,
    /// A protected evaluation input was added.
    ProtectedPathAdded,
    /// A protected path changed, and the task explicitly permitted it.
    ProtectedPathAllowed,
}

impl WarningKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::BinaryFile => "binary_file",
            Self::LargeFileExcluded => "large_file_excluded",
            Self::ForgeArtifactExcluded => "forge_artifact_excluded",
            Self::GitInternalExcluded => "git_internal_excluded",
            Self::GitIgnoredExcluded => "git_ignored_excluded",
            Self::ProtectedPathModified => "protected_path_modified",
            Self::ProtectedPathDeleted => "protected_path_deleted",
            Self::ProtectedPathAdded => "protected_path_added",
            Self::ProtectedPathAllowed => "protected_path_allowed",
        }
    }

    /// Whether this warning describes tampering with the evaluation itself,
    /// as opposed to untidiness in the patch.
    pub fn concerns_integrity(self) -> bool {
        matches!(
            self,
            Self::ProtectedPathModified | Self::ProtectedPathDeleted | Self::ProtectedPathAdded
        )
    }
}

impl std::fmt::Display for WarningKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PatchWarning {
    pub kind: WarningKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub detail: String,
}

impl PatchWarning {
    pub fn new(kind: WarningKind, path: Option<String>, detail: impl Into<String>) -> Self {
        Self {
            kind,
            path,
            detail: detail.into(),
        }
    }
}

/// What Forge is willing to treat as an engineering result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PatchPolicy {
    /// Files above this size are recorded but left out of the patch.
    pub max_file_bytes: u64,
    /// Path prefixes excluded outright.
    pub excluded_prefixes: Vec<String>,
    /// Exact repository-relative runtime outputs excluded outright.
    pub excluded_paths: Vec<String>,
}

impl Default for PatchPolicy {
    fn default() -> Self {
        Self {
            max_file_bytes: DEFAULT_MAX_FILE_BYTES,
            excluded_prefixes: ALWAYS_EXCLUDED_PREFIXES
                .iter()
                .map(|p| p.to_string())
                .collect(),
            excluded_paths: Vec::new(),
        }
    }
}

impl PatchPolicy {
    pub fn with_max_file_bytes(mut self, bytes: u64) -> Self {
        self.max_file_bytes = bytes;
        self
    }

    pub fn with_excluded_path(mut self, path: impl Into<String>) -> Self {
        self.excluded_paths.push(path.into());
        self
    }

    /// Splits a delta into the candidate patch and what was left out.
    pub fn apply(&self, delta: &WorkspaceDelta) -> CandidatePatch {
        let mut included = Vec::new();
        let mut excluded = Vec::new();
        let mut warnings = Vec::new();

        for entry in &delta.entries {
            if let Some(reason) = self.exclusion_for(entry) {
                warnings.push(warning_for(&reason, entry));
                excluded.push(ExcludedEntry {
                    path: entry.path.clone(),
                    change: entry.change,
                    reason,
                });
                continue;
            }

            if entry.is_binary && entry.change != ChangeKind::Deleted {
                warnings.push(PatchWarning::new(
                    WarningKind::BinaryFile,
                    Some(entry.path.clone()),
                    format!(
                        "binary file {} ({})",
                        entry.change,
                        entry
                            .size_bytes
                            .map(human_bytes)
                            .unwrap_or_else(|| "unknown size".to_string())
                    ),
                ));
            }

            included.push(entry.clone());
        }

        CandidatePatch {
            included,
            excluded,
            warnings,
        }
    }

    fn exclusion_for(&self, entry: &DeltaEntry) -> Option<ExclusionReason> {
        if self.excluded_paths.iter().any(|path| path == &entry.path) {
            return Some(ExclusionReason::ForgeArtifact);
        }
        for prefix in &self.excluded_prefixes {
            if entry.path.starts_with(prefix.as_str()) {
                return Some(if prefix.starts_with(".git/") {
                    ExclusionReason::GitInternal
                } else {
                    ExclusionReason::ForgeArtifact
                });
            }
        }
        if entry.is_ignored {
            return Some(ExclusionReason::GitIgnored);
        }
        match entry.size_bytes {
            Some(bytes) if bytes > self.max_file_bytes => Some(ExclusionReason::TooLarge {
                bytes,
                limit: self.max_file_bytes,
            }),
            _ => None,
        }
    }
}

fn warning_for(reason: &ExclusionReason, entry: &DeltaEntry) -> PatchWarning {
    let kind = match reason {
        ExclusionReason::ForgeArtifact => WarningKind::ForgeArtifactExcluded,
        ExclusionReason::GitInternal => WarningKind::GitInternalExcluded,
        ExclusionReason::GitIgnored => WarningKind::GitIgnoredExcluded,
        ExclusionReason::TooLarge { .. } => WarningKind::LargeFileExcluded,
    };
    PatchWarning::new(kind, Some(entry.path.clone()), reason.describe())
}

/// The change Forge will evaluate, and what it declined to include.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CandidatePatch {
    pub included: Vec<DeltaEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub excluded: Vec<ExcludedEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<PatchWarning>,
}

impl CandidatePatch {
    pub fn is_empty(&self) -> bool {
        self.included.is_empty()
    }

    pub fn files_changed(&self) -> u64 {
        self.included.len() as u64
    }

    pub fn insertions(&self) -> u64 {
        self.included.iter().map(|e| e.insertions).sum()
    }

    pub fn deletions(&self) -> u64 {
        self.included.iter().map(|e| e.deletions).sum()
    }

    pub fn lines_changed(&self) -> u64 {
        self.insertions() + self.deletions()
    }

    pub fn binary_files(&self) -> u64 {
        self.included.iter().filter(|e| e.is_binary).count() as u64
    }

    pub fn paths(&self) -> Vec<String> {
        self.included.iter().map(|e| e.path.clone()).collect()
    }

    /// Excluded paths grouped by whether they existed at the base commit,
    /// which determines how they are removed from the index.
    pub fn excluded_by_change(&self) -> BTreeMap<ChangeKind, Vec<String>> {
        let mut grouped: BTreeMap<ChangeKind, Vec<String>> = BTreeMap::new();
        for entry in &self.excluded {
            grouped
                .entry(entry.change)
                .or_default()
                .push(entry.path.clone());
        }
        grouped
    }
}

/// Renders a byte count for human readers.
pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [(&str, u64); 4] = [
        ("GiB", 1024 * 1024 * 1024),
        ("MiB", 1024 * 1024),
        ("KiB", 1024),
        ("B", 1),
    ];
    for (unit, size) in UNITS {
        if bytes >= size {
            let value = bytes as f64 / size as f64;
            return if unit == "B" {
                format!("{bytes} B")
            } else {
                format!("{value:.1} {unit}")
            };
        }
    }
    "0 B".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(path: &str) -> DeltaEntry {
        DeltaEntry {
            path: path.to_string(),
            change: ChangeKind::Modified,
            insertions: 10,
            deletions: 2,
            is_binary: false,
            size_bytes: Some(400),
            is_ignored: false,
        }
    }

    fn binary(path: &str, bytes: u64) -> DeltaEntry {
        DeltaEntry {
            path: path.to_string(),
            change: ChangeKind::Added,
            insertions: 0,
            deletions: 0,
            is_binary: true,
            size_bytes: Some(bytes),
            is_ignored: false,
        }
    }

    #[test]
    fn a_source_only_change_passes_through_untouched() {
        let delta = WorkspaceDelta::new(vec![source("src/lib.rs"), source("src/store.rs")]);
        let candidate = PatchPolicy::default().apply(&delta);

        assert_eq!(candidate.files_changed(), 2);
        assert_eq!(candidate.insertions(), 20);
        assert_eq!(candidate.deletions(), 4);
        assert!(candidate.excluded.is_empty());
        assert!(candidate.warnings.is_empty());
    }

    #[test]
    fn forge_runtime_state_never_becomes_a_result() {
        // An agent editing the ledger or another run's artifacts is changing
        // the instruments, not the code.
        let delta = WorkspaceDelta::new(vec![
            source("src/lib.rs"),
            source(".forge/forge.db"),
            source(".forge/runs/R-0001/patch.diff"),
            source(".forge/config.toml"),
        ]);
        let candidate = PatchPolicy::default().apply(&delta);

        assert_eq!(candidate.paths(), vec!["src/lib.rs"]);
        assert_eq!(candidate.excluded.len(), 3);
        assert!(
            candidate
                .excluded
                .iter()
                .all(|e| e.reason == ExclusionReason::ForgeArtifact)
        );
        assert_eq!(
            candidate
                .warnings
                .iter()
                .filter(|w| w.kind == WarningKind::ForgeArtifactExcluded)
                .count(),
            3
        );
    }

    #[test]
    fn configured_runtime_output_is_excluded_by_exact_path() {
        let candidate = PatchPolicy::default()
            .with_excluded_path(".forge-metrics.json")
            .apply(&WorkspaceDelta::new(vec![
                source(".forge-metrics.json"),
                source(".forge-metrics.json.example"),
            ]));
        assert_eq!(candidate.paths(), vec![".forge-metrics.json.example"]);
        assert_eq!(candidate.excluded[0].reason, ExclusionReason::ForgeArtifact);
    }

    #[test]
    fn git_internals_are_never_patch_content() {
        let delta = WorkspaceDelta::new(vec![source(".git/config"), source("src/lib.rs")]);
        let candidate = PatchPolicy::default().apply(&delta);

        assert_eq!(candidate.paths(), vec!["src/lib.rs"]);
        assert_eq!(candidate.excluded[0].reason, ExclusionReason::GitInternal);
    }

    #[test]
    fn very_large_files_are_excluded_with_their_size_recorded() {
        let policy = PatchPolicy::default().with_max_file_bytes(1024);
        let mut huge = source("data/dump.sql");
        huge.size_bytes = Some(50 * 1024);

        let candidate = policy.apply(&WorkspaceDelta::new(vec![source("src/lib.rs"), huge]));

        assert_eq!(candidate.paths(), vec!["src/lib.rs"]);
        assert_eq!(
            candidate.excluded[0].reason,
            ExclusionReason::TooLarge {
                bytes: 51_200,
                limit: 1024
            }
        );
        let warning = &candidate.warnings[0];
        assert_eq!(warning.kind, WarningKind::LargeFileExcluded);
        assert!(warning.detail.contains("50.0 KiB"), "{}", warning.detail);
    }

    #[test]
    fn gitignored_build_output_is_recorded_but_not_a_candidate_change() {
        let mut ignored = binary("target/debug/app", 2048);
        ignored.is_ignored = true;
        let candidate =
            PatchPolicy::default().apply(&WorkspaceDelta::new(vec![source("src/lib.rs"), ignored]));

        assert_eq!(candidate.paths(), vec!["src/lib.rs"]);
        assert_eq!(candidate.excluded[0].reason, ExclusionReason::GitIgnored);
        assert_eq!(candidate.warnings[0].kind, WarningKind::GitIgnoredExcluded);
    }

    #[test]
    fn binary_files_are_kept_but_flagged() {
        // A legitimate change may add an image; it is still worth noticing.
        let delta =
            WorkspaceDelta::new(vec![source("src/lib.rs"), binary("assets/logo.png", 2048)]);
        let candidate = PatchPolicy::default().apply(&delta);

        assert_eq!(candidate.files_changed(), 2);
        assert_eq!(candidate.binary_files(), 1);
        assert!(candidate.excluded.is_empty());

        let warning = &candidate.warnings[0];
        assert_eq!(warning.kind, WarningKind::BinaryFile);
        assert_eq!(warning.path.as_deref(), Some("assets/logo.png"));
    }

    #[test]
    fn a_deleted_binary_file_is_not_warned_about() {
        // Removing a binary is tidy, not suspicious.
        let mut deleted = binary("assets/old.png", 0);
        deleted.change = ChangeKind::Deleted;
        deleted.size_bytes = None;

        let candidate = PatchPolicy::default().apply(&WorkspaceDelta::new(vec![deleted]));
        assert!(candidate.warnings.is_empty());
        assert_eq!(candidate.files_changed(), 1);
    }

    #[test]
    fn exclusions_are_grouped_by_how_they_must_be_unstaged() {
        let mut added = source(".forge/new.txt");
        added.change = ChangeKind::Added;
        let mut modified = source(".forge/config.toml");
        modified.change = ChangeKind::Modified;

        let candidate = PatchPolicy::default().apply(&WorkspaceDelta::new(vec![added, modified]));
        let grouped = candidate.excluded_by_change();

        assert_eq!(grouped[&ChangeKind::Added], vec![".forge/new.txt"]);
        assert_eq!(grouped[&ChangeKind::Modified], vec![".forge/config.toml"]);
    }

    #[test]
    fn status_letters_map_to_change_kinds() {
        assert_eq!(ChangeKind::from_status('A'), ChangeKind::Added);
        assert_eq!(ChangeKind::from_status('M'), ChangeKind::Modified);
        assert_eq!(ChangeKind::from_status('D'), ChangeKind::Deleted);
        // Anything unexpected is a modification rather than a dropped change.
        assert_eq!(ChangeKind::from_status('T'), ChangeKind::Modified);
    }

    #[test]
    fn byte_counts_read_naturally() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(2048), "2.0 KiB");
        assert_eq!(human_bytes(5 * 1024 * 1024), "5.0 MiB");
        assert_eq!(human_bytes(0), "0 B");
    }

    #[test]
    fn candidate_patches_round_trip() {
        let delta = WorkspaceDelta::new(vec![source("src/lib.rs"), source(".forge/x")]);
        let candidate = PatchPolicy::default().apply(&delta);
        let json = serde_json::to_string(&candidate).unwrap();
        let back: CandidatePatch = serde_json::from_str(&json).unwrap();
        assert_eq!(back, candidate);
    }
}
