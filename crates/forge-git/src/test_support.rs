//! A throwaway Git repository for tests.

use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

use crate::repository::Repository;

/// A committed repository inside a temporary directory.
///
/// Laid out as `<temp>/repo`, leaving `<temp>` available as scratch space for
/// paths that must sit outside the repository.
pub struct TestRepo {
    temp: TempDir,
    repo: Repository,
}

impl TestRepo {
    pub fn new() -> Self {
        let temp = tempfile::tempdir().expect("temp dir");
        let root = temp.path().join("repo");
        std::fs::create_dir_all(&root).expect("repo dir");

        git(&root, &["init", "--quiet", "--initial-branch=main"]);
        // Identity and signing are set locally so the tests do not depend on
        // (or disturb) the developer's global Git configuration.
        git(
            &root,
            &["config", "user.email", "forge-tests@example.invalid"],
        );
        git(&root, &["config", "user.name", "Forge Tests"]);
        git(&root, &["config", "commit.gpgsign", "false"]);

        std::fs::write(root.join("README.md"), "# test repo\n").expect("write README");
        git(&root, &["add", "-A"]);
        git(&root, &["commit", "--quiet", "-m", "initial commit"]);

        let repo = Repository::open(&root).expect("open repository");
        Self { temp, repo }
    }

    /// Repository root.
    pub fn path(&self) -> &Path {
        self.repo.root()
    }

    /// Temporary directory containing the repository; usable for paths that
    /// must live outside it.
    pub fn scratch(&self) -> PathBuf {
        self.temp.path().to_path_buf()
    }

    pub fn repository(&self) -> &Repository {
        &self.repo
    }

    /// Writes a file relative to the repository root, creating parents.
    pub fn write(&self, relative: &str, contents: &str) {
        let path = self.path().join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("parent dir");
        }
        std::fs::write(path, contents).expect("write file");
    }

    /// Stages everything and commits, returning the new commit SHA.
    pub fn commit(&self, message: &str) -> String {
        git(self.path(), &["add", "-A"]);
        git(self.path(), &["commit", "--quiet", "-m", message]);
        self.repo.head_commit().expect("head commit")
    }
}

impl Default for TestRepo {
    fn default() -> Self {
        Self::new()
    }
}

fn git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
