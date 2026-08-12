//! Deterministic optimization scenarios.
//!
//! No network, no models, no wall-clock waiting: every scenario constructs its
//! evidence explicitly so the optimizer's conclusion can be checked against
//! arithmetic anyone can redo by hand.

use chrono::{TimeDelta, Utc};
use forge_core::health::HealthDimensionKind;
use forge_core::ids::{PolicyId, PolicyProposalId, RunId};
use forge_core::optimization::{
    EvidenceExclusion, EvidenceStrength, ExcludedObservation, ExperimentArm, ExperimentMembership,
    HealthEvidenceRef, ObservationSource, POLICY_EVIDENCE_VERSION, PolicyEvidenceSnapshot,
    PolicyObservation, ProposalRecommendation,
};
use forge_core::policy::{
    ApprovalRequirement, EngineeringPolicy, ObjectiveConstraint, ObjectiveMetric, ObjectiveTerm,
    OptimizationObjective, PolicyBounds, PolicyComparison, PolicyStatus,
};
use forge_core::result::Direction;
use forge_core::run::{ExecutionProvenance, RunOutcome};
use forge_core::task::TaskRevisionId;
use forge_policy::{
    Approval, BaselineOptimizer, HealthEvidenceValues, OptimizationRequest, PolicyOptimizer,
    PromotionBlocker, evaluate_promotion, prepare_rollback, rollback_recommended,
};

const REPOSITORY: &str = "forge";

fn active_policy() -> EngineeringPolicy {
    EngineeringPolicy::bootstrap(PolicyId::sequential(1), REPOSITORY)
}

/// A candidate that differs only in a bounded context change.
fn candidate_policy() -> EngineeringPolicy {
    let mut candidate = active_policy();
    candidate.policy_id = PolicyId::sequential(2);
    candidate.parent_policy_id = Some(PolicyId::sequential(1));
    candidate.status = PolicyStatus::Canary;
    candidate.context.max_world_facts = 8;
    candidate
}

#[allow(clippy::too_many_arguments)]
fn observation(
    run: u64,
    fingerprint: &str,
    source: ObservationSource,
    outcome: RunOutcome,
    runtime_ms: u64,
    integrity_clean: bool,
) -> PolicyObservation {
    PolicyObservation {
        run_id: RunId::sequential(run),
        task_revision_id: TaskRevisionId::for_definition(&format!("task-{run}")),
        policy_id: Some(PolicyId::sequential(1)),
        policy_fingerprint: Some(fingerprint.to_string()),
        source,
        experiment: None,
        provenance: ExecutionProvenance::Live,
        outcome,
        integrity_clean,
        config_fingerprint: "cfg-a".into(),
        runtime_ms: Some(runtime_ms),
        cost_usd: Some(0.10),
        tokens: Some(1_000),
        patch_lines: Some(20),
        measured_commit: Some("a".repeat(40)),
        observed_at: Utc::now() - TimeDelta::try_days(1).unwrap(),
    }
}

fn evidence(
    active: &EngineeringPolicy,
    candidate: &EngineeringPolicy,
    observations: Vec<PolicyObservation>,
    health: Vec<HealthEvidenceRef>,
) -> PolicyEvidenceSnapshot {
    PolicyEvidenceSnapshot {
        repository: REPOSITORY.into(),
        cutoff: Utc::now(),
        active_policy_id: active.policy_id.clone(),
        active_policy_fingerprint: active.fingerprint(),
        candidate_policy_fingerprints: vec![candidate.fingerprint()],
        eligible: observations,
        excluded: Vec::new(),
        health,
        world_model_snapshot_id: None,
        evidence_version: POLICY_EVIDENCE_VERSION.into(),
        observation_window_days: 30,
    }
}

