//! End-to-end `forge policy` tests against the real binary and SQLite store.
//! No coding agent, model, or network call is involved.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use chrono::{TimeDelta, Utc};
use forge_core::agent::AgentConfig;
use forge_core::ids::{AgentId, PolicyId, TaskId};
use forge_core::integrity::EvaluationIntegrity;
use forge_core::optimization::{
    ExperimentArm, ExperimentAssignment, ExperimentMembership, PolicyDecision, PolicyEvent,
    PolicyEventPayload, PolicyEventSubject, PolicySelectionSource, ProposalRecommendation,
};
use forge_core::policy::{
    EngineeringPolicy, MinimumEvidence, ObjectiveConstraint, ObjectiveMetric, ObjectiveTerm,
    OptimizationObjective, PolicyBounds, PolicyProvenance, PolicyStatus,
};
use forge_core::result::Direction;
use forge_core::run::{
    AgentExecution, AgentExecutionStatus, AgentRun, ExecutionProvenance, PatchSummary, RunOutcome,
    RunStatus, SelectionSource, Usage,
};
use forge_core::task::{EngineeringTask, TaskMetadata};
use forge_policy::{
    BaselineOptimizer, OptimizationRequest, PolicyEvidenceResolver, PolicyOptimizer,
    create_policy_experiment, promote_proposal,
};
use forge_store::Store;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

