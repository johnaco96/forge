//! `policy-baseline-v1` — the deterministic policy optimizer.
//!
//! Simple on purpose. The algorithm is arithmetic over aggregated counts, in a
//! fixed order, with every step stated in the proposal it produces. There is no
//! model, no learning rate, and no LLM: a recommendation that cannot be
//! re-derived by hand from the recorded evidence is not auditable, and an
//! optimizer that cannot be audited has no business changing how Forge works.
//!
//! The order of operations is the policy:
//!
//! 1. **Validate** the candidate. An invalid policy is rejected before any
//!    evidence is consulted.
//! 2. **Hard constraints.** Any violation ends the evaluation in `Reject`.
//!    No soft improvement is consulted, because none can compensate.
//! 3. **Minimum evidence.** Too little comparable evidence yields
//!    `InsufficientEvidence`, distinguishing task shortfall from health
//!    shortfall.
//! 4. **Soft objectives**, compared in priority order on comparable
//!    observations only, producing Pareto semantics.
//! 5. **Delayed health.** If the objective needs longitudinal evidence that has
//!    not accrued, the answer is `HealthObservationPending` — never `Promote`.
//! 6. **Recommendation**, scaled to evidence strength: weak evidence earns a
//!    shadow or canary test, not a promotion.

use chrono::Utc;
use forge_core::health::HealthDimensionKind;
use forge_core::ids::{PolicyId, PolicyProposalId};
use forge_core::optimization::{
    ConstraintResult, EvidenceStrength, ObjectiveOutcome, POLICY_EVIDENCE_VERSION,
    PolicyEvidenceSnapshot, PolicyOutcomeSummary, PolicyProposal, ProposalRecommendation,
};
use forge_core::policy::{
    EngineeringPolicy, ObjectiveConstraint, ObjectiveKind, ObjectiveMetric, OptimizationObjective,
    PolicyBounds, PolicyComparison,
};
use forge_core::result::Direction;

use crate::error::{PolicyOptimizationError, PolicyOptimizationResult};

/// Identity of this optimizer. Any change that can alter a recommendation must
/// change this string.
pub const OPTIMIZER_VERSION: &str = "policy-baseline-v1";

/// Longitudinal health values supplied to the optimizer.
///
/// Provided by the caller rather than fetched here, so the optimizer stays a
/// pure function of its inputs and a historical proposal can be recomputed
/// exactly.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HealthEvidenceValues {
    /// Baseline value per dimension, measured before the cutoff.
    pub baseline: Vec<(HealthDimensionKind, f64)>,
    /// Candidate value per dimension, measured before the cutoff.
    pub candidate: Vec<(HealthDimensionKind, f64)>,
    /// Comparable health snapshots available.
    pub snapshots: u64,
}

impl HealthEvidenceValues {
    fn value(pairs: &[(HealthDimensionKind, f64)], dimension: HealthDimensionKind) -> Option<f64> {
        pairs
            .iter()
            .find(|(kind, _)| *kind == dimension)
            .map(|(_, value)| *value)
    }
}

/// What the optimizer needs to produce a proposal.
pub struct OptimizationRequest<'a> {
    pub proposal_id: PolicyProposalId,
    pub active: &'a EngineeringPolicy,
    pub candidate: &'a EngineeringPolicy,
    pub evidence: &'a PolicyEvidenceSnapshot,
    pub objective: &'a OptimizationObjective,
    pub bounds: &'a PolicyBounds,
    pub health: HealthEvidenceValues,
}

/// A provider-neutral policy optimizer.
pub trait PolicyOptimizer {
    fn version(&self) -> &'static str;

    fn propose(&self, request: OptimizationRequest<'_>)
    -> PolicyOptimizationResult<PolicyProposal>;
}

/// The conservative baseline optimizer.
#[derive(Debug, Clone, Default)]
pub struct BaselineOptimizer;

impl BaselineOptimizer {
    pub fn new() -> Self {
        Self
    }
}

impl PolicyOptimizer for BaselineOptimizer {
    fn version(&self) -> &'static str {
        OPTIMIZER_VERSION
    }