/// Builds arms where the candidate is faster and equally correct.
fn favourable_observations(
    active: &EngineeringPolicy,
    candidate: &EngineeringPolicy,
    per_arm: u64,
) -> Vec<PolicyObservation> {
    let mut observations = Vec::new();
    for index in 0..per_arm {
        observations.push(observation(
            index * 2,
            &active.fingerprint(),
            ObservationSource::CanaryControl,
            RunOutcome::Passed,
            1_000,
            true,
        ));
        observations.push(observation(
            index * 2 + 1,
            &candidate.fingerprint(),
            ObservationSource::CanaryCandidate,
            RunOutcome::Passed,
            800,
            true,
        ));
    }
    observations
}

/// An objective with no longitudinal terms, for short-term scenarios.
fn short_term_objective() -> OptimizationObjective {
    let mut objective = OptimizationObjective::conservative_default();
    objective
        .terms
        .retain(|term| !matches!(term.metric, ObjectiveMetric::RepositoryHealth { .. }));
    objective
}

fn propose(
    active: &EngineeringPolicy,
    candidate: &EngineeringPolicy,
    evidence: &PolicyEvidenceSnapshot,
    objective: &OptimizationObjective,
    health: HealthEvidenceValues,
) -> forge_core::optimization::PolicyProposal {
    BaselineOptimizer::new()
        .propose(OptimizationRequest {
            proposal_id: PolicyProposalId::sequential(1),
            active,
            candidate,
            evidence,
            objective,
            bounds: &PolicyBounds::default(),
            health,
        })
        .expect("proposal")
}

// ============================================================ controlled smoke

#[test]
fn a_bounded_improvement_with_strong_evidence_is_promotable_end_to_end() {
    let active = active_policy();
    let candidate = candidate_policy();
    let objective = short_term_objective();

    // 30 comparable observations per arm: strong evidence.
    let observations = favourable_observations(&active, &candidate, 30);
    let evidence = evidence(&active, &candidate, observations, Vec::new());

    let proposal = propose(
        &active,
        &candidate,
        &evidence,
        &objective,
        HealthEvidenceValues::default(),
    );

    // The optimizer sees a faster, equally-correct candidate.
    assert!(proposal.satisfies_hard_constraints());
    assert_eq!(proposal.comparison, PolicyComparison::Dominates);
    assert_eq!(proposal.evidence_strength, EvidenceStrength::Strong);
    assert_eq!(proposal.recommendation, ProposalRecommendation::Promote);

    // The explanation cites the cutoff and the evidence it used.
    assert!(
        proposal
            .explanation
            .iter()
            .any(|line| line.contains(&evidence.cutoff.to_rfc3339()))
    );
    assert!(
        proposal
            .explanation
            .iter()
            .any(|line| line.contains("30 control observations, 30 candidate observations"))
    );
    assert_eq!(proposal.changes, vec!["context.max_world_facts 12 → 8"]);
    assert_eq!(proposal.evidence_fingerprint, evidence.fingerprint());

    // Changing context requires a person, so automatic promotion is refused.
    assert_eq!(
        proposal.approval_requirement,
        ApprovalRequirement::ApprovalRequired
    );
    let unapproved = evaluate_promotion(
        &proposal,
        &candidate,
        &active,
        &PolicyBounds::default(),
        &Approval::None,
        true,
    );
    assert!(unapproved.is_blocked());
    assert!(
        unapproved
            .blockers
            .contains(&PromotionBlocker::ApprovalRequired(
                ApprovalRequirement::ApprovalRequired
            ))
    );

    // With an explicit human approval, the gate opens.
    let approved = evaluate_promotion(
        &proposal,
        &candidate,
        &active,
        &PolicyBounds::default(),
        &Approval::Explicit {
            actor: "operator".into(),
        },
        false,
    );
    assert!(approved.allowed, "{:?}", approved.reasons());
}

