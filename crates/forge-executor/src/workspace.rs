//! Provisioning the isolated environment a run executes in.
//!
//! The provider interface is the seam where containers or remote workers will
//! slot in later. Nothing above this layer knows that today's isolation is a
//! Git worktree.

use std::fs;
use std::path::Path;

use forge_core::events::{EventPayload, EventSink};
use forge_core::ids::RunId;
use forge_core::patch::{CandidatePatch, PatchPolicy, WorkspaceDelta};
use forge_core::run::PatchSummary;
use forge_core::workspace::{Workspace, WorkspaceKind};
use forge_git::{Repository, Worktree, WorktreeManager};

use crate::error::{ExecError, ExecResult};

/// Creates and destroys the environment a run executes in.
pub trait WorkspaceProvider: Send + Sync {
    /// Creates an isolated environment for `run_id` at `base_commit`.
    fn provision(
        &self,
        run_id: &RunId,
        base_commit: &str,
        events: &dyn EventSink,
    ) -> ExecResult<Workspace>;

    /// Releases the environment. Must be safe to call on an already-released
    /// workspace, because cleanup runs on the failure path too.
    fn teardown(&self, workspace: &Workspace) -> ExecResult<()>;
}

/// Git-worktree-backed isolation: the V0 mechanism.
#[derive(Debug, Clone)]
pub struct WorktreeProvider {
    manager: WorktreeManager,
    branch_prefix: String,
    keep_after_run: bool,
}

impl WorktreeProvider {
    pub fn new(
        repository: Repository,
        worktrees_root: impl AsRef<Path>,
        branch_prefix: impl Into<String>,
    ) -> ExecResult<Self> {
        Ok(Self {
            manager: WorktreeManager::new(repository, worktrees_root)?,
            branch_prefix: branch_prefix.into(),
            keep_after_run: false,
        })
    }

    /// Keeps worktrees after a run finishes, for inspecting what an agent did.
    pub fn keep_after_run(mut self, keep: bool) -> Self {
        self.keep_after_run = keep;
        self
    }

    pub fn manager(&self) -> &WorktreeManager {
        &self.manager
    }

    /// The branch a run's work lands on.
    pub fn branch_for(&self, run_id: &RunId) -> String {
        format!("{}{}", self.branch_prefix, run_id)
    }
}

impl WorkspaceProvider for WorktreeProvider {
    fn provision(
        &self,
        run_id: &RunId,
        base_commit: &str,
        events: &dyn EventSink,
    ) -> ExecResult<Workspace> {
        let branch = self.branch_for(run_id);
        let worktree = self.manager.create(run_id.as_str(), base_commit, &branch)?;

        events.emit(EventPayload::WorkspaceCreated {
            path: worktree.path().to_path_buf(),
            branch: branch.clone(),
            base_commit: worktree.base_commit().to_string(),
        });

        Ok(Workspace::new(
            run_id.clone(),
            WorkspaceKind::Worktree,
            worktree.path().to_path_buf(),
            branch,
            worktree.base_commit(),
        ))
    }

    fn teardown(&self, workspace: &Workspace) -> ExecResult<()> {
        if self.keep_after_run {
            tracing::debug!(path = %workspace.path.display(), "keeping workspace");
            return Ok(());
        }
        let worktree = Worktree::describe(
            workspace.path.clone(),
            workspace.branch.clone(),
            workspace.base_commit.clone(),
        );
        // The branch is kept: it is the durable record of what the agent wrote,
        // and the run record references it.
        self.manager.remove(&worktree, false)?;
        Ok(())
    }
}

/// Records what a run changed, writing the full diff to `diff_path` if given.
///
/// This is Forge's evidence of the agent's work, gathered from the repository
/// rather than from anything the agent reported about itself.
pub fn capture_patch(
    workspace: &Workspace,
    diff_path: Option<&Path>,
    commit_message: Option<&str>,
) -> ExecResult<PatchSummary> {
    Ok(capture_candidate_patch(
        workspace,
        diff_path,
        commit_message,
        &PatchPolicy::default(),
    )?
    .summary)
}

/// Ignored-file exclusions kept path-by-path in the durable run record.
///
/// Enough to show what was excluded and what it looked like, without storing an
/// agent's entire build tree. One real run excluded 26,286 ignored paths and
/// produced an 8.8 MB record; the exact count is preserved in
/// `PatchSummary::excluded_counts` regardless of how many paths are retained.
pub const RETAINED_IGNORED_EXCLUSIONS: usize = 20;

