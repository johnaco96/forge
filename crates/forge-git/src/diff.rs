//! Extracting what an agent actually changed.
//!
//! The patch is the agent's real output. Everything else it says about its work
//! is a claim; the diff is evidence.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read;
use std::path::{Component, Path};

use forge_core::patch::{CandidatePatch, ChangeKind, DeltaEntry, PatchPolicy, WorkspaceDelta};

use crate::error::GitResult;
use crate::repository::run_git;

/// Summary of a diff.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DiffStat {
    pub files_changed: u64,
    pub insertions: u64,
    pub deletions: u64,
    /// Binary files report no line counts, so they are tracked separately
    /// rather than silently counted as zero-line changes.
    pub binary_files: u64,
}

impl DiffStat {
    pub fn is_empty(&self) -> bool {
        self.files_changed == 0
    }

    pub fn lines_changed(&self) -> u64 {
        self.insertions + self.deletions
    }
}

/// Parses `git diff --numstat` output.
pub fn parse_numstat(output: &str) -> DiffStat {
    let mut stat = DiffStat::default();
    for line in output.lines().filter(|l| !l.trim().is_empty()) {
        let mut fields = line.split('\t');
        let (Some(added), Some(removed)) = (fields.next(), fields.next()) else {
            continue;
        };
        stat.files_changed += 1;
        match (added.parse::<u64>(), removed.parse::<u64>()) {
            (Ok(a), Ok(r)) => {
                stat.insertions += a;
                stat.deletions += r;
            }
            // "-\t-\tpath" marks a binary file.
            _ => stat.binary_files += 1,
        }
    }
    stat
}

/// Diff statistics between two revisions.
pub fn stat_between(dir: impl AsRef<Path>, base: &str, head: &str) -> GitResult<DiffStat> {
    let output = run_git(
        dir,
        [
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--numstat",
            base,
            head,
        ],
    )?;
    Ok(parse_numstat(&output))
}

/// Unified diff between two revisions.
pub fn patch_between(dir: impl AsRef<Path>, base: &str, head: &str) -> GitResult<String> {
    run_git(dir, ["diff", "--no-ext-diff", "--no-textconv", base, head])
}

/// Everything a workspace changed relative to `base`, committed or not.
///
/// Agents differ in whether they commit their work, so the whole worktree is
/// staged first and the diff taken against the base commit. That makes the
/// captured patch identical whether the agent committed, left changes staged,
/// or left them loose — including files it created.
///
/// This stages changes in the workspace's index. The workspace is disposable
/// and belongs to a single run, so that side effect is contained.
pub fn capture_workspace_patch(
    workspace: impl AsRef<Path>,
    base: &str,
) -> GitResult<(DiffStat, String)> {
    let workspace = workspace.as_ref();
    let delta = workspace_delta(workspace, base)?;
    let candidate = PatchPolicy::default().apply(&delta);
    stage_candidate_patch(workspace, base, &candidate)?;
    let stat = DiffStat {
        files_changed: candidate.files_changed(),
        insertions: candidate.insertions(),
        deletions: candidate.deletions(),
        binary_files: candidate.binary_files(),
    };
    let patch = cached_patch(workspace, base)?;
    Ok((stat, patch))
}

