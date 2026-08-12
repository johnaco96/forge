//! End-to-end `forge health` tests, driving the real binary.
//!
//! No agents, no network: world models come from the deterministic Phase 6
//! extractors, and health is built from whatever evidence the ledger holds.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Fixture {
    _temp: tempfile::TempDir,
    repo: PathBuf,
}

impl Fixture {
    /// A committed repository with Forge initialized.
    fn new() -> Self {
        let temp = tempfile::tempdir().expect("temp dir");
        let repo = temp.path().join("health-fixture");
        std::fs::create_dir_all(repo.join("src")).unwrap();

        git(&repo, &["init", "--quiet", "--initial-branch=main"]);
        git(&repo, &["config", "user.email", "health@example.invalid"]);
        git(&repo, &["config", "user.name", "Health Fixture"]);
        std::fs::write(repo.join("README.md"), "# health fixture\n").unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "--quiet", "-m", "initial commit"]);

        let fixture = Self { _temp: temp, repo };
        assert!(fixture.forge(&["init"]).status.success());
        // `forge init` writes tracked config; commit it so the tree is clean.
        git(&fixture.repo, &["add", "-A"]);
        git(&fixture.repo, &["commit", "--quiet", "-m", "forge init"]);
        fixture
    }

    fn forge(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_forge"))
            .args(args)
            .current_dir(&self.repo)
            .output()
            .expect("run forge")
    }
}

fn git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("git");
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
fn health_build_requires_an_exact_world_model_first() {
    let fixture = Fixture::new();
    let output = fixture.forge(&["health", "build"]);

    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("forge world build"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn health_build_records_a_snapshot_and_reports_availability_honestly() {
    let fixture = Fixture::new();
    assert!(fixture.forge(&["world", "build"]).status.success());

    let output = fixture.forge(&["health", "build"]);
    let text = stdout(&output);

    // A repository with no evaluation history is legitimately partial.
    assert!(text.contains("Forge repository health H-0001"), "{text}");
    assert!(text.contains("World model"), "{text}");
    assert!(text.contains("Dimensions"), "{text}");
    assert!(text.contains("unavailable"), "{text}");
    // Nothing is invented for dimensions with no evidence.
    assert!(!text.contains("NaN"), "{text}");
}

#[test]
fn health_build_refuses_a_dirty_working_tree() {
    // Health describes a commit; a dirty tree is not one.
    let fixture = Fixture::new();
    assert!(fixture.forge(&["world", "build"]).status.success());
    std::fs::write(fixture.repo.join("uncommitted.txt"), "dirty\n").unwrap();

    let output = fixture.forge(&["health", "build"]);
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("uncommitted changes"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn health_show_displays_provenance_and_missing_dimensions() {
    let fixture = Fixture::new();
    assert!(fixture.forge(&["world", "build"]).status.success());
    fixture.forge(&["health", "build"]);

    let output = fixture.forge(&["health", "show"]);
    let text = stdout(&output);

    assert!(output.status.success(), "{}", stderr(&output));
    assert!(text.contains("World model"), "{text}");
    assert!(text.contains("health-builder-v1"), "{text}");
    assert!(text.contains("Not measurable here"), "{text}");
}

#[test]
fn health_show_reports_a_missing_snapshot_rather_than_inventing_one() {
    let fixture = Fixture::new();
    let output = fixture.forge(&["health", "show"]);
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("forge health build"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn health_trend_reports_insufficient_data_rather_than_a_manufactured_trend() {
    let fixture = Fixture::new();
    assert!(fixture.forge(&["world", "build"]).status.success());
    fixture.forge(&["health", "build"]);

    let output = fixture.forge(&["health", "trend"]);
    let text = stdout(&output);

    assert!(output.status.success(), "{}", stderr(&output));
    assert!(text.contains("Repository trend"), "{text}");
    // One snapshot cannot be a trend.
    assert!(
        text.contains("InsufficientData") || text.contains("no comparable measurements"),
        "{text}"
    );
    assert!(text.contains("longitudinal-trend-v1"), "{text}");
}

#[test]
fn health_diff_needs_a_comparable_baseline() {
    let fixture = Fixture::new();
    assert!(fixture.forge(&["world", "build"]).status.success());
    fixture.forge(&["health", "build"]);

    // Only one snapshot exists, so there is no earlier ancestor to compare to.
    let output = fixture.forge(&["health", "diff"]);
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("ancestry chain"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn health_diff_compares_two_commits_across_real_ancestry() {
    let fixture = Fixture::new();
    assert!(fixture.forge(&["world", "build"]).status.success());
    assert!(fixture.forge(&["health", "build"]).status.code().is_some());

    // A second commit, with its own world model and health snapshot.
    std::fs::write(fixture.repo.join("src/added.txt"), "more\n").unwrap();
    git(&fixture.repo, &["add", "-A"]);
    git(&fixture.repo, &["commit", "--quiet", "-m", "second commit"]);
    assert!(fixture.forge(&["world", "build"]).status.success());
    fixture.forge(&["health", "build"]);

    let output = fixture.forge(&["health", "diff"]);
    let text = stdout(&output);

    assert!(output.status.success(), "{}", stderr(&output));
    assert!(text.contains("Forge health diff"), "{text}");
    // The default baseline is the nearest prior ancestor, so this is a real
    // chronology and must not be flagged as diverged.
    assert!(text.contains("ancestor → descendant"), "{text}");
    assert!(!text.contains("not on one ancestry chain"), "{text}");
    assert!(text.contains("longitudinal-trend-v1"), "{text}");
}

#[test]
fn health_snapshots_accumulate_across_commits() {
    let fixture = Fixture::new();
    for index in 0..3 {
        if index > 0 {
            std::fs::write(
                fixture.repo.join(format!("src/file{index}.txt")),
                format!("{index}\n"),
            )
            .unwrap();
            git(&fixture.repo, &["add", "-A"]);
            git(&fixture.repo, &["commit", "--quiet", "-m", "next"]);
        }
        assert!(fixture.forge(&["world", "build"]).status.success());
        fixture.forge(&["health", "build"]);
    }

    let output = fixture.forge(&["health", "trend"]);
    let text = stdout(&output);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(text.contains("3 health snapshots"), "{text}");
}

#[test]
fn phase_zero_to_six_commands_remain_green_after_the_health_migration() {
    // The Phase 7 migration is additive; earlier surfaces must be unaffected.
    let fixture = Fixture::new();
    for args in [
        vec!["agent", "list"],
        vec!["history"],
        vec!["experiments", "list"],
        vec!["world", "build"],
        vec!["world", "show"],
    ] {
        let output = fixture.forge(&args);
        assert!(
            output.status.success(),
            "`forge {}` failed: {}",
            args.join(" "),
            stderr(&output)
        );
    }
}