    fn propose(
        &self,
        request: OptimizationRequest<'_>,
    ) -> PolicyOptimizationResult<PolicyProposal> {
        let OptimizationRequest {
            proposal_id,
            active,
            candidate,
            evidence,
            objective,
            bounds,
            health,
        } = request;

        // 1. An invalid candidate never reaches the evidence.
        candidate
            .validate(bounds)
            .map_err(PolicyOptimizationError::InvalidCandidate)?;
        if candidate.repository != evidence.repository {
            return Err(PolicyOptimizationError::RepositoryMismatch {
                candidate: candidate.repository.clone(),
                evidence: evidence.repository.clone(),
            });
        }

        let control_observations = evidence.observations_for(&active.fingerprint());
        let candidate_observations = evidence.observations_for(&candidate.fingerprint());
        let control = PolicyOutcomeSummary::from_observations(&control_observations);
        let candidate_summary = PolicyOutcomeSummary::from_observations(&candidate_observations);

        let mut explanation = Vec::new();
        explanation.push(format!(
            "evidence cutoff {} · {} eligible, {} excluded",
            evidence.cutoff.to_rfc3339(),
            evidence.eligible.len(),
            evidence.excluded.len()
        ));
        explanation.push(format!(
            "{} control observations, {} candidate observations",
            control.observations, candidate_summary.observations
        ));

        // 2. Hard constraints, before anything else is considered.
        let constraint_results =
            evaluate_constraints(objective, &control, &candidate_summary, &health);
        let violated: Vec<&ConstraintResult> = constraint_results
            .iter()
            .filter(|result| !result.satisfied)
            .collect();

        let objective_outcomes =
            evaluate_objectives(objective, &control, &candidate_summary, &health);

        let strength = EvidenceStrength::from_counts(
            control.observations,
            candidate_summary.observations,
            objective.minimum_evidence.comparable_observations_per_arm,
        );

        let changed_dimensions = candidate.changed_dimensions(active);
        let changes = candidate.describe_changes(active);
        let approval_requirement = candidate.approval_requirement(active);

        // A candidate that has never run has not violated anything — it has
        // simply not been observed. Reporting an unmeasured constraint as a
        // violation here would tell an operator to reject a strategy nobody has
        // tried, which is the opposite of what cold start needs.
        let (comparison, recommendation) = if candidate_summary.observations == 0 {
            explanation.push(
                "the candidate has never run, so nothing about it can be evaluated yet; \
                 a controlled experiment is the way to find out"
                    .to_string(),
            );
            (
                PolicyComparison::InsufficientEvidence,
                ProposalRecommendation::CanaryTest,
            )
        } else if !violated.is_empty() {
            for result in &violated {
                explanation.push(format!(
                    "hard constraint failed: {} — {}",
                    result.metric, result.detail
                ));
            }
            explanation.push(
                "a hard constraint dominates every preference; no speed or cost improvement \
                 can compensate"
                    .to_string(),
            );
            (
                PolicyComparison::ConstraintViolated,
                ProposalRecommendation::Reject,
            )
        } else {
            decide(
                objective,
                &control,
                &candidate_summary,
                &objective_outcomes,
                strength,
                &health,
                &mut explanation,
            )
        };

        Ok(PolicyProposal {
            proposal_id,
            repository: evidence.repository.clone(),
            created_at: Utc::now(),
            active_policy_id: active.policy_id.clone(),
            candidate_policy_id: candidate.policy_id.clone(),
            candidate_fingerprint: candidate.fingerprint(),
            changed_dimensions,
            changes,
            objective: objective.clone(),
            cutoff: evidence.cutoff,
            evidence_fingerprint: evidence.fingerprint(),
            eligible_observations: evidence.eligible.len() as u64,
            excluded_observations: evidence.excluded.len() as u64,
            control_summary: control,
            candidate_summary,
            constraint_results,
            objective_outcomes,
            comparison,
            evidence_strength: strength,
            recommendation,
            approval_requirement,
            explanation,
            optimizer_version: OPTIMIZER_VERSION.to_string(),
            evidence_version: POLICY_EVIDENCE_VERSION.to_string(),
        })
    }
}

/// Applies every hard constraint.
fn evaluate_constraints(
    objective: &OptimizationObjective,
    control: &PolicyOutcomeSummary,
    candidate: &PolicyOutcomeSummary,
    health: &HealthEvidenceValues,
) -> Vec<ConstraintResult> {
    objective
        .hard_terms()
        .map(|term| {
            let ObjectiveKind::Hard { constraint } = &term.kind else {
                unreachable!("filtered to hard terms");
            };
            let observed = metric_value(&term.metric, candidate, &health.candidate);
            let baseline = metric_value(&term.metric, control, &health.baseline);
            let satisfied = constraint.is_satisfied(observed, baseline, term.direction);

            let detail = match (observed, baseline) {
                (None, _) => format!(
                    "{} was not measured; an unmeasured hard constraint is never satisfied",
                    term.metric
                ),
                (Some(value), None)
                    if matches!(constraint, ObjectiveConstraint::NonRegression { .. }) =>
                {
                    format!(
                        "{} is {value:.4} but no baseline exists to compare against",
                        term.metric
                    )
                }
                (Some(value), Some(base)) => {
                    format!("{} {base:.4} → {value:.4}", term.metric)
                }
                (Some(value), None) => format!("{} {value:.4}", term.metric),
            };

            ConstraintResult {
                metric: term.metric.clone(),
                constraint: constraint.clone(),
                observed,
                baseline,
                satisfied,
                detail,
            }
        })
        .collect()
}

