//! Extracting what an agent actually changed.
//!
//! The patch is the agent's real output. Everything else it says about its work
//! is a claim; the diff is evidence.

use std::collections::BTreeMap;
use std::fs;
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
    let output = run_git(dir, ["diff", "--numstat", base, head])?;
    Ok(parse_numstat(&output))
}

/// Unified diff between two revisions.
pub fn patch_between(dir: impl AsRef<Path>, base: &str, head: &str) -> GitResult<String> {
    run_git(dir, ["diff", base, head])
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
    run_git(workspace, ["add", "-A"])?;

    let statuses = run_git(
        workspace,
        [
            "diff",
            "--cached",
            "--name-status",
            "-z",
            "--no-renames",
            base,
        ],
    )?;
    let numstat = run_git(workspace, ["diff", "--cached", "--numstat", "-z", base])?;
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
    run_git(workspace, ["diff", "--cached", base])
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
    let has_candidate = run_git(workspace, ["diff", "--cached", "--quiet", base]).is_err();
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
    run_git(workspace, ["add", "-A"])?;

    // `diff --cached --quiet` exits 1 when something is staged.
    let has_staged = run_git(workspace, ["diff", "--cached", "--quiet"]).is_err();
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