#[test]
fn a_bounded_parameter_change_may_promote_automatically_when_enabled() {
    let active = active_policy();
    let mut candidate = active_policy();
    candidate.policy_id = PolicyId::sequential(2);
    candidate.status = PolicyStatus::Canary;
    // Routing parameters are the only automatic-eligible dimension.
    candidate.routing.minimum_score_margin = 0.08;

    let observations = favourable_observations(&active, &candidate, 30);
    let evidence = evidence(&active, &candidate, observations, Vec::new());
    let proposal = propose(
        &active,
        &candidate,
        &evidence,
        &short_term_objective(),
        HealthEvidenceValues::default(),
    );

    assert_eq!(
        proposal.approval_requirement,
        ApprovalRequirement::AutomaticAllowed
    );
    let gate = evaluate_promotion(
        &proposal,
        &candidate,
        &active,
        &PolicyBounds::default(),
        &Approval::None,
        true,
    );
    assert!(gate.allowed, "{:?}", gate.reasons());

    // With automatic promotion disabled, even this needs a person.
    let gate = evaluate_promotion(
        &proposal,
        &candidate,
        &active,
        &PolicyBounds::default(),
        &Approval::None,
        false,
    );
    assert!(gate.is_blocked());
}

#[test]
fn a_candidate_cannot_rewrite_the_objective_that_judges_it() {
    let active = active_policy();
    let candidate = candidate_policy();
    let evidence = evidence(
        &active,
        &candidate,
        favourable_observations(&active, &candidate, 30),
        Vec::new(),
    );
    let proposal = propose(
        &active,
        &candidate,
        &evidence,
        &short_term_objective(),
        HealthEvidenceValues::default(),
    );
    let mut self_judging_candidate = candidate;
    self_judging_candidate.objective.terms.clear();

    let gate = evaluate_promotion(
        &proposal,
        &self_judging_candidate,
        &active,
        &PolicyBounds::default(),
        &Approval::Explicit {
            actor: "operator".into(),
        },
        false,
    );
    assert!(gate.is_blocked());
    assert!(gate.blockers.contains(&PromotionBlocker::ObjectiveChanged));
}

#[test]
fn moderate_evidence_earns_a_canary_and_thin_evidence_earns_a_shadow() {
    let active = active_policy();
    let candidate = candidate_policy();
    let objective = short_term_objective();

    // 10 per arm clears the minimum but is only moderate.
    let moderate = evidence(
        &active,
        &candidate,
        favourable_observations(&active, &candidate, 10),
        Vec::new(),
    );
    let proposal = propose(
        &active,
        &candidate,
        &moderate,
        &objective,
        HealthEvidenceValues::default(),
    );
    assert_eq!(proposal.evidence_strength, EvidenceStrength::Moderate);
    assert_eq!(proposal.recommendation, ProposalRecommendation::CanaryTest);

    // A candidate nobody has run needs running, not judging.
    let mut untested = Vec::new();
    for index in 0..30 {
        untested.push(observation(
            index,
            &active.fingerprint(),
            ObservationSource::ActivePolicy,
            RunOutcome::Passed,
            1_000,
            true,
        ));
    }
    let untried = evidence(&active, &candidate, untested, Vec::new());
    let proposal = propose(
        &active,
        &candidate,
        &untried,
        &objective,
        HealthEvidenceValues::default(),
    );
    assert_eq!(proposal.recommendation, ProposalRecommendation::CanaryTest);
    assert_eq!(proposal.comparison, PolicyComparison::InsufficientEvidence);
}

#[test]
fn one_lucky_observation_never_moves_policy() {
    let active = active_policy();
    let candidate = candidate_policy();
    let evidence = evidence(
        &active,
        &candidate,
        favourable_observations(&active, &candidate, 1),
        Vec::new(),
    );

    let proposal = propose(
        &active,
        &candidate,
        &evidence,
        &short_term_objective(),
        HealthEvidenceValues::default(),
    );
    assert_eq!(
        proposal.recommendation,
        ProposalRecommendation::InsufficientEvidence
    );
    assert!(
        proposal
            .explanation
            .iter()
            .any(|line| line.contains("required before any conclusion"))
    );
}

// ====================================================== degradation smoke

