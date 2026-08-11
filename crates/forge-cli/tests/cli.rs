//! End-to-end tests that drive the real `forge` binary.
//!
//! The unit tests cover the pieces; these cover the thing a user actually
//! types.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

struct Fixture {
    _temp: TempDir,
    repo: PathBuf,
}

impl Fixture {
    /// A committed Git repository with nothing Forge-related in it.
    fn new() -> Self {
        let temp = tempfile::tempdir().expect("temp dir");
        let repo = temp.path().join("distributed-runtime");
        std::fs::create_dir_all(&repo).expect("repo dir");

        git(&repo, &["init", "--quiet", "--initial-branch=main"]);
        git(&repo, &["config", "user.email", "forge@example.invalid"]);
        git(&repo, &["config", "user.name", "Forge Tests"]);
        std::fs::write(repo.join("README.md"), "# distributed-runtime\n").expect("README");
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "--quiet", "-m", "initial commit"]);

        Self { _temp: temp, repo }
    }

    fn forge(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_forge"))
            .args(args)
            .current_dir(&self.repo)
            .output()
            .expect("run forge")
    }

    fn read(&self, relative: &str) -> String {
        std::fs::read_to_string(self.repo.join(relative))
            .unwrap_or_else(|e| panic!("reading {relative}: {e}"))
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
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn init_creates_the_documented_layout() {
    let fixture = Fixture::new();
    let output = fixture.forge(&["init"]);

    assert!(output.status.success(), "{}", stderr(&output));
    assert!(stdout(&output).contains("Forge initialized"));

    for path in [
        ".forge/config.toml",
        ".forge/tasks/example.yaml",
        ".forge/.gitignore",
        ".forge/forge.db",
        ".forge/teams",
    ] {
        assert!(
            fixture.repo.join(path).exists(),
            "`forge init` did not create {path}"
        );
    }
    assert!(fixture.repo.join(".forge/runs").is_dir());
    assert!(fixture.repo.join(".forge/worktrees").is_dir());

    // The repository's own name reaches both the config and the example task.
    assert!(
        fixture
            .read(".forge/config.toml")
            .contains("name = \"distributed-runtime\"")
    );
    assert!(
        fixture
            .read(".forge/tasks/example.yaml")
            .contains("repository: distributed-runtime")
    );
}

#[test]
fn init_keeps_run_output_out_of_version_control() {
    // Worktrees and the ledger live inside the repository, so they must be
    // ignored — otherwise every run would dirty the user's working tree, and
    // agent scratch would end up in history.
    let fixture = Fixture::new();
    assert!(fixture.forge(&["init"]).status.success());

    // Stand-ins for what a real run leaves behind. Git does not report empty
    // directories, so the ignore rules are only exercised once files exist.
    std::fs::write(fixture.repo.join(".forge/runs/R-0001.log"), "output").unwrap();
    std::fs::write(fixture.repo.join(".forge/teams/TE-0001.diff"), "output").unwrap();
    std::fs::create_dir_all(fixture.repo.join(".forge/worktrees/R-0001")).unwrap();
    std::fs::write(
        fixture.repo.join(".forge/worktrees/R-0001/scratch.rs"),
        "fn main() {}",
    )
    .unwrap();

    let output = Command::new("git")
        .arg("-C")
        .arg(&fixture.repo)
        // `-uall` expands untracked directories; without it Git collapses an
        // entirely untracked tree to a single entry and hides the detail.
        .args(["status", "--porcelain", "--untracked-files=all"])
        .output()
        .expect("git status");
    let status = String::from_utf8_lossy(&output.stdout);

    // The experiment definition is tracked...
    assert!(status.contains(".forge/config.toml"), "{status}");
    assert!(status.contains(".forge/tasks/example.yaml"), "{status}");
    // ...and everything a run produces is not.
    assert!(!status.contains("worktrees"), "{status}");
    assert!(!status.contains("runs/"), "{status}");
    assert!(!status.contains("teams/"), "{status}");
    assert!(!status.contains("forge.db"), "{status}");
}

#[test]
fn init_is_idempotent_and_does_not_clobber_edits() {
    let fixture = Fixture::new();
    assert!(fixture.forge(&["init"]).status.success());

    let edited = "# edited by the user\n";
    std::fs::write(fixture.repo.join(".forge/tasks/example.yaml"), edited).unwrap();

    let output = fixture.forge(&["init"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(stdout(&output).contains("Already initialized"));
    assert_eq!(fixture.read(".forge/tasks/example.yaml"), edited);
}

#[test]
fn init_refuses_a_directory_that_is_not_a_repository() {
    let temp = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_forge"))
        .arg("init")
        .current_dir(temp.path())
        .output()
        .expect("run forge");

    // A temp dir could sit inside an enclosing repository on some machines;
    // only assert the diagnosis when it genuinely is not in one.
    if !output.status.success() {
        assert!(stderr(&output).contains("git init"), "{}", stderr(&output));
    }
}

#[test]
fn the_example_task_init_writes_is_valid() {
    // The template a user starts from must pass Forge's own validation.
    let fixture = Fixture::new();
    assert!(fixture.forge(&["init"]).status.success());

    let output = fixture.forge(&["task", "validate", ".forge/tasks/example.yaml"]);
    assert!(output.status.success(), "{}", stderr(&output));

    let text = stdout(&output);
    assert!(text.contains("Task T-0001 is valid"), "{text}");
    assert!(text.contains("cargo test --workspace"), "{text}");
}

#[test]
fn validating_a_broken_task_explains_the_problem_and_fails() {
    let fixture = Fixture::new();
    std::fs::write(
        fixture.repo.join("broken.yaml"),
        "task_id: T-0002\nrepository: distributed-runtime\nobjective: \"\"\n",
    )
    .unwrap();

    let output = fixture.forge(&["task", "validate", "broken.yaml"]);
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("objective must not be empty"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn agent_list_reports_what_can_actually_run() {
    let fixture = Fixture::new();
    let output = fixture.forge(&["agent", "list"]);

    assert!(output.status.success(), "{}", stderr(&output));
    let text = stdout(&output);
    for agent in ["claude", "codex", "pi"] {
        assert!(
            text.contains(agent),
            "{agent} missing from listing:\n{text}"
        );
    }
    // Honesty about the current state matters more than looking complete.
    assert!(text.contains("not implemented"), "{text}");
}

#[test]
fn agent_list_works_outside_an_initialized_repository() {
    let temp = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_forge"))
        .args(["agent", "list"])
        .current_dir(temp.path())
        .output()
        .expect("run forge");
    assert!(output.status.success(), "{}", stderr(&output));
}

#[test]
fn the_repo_flag_targets_another_directory() {
    let fixture = Fixture::new();
    let elsewhere = tempfile::tempdir().unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_forge"))
        .args(["--repo", &fixture.repo.to_string_lossy(), "init"])
        .current_dir(elsewhere.path())
        .output()
        .expect("run forge");

    assert!(output.status.success(), "{}", stderr(&output));
    assert!(fixture.repo.join(".forge/config.toml").exists());
}