/// Reads every workspace change relative to the recorded base commit.
///
/// Tracked and untracked non-ignored changes are staged temporarily so agent
/// commits, loose edits, and new files have one representation. Ignored files
/// are collected separately as policy evidence; they are never staged.
pub fn workspace_delta(workspace: impl AsRef<Path>, base: &str) -> GitResult<WorkspaceDelta> {
    let workspace = workspace.as_ref();
    add_all_without_external_filters(workspace)?;

    let statuses = run_git(
        workspace,
        [
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--cached",
            "--name-status",
            "-z",
            "--no-renames",
            base,
        ],
    )?;
    let numstat = run_git(
        workspace,
        [
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--cached",
            "--numstat",
            "-z",
            base,
        ],
    )?;
    let counts = parse_numstat_entries(&numstat)?;

    let mut entries = Vec::new();
    let mut fields = statuses.split_terminator('\0');
    while let Some(status) = fields.next() {
        let Some(path) = fields.next() else {
            break;
        };
        validate_git_path(path)?;
        let change = ChangeKind::from_status(status.chars().next().unwrap_or('M'));
        let (insertions, deletions, is_binary) = counts.get(path).copied().unwrap_or_default();
        entries.push(DeltaEntry {
            path: path.to_string(),
            change,
            insertions,
            deletions,
            is_binary,
            size_bytes: file_size(workspace, path, change),
            is_ignored: false,
        });
    }

    let ignored = run_git(
        workspace,
        [
            "ls-files",
            "--others",
            "--ignored",
            "--exclude-standard",
            "-z",
        ],
    )?;
    for path in ignored.split_terminator('\0') {
        validate_git_path(path)?;
        entries.push(DeltaEntry {
            path: path.to_string(),
            change: ChangeKind::Added,
            insertions: 0,
            deletions: 0,
            is_binary: false,
            size_bytes: file_size(workspace, path, ChangeKind::Added),
            is_ignored: true,
        });
    }

    entries.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(WorkspaceDelta::new(entries))
}

/// Returns whether any changed workspace path or content contains an exact
/// invocation secret.
///
/// This runs before staging or durable patch capture. It includes tracked,
/// untracked, and ignored paths, does not execute repository-defined filters,
/// and never returns the matching value or path. Callers can therefore destroy
/// a contaminated disposable workspace without putting the credential in a
/// branch, patch, event, or diagnostic.
pub fn workspace_contains_secret(
    workspace: impl AsRef<Path>,
    base: &str,
    secrets: &[String],
) -> GitResult<bool> {
    let secrets = secrets
        .iter()
        .map(String::as_bytes)
        .filter(|secret| !secret.is_empty())
        .collect::<Vec<_>>();
    if secrets.is_empty() {
        return Ok(false);
    }

    let workspace = workspace.as_ref();
    let mut paths = BTreeSet::new();
    for listed in [
        run_git(
            workspace,
            [
                "diff",
                "--no-ext-diff",
                "--no-textconv",
                "--name-only",
                "-z",
                base,
                "--",
            ],
        )?,
        run_git(
            workspace,
            ["ls-files", "--others", "--exclude-standard", "-z", "--"],
        )?,
        run_git(
            workspace,
            [
                "ls-files",
                "--others",
                "--ignored",
                "--exclude-standard",
                "-z",
                "--",
            ],
        )?,
    ] {
        for path in listed.split_terminator('\0') {
            validate_git_path(path)?;
            if contains_any_secret(path.as_bytes(), &secrets) {
                return Ok(true);
            }
            paths.insert(path.to_string());
        }
    }

    for path in paths {
        if path_contains_secret(&workspace.join(path), &secrets)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn path_contains_secret(path: &Path, secrets: &[&[u8]]) -> GitResult<bool> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(source) => {
            return Err(crate::error::GitError::Io {
                context: "inspecting a candidate path for credential contamination".into(),
                source,
            });
        }
    };

    if metadata.file_type().is_symlink() {
        let target = fs::read_link(path).map_err(|source| crate::error::GitError::Io {
            context: "reading a candidate symlink for credential contamination".into(),
            source,
        })?;
        return Ok(contains_any_secret(
            target.to_string_lossy().as_bytes(),
            secrets,
        ));
    }
    if metadata.is_dir() {
        let entries = fs::read_dir(path).map_err(|source| crate::error::GitError::Io {
            context: "reading a candidate directory for credential contamination".into(),
            source,
        })?;
        for entry in entries {
            let entry = entry.map_err(|source| crate::error::GitError::Io {
                context: "reading a candidate directory entry for credential contamination".into(),
                source,
            })?;
            if contains_any_secret(entry.file_name().to_string_lossy().as_bytes(), secrets)
                || path_contains_secret(&entry.path(), secrets)?
            {
                return Ok(true);
            }
        }
        return Ok(false);
    }
    if !metadata.is_file() {
        return Ok(false);
    }

    let mut file = fs::File::open(path).map_err(|source| crate::error::GitError::Io {
        context: "opening a candidate file for credential contamination".into(),
        source,
    })?;
    let overlap = secrets
        .iter()
        .map(|secret| secret.len().saturating_sub(1))
        .max()
        .unwrap_or(0);
    let mut carry = Vec::new();
    let mut chunk = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut chunk)
            .map_err(|source| crate::error::GitError::Io {
                context: "reading a candidate file for credential contamination".into(),
                source,
            })?;
        if read == 0 {
            break;
        }
        carry.extend_from_slice(&chunk[..read]);
        if contains_any_secret(&carry, secrets) {
            return Ok(true);
        }
        if carry.len() > overlap {
            carry.drain(..carry.len() - overlap);
        }
    }
    Ok(false)
}