#[test]
fn a_hard_constraint_violation_cannot_be_bought_with_a_soft_improvement() {
    let active = active_policy();
    let candidate = candidate_policy();

    // The candidate is dramatically faster but compromises integrity.
    let mut observations = Vec::new();
    for index in 0..30 {
        observations.push(observation(
            index * 2,
            &active.fingerprint(),
            ObservationSource::CanaryControl,
            RunOutcome::Passed,
            10_000,
            true,
        ));
        observations.push(observation(
            index * 2 + 1,
            &candidate.fingerprint(),
            ObservationSource::CanaryCandidate,
            RunOutcome::Passed,
            100,
            // Integrity compromised on the candidate arm.
            false,
        ));
    }
    let evidence = evidence(&active, &candidate, observations, Vec::new());

    let proposal = propose(
        &active,
        &candidate,
        &evidence,
        &short_term_objective(),
        HealthEvidenceValues::default(),
    );

    // A 99% runtime improvement does not rescue it.
    assert_eq!(proposal.comparison, PolicyComparison::ConstraintViolated);
    assert_eq!(proposal.recommendation, ProposalRecommendation::Reject);
    assert!(!proposal.satisfies_hard_constraints());

    let violated = proposal.violated_constraints();
    assert!(
        violated
            .iter()
            .any(|result| result.metric == ObjectiveMetric::IntegrityCleanRate),
        "the integrity constraint must be the named blocker"
    );
    assert!(
        proposal
            .explanation
            .iter()
            .any(|line| line.contains("no speed or cost improvement can compensate"))
    );

    // The gate names the constraint too, and refuses even with approval.
    let gate = evaluate_promotion(
        &proposal,
        &candidate,
        &active,
        &PolicyBounds::default(),
        &Approval::Explicit {
            actor: "operator".into(),
        },
        true,
    );
    assert!(gate.is_blocked());
    assert!(
        gate.reasons()
            .iter()
            .any(|reason| reason.contains("integrity_clean_rate")),
        "{:?}",
        gate.reasons()
    );

    // And it warrants a rollback recommendation.
    let recommendation = rollback_recommended(&proposal).expect("recommendation");
    assert!(recommendation.contains("integrity_clean_rate"));
}

#[test]
fn a_security_health_regression_blocks_promotion() {
    let active = active_policy();
    let candidate = candidate_policy();
    // Default objective keeps security non-regression as a hard constraint.
    let objective = OptimizationObjective::conservative_default();

    let evidence = evidence(
        &active,
        &candidate,
        favourable_observations(&active, &candidate, 30),
        (0..5)
            .map(|n| HealthEvidenceRef {
                health_snapshot_id: forge_core::ids::HealthSnapshotId::sequential(n + 1),
                commit: "b".repeat(40),
                observed_at: Utc::now() - TimeDelta::try_days(2).unwrap(),
            })
            .collect(),
    );

    let health = HealthEvidenceValues {
        baseline: vec![(HealthDimensionKind::Security, 1.0)],
        // Security got worse under the candidate.
        candidate: vec![(HealthDimensionKind::Security, 0.5)],
        snapshots: 5,
    };

    let proposal = propose(&active, &candidate, &evidence, &objective, health);
    assert_eq!(proposal.recommendation, ProposalRecommendation::Reject);
    assert!(
        proposal
            .violated_constraints()
            .iter()
            .any(|result| result.metric
                == ObjectiveMetric::RepositoryHealth {
                    dimension: HealthDimensionKind::Security
                })
    );
}

#[test]
fn a_candidate_worse_on_every_objective_is_rejected() {
    let active = active_policy();
    let candidate = candidate_policy();

    let mut observations = Vec::new();
    for index in 0..30 {
        observations.push(observation(
            index * 2,
            &active.fingerprint(),
            ObservationSource::CanaryControl,
            RunOutcome::Passed,
            500,
            true,
        ));
        observations.push(observation(
            index * 2 + 1,
            &candidate.fingerprint(),
            ObservationSource::CanaryCandidate,
            RunOutcome::Passed,
            5_000,
            true,
        ));
    }
    let evidence = evidence(&active, &candidate, observations, Vec::new());
    let proposal = propose(
        &active,
        &candidate,
        &evidence,
        &short_term_objective(),
        HealthEvidenceValues::default(),
    );

    assert_eq!(proposal.comparison, PolicyComparison::Dominated);
    assert_eq!(proposal.recommendation, ProposalRecommendation::Reject);

    let gate = evaluate_promotion(
        &proposal,
        &candidate,
        &active,
        &PolicyBounds::default(),
        &Approval::Explicit {
            actor: "operator".into(),
        },
        true,
    );
    assert!(gate.blockers.contains(&PromotionBlocker::Dominated));
}