/// Patch capture at the explicit workspace-delta/policy/candidate boundary.
#[derive(Debug, Clone)]
pub struct PatchCapture {
    pub summary: PatchSummary,
    pub delta: WorkspaceDelta,
    pub candidate: CandidatePatch,
}

/// Records the complete delta, applies policy, writes only the candidate diff,
/// and preserves the candidate tree on the run branch.
pub fn capture_candidate_patch(
    workspace: &Workspace,
    diff_path: Option<&Path>,
    commit_message: Option<&str>,
    policy: &PatchPolicy,
) -> ExecResult<PatchCapture> {
    let delta = forge_git::workspace_delta(&workspace.path, &workspace.base_commit)?;
    let candidate = policy.apply(&delta);
    forge_git::stage_candidate_patch(&workspace.path, &workspace.base_commit, &candidate)?;
    let patch = forge_git::cached_patch(&workspace.path, &workspace.base_commit)?;

    let written = match diff_path {
        Some(path) if !patch.is_empty() => {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).map_err(|source| ExecError::Io {
                    context: format!("creating {}", parent.display()),
                    source,
                })?;
            }
            fs::write(path, &patch).map_err(|source| ExecError::Io {
                context: format!("writing patch to {}", path.display()),
                source,
            })?;
            Some(path.to_path_buf())
        }
        _ => None,
    };

    // Committing makes the run branch outlive the workspace directory.
    let head_commit = match commit_message {
        Some(message) if !delta.is_empty() => {
            forge_git::commit_staged_workspace(&workspace.path, &workspace.base_commit, message)?
        }
        _ => Worktree::describe(
            workspace.path.clone(),
            workspace.branch.clone(),
            workspace.base_commit.clone(),
        )
        .head_commit()
        .ok()
        .filter(|head| head != &workspace.base_commit),
    }
    .filter(|head| head != &workspace.base_commit);

    Ok(PatchCapture {
        summary: PatchSummary {
            base_commit: workspace.base_commit.clone(),
            head_commit,
            files_changed: candidate.files_changed(),
            insertions: candidate.insertions(),
            deletions: candidate.deletions(),
            binary_files: candidate.binary_files(),
            diff_path: written,
            excluded: candidate.retained_exclusions(RETAINED_IGNORED_EXCLUSIONS),
            excluded_counts: candidate.exclusion_counts(),
        },
        delta,
        candidate,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_core::events::{NullSink, RecordingSink};

    struct Fixture {
        _temp: tempfile::TempDir,
        repo: Repository,
        root: std::path::PathBuf,
    }

    fn fixture() -> Fixture {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("repo");
        fs::create_dir_all(&root).unwrap();
        for args in [
            vec!["init", "--quiet", "--initial-branch=main"],
            vec!["config", "user.email", "forge-tests@example.invalid"],
            vec!["config", "user.name", "Forge Tests"],
        ] {
            let status = std::process::Command::new("git")
                .arg("-C")
                .arg(&root)
                .args(&args)
                .status()
                .unwrap();
            assert!(status.success());
        }
        fs::write(root.join("README.md"), "# repo\n").unwrap();
        fs::write(root.join(".gitignore"), "/target\n").unwrap();
        for args in [
            vec!["add", "-A"],
            vec!["commit", "--quiet", "-m", "initial"],
        ] {
            let status = std::process::Command::new("git")
                .arg("-C")
                .arg(&root)
                .args(&args)
                .status()
                .unwrap();
            assert!(status.success());
        }

        let repo = Repository::open(&root).unwrap();
        let worktrees = root.join(".forge/worktrees");
        Fixture {
            _temp: temp,
            repo,
            root: worktrees,
        }
    }

    fn provider(fixture: &Fixture) -> WorktreeProvider {
        WorktreeProvider::new(fixture.repo.clone(), &fixture.root, "forge/").unwrap()
    }

    #[test]
    fn provisioning_creates_an_isolated_branch_and_records_it() {
        let fixture = fixture();
        let provider = provider(&fixture);
        let base = fixture.repo.head_commit().unwrap();
        let run_id = RunId::sequential(1);
        let sink = RecordingSink::new(run_id.clone());

        let workspace = provider.provision(&run_id, &base, &sink).unwrap();

        assert_eq!(workspace.branch, "forge/R-0001");
        assert_eq!(workspace.base_commit, base);
        assert_eq!(workspace.kind, WorkspaceKind::Worktree);
        assert!(workspace.path.join("README.md").exists());
        assert!(matches!(
            sink.events()[0].payload,
            EventPayload::WorkspaceCreated { .. }
        ));
    }

    #[test]
    fn teardown_removes_the_workspace_but_keeps_the_work() {
        let fixture = fixture();
        let provider = provider(&fixture);
        let base = fixture.repo.head_commit().unwrap();
        let run_id = RunId::sequential(1);

        let workspace = provider.provision(&run_id, &base, &NullSink).unwrap();
        provider.teardown(&workspace).unwrap();

        assert!(!workspace.path.exists());
        assert!(fixture.repo.branch_exists("forge/R-0001"));
    }

    #[test]
    fn teardown_is_idempotent() {
        let fixture = fixture();
        let provider = provider(&fixture);
        let base = fixture.repo.head_commit().unwrap();
        let workspace = provider
            .provision(&RunId::sequential(1), &base, &NullSink)
            .unwrap();

        provider.teardown(&workspace).unwrap();
        provider.teardown(&workspace).unwrap();
    }

    #[test]
    fn workspaces_can_be_kept_for_inspection() {
        let fixture = fixture();
        let provider = provider(&fixture).keep_after_run(true);
        let base = fixture.repo.head_commit().unwrap();
        let workspace = provider
            .provision(&RunId::sequential(1), &base, &NullSink)
            .unwrap();

        provider.teardown(&workspace).unwrap();
        assert!(workspace.path.join("README.md").exists());
    }

    #[test]
    fn the_users_working_tree_is_never_touched() {
        let fixture = fixture();
        let provider = provider(&fixture);
        let base = fixture.repo.head_commit().unwrap();
        let workspace = provider
            .provision(&RunId::sequential(1), &base, &NullSink)
            .unwrap();

        fs::write(workspace.path.join("README.md"), "agent edit\n").unwrap();

        assert_eq!(
            fs::read_to_string(fixture.repo.root().join("README.md")).unwrap(),
            "# repo\n"
        );
    }

    #[test]
    fn capture_records_uncommitted_agent_work_and_writes_the_diff() {
        let fixture = fixture();
        let provider = provider(&fixture);
        let base = fixture.repo.head_commit().unwrap();
        let workspace = provider
            .provision(&RunId::sequential(1), &base, &NullSink)
            .unwrap();

        fs::write(workspace.path.join("README.md"), "# repo\nagent line\n").unwrap();
        let diff_path = fixture.repo.root().join(".forge/runs/R-0001/patch.diff");

        let patch = capture_patch(&workspace, Some(&diff_path), None).unwrap();

        assert_eq!(patch.files_changed, 1);
        assert_eq!(patch.insertions, 1);
        assert_eq!(patch.base_commit, base);
        // The agent did not commit, so there is no head commit to record.
        assert_eq!(patch.head_commit, None);
        assert_eq!(patch.diff_path.as_deref(), Some(diff_path.as_path()));
        assert!(
            fs::read_to_string(&diff_path)
                .unwrap()
                .contains("agent line")
        );
    }

    #[test]
    fn capture_can_commit_so_the_work_survives_teardown() {
        let fixture = fixture();
        let provider = provider(&fixture);
        let base = fixture.repo.head_commit().unwrap();
        let workspace = provider
            .provision(&RunId::sequential(1), &base, &NullSink)
            .unwrap();

        fs::write(workspace.path.join("agent.rs"), "fn added() {}\n").unwrap();
        let patch = capture_patch(&workspace, None, Some("forge run R-0001")).unwrap();

        let head = patch.head_commit.expect("work was committed");
        assert_ne!(head, base);

        provider.teardown(&workspace).unwrap();
        // The branch still holds the agent's work after the workspace is gone.
        assert_eq!(fixture.repo.resolve("forge/R-0001").unwrap(), head);
    }

    #[test]
    fn capture_of_an_untouched_workspace_writes_nothing() {
        let fixture = fixture();
        let provider = provider(&fixture);
        let base = fixture.repo.head_commit().unwrap();
        let workspace = provider
            .provision(&RunId::sequential(1), &base, &NullSink)
            .unwrap();

        let diff_path = fixture.repo.root().join(".forge/runs/R-0001/patch.diff");
        let patch = capture_patch(&workspace, Some(&diff_path), None).unwrap();

        assert!(patch.is_empty());
        assert_eq!(patch.diff_path, None);
        assert!(!diff_path.exists());
    }

    #[test]
    fn ignored_build_output_is_evidence_but_not_candidate_content() {
        let fixture = fixture();
        let provider = provider(&fixture);
        let base = fixture.repo.head_commit().unwrap();
        let workspace = provider
            .provision(&RunId::sequential(1), &base, &NullSink)
            .unwrap();
        fs::create_dir_all(workspace.path.join("target/debug")).unwrap();
        fs::write(
            workspace.path.join("target/debug/generated.bin"),
            b"build output",
        )
        .unwrap();

        let captured = capture_candidate_patch(
            &workspace,
            None,
            Some("forge run R-0001"),
            &PatchPolicy::default(),
        )
        .unwrap();

        assert!(captured.summary.is_empty());
        assert_eq!(captured.summary.head_commit, None);
        assert_eq!(captured.summary.excluded.len(), 1);
        assert!(matches!(
            captured.summary.excluded[0].reason,
            forge_core::patch::ExclusionReason::GitIgnored
        ));
    }

    #[test]
    fn binary_additions_are_candidate_content_with_a_structured_warning() {
        let fixture = fixture();
        let provider = provider(&fixture);
        let base = fixture.repo.head_commit().unwrap();
        let workspace = provider
            .provision(&RunId::sequential(1), &base, &NullSink)
            .unwrap();
        fs::write(workspace.path.join("image.bin"), b"a\0b\0c").unwrap();

        let captured =
            capture_candidate_patch(&workspace, None, None, &PatchPolicy::default()).unwrap();
        assert_eq!(captured.summary.binary_files, 1);
        assert!(captured.candidate.warnings.iter().any(|warning| {
            warning.kind == forge_core::patch::WarningKind::BinaryFile
                && warning.path.as_deref() == Some("image.bin")
        }));
    }

    #[test]
    fn oversized_files_are_excluded_from_the_diff_and_durable_branch() {
        let fixture = fixture();
        let provider = provider(&fixture);
        let base = fixture.repo.head_commit().unwrap();
        let workspace = provider
            .provision(&RunId::sequential(1), &base, &NullSink)
            .unwrap();
        fs::write(workspace.path.join("huge.dat"), vec![b'x'; 1024]).unwrap();
        let committed = std::process::Command::new("git")
            .arg("-C")
            .arg(&workspace.path)
            .args(["add", "-A"])
            .status()
            .unwrap();
        assert!(committed.success());
        let committed = std::process::Command::new("git")
            .arg("-C")
            .arg(&workspace.path)
            .args([
                "commit",
                "--quiet",
                "-m",
                "agent committed oversized output",
            ])
            .status()
            .unwrap();
        assert!(committed.success());

        let captured = capture_candidate_patch(
            &workspace,
            None,
            Some("forge run R-0001"),
            &PatchPolicy::default().with_max_file_bytes(32),
        )
        .unwrap();

        assert!(captured.summary.is_empty());
        assert_eq!(captured.summary.head_commit, None);
        // The exclusion itself is the record, and it carries the size that
        // caused it. A size-limit exclusion is one of Forge's own judgments, so
        // it is retained in the durable summary rather than sampled away.
        assert!(captured.candidate.excluded.iter().any(|entry| {
            matches!(
                entry.reason,
                forge_core::patch::ExclusionReason::TooLarge { .. }
            )
        }));
        assert!(captured.summary.excluded.iter().any(|entry| {
            matches!(
                entry.reason,
                forge_core::patch::ExclusionReason::TooLarge { .. }
            )
        }));
        assert_eq!(captured.summary.excluded_counts.get("too_large"), Some(&1));
        assert_eq!(
            forge_git::patch_between(
                fixture.repo.root(),
                &base,
                captured.summary.head_commit.as_deref().unwrap_or(&base)
            )
            .unwrap(),
            ""
        );
        assert_eq!(fixture.repo.resolve("forge/R-0001").unwrap(), base);
    }
}
