//! Offline end-to-end coverage for `forge world build/show/query`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use forge_core::ids::WorldModelSnapshotId;
use forge_store::Store;
use tempfile::TempDir;

struct Fixture {
    _temp: TempDir,
    repo: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("world-fixture");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "--quiet", "--initial-branch=main"]);
        git(&repo, &["config", "user.email", "forge@example.invalid"]);
        git(&repo, &["config", "user.name", "Forge Tests"]);
        std::fs::write(
            repo.join("Cargo.toml"),
            "[workspace]\nresolver = \"3\"\nmembers = [\"crates/*\"]\n",
        )
        .unwrap();
        write_crate(&repo, "core", None);
        write_crate(&repo, "api", Some("core"));
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "--quiet", "-m", "initial workspace"]);
        let fixture = Self { _temp: temp, repo };
        let initialized = fixture.forge(&["init"]);
        assert!(initialized.status.success(), "{}", stderr(&initialized));
        git(&fixture.repo, &["add", ".forge"]);
        git(
            &fixture.repo,
            &["commit", "--quiet", "-m", "initialize Forge"],
        );
        fixture
    }

    fn forge(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_forge"))
            .args(args)
            .current_dir(&self.repo)
            .output()
            .unwrap()
    }
}

fn write_crate(root: &Path, name: &str, dependency: Option<&str>) {
    let crate_root = root.join("crates").join(name);
    std::fs::create_dir_all(crate_root.join("src")).unwrap();
    let dependency = dependency
        .map(|dependency| {
            format!("\n[dependencies]\n{dependency} = {{ path = \"../{dependency}\" }}\n")
        })
        .unwrap_or_default();
    std::fs::write(
        crate_root.join("Cargo.toml"),
        format!(
            "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2024\"\n{dependency}"
        ),
    )
    .unwrap();
    std::fs::write(crate_root.join("src/lib.rs"), "pub fn marker() {}\n").unwrap();
}

fn git(directory: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(directory)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git_stdout(directory: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(directory)
        .args(args)
        .output()
        .unwrap();
    assert!(output.status.success());
    String::from_utf8_lossy(&output.stdout).trim().into()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[tokio::test]
async fn build_show_query_and_rebuild_preserve_commit_bound_history() {
    let fixture = Fixture::new();
    let first_commit = git_stdout(&fixture.repo, &["rev-parse", "HEAD"]);
    let built = fixture.forge(&["world", "build"]);
    assert!(built.status.success(), "{}", stderr(&built));
    assert!(stdout(&built).contains("Forge world model WM-0001"));
    assert!(stdout(&built).contains("complete"));

    let shown = fixture.forge(&["world", "show"]);
    assert!(shown.status.success(), "{}", stderr(&shown));
    let shown_text = stdout(&shown);
    assert!(
        shown_text
            .lines()
            .any(|line| { line.contains("Current pointer") && line.trim_end().ends_with("yes") })
    );
    assert!(
        shown_text.lines().any(|line| {
            line.contains("Relation to HEAD") && line.trim_end().ends_with("exact")
        })
    );

    let queried = fixture.forge(&["world", "query", "dependencies", "core"]);
    assert!(queried.status.success(), "{}", stderr(&queried));
    assert!(stdout(&queried).contains("dependency"));
    assert!(
        stdout(&queried)
            .lines()
            .any(|line| { line.contains("Matches") && line.trim_end().ends_with('1') })
    );

    let store = Store::open(fixture.repo.join(".forge/forge.db"))
        .await
        .unwrap();
    let first = store
        .load_world_model_snapshot(&WorldModelSnapshotId::sequential(1))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.commit, first_commit);
    assert!(first.facts.components.len() >= 2);
    assert!(!first.facts.dependencies.is_empty());
    assert!(
        first
            .facts
            .records()
            .iter()
            .all(|fact| !fact.metadata().provenance.is_empty())
    );

    write_crate(&fixture.repo, "worker", Some("core"));
    git(&fixture.repo, &["add", "crates/worker"]);
    git(&fixture.repo, &["commit", "--quiet", "-m", "add worker"]);
    let second_commit = git_stdout(&fixture.repo, &["rev-parse", "HEAD"]);
    let rebuilt = fixture.forge(&["world", "build"]);
    assert!(rebuilt.status.success(), "{}", stderr(&rebuilt));
    assert!(stdout(&rebuilt).contains("Forge world model WM-0002"));

    let second = store
        .load_world_model_snapshot(&WorldModelSnapshotId::sequential(2))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(second.commit, second_commit);
    assert_eq!(
        store
            .load_world_model_snapshot(&WorldModelSnapshotId::sequential(1))
            .await
            .unwrap()
            .unwrap(),
        first
    );
    assert!(!first.diff(&second).added.is_empty());
    assert_eq!(
        store
            .current_world_model("world-fixture")
            .await
            .unwrap()
            .unwrap()
            .snapshot_id,
        WorldModelSnapshotId::sequential(2)
    );

    let old = fixture.forge(&["world", "show", "WM-0001"]);
    assert!(old.status.success(), "{}", stderr(&old));
    assert!(stdout(&old).lines().any(|line| {
        line.contains("Relation to HEAD") && line.trim_end().ends_with("ancestor")
    }));
    let worker = fixture.forge(&[
        "world",
        "query",
        "component",
        "worker",
        "--snapshot",
        "WM-0002",
    ]);
    assert!(worker.status.success(), "{}", stderr(&worker));
    assert!(stdout(&worker).contains("worker"));
}

#[test]
fn build_refuses_to_mislabel_dirty_repository_content() {
    let fixture = Fixture::new();
    std::fs::write(fixture.repo.join("Cargo.toml"), "# dirty\n").unwrap();
    let output = fixture.forge(&["world", "build"]);
    assert!(!output.status.success());
    assert!(stderr(&output).contains("clean repository checkout"));
}