// ===================================================== delayed-health smoke

#[test]
fn short_term_wins_do_not_conclude_a_longitudinal_objective() {
    let active = active_policy();
    let candidate = candidate_policy();
    // The default objective includes repository-health terms.
    let objective = OptimizationObjective::conservative_default();

    // Plenty of task evidence, but only one health snapshot.
    let evidence = evidence(
        &active,
        &candidate,
        favourable_observations(&active, &candidate, 30),
        vec![HealthEvidenceRef {
            health_snapshot_id: forge_core::ids::HealthSnapshotId::sequential(1),
            commit: "b".repeat(40),
            observed_at: Utc::now() - TimeDelta::try_days(2).unwrap(),
        }],
    );

    let pending = propose(
        &active,
        &candidate,
        &evidence,
        &objective,
        HealthEvidenceValues {
            baseline: vec![(HealthDimensionKind::Security, 1.0)],
            candidate: vec![(HealthDimensionKind::Security, 1.0)],
            snapshots: 1,
        },
    );

    assert_eq!(
        pending.recommendation,
        ProposalRecommendation::HealthObservationPending
    );
    assert!(
        pending
            .explanation
            .iter()
            .any(|line| line.contains("health-health") || line.contains("health snapshots"))
    );
    // Short-term evidence still favoured the candidate; it simply is not enough.
    assert!(matches!(
        pending.comparison,
        PolicyComparison::Dominates | PolicyComparison::Tradeoff
    ));

    let gate = evaluate_promotion(
        &pending,
        &candidate,
        &active,
        &PolicyBounds::default(),
        &Approval::Explicit {
            actor: "operator".into(),
        },
        true,
    );
    assert!(gate.is_blocked());
    assert!(
        gate.reasons()
            .iter()
            .any(|reason| reason.contains("health observation incomplete"))
    );

    // Later, enough health evidence accrues. A NEW evaluation is computed;
    // the earlier one is untouched.
    let later_evidence = evidence_with_health(&active, &candidate, 5);
    let concluded = BaselineOptimizer::new()
        .propose(OptimizationRequest {
            // A new immutable proposal, not a mutation of the old one.
            proposal_id: PolicyProposalId::sequential(2),
            active: &active,
            candidate: &candidate,
            evidence: &later_evidence,
            objective: &objective,
            bounds: &PolicyBounds::default(),
            health: HealthEvidenceValues {
                baseline: vec![
                    (HealthDimensionKind::Security, 1.0),
                    (HealthDimensionKind::Complexity, 100.0),
                ],
                candidate: vec![
                    (HealthDimensionKind::Security, 1.0),
                    (HealthDimensionKind::Complexity, 90.0),
                ],
                snapshots: 5,
            },
        })
        .expect("second proposal");

    assert_eq!(concluded.recommendation, ProposalRecommendation::Promote);
    assert_ne!(concluded.proposal_id, pending.proposal_id);
    // The historical conclusion is unchanged.
    assert_eq!(
        pending.recommendation,
        ProposalRecommendation::HealthObservationPending
    );
}

fn evidence_with_health(
    active: &EngineeringPolicy,
    candidate: &EngineeringPolicy,
    snapshots: u64,
) -> PolicyEvidenceSnapshot {
    evidence(
        active,
        candidate,
        favourable_observations(active, candidate, 30),
        (0..snapshots)
            .map(|n| HealthEvidenceRef {
                health_snapshot_id: forge_core::ids::HealthSnapshotId::sequential(n + 1),
                commit: "b".repeat(40),
                observed_at: Utc::now() - TimeDelta::try_days(2).unwrap(),
            })
            .collect(),
    )
}

