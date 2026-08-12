//! Store-backed Phase 8 lifecycle smokes. No model, network, or repository
//! command is involved; all engineering outcomes are explicit synthetic facts.

use chrono::{TimeDelta, Utc};
use forge_core::agent::AgentConfig;
use forge_core::health::{
    DimensionStatus, HEALTH_BUILDER_VERSION, HEALTH_SCHEMA_VERSION, HealthDimension,
    HealthDimensionKind, HealthMeasurement, HealthProvenance, HealthSnapshotStatus,
    MeasurementIdentity, ObservationScope, RepositoryHealthSnapshot,
};
use forge_core::ids::{AgentId, HealthSnapshotId, PolicyId, TaskId, WorldModelSnapshotId};
use forge_core::integrity::{EvaluationIntegrity, IntegrityStatus};
use forge_core::optimization::{
    ExperimentArm, ExperimentAssignment, ExperimentMembership, PolicyDecision, PolicyEvent,
    PolicyEventPayload, PolicyEventSubject, PolicyExperiment, PolicyProposal,
    PolicySelectionSource, ProposalRecommendation,
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
use forge_core::world::{
    WORLD_MODEL_SCHEMA_VERSION, WorldModelFacts, WorldModelSnapshot, WorldModelSnapshotSource,
    WorldModelSnapshotStatus,
};
use forge_policy::{
    BaselineOptimizer, OptimizationRequest, PolicyEvidenceResolver, PolicyOptimizer,
    create_policy_experiment, promote_proposal, rollback_policy,
};
use forge_store::Store;

const COMMIT: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn objective(with_health: bool, observations: u64) -> OptimizationObjective {
    let mut terms = vec![
        ObjectiveTerm::hard(
            ObjectiveMetric::IntegrityCleanRate,
            Direction::HigherIsBetter,
            ObjectiveConstraint::AtLeast { value: 1.0 },
        ),
        ObjectiveTerm::soft(ObjectiveMetric::Runtime, Direction::LowerIsBetter, 1),
    ];
    if with_health {
        terms.push(ObjectiveTerm::soft(
            ObjectiveMetric::RepositoryHealth {
                dimension: HealthDimensionKind::Security,
            },
            Direction::HigherIsBetter,
            2,
        ));
    }
    OptimizationObjective {
        version: "store-smoke-objective-v1".into(),
        terms,
        observation_window_days: 30,
        minimum_evidence: MinimumEvidence {
            observations,
            comparable_observations_per_arm: 1,
            health_snapshots: u64::from(with_health),
            minimum_improvement_percent: 1.0,
        },
    }
}

fn bootstrap_event(policy: &EngineeringPolicy) -> PolicyEvent {
    PolicyEvent {
        subject: PolicyEventSubject::Policy(policy.policy_id.clone()),
        seq: 1,
        timestamp: policy.created_at,
        payload: PolicyEventPayload::PolicyCreated {
            provenance: policy.provenance.as_str().into(),
            fingerprint: policy.fingerprint(),
        },
    }
}

async fn policies(
    store: &Store,
    with_health: bool,
    observations: u64,
) -> (EngineeringPolicy, EngineeringPolicy) {
    let mut active = EngineeringPolicy::bootstrap(PolicyId::sequential(1), "forge");
    active.objective = objective(with_health, observations);
    store
        .install_bootstrap_policy(&active, &bootstrap_event(&active))
        .await
        .unwrap();
    let mut candidate = active.clone();
    candidate.policy_id = PolicyId::sequential(2);
    candidate.parent_policy_id = Some(active.policy_id.clone());
    candidate.status = PolicyStatus::Draft;
    candidate.provenance = PolicyProvenance::OptimizerProposed;
    candidate.context.max_world_facts = 8;
    candidate.created_at = Utc::now();
    candidate.optimizer_version = Some(forge_policy::OPTIMIZER_VERSION.into());
    store.insert_policy(&candidate).await.unwrap();
    (active, candidate)
}

async fn propose(
    store: &Store,
    active: &EngineeringPolicy,
    candidate: &EngineeringPolicy,
    proposal_number: u64,
) -> (PolicyProposal, forge_core::PolicyEvidenceSnapshot) {
    let evidence = PolicyEvidenceResolver::new(store.clone())
        .with_allowed_provenance([ExecutionProvenance::Live, ExecutionProvenance::Synthetic])
        .resolve(active, candidate, Utc::now())
        .await
        .unwrap();
    let proposal = BaselineOptimizer::new()
        .propose(OptimizationRequest {
            proposal_id: forge_core::ids::PolicyProposalId::sequential(proposal_number),
            active,
            candidate,
            evidence: &evidence.snapshot,
            objective: &active.objective,
            bounds: &PolicyBounds::default(),
            health: evidence.health,
        })
        .unwrap();
    store
        .insert_policy_proposal(&proposal, &evidence.snapshot)
        .await
        .unwrap();
    (proposal, evidence.snapshot)
}

fn task(number: u64) -> EngineeringTask {
    EngineeringTask {
        task_id: TaskId::sequential(number),
        repository: "forge".into(),
        objective: format!("synthetic policy observation {number}"),
        constraints: Vec::new(),
        evaluation: Default::default(),
        protection: Default::default(),
        metadata: TaskMetadata::default(),
        classification: Default::default(),
        components: Vec::new(),
        tags: Vec::new(),
    }
}

async fn add_arm_observation(
    store: &Store,
    experiment: &PolicyExperiment,
    active: &EngineeringPolicy,
    candidate: &EngineeringPolicy,
    task_number: u64,
    runtime_ms: i64,
    integrity_clean: bool,
) -> ExperimentArm {
    let task = task(task_number);
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
    let selected = match arm {
        ExperimentArm::Control => active,
        ExperimentArm::Candidate => candidate,
    };
    let source = match arm {
        ExperimentArm::Control => PolicySelectionSource::CanaryControl,
        ExperimentArm::Candidate => PolicySelectionSource::CanaryCandidate,
    };
    let decision = PolicyDecision {
        decision_id: store.next_policy_decision_id().await.unwrap(),
        repository: "forge".into(),
        created_at: Utc::now(),
        task_revision_id: revision.clone(),
        base_commit: Some(COMMIT.into()),
        active_policy_id: active.policy_id.clone(),
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
        explanation: vec!["deterministic store smoke".into()],
    };
    store.insert_policy_decision(&decision).await.unwrap();

    let run_id = store.next_run_id().await.unwrap();
    let now = Utc::now() - TimeDelta::try_seconds(2).unwrap();
    let mut run = AgentRun::new(
        run_id.clone(),
        task.task_id,
        AgentConfig::new(AgentId::new("stub").unwrap(), "deterministic-stub"),
        COMMIT,
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
    });
    run.patch = Some(PatchSummary {
        base_commit: COMMIT.into(),
        head_commit: None,
        files_changed: 0,
        insertions: 0,
        deletions: 0,
        binary_files: 0,
        diff_path: None,
        excluded: Vec::new(),
    });
    let mut integrity = EvaluationIntegrity::unchecked();
    if !integrity_clean {
        integrity.status = IntegrityStatus::Modified;
        integrity.modified.push("tests/integrity.sh".into());
    }
    run.integrity = Some(integrity);
    run.outcome = Some(RunOutcome::Passed);
    store
        .save_run_at_task_revision(&run, None, &revision)
        .await
        .unwrap();
    store
        .link_run_to_policy(
            &run_id,
            &selected.policy_id,
            &selected.fingerprint(),
            &decision.decision_id,
        )
        .await
        .unwrap();
    store
        .record_experiment_observation(&experiment.experiment_id, &run_id, arm)
        .await
        .unwrap();
    arm
}