/// Evaluates soft preferences.
fn evaluate_objectives(
    objective: &OptimizationObjective,
    control: &PolicyOutcomeSummary,
    candidate: &PolicyOutcomeSummary,
    health: &HealthEvidenceValues,
) -> Vec<ObjectiveOutcome> {
    let mut outcomes: Vec<(u32, ObjectiveOutcome)> = objective
        .soft_terms()
        .map(|term| {
            let priority = match &term.kind {
                ObjectiveKind::Soft { priority, .. } => *priority,
                ObjectiveKind::Hard { .. } => unreachable!("filtered to soft terms"),
            };
            let baseline = metric_value(&term.metric, control, &health.baseline);
            let candidate_value = metric_value(&term.metric, candidate, &health.candidate);

            let (percent_change, better, unmeasured) = match (baseline, candidate_value) {
                (Some(base), Some(value)) => {
                    let percent = forge_core::health::percent_change(base, value);
                    // Equality is neither better nor worse. Treating it as
                    // "worse" would make every unchanged metric drag a
                    // candidate into a false tradeoff.
                    let better = match term.direction {
                        _ if value == base => None,
                        Direction::HigherIsBetter => Some(value > base),
                        Direction::LowerIsBetter => Some(value < base),
                        // A neutral metric has no better; it merely changed.
                        Direction::Neutral => None,
                    };
                    (percent, better, None)
                }
                _ => (
                    None,
                    None,
                    Some(format!("{} was not measured on both sides", term.metric)),
                ),
            };

            (
                priority,
                ObjectiveOutcome {
                    metric: term.metric.clone(),
                    direction: term.direction,
                    is_hard: false,
                    baseline,
                    candidate: candidate_value,
                    percent_change,
                    candidate_better: better,
                    unmeasured_reason: unmeasured,
                },
            )
        })
        .collect();

    outcomes.sort_by_key(|(priority, _)| *priority);
    outcomes.into_iter().map(|(_, outcome)| outcome).collect()
}

fn metric_value(
    metric: &ObjectiveMetric,
    summary: &PolicyOutcomeSummary,
    health: &[(HealthDimensionKind, f64)],
) -> Option<f64> {
    match metric {
        ObjectiveMetric::RepositoryHealth { dimension } => {
            HealthEvidenceValues::value(health, *dimension)
        }
        other => summary.value_for(other),
    }
}