// ============================================================= safety tests

#[test]
fn an_invalid_candidate_is_refused_before_any_evidence_is_consulted() {
    let active = active_policy();
    let mut candidate = candidate_policy();
    // Beyond the configured maximum.
    candidate.context.max_world_facts = PolicyBounds::default().max_world_facts + 1;

    let evidence = evidence(
        &active,
        &candidate,
        favourable_observations(&active, &candidate, 30),
        Vec::new(),
    );
    let error = BaselineOptimizer::new()
        .propose(OptimizationRequest {
            proposal_id: PolicyProposalId::sequential(1),
            active: &active,
            candidate: &candidate,
            evidence: &evidence,
            objective: &short_term_objective(),
            bounds: &PolicyBounds::default(),
            health: HealthEvidenceValues::default(),
        })
        .expect_err("must refuse");

    assert!(error.to_string().contains("invalid"), "{error}");
}

#[test]
fn a_policy_missing_a_guardrail_can_never_be_promoted() {
    let active = active_policy();
    let mut candidate = candidate_policy();
    // Simulate a record that dropped a guardrail.
    let reduced = serde_json::to_value(&candidate).unwrap();
    let mut object = reduced.as_object().unwrap().clone();
    object.insert(
        "guardrails".into(),
        serde_json::json!(["required_evaluators"]),
    );
    candidate = serde_json::from_value(serde_json::Value::Object(object)).unwrap();

    let evidence = evidence(
        &active,
        &candidate,
        favourable_observations(&active, &candidate, 30),
        Vec::new(),
    );
    // A proposal cannot even be produced for it.
    assert!(
        BaselineOptimizer::new()
            .propose(OptimizationRequest {
                proposal_id: PolicyProposalId::sequential(1),
                active: &active,
                candidate: &candidate,
                evidence: &evidence,
                objective: &short_term_objective(),
                bounds: &PolicyBounds::default(),
                health: HealthEvidenceValues::default(),
            })
            .is_err()
    );
}

#[test]
fn a_draft_policy_cannot_be_activated_however_good_the_evidence() {
    let active = active_policy();
    let mut candidate = candidate_policy();
    candidate.status = PolicyStatus::Draft;

    let evidence = evidence(
        &active,
        &candidate,
        favourable_observations(&active, &candidate, 30),
        Vec::new(),
    );
    let proposal = propose(
        &active,
        &candidate,
        &evidence,
        &short_term_objective(),
        HealthEvidenceValues::default(),
    );
    assert_eq!(proposal.recommendation, ProposalRecommendation::Promote);

    let gate = evaluate_promotion(
        &proposal,
        &candidate,
        &active,
        &PolicyBounds::default(),
        &Approval::Explicit {
            actor: "operator".into(),
        },
        true,
    );
    assert!(gate.is_blocked());
    assert!(
        gate.blockers
            .contains(&PromotionBlocker::InvalidStatus(PolicyStatus::Draft))
    );
}

#[test]
fn manual_overrides_do_not_count_as_policy_evidence() {
    // The optimizer aggregates by policy fingerprint, and a user-chosen run is
    // recorded under its own source so it can be excluded.
    let active = active_policy();
    let candidate = candidate_policy();

    let mut observations = favourable_observations(&active, &candidate, 30);
    observations.push(observation(
        999,
        &candidate.fingerprint(),
        ObservationSource::ManualOverride,
        RunOutcome::Passed,
        1,
        true,
    ));

    let snapshot = evidence(&active, &candidate, observations, Vec::new());
    let breakdown = snapshot.source_breakdown();
    assert_eq!(breakdown[&ObservationSource::ManualOverride], 1);
    assert_eq!(breakdown[&ObservationSource::CanaryCandidate], 30);
    assert!(!ObservationSource::ManualOverride.is_policy_controlled());
}

