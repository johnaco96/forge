//! Repository handle backed by the `git` CLI.
//!
//! The CLI is used rather than a library binding because it matches what the
//! agents themselves run, needs no build-time dependency, and behaves
//! identically to what a developer would see by hand. A libgit2 backend can
//! replace this behind the same API if it ever pays for itself.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::{GitError, GitResult};

/// A Git repository on the local filesystem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repository {
    root: PathBuf,
}

impl Repository {
    /// Finds the repository containing `start`.
    pub fn discover(start: impl AsRef<Path>) -> GitResult<Self> {
        let start = start.as_ref();
        let output = run_git(start, ["rev-parse", "--show-toplevel"])?;
        let root = PathBuf::from(output.trim());
        Ok(Self {
            root: canonicalize(&root)?,
        })
    }

    /// Opens `root`, requiring it to be a repository root rather than a
    /// subdirectory.
    pub fn open(root: impl AsRef<Path>) -> GitResult<Self> {
        let discovered = Self::discover(root.as_ref())?;
        let requested = canonicalize(root.as_ref())?;
        if discovered.root != requested {
            return Err(GitError::NotRepositoryRoot {
                requested,
                root: discovered.root,
            });
        }
        Ok(discovered)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Canonical shared Git metadata directory.
    ///
    /// Linked worktrees use a small per-worktree `.git` file which points
    /// into this directory. Containerized commands need the shared directory
    /// mounted read-only so ordinary Git inspection still works without
    /// exposing the operator's whole repository checkout.
    pub fn git_common_dir(&self) -> GitResult<PathBuf> {
        let raw = self.git(["rev-parse", "--git-common-dir"])?;
        let path = PathBuf::from(raw.trim());
        let resolved = if path.is_absolute() {
            path
        } else {
            self.root.join(path)
        };
        canonicalize(&resolved)
    }

    /// Directory name of the repository, used as the default logical name.
    pub fn name(&self) -> String {
        self.root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "repository".to_string())
    }

    /// Full SHA of `HEAD`.
    pub fn head_commit(&self) -> GitResult<String> {
        self.resolve("HEAD")
    }

    /// Resolves a revision to a full commit SHA.
    ///
    /// `rev-parse --verify` exits non-zero for anything it cannot resolve, so
    /// the failure is reported as an unknown revision rather than as a generic
    /// Git error.
    pub fn resolve(&self, rev: &str) -> GitResult<String> {
        let arg = format!("{rev}^{{commit}}");
        let out = self
            .git(["rev-parse", "--verify", "--quiet", &arg])
            .map_err(|source| match source {
                GitError::CommandFailed { .. } => GitError::UnknownRevision(rev.to_string()),
                other => other,
            })?;
        let sha = out.trim().to_string();
        if sha.is_empty() {
            return Err(GitError::UnknownRevision(rev.to_string()));
        }
        Ok(sha)
    }

    /// Short SHA, for display only.
    pub fn short(&self, commit: &str) -> String {
        commit.chars().take(7).collect()
    }

    /// Whether the working tree and index have no changes.
    ///
    /// Forge cares because a dirty tree means the base commit does not fully
    /// describe what an agent started from, which breaks reproducibility and
    /// makes two agents' results incomparable.
    pub fn is_clean(&self) -> GitResult<bool> {
        Ok(self
            .git(["status", "--porcelain", "--untracked-files=normal"])?
            .trim()
            .is_empty())
    }

    /// Whether `HEAD` points at a commit (false for a fresh repository).
    pub fn has_commits(&self) -> bool {
        self.head_commit().is_ok()
    }

    pub fn branch_exists(&self, branch: &str) -> bool {
        self.git([
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ])
        .is_ok()
    }

    /// Runs `git` in the repository root.
    pub fn git<I, S>(&self, args: I) -> GitResult<String>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        run_git(&self.root, args)
    }
}

/// Runs `git` in `dir`, returning stdout on success.
pub(crate) fn run_git<I, S>(dir: impl AsRef<Path>, args: I) -> GitResult<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let dir = dir.as_ref();
    let args: Vec<String> = args
        .into_iter()
        .map(|a| a.as_ref().to_string_lossy().into_owned())
        .collect();

    tracing::debug!(dir = %dir.display(), args = ?args, "git");

    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        // Git is part of Forge's trusted measurement/control plane. Repository
        // configuration must not turn a bookkeeping command into host code
        // execution after an untrusted agent has edited the worktree.
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.untrackedCache=false",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(&args)
        // Keep Git non-interactive: a credential or editor prompt inside an
        // unattended agent run would hang until the timeout.
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env_remove("GIT_EXTERNAL_DIFF")
        .env_remove("GIT_DIFF_OPTS")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
        .env_remove("GIT_CONFIG_COUNT")
        .env_remove("GIT_CONFIG_PARAMETERS")
        .output()
        .map_err(|source| {
            if source.kind() == std::io::ErrorKind::NotFound {
                GitError::GitNotFound
            } else {
                GitError::Io {
                    context: format!("running git in {}", dir.display()),
                    source,
                }
            }
        })?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        if stderr.contains("not a git repository") {
            return Err(GitError::NotARepository(dir.to_path_buf()));
        }
        Err(GitError::CommandFailed {
            command: format!("git {}", args.join(" ")),
            code: output.status.code(),
            stderr,
        })
    }
}

pub(crate) fn canonicalize(path: &Path) -> GitResult<PathBuf> {
    path.canonicalize().map_err(|source| GitError::Io {
        context: format!("resolving {}", path.display()),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestRepo;

    #[test]
    fn discovers_a_repository_from_a_subdirectory() {
        let repo = TestRepo::new();
        let nested = repo.path().join("src/deep");
        std::fs::create_dir_all(&nested).unwrap();

        let discovered = Repository::discover(&nested).unwrap();
        assert_eq!(discovered.root(), repo.repository().root());
    }

    #[test]
    fn open_requires_the_repository_root() {
        let repo = TestRepo::new();
        let nested = repo.path().join("src");
        std::fs::create_dir_all(&nested).unwrap();

        let err = Repository::open(&nested).unwrap_err();
        assert!(matches!(err, GitError::NotRepositoryRoot { .. }), "{err}");
    }

    #[test]
    fn reports_a_helpful_error_outside_a_repository() {
        let dir = tempfile::tempdir().unwrap();
        // A temp dir could sit inside an enclosing repository on some machines;
        // only assert when it genuinely is not in one.
        if let Err(err) = Repository::discover(dir.path()) {
            assert!(matches!(err, GitError::NotARepository(_)), "{err}");
        }
    }

    #[test]
    fn resolves_head_and_rejects_unknown_revisions() {
        let repo = TestRepo::new();
        let head = repo.repository().head_commit().unwrap();
        assert_eq!(head.len(), 40);
        assert_eq!(repo.repository().resolve("HEAD").unwrap(), head);

        let err = repo.repository().resolve("no-such-ref").unwrap_err();
        assert!(matches!(err, GitError::UnknownRevision(_)), "{err}");
    }

    #[test]
    fn detects_a_dirty_working_tree() {
        let repo = TestRepo::new();
        assert!(repo.repository().is_clean().unwrap());

        std::fs::write(repo.path().join("dirty.txt"), "uncommitted").unwrap();
        assert!(!repo.repository().is_clean().unwrap());
    }

    #[test]
    fn knows_which_branches_exist() {
        let repo = TestRepo::new();
        assert!(repo.repository().branch_exists("main"));
        assert!(!repo.repository().branch_exists("forge/nope"));
    }
}
