//! Git worktrees as agent workspaces.
//!
//! Every competing agent starts from the same commit in its own worktree on its
//! own branch. That is enough isolation to make results comparable — no
//! cross-contamination, no file collisions, trivial diffs — without reaching
//! for containers.
//!
//! The invariant this module enforces: **Forge only ever creates or destroys
//! directories inside its configured worktree root.** An agent must never be
//! able to reach the user's primary working tree, and a cleanup bug must never
//! be able to delete anything Forge did not create.

use std::fs;
use std::path::{Path, PathBuf};

use forge_core::ids::validate_id;

use crate::error::{GitError, GitResult};
use crate::repository::{Repository, canonicalize, run_git};

/// One isolated checkout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worktree {
    path: PathBuf,
    branch: String,
    base_commit: String,
}

impl Worktree {
    /// Describes an existing worktree without creating one.
    ///
    /// Used to reconstruct a handle from a stored record. Constructing a
    /// description grants no authority: [`WorktreeManager::remove`] still
    /// refuses any path outside its managed root.
    pub fn describe(path: PathBuf, branch: String, base_commit: String) -> Self {
        Self {
            path,
            branch,
            base_commit,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn branch(&self) -> &str {
        &self.branch
    }

    /// The commit this worktree started from.
    pub fn base_commit(&self) -> &str {
        &self.base_commit
    }

    /// Current `HEAD` of the worktree, which differs from the base commit once
    /// the agent commits anything.
    pub fn head_commit(&self) -> GitResult<String> {
        Ok(run_git(&self.path, ["rev-parse", "--verify", "HEAD"])?
            .trim()
            .to_string())
    }

    pub fn git<I, S>(&self, args: I) -> GitResult<String>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<std::ffi::OsStr>,
    {
        run_git(&self.path, args)
    }
}

/// Creates and destroys worktrees under a single managed root.
#[derive(Debug, Clone)]
pub struct WorktreeManager {
    repo: Repository,
    /// Canonical path of the directory Forge is allowed to manage.
    root: PathBuf,
}

impl WorktreeManager {
    /// `root` is created if missing, then canonicalized.
    ///
    /// Refuses a root that is the repository root itself, which would put
    /// agents directly in the user's working tree.
    pub fn new(repo: Repository, root: impl AsRef<Path>) -> GitResult<Self> {
        let root = root.as_ref();
        fs::create_dir_all(root).map_err(|source| GitError::Io {
            context: format!("creating worktree root {}", root.display()),
            source,
        })?;
        let root = canonicalize(root)?;

        if root == *repo.root() {
            return Err(GitError::UnsafePath {
                path: root,
                reason: "the worktree root cannot be the repository root".to_string(),
            });
        }

        Ok(Self { repo, root })
    }

    pub fn repository(&self) -> &Repository {
        &self.repo
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Creates a worktree named `name` at `base_commit` on a new `branch`.
    ///
    /// `name` must be a single safe path segment — the same validation used for
    /// run ids — so the worktree cannot land outside the managed root.
    pub fn create(&self, name: &str, base_commit: &str, branch: &str) -> GitResult<Worktree> {
        validate_id(name).map_err(|source| GitError::InvalidWorkspaceName {
            name: name.to_string(),
            reason: source.to_string(),
        })?;
        if branch.trim().is_empty() {
            return Err(GitError::InvalidWorkspaceName {
                name: name.to_string(),
                reason: "branch name is empty".to_string(),
            });
        }

        let base_commit = self.repo.resolve(base_commit)?;
        let path = self.root.join(name);

        // `exists()` follows symlinks, so a dangling symlink would slip past it.
        if fs::symlink_metadata(&path).is_ok() {
            return Err(GitError::WorkspaceExists(path));
        }

        self.repo.git([
            "worktree",
            "add",
            "--quiet",
            "-b",
            branch,
            &path.to_string_lossy(),
            &base_commit,
        ])?;

        // Defense in depth: confirm what Git actually created is inside the
        // managed root before handing it to an agent.
        let created = canonicalize(&path)?;
        if let Err(err) = self.ensure_managed(&created) {
            let _ = self
                .repo
                .git(["worktree", "remove", "--force", &created.to_string_lossy()]);
            return Err(err);
        }

        tracing::debug!(path = %created.display(), branch, base = %base_commit, "worktree created");

        Ok(Worktree {
            path: created,
            branch: branch.to_string(),
            base_commit,
        })
    }

    /// Removes a worktree. The branch is kept by default so the agent's work
    /// stays recoverable after the workspace is gone.
    pub fn remove(&self, worktree: &Worktree, delete_branch: bool) -> GitResult<()> {
        let path = match canonicalize(worktree.path()) {
            Ok(path) => path,
            // Already gone; just clean up Git's administrative record.
            Err(_) => {
                self.prune()?;
                return Ok(());
            }
        };
        self.ensure_managed(&path)?;

        self.repo
            .git(["worktree", "remove", "--force", &path.to_string_lossy()])?;

        if delete_branch {
            self.repo.git(["branch", "-D", worktree.branch()])?;
        }
        Ok(())
    }

    /// Worktrees Git knows about that live under the managed root.
    pub fn list(&self) -> GitResult<Vec<Worktree>> {
        let output = self.repo.git(["worktree", "list", "--porcelain"])?;
        let mut worktrees = Vec::new();
        let mut path: Option<PathBuf> = None;
        let mut head = String::new();
        let mut branch = String::new();

        let mut flush = |path: &mut Option<PathBuf>, head: &mut String, branch: &mut String| {
            if let Some(p) = path.take() {
                // Skip the primary worktree and anything outside the root.
                if p.starts_with(&self.root) && p != self.root {
                    worktrees.push(Worktree {
                        path: p,
                        branch: std::mem::take(branch),
                        base_commit: std::mem::take(head),
                    });
                }
            }
            head.clear();
            branch.clear();
        };

        for line in output.lines() {
            if let Some(rest) = line.strip_prefix("worktree ") {
                flush(&mut path, &mut head, &mut branch);
                path = Some(PathBuf::from(rest));
            } else if let Some(rest) = line.strip_prefix("HEAD ") {
                head = rest.to_string();
            } else if let Some(rest) = line.strip_prefix("branch ") {
                branch = rest.strip_prefix("refs/heads/").unwrap_or(rest).to_string();
            }
        }
        flush(&mut path, &mut head, &mut branch);

        Ok(worktrees)
    }

    /// Drops Git's records of worktrees whose directories are gone.
    pub fn prune(&self) -> GitResult<()> {
        self.repo.git(["worktree", "prune"])?;
        Ok(())
    }

    /// Removes every worktree under the managed root.
    pub fn remove_all(&self, delete_branches: bool) -> GitResult<usize> {
        let worktrees = self.list()?;
        let count = worktrees.len();
        for worktree in &worktrees {
            self.remove(worktree, delete_branches)?;
        }
        self.prune()?;
        Ok(count)
    }

    /// The check that keeps destructive operations inside the managed root.
    fn ensure_managed(&self, path: &Path) -> GitResult<()> {
        if path == self.root {
            return Err(GitError::UnsafePath {
                path: path.to_path_buf(),
                reason: "path is the worktree root itself".to_string(),
            });
        }
        if path == self.repo.root() {
            return Err(GitError::UnsafePath {
                path: path.to_path_buf(),
                reason: "path is the repository's primary working tree".to_string(),
            });
        }
        if !path.starts_with(&self.root) {
            return Err(GitError::UnsafePath {
                path: path.to_path_buf(),
                reason: format!("path is outside the worktree root {}", self.root.display()),
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestRepo;

    fn manager(repo: &TestRepo) -> WorktreeManager {
        WorktreeManager::new(
            repo.repository().clone(),
            repo.path().join(".forge/worktrees"),
        )
        .unwrap()
    }

    #[test]
    fn creates_an_isolated_checkout_at_the_base_commit() {
        let repo = TestRepo::new();
        let base = repo.repository().head_commit().unwrap();
        let manager = manager(&repo);

        let worktree = manager.create("R-0001", &base, "forge/R-0001").unwrap();

        assert!(worktree.path().join("README.md").exists());
        assert_eq!(worktree.base_commit(), base);
        assert_eq!(worktree.head_commit().unwrap(), base);
        assert!(worktree.path().starts_with(manager.root()));
    }

    #[test]
    fn two_agents_start_from_identical_state_and_do_not_interfere() {
        let repo = TestRepo::new();
        let base = repo.repository().head_commit().unwrap();
        let manager = manager(&repo);

        let claude = manager.create("R-0001", &base, "forge/R-0001").unwrap();
        let codex = manager.create("R-0002", &base, "forge/R-0002").unwrap();

        assert_eq!(claude.head_commit().unwrap(), codex.head_commit().unwrap());

        // A change in one workspace is invisible in the other and in the
        // primary working tree.
        fs::write(claude.path().join("README.md"), "claude was here").unwrap();
        assert_eq!(
            fs::read_to_string(codex.path().join("README.md")).unwrap(),
            "# test repo\n"
        );
        assert_eq!(
            fs::read_to_string(repo.path().join("README.md")).unwrap(),
            "# test repo\n"
        );
    }

    #[test]
    fn worktree_names_that_could_escape_the_root_are_rejected() {
        let repo = TestRepo::new();
        let base = repo.repository().head_commit().unwrap();
        let manager = manager(&repo);

        for hostile in ["../escape", "..", "a/b", "/absolute", ""] {
            let err = manager
                .create(hostile, &base, "forge/x")
                .expect_err("hostile workspace name was accepted");
            assert!(
                matches!(err, GitError::InvalidWorkspaceName { .. }),
                "unexpected error for `{hostile}`: {err}"
            );
        }
    }

    #[test]
    fn the_worktree_root_cannot_be_the_repository_root() {
        let repo = TestRepo::new();
        let err = WorktreeManager::new(repo.repository().clone(), repo.path()).unwrap_err();
        assert!(matches!(err, GitError::UnsafePath { .. }), "{err}");
    }

    #[test]
    fn removal_refuses_paths_outside_the_managed_root() {
        let repo = TestRepo::new();
        let manager = manager(&repo);

        // A worktree record pointing at the user's working tree must never be
        // acted on, however it got constructed.
        let hostile = Worktree {
            path: repo.path().to_path_buf(),
            branch: "main".to_string(),
            base_commit: repo.repository().head_commit().unwrap(),
        };
        let err = manager.remove(&hostile, false).unwrap_err();
        assert!(matches!(err, GitError::UnsafePath { .. }), "{err}");
        assert!(repo.path().join("README.md").exists());
    }

    #[test]
    fn removing_a_worktree_keeps_the_branch_so_work_survives() {
        let repo = TestRepo::new();
        let base = repo.repository().head_commit().unwrap();
        let manager = manager(&repo);

        let worktree = manager.create("R-0001", &base, "forge/R-0001").unwrap();
        fs::write(worktree.path().join("new.txt"), "agent work").unwrap();
        worktree.git(["add", "-A"]).unwrap();
        worktree
            .git([
                "-c",
                "user.email=a@b.c",
                "-c",
                "user.name=t",
                "commit",
                "-m",
                "work",
            ])
            .unwrap();
        let head = worktree.head_commit().unwrap();

        manager.remove(&worktree, false).unwrap();

        assert!(!worktree.path().exists());
        assert!(repo.repository().branch_exists("forge/R-0001"));
        assert_eq!(repo.repository().resolve("forge/R-0001").unwrap(), head);
    }

    #[test]
    fn removal_can_also_drop_the_branch() {
        let repo = TestRepo::new();
        let base = repo.repository().head_commit().unwrap();
        let manager = manager(&repo);

        let worktree = manager.create("R-0001", &base, "forge/R-0001").unwrap();
        manager.remove(&worktree, true).unwrap();
        assert!(!repo.repository().branch_exists("forge/R-0001"));
    }

    #[test]
    fn removing_an_already_deleted_worktree_is_not_an_error() {
        let repo = TestRepo::new();
        let base = repo.repository().head_commit().unwrap();
        let manager = manager(&repo);

        let worktree = manager.create("R-0001", &base, "forge/R-0001").unwrap();
        fs::remove_dir_all(worktree.path()).unwrap();

        manager.remove(&worktree, false).unwrap();
        assert!(manager.list().unwrap().is_empty());
    }

    #[test]
    fn duplicate_workspaces_are_refused_rather_than_reused() {
        let repo = TestRepo::new();
        let base = repo.repository().head_commit().unwrap();
        let manager = manager(&repo);

        manager.create("R-0001", &base, "forge/R-0001").unwrap();
        let err = manager.create("R-0001", &base, "forge/other").unwrap_err();
        assert!(matches!(err, GitError::WorkspaceExists(_)), "{err}");
    }

    #[test]
    fn listing_reports_only_managed_worktrees() {
        let repo = TestRepo::new();
        let base = repo.repository().head_commit().unwrap();
        let manager = manager(&repo);

        // A worktree outside the managed root must not appear in the listing
        // and must survive `remove_all`.
        let outside = repo.scratch().join("unmanaged-worktree");
        repo.repository()
            .git([
                "worktree",
                "add",
                "--quiet",
                "-b",
                "unmanaged",
                &outside.to_string_lossy(),
                &base,
            ])
            .unwrap();

        manager.create("R-0001", &base, "forge/R-0001").unwrap();
        manager.create("R-0002", &base, "forge/R-0002").unwrap();

        let listed = manager.list().unwrap();
        assert_eq!(listed.len(), 2);
        assert!(listed.iter().all(|w| w.path().starts_with(manager.root())));

        assert_eq!(manager.remove_all(false).unwrap(), 2);
        assert!(manager.list().unwrap().is_empty());
        assert!(outside.join("README.md").exists());

        repo.repository()
            .git(["worktree", "remove", "--force", &outside.to_string_lossy()])
            .unwrap();
    }
}