/// Turns measured outcomes into a comparison and a recommendation.
#[allow(clippy::too_many_arguments)]
fn decide(
    objective: &OptimizationObjective,
    control: &PolicyOutcomeSummary,
    candidate: &PolicyOutcomeSummary,
    outcomes: &[ObjectiveOutcome],
    strength: EvidenceStrength,
    health: &HealthEvidenceValues,
    explanation: &mut Vec<String>,
) -> (PolicyComparison, ProposalRecommendation) {
    let minimum = &objective.minimum_evidence;

    // 3. Minimum evidence, with the shortfall named precisely.
    let total = control.observations + candidate.observations;
    if total < minimum.observations {
        explanation.push(format!(
            "{total} comparable observations; {} required before any conclusion",
            minimum.observations
        ));
        return (
            PolicyComparison::InsufficientEvidence,
            ProposalRecommendation::InsufficientEvidence,
        );
    }
    if control.observations < minimum.comparable_observations_per_arm
        || candidate.observations < minimum.comparable_observations_per_arm
    {
        explanation.push(format!(
            "arms hold {} control and {} candidate observations; {} per arm required",
            control.observations, candidate.observations, minimum.comparable_observations_per_arm
        ));
        // A candidate nobody has run needs running, not judging.
        let recommendation = if candidate.observations == 0 {
            ProposalRecommendation::CanaryTest
        } else {
            ProposalRecommendation::InsufficientEvidence
        };
        return (PolicyComparison::InsufficientEvidence, recommendation);
    }

    // 4. Soft objectives on measured terms only.
    let measured: Vec<&ObjectiveOutcome> = outcomes
        .iter()
        .filter(|outcome| outcome.is_measured())
        .collect();
    for outcome in outcomes {
        if let Some(reason) = &outcome.unmeasured_reason {
            explanation.push(format!("not compared: {reason}"));
        }
    }

    if measured.is_empty() {
        explanation.push("no soft objective could be compared on both sides".to_string());
        return (
            PolicyComparison::InsufficientEvidence,
            ProposalRecommendation::InsufficientEvidence,
        );
    }

    // The minimum-improvement threshold applies symmetrically: a movement too
    // small to call an improvement is also too small to call a regression.
    // Applying it to only one side would let noise manufacture a tradeoff.
    let is_material = |outcome: &ObjectiveOutcome| {
        outcome
            .percent_change
            .is_none_or(|percent| percent.abs() >= minimum.minimum_improvement_percent)
    };
    let improved: Vec<&&ObjectiveOutcome> = measured
        .iter()
        .filter(|outcome| outcome.candidate_better == Some(true) && is_material(outcome))
        .collect();
    let worsened: Vec<&&ObjectiveOutcome> = measured
        .iter()
        .filter(|outcome| outcome.candidate_better == Some(false) && is_material(outcome))
        .collect();

    for outcome in &measured {
        explanation.push(format!(
            "{} {} [{}]",
            outcome.metric,
            outcome.describe(),
            match outcome.candidate_better {
                Some(true) if is_material(outcome) => "better",
                Some(false) if is_material(outcome) => "worse",
                Some(_) => "within noise",
                None => "unchanged",
            }
        ));
    }

    let comparison = match (improved.is_empty(), worsened.is_empty()) {
        (false, true) => PolicyComparison::Dominates,
        (true, false) => PolicyComparison::Dominated,
        (false, false) => PolicyComparison::Tradeoff,
        (true, true) => PolicyComparison::Equivalent,
    };

    // 5. Delayed health: a longitudinal objective cannot be concluded from
    //    short-term wins alone.
    if objective.has_longitudinal_terms() && health.snapshots < minimum.health_snapshots {
        explanation.push(format!(
            "{} comparable health snapshots; {} required before a longitudinal \
             objective can be concluded",
            health.snapshots, minimum.health_snapshots
        ));
        explanation.push(
            "short-term results look favourable, but the repository-health window is \
             incomplete"
                .to_string(),
        );
        return (comparison, ProposalRecommendation::HealthObservationPending);
    }

    // 6. Recommendation, scaled to how much the evidence actually supports.
    let recommendation = match comparison {
        PolicyComparison::Dominated => {
            explanation.push("candidate is worse on every compared objective".to_string());
            ProposalRecommendation::Reject
        }
        PolicyComparison::Equivalent => {
            explanation.push("candidate is indistinguishable from the active policy".to_string());
            ProposalRecommendation::Reject
        }
        PolicyComparison::Dominates | PolicyComparison::Tradeoff => match strength {
            EvidenceStrength::Strong => {
                explanation.push("evidence is strong enough to support promotion".to_string());
                ProposalRecommendation::Promote
            }
            EvidenceStrength::Moderate => {
                explanation.push(
                    "evidence favours the candidate but is only moderate; a controlled \
                     experiment should confirm it"
                        .to_string(),
                );
                ProposalRecommendation::CanaryTest
            }
            EvidenceStrength::Weak | EvidenceStrength::None => {
                explanation.push(
                    "evidence is too thin to act on; observe the candidate without letting \
                     it control execution"
                        .to_string(),
                );
                ProposalRecommendation::ShadowTest
            }
        },
        PolicyComparison::ConstraintViolated | PolicyComparison::InsufficientEvidence => {
            ProposalRecommendation::InsufficientEvidence
        }
    };

    (comparison, recommendation)
}

/// Builds the successor policy record for a promoted candidate.
pub fn successor(
    candidate: &EngineeringPolicy,
    policy_id: PolicyId,
    parent: &EngineeringPolicy,
) -> EngineeringPolicy {
    let mut successor = candidate.clone();
    successor.policy_id = policy_id;
    successor.parent_policy_id = Some(parent.policy_id.clone());
    successor.created_at = Utc::now();
    successor.optimizer_version = Some(OPTIMIZER_VERSION.to_string());
    successor
}