struct Fixture {
    _temp: tempfile::TempDir,
    repo: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().expect("temp dir");
        let repo = temp.path().join("policy-fixture");
        std::fs::create_dir_all(repo.join("src")).unwrap();
        git(&repo, &["init", "--quiet", "--initial-branch=main"]);
        git(&repo, &["config", "user.email", "policy@example.invalid"]);
        git(&repo, &["config", "user.name", "Policy Fixture"]);
        std::fs::write(repo.join("README.md"), "# policy fixture\n").unwrap();
        std::fs::write(repo.join("value.txt"), "1\n").unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "--quiet", "-m", "initial commit"]);

        let fixture = Self { _temp: temp, repo };
        assert!(fixture.forge(&["init"]).status.success());
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

    fn use_stub_agent(&self) {
        let stub = self._temp.path().join("claude-policy-stub");
        std::fs::write(
            &stub,
            "#!/bin/sh\necho 2 > value.txt\nprintf '%s\\n' '{\"is_error\":false,\"subtype\":\"success\",\"result\":\"done\",\"session_id\":\"policy-smoke\",\"type\":\"result\",\"duration_ms\":5}'\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let config_path = self.repo.join(".forge/config.toml");
        let mut config = std::fs::read_to_string(&config_path).unwrap();
        config.push_str(&format!(
            "\n[agents.claude]\nexecutable = \"{}\"\nexecution_provenance = \"synthetic\"\n",
            stub.display()
        ));
        std::fs::write(config_path, config).unwrap();
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

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

async fn create_phase_seven_database(fixture: &Fixture, database: &Path) {
    // `forge init` creates a current empty ledger. This fixture deliberately
    // replaces only that temporary test file with a Phase 7 ledger.
    for path in [
        database.to_path_buf(),
        PathBuf::from(format!("{}-shm", database.display())),
        PathBuf::from(format!("{}-wal", database.display())),
    ] {
        if path.exists() {
            std::fs::remove_file(path).unwrap();
        }
    }
    let old_migrations = fixture._temp.path().join("phase7-migrations");
    std::fs::create_dir(&old_migrations).unwrap();
    let store_manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("../forge-store/migrations");
    for name in [
        "0001_init.sql",
        "0002_run_outcome.sql",
        "0003_experiments.sql",
        "0004_evaluator_results.sql",
        "0005_experience_queries.sql",
        "0006_immutable_task_revisions.sql",
        "0007_execution_provenance.sql",
        "0008_routing_decisions.sql",
        "0009_team_executions.sql",
        "0010_world_model.sql",
        "0011_repository_health.sql",
    ] {
        std::fs::copy(store_manifest.join(name), old_migrations.join(name)).unwrap();
    }
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(database)
                .create_if_missing(true)
                .foreign_keys(true),
        )
        .await
        .unwrap();
    sqlx::migrate::Migrator::new(old_migrations)
        .await
        .unwrap()
        .run(&pool)
        .await
        .unwrap();
    pool.close().await;
}

fn policy_task(number: u64) -> EngineeringTask {
    EngineeringTask {
        task_id: TaskId::sequential(number),
        repository: "policy-fixture".into(),
        objective: format!("policy evidence {number}"),
        constraints: Vec::new(),
        evaluation: Default::default(),
        protection: Default::default(),
        metadata: TaskMetadata::default(),
        classification: Default::default(),
        components: Vec::new(),
        tags: Vec::new(),
    }
}

async fn add_policy_observation(
    store: &Store,
    experiment: &forge_core::PolicyExperiment,
    control: &EngineeringPolicy,
    candidate: &EngineeringPolicy,
    number: u64,
    commit: &str,
) -> (ExperimentArm, forge_core::RunId) {
    let task = policy_task(number);
    let revision = store.upsert_task(&task).await.unwrap();
    let arm = experiment.arm_for(&revision);
    store
        .record_experiment_assignment(&ExperimentAssignment {
            experiment_id: experiment.experiment_id.clone(),
            task_revision_id: revision.clone(),
            arm,
            assignment_version: experiment.assignment.version.clone(),
            assigned_at: Utc::now(),
        })
        .await
        .unwrap();
    let (selected, source, runtime_ms) = match arm {
        ExperimentArm::Control => (control, PolicySelectionSource::CanaryControl, 100),
        ExperimentArm::Candidate => (candidate, PolicySelectionSource::CanaryCandidate, 20),
    };
    let decision = PolicyDecision {
        decision_id: store.next_policy_decision_id().await.unwrap(),
        repository: "policy-fixture".into(),
        created_at: Utc::now(),
        task_revision_id: revision.clone(),
        base_commit: Some(commit.into()),
        active_policy_id: control.policy_id.clone(),
        selected_policy_id: selected.policy_id.clone(),
        policy_fingerprint: selected.fingerprint(),
        source,
        manual_override: None,
        experiment: Some(ExperimentMembership {
            experiment_id: experiment.experiment_id.clone(),
            arm,
        }),
        world_model_snapshot_id: None,
        context_fact_ids: Vec::new(),
        health_snapshot_id: None,
        evidence_cutoff: None,
        evidence_fingerprint: None,
        optimizer_version: selected.optimizer_version.clone(),
        explanation: vec!["deterministic CLI lifecycle smoke".into()],
    };
    store.insert_policy_decision(&decision).await.unwrap();

    let now = Utc::now() - TimeDelta::try_seconds(2).unwrap();
    let run_id = store.next_run_id().await.unwrap();
    let mut run = AgentRun::new(
        run_id.clone(),
        task.task_id,
        AgentConfig::new(AgentId::new("stub").unwrap(), "policy-smoke"),
        commit,
    );
    run.execution_provenance = ExecutionProvenance::Synthetic;
    run.selection_source = SelectionSource::Manual;
    run.status = RunStatus::Completed;
    run.created_at = now;
    run.started_at = Some(now);
    run.finished_at = Some(now + TimeDelta::try_milliseconds(runtime_ms).unwrap());
    run.execution = Some(AgentExecution {
        status: AgentExecutionStatus::Completed,
        exit_code: Some(0),
        timed_out: false,
        started_at: now,
        finished_at: now + TimeDelta::try_milliseconds(runtime_ms).unwrap(),
        duration_ms: runtime_ms as u64,
        stdout_path: None,
        stderr_path: None,
        usage: Usage::default(),
        self_report: None,
        harness_metadata: Default::default(),
        infrastructure_failures: Vec::new(),
    });
    run.patch = Some(PatchSummary {
        base_commit: commit.into(),
        head_commit: None,
        files_changed: 0,
        insertions: 0,
        deletions: 0,
        binary_files: 0,
        diff_path: None,
        excluded: Vec::new(),
        excluded_counts: Default::default(),
    });
    run.integrity = Some(EvaluationIntegrity::unchecked());
    run.outcome = Some(RunOutcome::Passed);
    store
        .save_run_at_task_revision(&run, None, &revision)
        .await
        .unwrap();
    store
        .link_run_to_policy(
            &run.run_id,
            &selected.policy_id,
            &selected.fingerprint(),
            &decision.decision_id,
        )
        .await
        .unwrap();
    store
        .record_experiment_observation(&experiment.experiment_id, &run.run_id, arm)
        .await
        .unwrap();
    (arm, run_id)
}

#[test]
fn policy_cli_bootstraps_proposes_compares_and_runs_an_experiment_lifecycle() {
    let fixture = Fixture::new();

    let show = fixture.forge(&["policy", "show"]);
    let show_text = text(&show);
    assert!(show.status.success(), "{show_text}");
    assert!(show_text.contains("Forge engineering policy P-0001"));
    assert!(show_text.contains("provenance bootstrap"));
    assert!(show_text.contains("Fixed guardrails"));

    let proposal = fixture.forge(&["policy", "propose", "--max-world-facts", "8"]);
    let proposal_text = text(&proposal);
    assert!(proposal.status.success(), "{proposal_text}");
    assert!(proposal_text.contains("Forge policy proposal PP-0001"));
    assert!(proposal_text.contains("insufficient-evidence"));

    let compare = fixture.forge(&["policy", "compare", "PP-0001"]);
    let compare_text = text(&compare);
    assert!(compare.status.success(), "{compare_text}");
    assert!(compare_text.contains("0 eligible"));
    assert!(compare_text.contains("Hard constraints"));

    let create = fixture.forge(&[
        "policy",
        "experiment",
        "create",
        "PP-0001",
        "--max-tasks",
        "3",
    ]);
    let create_text = text(&create);
    assert!(create.status.success(), "{create_text}");
    assert!(create_text.contains("Forge policy experiment PX-0001"));
    assert!(create_text.contains("3 tasks"));

    let show_experiment = fixture.forge(&["policy", "experiment", "show", "PX-0001"]);
    let experiment_text = text(&show_experiment);
    assert!(show_experiment.status.success(), "{experiment_text}");
    assert!(experiment_text.contains("assignments 0 · observations 0"));

    let status = fixture.forge(&["policy", "experiment", "status", "PX-0001", "cancelled"]);
    assert!(status.status.success(), "{}", text(&status));

    let history = fixture.forge(&["policy", "history"]);
    let history_text = text(&history);
    assert!(history.status.success(), "{history_text}");
    assert!(history_text.contains("P-0001 *"));
    assert!(history_text.contains("P-0002"));
}

#[test]
fn policy_cli_refuses_activation_without_real_evidence() {
    let fixture = Fixture::new();
    assert!(
        fixture
            .forge(&["policy", "propose", "--max-world-facts", "8"])
            .status
            .success()
    );
    let promotion = fixture.forge(&["policy", "promote", "PP-0001"]);
    let promotion_text = text(&promotion);
    assert!(!promotion.status.success(), "{promotion_text}");
    assert!(
        promotion_text.contains("no concluded control/candidate experiment")
            || promotion_text.contains("insufficient evidence"),
        "{promotion_text}"
    );

    let show = fixture.forge(&["policy", "show"]);
    assert!(text(&show).contains("Forge engineering policy P-0001"));
}

#[tokio::test]
async fn promoted_policy_governs_a_real_stub_run_and_survives_rollback() {
    let fixture = Fixture::new();
    let database = fixture.repo.join(".forge/forge.db");
    create_phase_seven_database(&fixture, &database).await;
    // Opening with the real store applies migration 0012 to the Phase 7 file.
    let store = Store::open(database).await.unwrap();
    let commit = {
        let output = Command::new("git")
            .args(["-C", fixture.repo.to_str().unwrap(), "rev-parse", "HEAD"])
            .output()
            .unwrap();
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    };

    let mut control = EngineeringPolicy::bootstrap(PolicyId::sequential(1), "policy-fixture");
    control.objective = OptimizationObjective {
        version: "cli-policy-smoke-v1".into(),
        terms: vec![
            ObjectiveTerm::hard(
                ObjectiveMetric::IntegrityCleanRate,
                Direction::HigherIsBetter,
                ObjectiveConstraint::AtLeast { value: 1.0 },
            ),
            ObjectiveTerm::soft(ObjectiveMetric::Runtime, Direction::LowerIsBetter, 1),
        ],
        observation_window_days: 30,
        minimum_evidence: MinimumEvidence {
            observations: 6,
            comparable_observations_per_arm: 1,
            health_snapshots: 0,
            minimum_improvement_percent: 1.0,
        },
    };
    let bootstrap_event = PolicyEvent {
        subject: PolicyEventSubject::Policy(control.policy_id.clone()),
        seq: 1,
        timestamp: control.created_at,
        payload: PolicyEventPayload::PolicyCreated {
            provenance: control.provenance.as_str().into(),
            fingerprint: control.fingerprint(),
        },
    };
    store
        .install_bootstrap_policy(&control, &bootstrap_event)
        .await
        .unwrap();
    let mut candidate = control.clone();
    candidate.policy_id = PolicyId::sequential(2);
    candidate.parent_policy_id = Some(control.policy_id.clone());
    candidate.status = PolicyStatus::Draft;
    candidate.provenance = PolicyProvenance::OptimizerProposed;
    candidate.context.max_world_facts = 8;
    candidate.created_at = Utc::now();
    candidate.optimizer_version = Some(forge_policy::OPTIMIZER_VERSION.into());
    store.insert_policy(&candidate).await.unwrap();

    let cold_evidence = PolicyEvidenceResolver::new(store.clone())
        .with_allowed_provenance([ExecutionProvenance::Synthetic])
        .resolve(&control, &candidate, Utc::now())
        .await
        .unwrap();
    let cold = BaselineOptimizer::new()
        .propose(OptimizationRequest {
            proposal_id: store.next_policy_proposal_id().await.unwrap(),
            active: &control,
            candidate: &candidate,
            evidence: &cold_evidence.snapshot,
            objective: &control.objective,
            bounds: &PolicyBounds::default(),
            health: cold_evidence.health,
        })
        .unwrap();
    assert_eq!(cold.recommendation, ProposalRecommendation::CanaryTest);
    store
        .insert_policy_proposal(&cold, &cold_evidence.snapshot)
        .await
        .unwrap();
    let experiment = create_policy_experiment(
        &store,
        "policy-fixture",
        &cold.proposal_id,
        50,
        forge_core::ExperimentBudget {
            max_tasks: 8,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    candidate.status = PolicyStatus::Canary;

    let mut control_seen = 0;
    let mut candidate_seen = 0;
    let mut first_control_run = None;
    for number in 1..100 {
        let revision = forge_core::TaskRevision::snapshot(policy_task(number)).unwrap();
        let arm = experiment.arm_for(revision.revision_id());
        if (arm == ExperimentArm::Control && control_seen >= 3)
            || (arm == ExperimentArm::Candidate && candidate_seen >= 3)
        {
            continue;
        }
        let (actual, observed_run_id) =
            add_policy_observation(&store, &experiment, &control, &candidate, number, &commit)
                .await;
        match actual {
            ExperimentArm::Control => {
                control_seen += 1;
                first_control_run.get_or_insert(observed_run_id);
            }
            ExperimentArm::Candidate => candidate_seen += 1,
        }
        if control_seen == 3 && candidate_seen == 3 {
            break;
        }
    }
    assert_eq!((control_seen, candidate_seen), (3, 3));

    let evidence = PolicyEvidenceResolver::new(store.clone())
        .with_allowed_provenance([ExecutionProvenance::Synthetic])
        .resolve(&control, &candidate, Utc::now())
        .await
        .unwrap();
    let proposal = BaselineOptimizer::new()
        .propose(OptimizationRequest {
            proposal_id: store.next_policy_proposal_id().await.unwrap(),
            active: &control,
            candidate: &candidate,
            evidence: &evidence.snapshot,
            objective: &control.objective,
            bounds: &PolicyBounds::default(),
            health: evidence.health,
        })
        .unwrap();
    assert_eq!(proposal.recommendation, ProposalRecommendation::Promote);
    store
        .insert_policy_proposal(&proposal, &evidence.snapshot)
        .await
        .unwrap();
    store
        .set_policy_experiment_status(
            &experiment.experiment_id,
            forge_core::PolicyExperimentStatus::Concluded,
            Some(Utc::now()),
        )
        .await
        .unwrap();
    promote_proposal(
        &store,
        "policy-fixture",
        &proposal.proposal_id,
        &PolicyBounds::default(),
        "test-operator",
    )
    .await
    .unwrap();

    fixture.use_stub_agent();
    let task_path = fixture.repo.join(".forge/tasks/promoted.yaml");
    std::fs::write(
        &task_path,
        "task_id: T-1042\nrepository: policy-fixture\nobjective: Raise value to two\nevaluation:\n  tests:\n    command: grep -q '^2$' value.txt\n",
    )
    .unwrap();
    let run = fixture.forge(&["run", task_path.to_str().unwrap()]);
    assert!(run.status.success(), "{}", text(&run));

    let linked_run = store.list_runs(1).await.unwrap()[0].run_id.clone();
    let (policy_id, _, decision_id) = store.run_policy_link(&linked_run).await.unwrap().unwrap();
    assert_eq!(policy_id, candidate.policy_id);
    let decision = store
        .policy_decision_by_id(&decision_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(decision.source, PolicySelectionSource::ActivePolicy);
    let (historical_policy, _, _) = store
        .run_policy_link(&first_control_run.unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(historical_policy, control.policy_id);

    let rollback = fixture.forge(&[
        "policy",
        "rollback",
        control.policy_id.as_str(),
        "--reason",
        "controlled lifecycle complete",
    ]);
    assert!(rollback.status.success(), "{}", text(&rollback));
    assert_eq!(
        store
            .active_policy("policy-fixture")
            .await
            .unwrap()
            .unwrap()
            .policy_id,
        control.policy_id
    );
    assert!(
        store
            .policy_by_id(&candidate.policy_id)
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        store
            .policy_proposal_by_id(&proposal.proposal_id)
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        store
            .policy_experiment_by_id(&experiment.experiment_id)
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        store
            .policy_decision_by_id(&decision_id)
            .await
            .unwrap()
            .is_some()
    );
}