#[test]
fn excluded_evidence_is_reported_rather_than_silently_dropped() {
    let active = active_policy();
    let candidate = candidate_policy();
    let mut snapshot = evidence(
        &active,
        &candidate,
        favourable_observations(&active, &candidate, 30),
        Vec::new(),
    );
    snapshot.excluded = vec![
        ExcludedObservation {
            run_id: RunId::sequential(900),
            exclusion: EvidenceExclusion::PostCutoff,
        },
        ExcludedObservation {
            run_id: RunId::sequential(901),
            exclusion: EvidenceExclusion::DisallowedProvenance {
                provenance: ExecutionProvenance::Synthetic,
            },
        },
    ];

    let proposal = propose(
        &active,
        &candidate,
        &snapshot,
        &short_term_objective(),
        HealthEvidenceValues::default(),
    );
    assert_eq!(proposal.excluded_observations, 2);
    assert!(
        proposal
            .explanation
            .iter()
            .any(|line| line.contains("2 excluded"))
    );
}

#[test]
fn post_cutoff_evidence_cannot_change_a_historical_proposal() {
    let active = active_policy();
    let candidate = candidate_policy();
    let snapshot = evidence(
        &active,
        &candidate,
        favourable_observations(&active, &candidate, 30),
        Vec::new(),
    );

    let first = propose(
        &active,
        &candidate,
        &snapshot,
        &short_term_objective(),
        HealthEvidenceValues::default(),
    );
    // Recomputing from the same snapshot reproduces the same conclusion.
    let again = propose(
        &active,
        &candidate,
        &snapshot,
        &short_term_objective(),
        HealthEvidenceValues::default(),
    );

    assert_eq!(first.recommendation, again.recommendation);
    assert_eq!(first.comparison, again.comparison);
    assert_eq!(first.evidence_fingerprint, again.evidence_fingerprint);
    assert_eq!(first.control_summary, again.control_summary);
    assert_eq!(first.candidate_summary, again.candidate_summary);
}

// ================================================================= rollback

#[test]
fn rollback_activates_a_prior_policy_without_editing_anything() {
    let mut active = active_policy();
    active.policy_id = PolicyId::sequential(2);
    active.status = PolicyStatus::Active;
    let prior = active_policy();

    let rollback =
        prepare_rollback(&active, &prior, "integrity regression", "operator").expect("rollback");

    assert_eq!(rollback.from_policy_id, active.policy_id);
    assert_eq!(rollback.to_policy_id, prior.policy_id);
    assert_eq!(rollback.reason, "integrity regression");
    assert_eq!(rollback.actor, "operator");
    // The policies themselves are untouched by preparing a rollback.
    assert_eq!(active.status, PolicyStatus::Active);
    assert_eq!(prior.status, PolicyStatus::Active);
}

#[test]
fn rollback_requires_a_reason_and_a_usable_target() {
    let mut active = active_policy();
    active.policy_id = PolicyId::sequential(2);
    let prior = active_policy();

    assert!(prepare_rollback(&active, &prior, "   ", "operator").is_err());
    assert!(prepare_rollback(&active, &active, "reason", "operator").is_err());

    // A policy that was rejected is not somewhere to revert to.
    let mut rejected = active_policy();
    rejected.policy_id = PolicyId::sequential(3);
    rejected.status = PolicyStatus::Rejected;
    assert!(prepare_rollback(&active, &rejected, "reason", "operator").is_err());
}

#[test]
fn no_rollback_is_recommended_from_soft_metrics_alone() {
    let active = active_policy();
    let candidate = candidate_policy();
    // Candidate is simply slower — noisy, not dangerous.
    let mut observations = Vec::new();
    for index in 0..30 {
        observations.push(observation(
            index * 2,
            &active.fingerprint(),
            ObservationSource::CanaryControl,
            RunOutcome::Passed,
            500,
            true,
        ));
        observations.push(observation(
            index * 2 + 1,
            &candidate.fingerprint(),
            ObservationSource::CanaryCandidate,
            RunOutcome::Passed,
            600,
            true,
        ));
    }
    let snapshot = evidence(&active, &candidate, observations, Vec::new());
    let proposal = propose(
        &active,
        &candidate,
        &snapshot,
        &short_term_objective(),
        HealthEvidenceValues::default(),
    );

    assert!(proposal.satisfies_hard_constraints());
    assert_eq!(rollback_recommended(&proposal), None);
}

