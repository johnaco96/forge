//! The promotion gate, approval enforcement, and rollback.
//!
//! Nothing becomes active by accident. A proposal is a recommendation; making
//! it real requires passing every check here and, for anything that changes the
//! shape of execution, an explicit human act.

use chrono::{DateTime, Utc};
use forge_core::ids::PolicyId;
use forge_core::optimization::{PolicyProposal, ProposalRecommendation};
use forge_core::policy::{
    ApprovalRequirement, EngineeringPolicy, FixedGuardrail, PolicyBounds, PolicyStatus,
};

/// Why a candidate may not be promoted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromotionBlocker {
    /// The policy itself is invalid or out of bounds.
    InvalidPolicy(String),
    /// A guardrail is not recorded as in force.
    GuardrailMissing(FixedGuardrail),
    /// A hard objective failed.
    HardConstraintViolated { metric: String, detail: String },
    /// Not enough comparable evidence.
    InsufficientEvidence(String),
    /// A required longitudinal observation has not completed.
    HealthObservationPending(String),
    /// The optimizer did not recommend promotion.
    RecommendationDoesNotSupport(ProposalRecommendation),
    /// The candidate is worse than what it would replace.
    Dominated,
    /// A human must approve this change and has not.
    ApprovalRequired(ApprovalRequirement),
    /// The change is not permitted by any means.
    Forbidden(String),
    /// The candidate is not in a status that can become active.
    InvalidStatus(PolicyStatus),
    /// The candidate governs a different repository.
    RepositoryMismatch { candidate: String, active: String },
    /// The optimizer's own judge is not an optimizable dimension.
    ObjectiveChanged,
}

impl PromotionBlocker {
    pub fn describe(&self) -> String {
        match self {
            Self::InvalidPolicy(detail) => format!("policy is invalid: {detail}"),
            Self::GuardrailMissing(guardrail) => format!(
                "guardrail `{guardrail}` is not recorded as in force ({})",
                guardrail.rationale()
            ),
            Self::HardConstraintViolated { metric, detail } => {
                format!("hard constraint `{metric}` failed: {detail}")
            }
            Self::InsufficientEvidence(detail) => format!("insufficient evidence: {detail}"),
            Self::HealthObservationPending(detail) => {
                format!("long-term health observation incomplete: {detail}")
            }
            Self::RecommendationDoesNotSupport(recommendation) => {
                format!("the optimizer recommended `{recommendation}`, not promotion")
            }
            Self::Dominated => "the candidate is worse on every compared objective".to_string(),
            Self::ApprovalRequired(requirement) => {
                format!("this change is `{requirement}` and has not been approved")
            }
            Self::Forbidden(detail) => format!("forbidden: {detail}"),
            Self::InvalidStatus(status) => {
                format!("a `{status}` policy cannot be activated directly")
            }
            Self::RepositoryMismatch { candidate, active } => {
                format!("candidate governs `{candidate}` but the active policy governs `{active}`")
            }
            Self::ObjectiveChanged => "the optimization objective differs from the active policy; \
                policy cannot rewrite its own judge"
                .to_string(),
        }
    }
}

/// The outcome of evaluating whether a candidate may be promoted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromotionGate {
    pub allowed: bool,
    pub approval: ApprovalRequirement,
    pub blockers: Vec<PromotionBlocker>,
}

impl PromotionGate {
    pub fn is_blocked(&self) -> bool {
        !self.allowed
    }

    pub fn reasons(&self) -> Vec<String> {
        self.blockers
            .iter()
            .map(PromotionBlocker::describe)
            .collect()
    }
}

/// Whether a human has authorized the change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Approval {
    /// No human has approved. Only `AutomaticAllowed` changes can proceed.
    None,
    /// A person explicitly approved, via the CLI.
    Explicit { actor: String },
}