fn contains_any_secret(haystack: &[u8], secrets: &[&[u8]]) -> bool {
    secrets.iter().any(|secret| {
        haystack
            .windows(secret.len())
            .any(|window| window == *secret)
    })
}

type LineCounts = (u64, u64, bool);

fn parse_numstat_entries(output: &str) -> GitResult<BTreeMap<String, LineCounts>> {
    let mut counts = BTreeMap::new();
    for record in output.split_terminator('\0') {
        let mut fields = record.splitn(3, '\t');
        let (Some(added), Some(deleted), Some(path)) =
            (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        validate_git_path(path)?;
        let parsed = match (added.parse::<u64>(), deleted.parse::<u64>()) {
            (Ok(added), Ok(deleted)) => (added, deleted, false),
            _ => (0, 0, true),
        };
        counts.insert(path.to_string(), parsed);
    }
    Ok(counts)
}

fn file_size(workspace: &Path, path: &str, change: ChangeKind) -> Option<u64> {
    if change == ChangeKind::Deleted {
        return None;
    }
    fs::symlink_metadata(workspace.join(path))
        .ok()
        .map(|metadata| metadata.len())
}

fn validate_git_path(path: &str) -> GitResult<()> {
    let candidate = Path::new(path);
    if path.is_empty()
        || candidate.is_absolute()
        || candidate
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(crate::error::GitError::UnsafePath {
            path: candidate.to_path_buf(),
            reason: "Git reported a path outside the repository root".to_string(),
        });
    }
    Ok(())
}

/// Total bytes of path arguments one `git` invocation may carry.
///
/// `execve` fails with `E2BIG` once the argument vector exceeds the platform
/// limit — roughly 1 MiB on macOS. This is not a pathological case: an agent
/// that compiles the project it is working on leaves tens of thousands of
/// ignored build artifacts in the workspace, every one of which is an excluded
/// path, so an ordinary Rust task overflows the limit and the patch can never
/// be captured. The bound is deliberately far below the platform maximum,
/// because the real limit counts the environment block too.
const MAX_RESET_ARG_BYTES: usize = 96 * 1024;
/// Independent ceiling on argument count, for platforms that limit that instead.
const MAX_RESET_ARG_COUNT: usize = 1_000;
/// A repository needing more filter drivers is either corrupt or deliberately
/// trying to exhaust the trusted capture command line. Normal repositories use
/// zero or a handful (for example Git LFS).
const MAX_FILTER_DRIVERS: usize = 128;