// ============================================================== objectives

#[test]
fn an_unmeasured_hard_constraint_blocks_rather_than_passes() {
    let active = active_policy();
    let candidate = candidate_policy();

    let mut objective = short_term_objective();
    // A constraint on a metric nothing reports.
    objective.terms.push(ObjectiveTerm::hard(
        ObjectiveMetric::RepositoryHealth {
            dimension: HealthDimensionKind::Duplication,
        },
        Direction::LowerIsBetter,
        ObjectiveConstraint::AtMost { value: 5.0 },
    ));

    let snapshot = evidence(
        &active,
        &candidate,
        favourable_observations(&active, &candidate, 30),
        Vec::new(),
    );
    let proposal = propose(
        &active,
        &candidate,
        &snapshot,
        &objective,
        HealthEvidenceValues::default(),
    );

    assert_eq!(proposal.recommendation, ProposalRecommendation::Reject);
    assert!(
        proposal
            .violated_constraints()
            .iter()
            .any(|result| result.detail.contains("not measured"))
    );
}

#[test]
fn tiny_improvements_below_the_minimum_do_not_count() {
    let active = active_policy();
    let candidate = candidate_policy();

    // 1% faster, against a 2% minimum improvement.
    let mut observations = Vec::new();
    for index in 0..30 {
        observations.push(observation(
            index * 2,
            &active.fingerprint(),
            ObservationSource::CanaryControl,
            RunOutcome::Passed,
            1_000,
            true,
        ));
        observations.push(observation(
            index * 2 + 1,
            &candidate.fingerprint(),
            ObservationSource::CanaryCandidate,
            RunOutcome::Passed,
            990,
            true,
        ));
    }
    let snapshot = evidence(&active, &candidate, observations, Vec::new());
    let proposal = propose(
        &active,
        &candidate,
        &snapshot,
        &short_term_objective(),
        HealthEvidenceValues::default(),
    );

    // Not counted as an improvement, so nothing to promote.
    assert_eq!(proposal.comparison, PolicyComparison::Equivalent);
    assert_eq!(proposal.recommendation, ProposalRecommendation::Reject);
}

#[test]
fn the_optimizer_reports_its_own_version_and_evidence_version() {
    let active = active_policy();
    let candidate = candidate_policy();
    let snapshot = evidence(
        &active,
        &candidate,
        favourable_observations(&active, &candidate, 30),
        Vec::new(),
    );
    let proposal = propose(
        &active,
        &candidate,
        &snapshot,
        &short_term_objective(),
        HealthEvidenceValues::default(),
    );

    assert_eq!(proposal.optimizer_version, "policy-baseline-v1");
    assert_eq!(proposal.evidence_version, POLICY_EVIDENCE_VERSION);
    assert_eq!(BaselineOptimizer::new().version(), "policy-baseline-v1");
}

#[test]
fn experiment_arms_are_visible_in_the_evidence() {
    let active = active_policy();
    let candidate = candidate_policy();
    let mut observations = favourable_observations(&active, &candidate, 10);
    for (index, observation) in observations.iter_mut().enumerate() {
        observation.experiment = Some(ExperimentMembership {
            experiment_id: forge_core::ids::PolicyExperimentId::sequential(1),
            arm: if index % 2 == 0 {
                ExperimentArm::Control
            } else {
                ExperimentArm::Candidate
            },
        });
    }
    let snapshot = evidence(&active, &candidate, observations, Vec::new());

    assert_eq!(
        snapshot.observations_on_arm(ExperimentArm::Control).len(),
        10
    );
    assert_eq!(
        snapshot.observations_on_arm(ExperimentArm::Candidate).len(),
        10
    );
}