/// Evaluates whether a candidate may become active.
///
/// Deliberately exhaustive rather than short-circuiting: a caller who is
/// blocked should see every reason at once, not discover them one promotion
/// attempt at a time.
pub fn evaluate_promotion(
    proposal: &PolicyProposal,
    candidate: &EngineeringPolicy,
    active: &EngineeringPolicy,
    bounds: &PolicyBounds,
    approval: &Approval,
    automatic_promotion_enabled: bool,
) -> PromotionGate {
    let mut blockers = Vec::new();

    // The policy must be valid and in bounds.
    if let Err(error) = candidate.validate(bounds) {
        blockers.push(PromotionBlocker::InvalidPolicy(error.to_string()));
    }

    // Guardrails must all be recorded as in force.
    for guardrail in FixedGuardrail::ALL {
        if !candidate.guardrails.contains(guardrail) {
            blockers.push(PromotionBlocker::GuardrailMissing(guardrail));
        }
    }

    if candidate.repository != active.repository {
        blockers.push(PromotionBlocker::RepositoryMismatch {
            candidate: candidate.repository.clone(),
            active: active.repository.clone(),
        });
    }

    if candidate.objective != active.objective {
        blockers.push(PromotionBlocker::ObjectiveChanged);
    }

    // Only a tested policy can become active.
    if !candidate.status.can_transition_to(PolicyStatus::Active) {
        blockers.push(PromotionBlocker::InvalidStatus(candidate.status));
    }

    // Hard constraints, individually named.
    for result in proposal.violated_constraints() {
        blockers.push(PromotionBlocker::HardConstraintViolated {
            metric: result.metric.as_str(),
            detail: result.detail.clone(),
        });
    }

    // The recommendation must actually support promotion.
    match proposal.recommendation {
        ProposalRecommendation::Promote => {}
        ProposalRecommendation::InsufficientEvidence => {
            blockers.push(PromotionBlocker::InsufficientEvidence(format!(
                "{} control and {} candidate observations",
                proposal.control_summary.observations, proposal.candidate_summary.observations
            )));
        }
        ProposalRecommendation::HealthObservationPending => {
            blockers.push(PromotionBlocker::HealthObservationPending(
                "the objective requires longitudinal health evidence that has not accrued"
                    .to_string(),
            ));
        }
        other => blockers.push(PromotionBlocker::RecommendationDoesNotSupport(other)),
    }

    if proposal.comparison == forge_core::policy::PolicyComparison::Dominated {
        blockers.push(PromotionBlocker::Dominated);
    }

    // Approval. Automatic promotion is available only for bounded parameter
    // changes, and only when the operator enabled it at all.
    let requirement = candidate.approval_requirement(active);
    match (&requirement, approval) {
        (ApprovalRequirement::Forbidden, _) => {
            blockers.push(PromotionBlocker::Forbidden(
                "this change is not permitted by policy".to_string(),
            ));
        }
        (_, Approval::Explicit { .. }) => {}
        (ApprovalRequirement::AutomaticAllowed, Approval::None) => {
            if !automatic_promotion_enabled {
                blockers.push(PromotionBlocker::ApprovalRequired(requirement));
            }
        }
        (ApprovalRequirement::ApprovalRequired, Approval::None) => {
            blockers.push(PromotionBlocker::ApprovalRequired(requirement));
        }
    }

    PromotionGate {
        allowed: blockers.is_empty(),
        approval: requirement,
        blockers,
    }
}

/// A recorded reversion to an earlier policy.
///
/// Rollback activates a prior immutable record; it never edits the policy being
/// left behind, and never touches the executions that policy governed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rollback {
    pub from_policy_id: PolicyId,
    pub to_policy_id: PolicyId,
    pub reason: String,
    pub actor: String,
    pub at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RollbackError {
    #[error("rollback target {0} is the policy already active")]
    AlreadyActive(String),
    #[error("rollback target {0} governs a different repository")]
    RepositoryMismatch(String),
    #[error("rollback target {target} has status `{status}` and cannot be reactivated")]
    UnusableTarget {
        target: String,
        status: PolicyStatus,
    },
    #[error("a rollback must state a reason")]
    MissingReason,
}

/// Prepares a rollback to a prior policy.
pub fn prepare_rollback(
    active: &EngineeringPolicy,
    target: &EngineeringPolicy,
    reason: &str,
    actor: &str,
) -> Result<Rollback, RollbackError> {
    if reason.trim().is_empty() {
        return Err(RollbackError::MissingReason);
    }
    if target.policy_id == active.policy_id {
        return Err(RollbackError::AlreadyActive(target.policy_id.to_string()));
    }
    if target.repository != active.repository {
        return Err(RollbackError::RepositoryMismatch(
            target.policy_id.to_string(),
        ));
    }
    // A policy that was rejected was rejected for a reason; reverting to one
    // would reinstate a strategy that failed its own evaluation.
    if matches!(target.status, PolicyStatus::Rejected | PolicyStatus::Draft) {
        return Err(RollbackError::UnusableTarget {
            target: target.policy_id.to_string(),
            status: target.status,
        });
    }

    Ok(Rollback {
        from_policy_id: active.policy_id.clone(),
        to_policy_id: target.policy_id.clone(),
        reason: reason.to_string(),
        actor: actor.to_string(),
        at: Utc::now(),
    })
}

/// Whether observed evidence warrants recommending a rollback.
///
/// Restricted to hard-constraint violations. Soft metrics are noisy, and a
/// system that reverted its own configuration whenever a runtime average
/// wobbled would be less trustworthy than one that did nothing.
pub fn rollback_recommended(proposal: &PolicyProposal) -> Option<String> {
    let violated = proposal.violated_constraints();
    if violated.is_empty() {
        return None;
    }
    Some(format!(
        "active policy violates {}: {}",
        if violated.len() == 1 {
            "a hard constraint".to_string()
        } else {
            format!("{} hard constraints", violated.len())
        },
        violated
            .iter()
            .map(|result| format!("{} ({})", result.metric, result.detail))
            .collect::<Vec<_>>()
            .join("; ")
    ))
}