/// Stages the complete workspace while neutralizing repository-defined clean
/// filter processes.
///
/// `git add` normally executes `filter.<driver>.clean` or long-running process
/// filters from repository configuration. Because `.gitattributes` is
/// candidate-controlled, doing that on the host after an agent run would turn
/// evidence capture into a sandbox escape. Resolve every effective filter name
/// first, override its executable commands to the empty passthrough, and only
/// then stage. The shared Git runner separately disables hooks and fsmonitor.
fn add_all_without_external_filters(workspace: &Path) -> GitResult<()> {
    let listed = run_git(
        workspace,
        [
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
        ],
    )?;
    let paths = listed.split_terminator('\0').collect::<Vec<_>>();
    for path in &paths {
        validate_git_path(path)?;
    }

    let mut drivers = BTreeSet::new();
    for batch in argument_batches(&paths) {
        let mut args = vec![
            "check-attr".to_string(),
            "-z".to_string(),
            "filter".to_string(),
            "--".to_string(),
        ];
        args.extend(batch.iter().map(|path| (*path).to_string()));
        let attributes = run_git(workspace, args)?;
        let mut fields = attributes.split_terminator('\0');
        while let (Some(path), Some(attribute), Some(value)) =
            (fields.next(), fields.next(), fields.next())
        {
            validate_git_path(path)?;
            if attribute != "filter" || matches!(value, "unspecified" | "unset") {
                continue;
            }
            if value.is_empty()
                || !value
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric() || "._-".contains(character))
            {
                return Err(crate::error::GitError::UnsafePath {
                    path: Path::new(".gitattributes").to_path_buf(),
                    reason: format!("candidate selected invalid Git filter driver `{value}`"),
                });
            }
            drivers.insert(value.to_string());
            if drivers.len() > MAX_FILTER_DRIVERS {
                return Err(crate::error::GitError::UnsafePath {
                    path: Path::new(".gitattributes").to_path_buf(),
                    reason: format!(
                        "candidate selected more than {MAX_FILTER_DRIVERS} Git filter drivers"
                    ),
                });
            }
        }
    }

    let mut args = Vec::new();
    for driver in drivers {
        args.extend([
            "-c".to_string(),
            format!("filter.{driver}.process="),
            "-c".to_string(),
            format!("filter.{driver}.clean="),
            "-c".to_string(),
            format!("filter.{driver}.required=false"),
        ]);
    }
    args.extend(["add".to_string(), "-A".to_string(), "--".to_string()]);
    run_git(workspace, args)?;
    Ok(())
}

