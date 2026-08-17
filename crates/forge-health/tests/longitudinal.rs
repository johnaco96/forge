//! Controlled longitudinal smoke: a real three-commit repository history.
//!
//! Exercises the real builder, the real SQLite store, real Git ancestry, and
//! the real analyzer — not constructed analyzer fixtures. World models and run
//! evidence are inserted through the ordinary store API, exactly as `forge
//! world build` and `forge run` would.
//!
//! This is infrastructure validation. The numbers are chosen to make the
//! classification rules observable; they are not a claim about any repository.

use std::path::Path;
use std::process::Command;

use chrono::{TimeDelta, Utc};
use forge_core::agent::AgentConfig;
use forge_core::health::{
    HealthDimensionKind, HealthSnapshotStatus, MaterialityPolicy, TrendDirection,
};
use forge_core::ids::{AgentId, HealthSnapshotId, RunId, TaskId, WorldModelSnapshotId};
use forge_core::result::{
    CheckResult, Direction, Evaluation, EvaluatorExecutionStatus, EvaluatorKind, Metric, Verdict,
};
use forge_core::run::{AgentRun, PatchSummary, RunOutcome, RunStatus};
use forge_core::task::{EngineeringTask, EvaluationSpec, TaskMetadata};
use forge_core::world::{
    Component, DependencyKind, EvidenceConfidence, ExtractorIdentity, ExtractorRecord,
    ExtractorStatus, FactMetadata, RepositoryPath, SourceLocation, WORLD_MODEL_SCHEMA_VERSION,
    WorldEntityKind, WorldEntityRef, WorldModelFacts, WorldModelProvenance,
    WorldModelProvenanceSource, WorldModelSnapshot, WorldModelSnapshotSource,
    WorldModelSnapshotStatus,
};
use forge_git::Repository;
use forge_health::{GitAncestry, RepositoryHealthBuilder};
use forge_store::Store;

const REPOSITORY: &str = "longitudinal-fixture";

/// A repository with three real commits.
struct History {
    _temp: tempfile::TempDir,
    repository: Repository,
    commits: Vec<String>,
}