async fn fill_arms(
    store: &Store,
    experiment: &PolicyExperiment,
    active: &EngineeringPolicy,
    candidate: &EngineeringPolicy,
    per_arm: usize,
    candidate_integrity_clean: bool,
) {
    let mut control = 0;
    let mut candidate_count = 0;
    for number in 1..500 {
        let revision = forge_core::TaskRevision::snapshot(task(number)).unwrap();
        let arm = experiment.arm_for(revision.revision_id());
        if (arm == ExperimentArm::Control && control >= per_arm)
            || (arm == ExperimentArm::Candidate && candidate_count >= per_arm)
        {
            continue;
        }
        let actual = add_arm_observation(
            store,
            experiment,
            active,
            candidate,
            number,
            if arm == ExperimentArm::Control {
                100
            } else {
                20
            },
            arm == ExperimentArm::Control || candidate_integrity_clean,
        )
        .await;
        match actual {
            ExperimentArm::Control => control += 1,
            ExperimentArm::Candidate => candidate_count += 1,
        }
        if control == per_arm && candidate_count == per_arm {
            return;
        }
    }
    panic!("could not fill both deterministic experiment arms");
}

#[tokio::test]
async fn controlled_store_lifecycle_promotes_and_rolls_back_without_erasing_history() {
    let store = Store::open_in_memory().await.unwrap();
    let (active, mut candidate) = policies(&store, false, 6).await;
    let (cold, _) = propose(&store, &active, &candidate, 1).await;
    assert_eq!(cold.recommendation, ProposalRecommendation::CanaryTest);
    let experiment = create_policy_experiment(
        &store,
        "forge",
        &cold.proposal_id,
        50,
        forge_core::ExperimentBudget {
            max_tasks: 10,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    candidate.status = PolicyStatus::Canary;
    fill_arms(&store, &experiment, &active, &candidate, 3, true).await;

    let (observed, _) = propose(&store, &active, &candidate, 2).await;
    assert_eq!(observed.recommendation, ProposalRecommendation::Promote);
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
        "forge",
        &observed.proposal_id,
        &PolicyBounds::default(),
        "test-operator",
    )
    .await
    .unwrap();
    assert_eq!(
        store
            .active_policy("forge")
            .await
            .unwrap()
            .unwrap()
            .policy_id,
        candidate.policy_id
    );

    rollback_policy(
        &store,
        "forge",
        &active.policy_id,
        "post-promotion hard guardrail alert",
        "test-operator",
    )
    .await
    .unwrap();
    assert_eq!(
        store
            .active_policy("forge")
            .await
            .unwrap()
            .unwrap()
            .policy_id,
        active.policy_id
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
            .policy_proposal_by_id(&observed.proposal_id)
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
        !store
            .policy_decisions("forge", 100)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn hard_integrity_failure_persists_and_blocks_the_fast_candidate() {
    let store = Store::open_in_memory().await.unwrap();
    let (active, mut candidate) = policies(&store, false, 2).await;
    let (cold, _) = propose(&store, &active, &candidate, 1).await;
    let experiment = create_policy_experiment(
        &store,
        "forge",
        &cold.proposal_id,
        50,
        forge_core::ExperimentBudget::default(),
    )
    .await
    .unwrap();
    candidate.status = PolicyStatus::Canary;
    fill_arms(&store, &experiment, &active, &candidate, 1, false).await;
    let (proposal, _) = propose(&store, &active, &candidate, 2).await;
    assert_eq!(proposal.recommendation, ProposalRecommendation::Reject);
    assert!(
        proposal
            .explanation
            .iter()
            .any(|line| line.contains("hard constraint failed"))
    );
    store
        .set_policy_experiment_status(
            &experiment.experiment_id,
            forge_core::PolicyExperimentStatus::Concluded,
            Some(Utc::now()),
        )
        .await
        .unwrap();
    assert!(
        promote_proposal(
            &store,
            "forge",
            &proposal.proposal_id,
            &PolicyBounds::default(),
            "test-operator",
        )
        .await
        .is_err()
    );
    assert_eq!(
        store
            .active_policy("forge")
            .await
            .unwrap()
            .unwrap()
            .policy_id,
        active.policy_id
    );
}

async fn add_health_snapshot(store: &Store, created_at: chrono::DateTime<Utc>) {
    let world = WorldModelSnapshotId::sequential(1);
    store
        .insert_world_model_snapshot(&WorldModelSnapshot {
            snapshot_id: world.clone(),
            repository: "forge".into(),
            commit: COMMIT.into(),
            created_at,
            source: WorldModelSnapshotSource::Deterministic,
            schema_version: WORLD_MODEL_SCHEMA_VERSION.into(),
            status: WorldModelSnapshotStatus::Complete,
            extractors: Vec::new(),
            facts: WorldModelFacts::default(),
        })
        .await
        .unwrap();
    let measurement = HealthMeasurement::new(
        MeasurementIdentity::new("security_findings", Direction::HigherIsBetter, "scanner-v1"),
        1.0,
        ObservationScope::point(COMMIT),
    );
    let snapshot = RepositoryHealthSnapshot {
        health_snapshot_id: HealthSnapshotId::sequential(1),
        repository: "forge".into(),
        commit: COMMIT.into(),
        world_model_snapshot_id: world.clone(),
        created_at,
        schema_version: HEALTH_SCHEMA_VERSION.into(),
        status: HealthSnapshotStatus::Complete,
        dimensions: vec![HealthDimension {
            kind: HealthDimensionKind::Security,
            status: DimensionStatus::Available,
            measurements: vec![measurement],
            notes: Vec::new(),
        }],
        provenance: HealthProvenance {
            builder_version: HEALTH_BUILDER_VERSION.into(),
            world_model_snapshot_id: world,
            world_model_status: WorldModelSnapshotStatus::Complete,
            window_start: None,
            runs_considered: 6,
        },
    };
    store.insert_health_snapshot(&snapshot).await.unwrap();
}

#[tokio::test]
async fn delayed_health_changes_only_a_new_later_proposal() {
    let store = Store::open_in_memory().await.unwrap();
    let (active, mut candidate) = policies(&store, true, 6).await;
    let (cold, _) = propose(&store, &active, &candidate, 1).await;
    let experiment = create_policy_experiment(
        &store,
        "forge",
        &cold.proposal_id,
        50,
        forge_core::ExperimentBudget {
            max_tasks: 10,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    candidate.status = PolicyStatus::Canary;
    fill_arms(&store, &experiment, &active, &candidate, 3, true).await;
    let (pending, snapshot) = propose(&store, &active, &candidate, 2).await;
    assert_eq!(
        pending.recommendation,
        ProposalRecommendation::HealthObservationPending
    );
    let original_json = serde_json::to_string(&pending).unwrap();

    add_health_snapshot(&store, Utc::now()).await;
    let historical = PolicyEvidenceResolver::new(store.clone())
        .with_allowed_provenance([ExecutionProvenance::Live, ExecutionProvenance::Synthetic])
        .resolve(&active, &candidate, snapshot.cutoff)
        .await
        .unwrap();
    assert_eq!(historical.snapshot, snapshot);
    let (later, later_snapshot) = propose(&store, &active, &candidate, 3).await;
    assert_eq!(later.recommendation, ProposalRecommendation::Promote);
    assert_ne!(snapshot.fingerprint(), later_snapshot.fingerprint());
    assert_eq!(
        serde_json::to_string(
            &store
                .policy_proposal_by_id(&pending.proposal_id)
                .await
                .unwrap()
                .unwrap()
        )
        .unwrap(),
        original_json
    );
}