fn argument_batches<'a>(paths: &'a [&'a str]) -> Vec<Vec<&'a str>> {
    let mut batches = Vec::new();
    let mut batch = Vec::new();
    let mut bytes = 0;
    for path in paths {
        let len = path.len() + 1;
        if !batch.is_empty()
            && (bytes + len > MAX_RESET_ARG_BYTES || batch.len() >= MAX_RESET_ARG_COUNT)
        {
            batches.push(std::mem::take(&mut batch));
            bytes = 0;
        }
        batch.push(*path);
        bytes += len;
    }
    if !batch.is_empty() {
        batches.push(batch);
    }
    batches
}

/// Leaves only policy-approved changes in the workspace index.
///
/// Excluded paths are reset in batches. Resetting a path to `base` is
/// idempotent and independent of every other path, so splitting the work across
/// several invocations produces exactly the index one invocation would have.
pub fn stage_candidate_patch(
    workspace: impl AsRef<Path>,
    base: &str,
    candidate: &CandidatePatch,
) -> GitResult<()> {
    let workspace = workspace.as_ref();
    if candidate.excluded.is_empty() {
        return Ok(());
    }

    // Validate every path before running anything: a rejected path must not
    // leave the index half-reset behind an already-executed batch.
    for entry in &candidate.excluded {
        validate_git_path(&entry.path)?;
    }

    for batch in reset_batches(&candidate.excluded) {
        let mut args = vec![
            "reset".to_string(),
            "--quiet".to_string(),
            base.to_string(),
            "--".to_string(),
        ];
        args.extend(batch.iter().map(|path| (*path).to_string()));
        run_git(workspace, args)?;
    }
    Ok(())
}

/// Splits excluded paths into groups that fit in one argument vector.
fn reset_batches(excluded: &[forge_core::patch::ExcludedEntry]) -> Vec<Vec<&str>> {
    let mut batches = Vec::new();
    let mut batch: Vec<&str> = Vec::new();
    let mut bytes = 0;

    for entry in excluded {
        // A single path longer than the whole budget still has to be attempted;
        // it goes out alone rather than being silently dropped.
        let len = entry.path.len() + 1;
        if !batch.is_empty()
            && (bytes + len > MAX_RESET_ARG_BYTES || batch.len() >= MAX_RESET_ARG_COUNT)
        {
            batches.push(std::mem::take(&mut batch));
            bytes = 0;
        }
        batch.push(entry.path.as_str());
        bytes += len;
    }
    if !batch.is_empty() {
        batches.push(batch);
    }
    batches
}

/// Unified diff for the currently staged candidate.
pub fn cached_patch(workspace: impl AsRef<Path>, base: &str) -> GitResult<String> {
    run_git(
        workspace,
        ["diff", "--no-ext-diff", "--no-textconv", "--cached", base],
    )
}

/// Commits the index exactly as policy prepared it, without restaging the
/// untrusted workspace.
///
/// The resulting commit is a direct child of `base`, so agent-authored commits
/// containing excluded content do not remain in the durable branch's ancestry.
pub fn commit_staged_workspace(
    workspace: impl AsRef<Path>,
    base: &str,
    message: &str,
) -> GitResult<Option<String>> {
    let workspace = workspace.as_ref();
    let has_candidate = run_git(
        workspace,
        [
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--cached",
            "--quiet",
            base,
        ],
    )
    .is_err();
    if !has_candidate {
        // An agent may have committed excluded content. Move the durable run
        // branch back to the base tree so that content is not retained in its
        // ancestry. Excluded workspace files remain available until teardown.
        run_git(workspace, ["reset", "--soft", base])?;
        return Ok(None);
    }

    let tree = run_git(workspace, ["write-tree"])?;
    let tree = tree.trim();
    let commit = run_git(
        workspace,
        [
            "-c",
            "user.name=Forge",
            "-c",
            "user.email=forge@localhost",
            "-c",
            "commit.gpgsign=false",
            "commit-tree",
            tree,
            "-p",
            base,
            "-m",
            message,
        ],
    )?;
    let commit = commit.trim().to_string();
    run_git(workspace, ["reset", "--soft", &commit])?;
    Ok(Some(commit))
}

/// Commits everything staged in a workspace, returning the new commit.
///
/// Returns `None` when there is nothing to commit.
///
/// Without this, an agent's uncommitted work would die with the worktree
/// directory: removing a worktree discards its index and working files, and the
/// branch would still point at the base commit. Committing makes the run branch
/// a durable record that outlives the workspace.
///
/// The identity is supplied per-invocation rather than read from Git config, so
/// a machine with no `user.email` set can still run Forge, and so agent commits
/// are never attributed to the operator.
pub fn commit_workspace(workspace: impl AsRef<Path>, message: &str) -> GitResult<Option<String>> {
    let workspace = workspace.as_ref();
    add_all_without_external_filters(workspace)?;

    // `diff --cached --quiet` exits 1 when something is staged.
    let has_staged = run_git(
        workspace,
        [
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--cached",
            "--quiet",
        ],
    )
    .is_err();
    if !has_staged {
        return Ok(None);
    }

    run_git(
        workspace,
        [
            "-c",
            "user.name=Forge",
            "-c",
            "user.email=forge@localhost",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--quiet",
            "--no-verify",
            "-m",
            message,
        ],
    )?;
    Ok(Some(
        run_git(workspace, ["rev-parse", "HEAD"])?
            .trim()
            .to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestRepo;
    use crate::worktree::WorktreeManager;

    fn workspace(repo: &TestRepo) -> (WorktreeManager, crate::worktree::Worktree) {
        let base = repo.repository().head_commit().unwrap();
        let manager = WorktreeManager::new(
            repo.repository().clone(),
            repo.path().join(".forge/worktrees"),
        )
        .unwrap();
        let worktree = manager.create("R-0001", &base, "forge/R-0001").unwrap();
        (manager, worktree)
    }

    fn excluded(paths: Vec<String>) -> Vec<forge_core::patch::ExcludedEntry> {
        paths
            .into_iter()
            .map(|path| forge_core::patch::ExcludedEntry {
                path,
                change: ChangeKind::Added,
                reason: forge_core::patch::ExclusionReason::GitIgnored,
            })
            .collect()
    }

    /// An agent that compiles the project it is working on leaves tens of
    /// thousands of ignored build artifacts behind, and every one is an excluded
    /// path. Passing them all to one `git reset` overflows the argument vector
    /// and fails with `E2BIG` ("Argument list too long"), which is exactly what
    /// happened to the first two real Forge-on-Forge runs: the agent succeeded,
    /// and Forge could not capture the patch.
    #[test]
    fn excluded_paths_are_batched_below_the_argument_limit() {
        let paths: Vec<String> = (0..15_338)
            .map(|i| format!("target/debug/build/some-crate-{i:012}/out/generated.rs"))
            .collect();
        let entries = excluded(paths.clone());

        let batches = reset_batches(&entries);
        assert!(
            batches.len() > 1,
            "15k paths must not go out in one invocation"
        );

        for batch in &batches {
            let bytes: usize = batch.iter().map(|p| p.len() + 1).sum();
            assert!(
                bytes <= MAX_RESET_ARG_BYTES,
                "batch of {bytes} bytes exceeds the budget"
            );
            assert!(
                batch.len() <= MAX_RESET_ARG_COUNT,
                "batch holds {} paths",
                batch.len()
            );
        }

        // Every path must be reset exactly once: dropping one would leave
        // excluded content staged, and repeating one is wasted work.
        let flattened: Vec<&str> = batches.iter().flatten().copied().collect();
        assert_eq!(flattened.len(), paths.len());
        assert_eq!(
            flattened,
            paths.iter().map(String::as_str).collect::<Vec<_>>()
        );
    }

    /// A single path larger than the whole budget is still attempted rather
    /// than silently skipped.
    #[test]
    fn an_oversized_single_path_is_still_emitted() {
        let long = "a".repeat(MAX_RESET_ARG_BYTES * 2);
        let entries = excluded(vec!["short.rs".to_string(), long.clone()]);
        let batches = reset_batches(&entries);
        let flattened: Vec<&str> = batches.iter().flatten().copied().collect();
        assert_eq!(flattened, vec!["short.rs", long.as_str()]);
    }

    #[test]
    fn numstat_parsing_handles_text_and_binary_entries() {
        let stat = parse_numstat("10\t2\tsrc/a.rs\n0\t5\tsrc/b.rs\n-\t-\tassets/logo.png\n");
        assert_eq!(stat.files_changed, 3);
        assert_eq!(stat.insertions, 10);
        assert_eq!(stat.deletions, 7);
        assert_eq!(stat.binary_files, 1);
        assert_eq!(stat.lines_changed(), 17);
    }

    #[test]
    fn an_untouched_workspace_produces_an_empty_patch() {
        let repo = TestRepo::new();
        let (_manager, worktree) = workspace(&repo);

        let (stat, patch) =
            capture_workspace_patch(worktree.path(), worktree.base_commit()).unwrap();
        assert!(stat.is_empty());
        assert!(patch.is_empty());
    }

    #[test]
    fn uncommitted_agent_work_is_captured() {
        let repo = TestRepo::new();
        let (_manager, worktree) = workspace(&repo);

        std::fs::write(worktree.path().join("README.md"), "# test repo\nmore\n").unwrap();
        std::fs::write(worktree.path().join("new.rs"), "fn main() {}\n").unwrap();

        let (stat, patch) =
            capture_workspace_patch(worktree.path(), worktree.base_commit()).unwrap();
        assert_eq!(stat.files_changed, 2);
        assert_eq!(stat.insertions, 2);
        assert!(patch.contains("new.rs"), "{patch}");
    }

    #[test]
    fn credential_scan_covers_tracked_untracked_and_ignored_candidate_bytes() {
        let repo = TestRepo::new();
        let (_manager, worktree) = workspace(&repo);
        let secret = "short-secret".to_string();

        std::fs::write(
            worktree.path().join("README.md"),
            format!("# test repo\n{secret}\n"),
        )
        .unwrap();
        assert!(
            workspace_contains_secret(
                worktree.path(),
                worktree.base_commit(),
                std::slice::from_ref(&secret),
            )
            .unwrap()
        );

        std::fs::write(worktree.path().join("README.md"), "# test repo\n").unwrap();
        std::fs::write(
            worktree.path().join("untracked.txt"),
            format!("prefix-{secret}-suffix"),
        )
        .unwrap();
        assert!(
            workspace_contains_secret(
                worktree.path(),
                worktree.base_commit(),
                std::slice::from_ref(&secret),
            )
            .unwrap()
        );

        std::fs::remove_file(worktree.path().join("untracked.txt")).unwrap();
        std::fs::write(worktree.path().join(".gitignore"), "ignored/\n").unwrap();
        std::fs::create_dir(worktree.path().join("ignored")).unwrap();
        std::fs::write(
            worktree.path().join("ignored/evidence.bin"),
            [vec![b'x'; 65_535], secret.as_bytes().to_vec()].concat(),
        )
        .unwrap();
        assert!(
            workspace_contains_secret(worktree.path(), worktree.base_commit(), &[secret]).unwrap()
        );
    }

    #[test]
    fn credential_scan_covers_candidate_path_names_without_staging() {
        let repo = TestRepo::new();
        let (_manager, worktree) = workspace(&repo);
        let secret = "credential-in-path".to_string();
        std::fs::write(
            worktree.path().join(format!("result-{secret}.txt")),
            "safe bytes",
        )
        .unwrap();

        assert!(
            workspace_contains_secret(worktree.path(), worktree.base_commit(), &[secret]).unwrap()
        );
        assert!(
            worktree
                .git(["diff", "--cached", "--quiet", worktree.base_commit()])
                .is_ok(),
            "the pre-capture scan must not stage candidate files"
        );
    }

    #[test]
    fn credential_scan_does_not_report_unrelated_candidate_bytes() {
        let repo = TestRepo::new();
        let (_manager, worktree) = workspace(&repo);
        std::fs::write(
            worktree.path().join("safe.txt"),
            "ordinary candidate output",
        )
        .unwrap();

        assert!(
            !workspace_contains_secret(
                worktree.path(),
                worktree.base_commit(),
                &["not-present".into()]
            )
            .unwrap()
        );
    }

    #[test]
    fn candidate_clean_filters_are_neutralized_during_host_capture() {
        let repo = TestRepo::new();
        let (_manager, worktree) = workspace(&repo);
        let escaped = repo.path().join("host-filter-escape-marker");
        worktree
            .git([
                "config",
                "filter.candidate.clean",
                &format!("touch '{}'; cat", escaped.display()),
            ])
            .unwrap();
        worktree
            .git(["config", "filter.candidate.required", "true"])
            .unwrap();
        std::fs::write(
            worktree.path().join(".gitattributes"),
            "*.txt filter=candidate\n",
        )
        .unwrap();
        std::fs::write(worktree.path().join("payload.txt"), "raw candidate bytes\n").unwrap();

        let delta = workspace_delta(worktree.path(), worktree.base_commit()).unwrap();
        assert!(
            !escaped.exists(),
            "candidate-controlled clean filter executed on the host"
        );
        assert!(
            delta
                .entries
                .iter()
                .any(|entry| entry.path == "payload.txt"),
            "candidate file was not captured"
        );
        let patch = cached_patch(worktree.path(), worktree.base_commit()).unwrap();
        assert!(patch.contains("raw candidate bytes"), "{patch}");
    }

    #[test]
    fn committed_agent_work_is_captured_identically() {
        let repo = TestRepo::new();
        let (_manager, worktree) = workspace(&repo);

        std::fs::write(worktree.path().join("new.rs"), "fn main() {}\n").unwrap();
        worktree.git(["add", "-A"]).unwrap();
        worktree
            .git([
                "-c",
                "user.email=a@b.c",
                "-c",
                "user.name=t",
                "commit",
                "--quiet",
                "-m",
                "agent work",
            ])
            .unwrap();

        let (stat, patch) =
            capture_workspace_patch(worktree.path(), worktree.base_commit()).unwrap();
        assert_eq!(stat.files_changed, 1);
        assert_eq!(stat.insertions, 1);
        assert!(patch.contains("fn main"), "{patch}");
    }

    #[test]
    fn deleted_files_are_counted() {
        let repo = TestRepo::new();
        let (_manager, worktree) = workspace(&repo);

        std::fs::remove_file(worktree.path().join("README.md")).unwrap();

        let (stat, _) = capture_workspace_patch(worktree.path(), worktree.base_commit()).unwrap();
        assert_eq!(stat.files_changed, 1);
        assert_eq!(stat.deletions, 1);
    }

    #[test]
    fn committing_preserves_agent_work_beyond_the_workspace() {
        let repo = TestRepo::new();
        let (manager, worktree) = workspace(&repo);

        std::fs::write(worktree.path().join("new.rs"), "fn main() {}\n").unwrap();
        let commit = commit_workspace(worktree.path(), "forge run R-0001").unwrap();
        let commit = commit.expect("a commit was made");

        // The workspace can now be destroyed without losing the work.
        manager.remove(&worktree, false).unwrap();
        assert!(!worktree.path().exists());
        assert_eq!(repo.repository().resolve("forge/R-0001").unwrap(), commit);

        let patch = patch_between(repo.path(), worktree.base_commit(), &commit).unwrap();
        assert!(patch.contains("fn main"), "{patch}");
    }

    #[test]
    fn committing_an_unchanged_workspace_does_nothing() {
        let repo = TestRepo::new();
        let (_manager, worktree) = workspace(&repo);
        assert_eq!(
            commit_workspace(worktree.path(), "forge run R-0001").unwrap(),
            None
        );
        assert_eq!(worktree.head_commit().unwrap(), worktree.base_commit());
    }

    #[test]
    fn committing_does_not_depend_on_local_git_identity() {
        // A machine with no `user.email` configured must still be able to run.
        let repo = TestRepo::new();
        let (_manager, worktree) = workspace(&repo);
        worktree.git(["config", "--unset", "user.email"]).ok();
        worktree.git(["config", "--unset", "user.name"]).ok();

        std::fs::write(worktree.path().join("a.txt"), "x").unwrap();
        assert!(commit_workspace(worktree.path(), "m").unwrap().is_some());
    }

    #[test]
    fn stats_between_revisions_ignore_the_working_tree() {
        let repo = TestRepo::new();
        let base = repo.repository().head_commit().unwrap();
        repo.write("src/lib.rs", "pub fn a() {}\n");
        let head = repo.commit("add lib");

        let stat = stat_between(repo.path(), &base, &head).unwrap();
        assert_eq!(stat.files_changed, 1);
        assert_eq!(stat.insertions, 1);

        let patch = patch_between(repo.path(), &base, &head).unwrap();
        assert!(patch.contains("pub fn a"), "{patch}");
    }
}