impl History {
    fn new() -> Self {
        let temp = tempfile::tempdir().expect("temp dir");
        let root = temp.path().join(REPOSITORY);
        std::fs::create_dir_all(&root).unwrap();

        git(&root, &["init", "--quiet", "--initial-branch=main"]);
        git(&root, &["config", "user.email", "health@example.invalid"]);
        git(&root, &["config", "user.name", "Health Fixture"]);

        let mut commits = Vec::new();
        for (index, contents) in ["baseline", "adds a dependency", "complexity increases"]
            .iter()
            .enumerate()
        {
            std::fs::write(root.join("src.txt"), format!("{index}: {contents}\n")).unwrap();
            git(&root, &["add", "-A"]);
            git(&root, &["commit", "--quiet", "-m", contents]);
            commits.push(
                String::from_utf8(
                    Command::new("git")
                        .arg("-C")
                        .arg(&root)
                        .args(["rev-parse", "HEAD"])
                        .output()
                        .unwrap()
                        .stdout,
                )
                .unwrap()
                .trim()
                .to_string(),
            );
        }

        let repository = Repository::open(&root).expect("open repository");
        Self {
            _temp: temp,
            repository,
            commits,
        }
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

/// A world model whose dependency-edge count is `dependencies`.
fn world_model(n: u64, commit: &str, dependencies: u64) -> WorldModelSnapshot {
    let snapshot_id = WorldModelSnapshotId::sequential(n);
    let provenance = |commit: &str| WorldModelProvenance {
        extractor: ExtractorIdentity::new("fixture", "1"),
        source: WorldModelProvenanceSource::SourceCode {
            location: SourceLocation::new(RepositoryPath::new("src.txt").unwrap(), commit),
        },
    };
    let metadata = |id: &str| FactMetadata {
        id: forge_core::ids::WorldModelFactId::new(id).unwrap(),
        snapshot_id: snapshot_id.clone(),
        confidence: EvidenceConfidence::Observed,
        provenance: vec![provenance(commit)],
        contradicts: Vec::new(),
    };

    let mut facts = WorldModelFacts::default();
    // Two components so dependency edges have real endpoints.
    for name in ["core", "storage"] {
        facts.components.push(Component {
            metadata: metadata(&format!("WF-component-{name}")),
            name: name.to_string(),
            description: format!("{name} component"),
            paths: vec![RepositoryPath::new("src.txt").unwrap()],
            parent: None,
            tags: Vec::new(),
            related_tasks: Vec::new(),
        });
    }
    for index in 0..dependencies {
        facts.dependencies.push(forge_core::world::Dependency {
            metadata: metadata(&format!("WF-dependency-{index}")),
            source: WorldEntityRef {
                kind: WorldEntityKind::Component,
                id: forge_core::ids::WorldModelFactId::new("WF-component-core").unwrap(),
            },
            target: WorldEntityRef {
                kind: WorldEntityKind::Component,
                id: forge_core::ids::WorldModelFactId::new("WF-component-storage").unwrap(),
            },
            dependency_kind: DependencyKind::DependsOn,
            evidence: None,
        });
    }

    WorldModelSnapshot {
        snapshot_id,
        repository: REPOSITORY.to_string(),
        commit: commit.to_string(),
        created_at: Utc::now(),
        source: WorldModelSnapshotSource::Deterministic,
        schema_version: WORLD_MODEL_SCHEMA_VERSION.to_string(),
        status: WorldModelSnapshotStatus::Complete,
        extractors: vec![ExtractorRecord {
            identity: ExtractorIdentity::new("fixture", "1"),
            required: true,
            status: ExtractorStatus::Completed,
            facts_produced: 2 + dependencies,
            configuration_fingerprint: "fixture-fp".to_string(),
            error: None,
        }],
        facts,
    }
}

fn task() -> EngineeringTask {
    EngineeringTask {
        task_id: TaskId::sequential(1),
        repository: REPOSITORY.to_string(),
        objective: "Keep the fixture measurable across commits".to_string(),
        constraints: Vec::new(),
        evaluation: EvaluationSpec::default(),
        protection: Default::default(),
        metadata: TaskMetadata::default(),
        classification: Default::default(),
        components: Vec::new(),
        tags: Vec::new(),
    }
}

fn check(name: &str, kind: EvaluatorKind, metrics: Vec<Metric>, duration_ms: u64) -> CheckResult {
    CheckResult {
        name: name.to_string(),
        kind,
        required: true,
        verdict: Verdict::Pass,
        execution_status: EvaluatorExecutionStatus::Completed,
        // Identical command at every commit, so the series is comparable.
        command: Some(format!("./{name}.sh")),
        exit_code: Some(0),
        duration_ms,
        detail: None,
        output_path: None,
        metrics,
        warnings: Vec::new(),
        execution_error: None,
        infrastructure_failures: Vec::new(),
    }
}

/// Records a run whose candidate was committed as `head`, carrying a build
/// duration and a throughput metric.
async fn record_run(
    store: &Store,
    run_number: u64,
    base: &str,
    head: &str,
    build_ms: u64,
    throughput: f64,
    extra: Vec<CheckResult>,
) {
    let run_id = RunId::sequential(run_number);
    let mut run = AgentRun::new(
        run_id.clone(),
        TaskId::sequential(1),
        AgentConfig::new(AgentId::new("claude").unwrap(), "claude-code"),
        base,
    );
    run.transition_to(RunStatus::Preparing).unwrap();
    run.transition_to(RunStatus::Running).unwrap();
    run.transition_to(RunStatus::Evaluating).unwrap();
    run.patch = Some(PatchSummary {
        base_commit: base.to_string(),
        head_commit: Some(head.to_string()),
        files_changed: 1,
        insertions: 1,
        deletions: 1,
        binary_files: 0,
        diff_path: None,
        excluded: Vec::new(),
        excluded_counts: Default::default(),
    });
    run.evaluation_verdict = Some(Verdict::Pass);
    run.outcome = Some(RunOutcome::Passed);
    run.transition_to(RunStatus::Completed).unwrap();

    store.save_run(&run, None).await.unwrap();
    store
        .record_patch(&run_id, run.patch.as_ref().unwrap())
        .await
        .unwrap();

    let mut checks = vec![
        check("build", EvaluatorKind::Build, Vec::new(), build_ms),
        check(
            "benchmark",
            EvaluatorKind::Benchmark,
            vec![Metric::new(
                "throughput",
                throughput,
                "benchmark",
                Direction::HigherIsBetter,
            )],
            10,
        ),
        check("tests", EvaluatorKind::Test, Vec::new(), 50),
    ];
    checks.extend(extra);

    let now = Utc::now();
    store
        .record_evaluation(&Evaluation::from_checks(run_id, checks, now, now))
        .await
        .unwrap();
}

/// Builds a health snapshot at `commit` through the real builder and store.
async fn build_health(
    store: &Store,
    history: &History,
    n: u64,
    commit: &str,
    world: &WorldModelSnapshot,
) -> forge_core::health::RepositoryHealthSnapshot {
    let evidence = store.health_run_evidence(1000).await.unwrap();
    let report = RepositoryHealthBuilder::new()
        .build(
            HealthSnapshotId::sequential(n),
            REPOSITORY,
            commit,
            world,
            &evidence,
            &GitAncestry {
                repository: &history.repository,
            },
        )
        .expect("health build");

    store
        .insert_health_snapshot(&report.snapshot)
        .await
        .unwrap();
    store.append_health_events(&report.events).await.unwrap();
    store
        .set_current_health_snapshot(REPOSITORY, &report.snapshot)
        .await
        .unwrap();
    report.snapshot
}

#[tokio::test]
async fn three_commits_produce_an_evidence_backed_mixed_repository_trend() {
    let history = History::new();
    let (a, b, c) = (
        history.commits[0].clone(),
        history.commits[1].clone(),
        history.commits[2].clone(),
    );

    let store = Store::open_in_memory().await.unwrap();
    store.upsert_task(&task()).await.unwrap();

    // Commit A: 2 dependencies, build 100ms, throughput 1000.
    let world_a = world_model(1, &a, 2);
    store.insert_world_model_snapshot(&world_a).await.unwrap();
    record_run(&store, 1, &a, &a, 100, 1000.0, Vec::new()).await;
    let health_a = build_health(&store, &history, 1, &a, &world_a).await;

    // Commit B: 3 dependencies, build 108ms, throughput 1150.
    let world_b = world_model(2, &b, 3);
    store.insert_world_model_snapshot(&world_b).await.unwrap();
    record_run(&store, 2, &a, &b, 108, 1150.0, Vec::new()).await;
    let health_b = build_health(&store, &history, 2, &b, &world_b).await;

    // Commit C: build 116ms, throughput 1170, and complexity now reported.
    let world_c = world_model(3, &c, 3);
    store.insert_world_model_snapshot(&world_c).await.unwrap();
    record_run(
        &store,
        3,
        &b,
        &c,
        116,
        1170.0,
        vec![check(
            "complexity",
            EvaluatorKind::Complexity,
            vec![Metric::new(
                "cyclomatic_complexity",
                42.0,
                "complexity",
                Direction::LowerIsBetter,
            )],
            5,
        )],
    )
    .await;
    let health_c = build_health(&store, &history, 3, &c, &world_c).await;

    // ---- each snapshot is bound to its own exact commit ----
    assert_eq!(health_a.commit, a);
    assert_eq!(health_b.commit, b);
    assert_eq!(health_c.commit, c);
    assert_eq!(health_a.world_model_snapshot_id, world_a.snapshot_id);

    // Candidate evidence attached to the head commit, not the base.
    assert_eq!(
        health_b
            .dimension(HealthDimensionKind::RuntimePerformance)
            .unwrap()
            .measurement("throughput")
            .unwrap()
            .value,
        1150.0,
        "commit B must carry the run whose candidate became B"
    );
    assert_eq!(
        health_a
            .dimension(HealthDimensionKind::RuntimePerformance)
            .unwrap()
            .measurement("throughput")
            .unwrap()
            .value,
        1000.0,
        "commit A must not absorb the later run's throughput"
    );

    // ---- structural evidence from the exact world model ----
    let deps = |snapshot: &forge_core::health::RepositoryHealthSnapshot| {
        snapshot
            .dimension(HealthDimensionKind::DependencyCount)
            .unwrap()
            .measurement("dependency_count")
            .unwrap()
            .value
    };
    assert_eq!(deps(&health_a), 2.0);
    assert_eq!(deps(&health_b), 3.0);

    // ---- A → B diff ----
    let ab = forge_health::diff(
        &health_a,
        &health_b,
        forge_world::snapshot_relation(&history.repository, &a, &b),
        &MaterialityPolicy::default(),
    );
    assert!(ab.is_chronological(), "B descends from A");
    assert!(
        ab.improvements
            .iter()
            .any(|change| change.identity.metric == "throughput"),
        "throughput 1000 → 1150 is an improvement"
    );
    assert!(
        ab.regressions
            .iter()
            .any(|change| change.identity.metric == "build_duration_ms"),
        "build 100 → 108 is a regression"
    );
    assert!(
        ab.neutral_changes
            .iter()
            .any(|change| change.identity.metric == "dependency_count"
                && change.delta == Some(1.0)),
        "a dependency count change is neither good nor bad"
    );

    // ---- B → C diff, including a newly available metric ----
    let bc = forge_health::diff(
        &health_b,
        &health_c,
        forge_world::snapshot_relation(&history.repository, &b, &c),
        &MaterialityPolicy::default(),
    );
    assert!(
        bc.newly_available
            .iter()
            .any(|change| change.identity.metric == "cyclomatic_complexity"),
        "complexity appears at C for the first time"
    );
    // Never reported as an infinite improvement.
    assert!(
        bc.newly_available
            .iter()
            .all(|change| change.percent_change.is_none())
    );

    // ---- trends across the whole chain ----
    let series = store.health_snapshots(REPOSITORY, 100).await.unwrap();
    assert_eq!(series.len(), 3);
    let trends = forge_health::trends(REPOSITORY, &series, &MaterialityPolicy::default());

    assert_eq!(
        trends.direction_for(HealthDimensionKind::BuildTime),
        Some(TrendDirection::Degrading),
        "100 → 108 → 116 ms"
    );
    assert_eq!(
        trends.direction_for(HealthDimensionKind::RuntimePerformance),
        Some(TrendDirection::Improving),
        "1000 → 1150 → 1170"
    );
    assert_eq!(
        trends.direction_for(HealthDimensionKind::DependencyCount),
        Some(TrendDirection::Changing),
        "2 → 3 → 3 is structural movement, not a verdict"
    );
    // Disagreement is reported as disagreement.
    assert_eq!(trends.overall, TrendDirection::Mixed);

    // Every trend explains itself.
    let build = trends
        .trends
        .iter()
        .find(|trend| trend.identity.metric == "build_duration_ms")
        .unwrap();
    assert_eq!(build.points.len(), 3);
    assert!((build.percent_change.unwrap() - 16.0).abs() < 0.01);
    assert!(build.evidence.contains("3 comparable measurements"));
    assert!(build.evidence.contains("fingerprint"));
}

#[tokio::test]
async fn recorded_health_snapshots_are_immutable() {
    let history = History::new();
    let commit = history.commits[0].clone();
    let store = Store::open_in_memory().await.unwrap();
    store.upsert_task(&task()).await.unwrap();

    let world = world_model(1, &commit, 2);
    store.insert_world_model_snapshot(&world).await.unwrap();
    let snapshot = build_health(&store, &history, 1, &commit, &world).await;

    // Re-inserting the identical record is fine.
    store.insert_health_snapshot(&snapshot).await.unwrap();

    // Rewriting history is not.
    let mut altered = snapshot.clone();
    altered.status = HealthSnapshotStatus::Failed;
    let error = store.insert_health_snapshot(&altered).await.unwrap_err();
    assert!(error.to_string().contains("immutable"), "{error}");

    // And the stored record is unchanged.
    let stored = store
        .load_health_snapshot(&snapshot.health_snapshot_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored, snapshot);
}

#[tokio::test]
async fn the_current_pointer_survives_a_failed_build() {
    let history = History::new();
    let commit = history.commits[0].clone();
    let store = Store::open_in_memory().await.unwrap();
    store.upsert_task(&task()).await.unwrap();

    let world = world_model(1, &commit, 2);
    store.insert_world_model_snapshot(&world).await.unwrap();
    let good = build_health(&store, &history, 1, &commit, &world).await;

    // A failed snapshot must not become "current health".
    let mut failed = good.clone();
    failed.health_snapshot_id = HealthSnapshotId::sequential(2);
    failed.status = HealthSnapshotStatus::Failed;
    assert!(
        !store
            .set_current_health_snapshot(REPOSITORY, &failed)
            .await
            .unwrap()
    );

    let current = store
        .current_health_snapshot(REPOSITORY)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(current.health_snapshot_id, good.health_snapshot_id);
}

#[tokio::test]
async fn a_health_snapshot_round_trips_through_sqlite_with_its_events() {
    let history = History::new();
    let commit = history.commits[0].clone();
    let store = Store::open_in_memory().await.unwrap();
    store.upsert_task(&task()).await.unwrap();

    let world = world_model(1, &commit, 2);
    store.insert_world_model_snapshot(&world).await.unwrap();
    record_run(&store, 1, &commit, &commit, 100, 1000.0, Vec::new()).await;
    let snapshot = build_health(&store, &history, 1, &commit, &world).await;

    let loaded = store
        .load_health_snapshot(&snapshot.health_snapshot_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(loaded, snapshot);
    assert_eq!(store.health_snapshot_count(REPOSITORY).await.unwrap(), 1);
    assert_eq!(
        store
            .health_snapshot_for_commit(REPOSITORY, &commit)
            .await
            .unwrap()
            .unwrap()
            .health_snapshot_id,
        snapshot.health_snapshot_id
    );

    // Events are subject to the health snapshot, never to a run.
    let events = store
        .health_events(&snapshot.health_snapshot_id)
        .await
        .unwrap();
    assert!(!events.is_empty());
    assert!(
        events
            .iter()
            .all(|event| event.health_snapshot_id == snapshot.health_snapshot_id)
    );
    assert_eq!(events[0].payload.event_type(), "HealthBuildStarted");
}

#[tokio::test]
async fn a_diverged_branch_is_not_treated_as_a_chronology() {
    let history = History::new();
    let root = history.repository.root().to_path_buf();
    let main_head = history.commits[2].clone();

    // Branch from the first commit, producing a commit that is not an ancestor
    // of main's head.
    git(
        &root,
        &["checkout", "--quiet", "-b", "side", &history.commits[0]],
    );
    std::fs::write(root.join("side.txt"), "divergent\n").unwrap();
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "--quiet", "-m", "diverged"]);
    let side = String::from_utf8(
        Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_string();

    let store = Store::open_in_memory().await.unwrap();
    store.upsert_task(&task()).await.unwrap();

    let world_main = world_model(1, &main_head, 3);
    let world_side = world_model(2, &side, 2);
    store
        .insert_world_model_snapshot(&world_main)
        .await
        .unwrap();
    store
        .insert_world_model_snapshot(&world_side)
        .await
        .unwrap();

    let health_main = build_health(&store, &history, 1, &main_head, &world_main).await;
    let health_side = build_health(&store, &history, 2, &side, &world_side).await;

    let relation = forge_world::snapshot_relation(
        &history.repository,
        &health_main.commit,
        &health_side.commit,
    );
    let diff = forge_health::diff(
        &health_main,
        &health_side,
        relation,
        &MaterialityPolicy::default(),
    );
    assert!(
        !diff.is_chronological(),
        "diverged commits do not describe an evolution"
    );

    // And no automatic baseline is offered across the divergence.
    let candidates = vec![(health_main.clone(), relation)];
    assert!(forge_health::nearest_ancestor_baseline(&health_side, &candidates).is_none());
}

#[tokio::test]
async fn the_nearest_ancestor_is_chosen_as_the_default_baseline() {
    let history = History::new();
    let store = Store::open_in_memory().await.unwrap();
    store.upsert_task(&task()).await.unwrap();

    let mut snapshots = Vec::new();
    for (index, commit) in history.commits.iter().enumerate() {
        let world = world_model(index as u64 + 1, commit, 2);
        store.insert_world_model_snapshot(&world).await.unwrap();
        let mut snapshot = build_health(&store, &history, index as u64 + 1, commit, &world).await;
        // Deterministic ordering for the baseline choice.
        snapshot.created_at = Utc::now() + TimeDelta::try_seconds(index as i64).unwrap();
        snapshots.push(snapshot);
    }

    let target = snapshots[2].clone();
    let candidates: Vec<_> = snapshots
        .iter()
        .take(2)
        .map(|snapshot| {
            (
                snapshot.clone(),
                forge_world::snapshot_relation(
                    &history.repository,
                    &snapshot.commit,
                    &target.commit,
                ),
            )
        })
        .collect();

    let baseline = forge_health::nearest_ancestor_baseline(&target, &candidates).unwrap();
    assert_eq!(
        baseline.commit, history.commits[1],
        "the immediately preceding ancestor, not the oldest"
    );
}
