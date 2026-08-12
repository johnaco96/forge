//! Durable bootstrap, execution selection, experiments, promotion, and rollback.

use chrono::{DateTime, Utc};
use forge_core::config::ForgeConfig;
use forge_core::ids::{PolicyId, PolicyProposalId};
use forge_core::optimization::{
    AssignmentRule, ExperimentAssignment, ExperimentBudget, ExperimentMembership, PolicyEvent,
    PolicyEventPayload, PolicyEventSubject, PolicyExperiment, PolicyExperimentStatus,
    PolicySelectionSource,
};
use forge_core::policy::{EngineeringPolicy, PolicyBounds, PolicyStatus};
use forge_core::task::TaskRevisionId;
use forge_store::{Store, StoreError};

use crate::gate::{Approval, PromotionGate, evaluate_promotion, prepare_rollback};

#[derive(Debug, thiserror::Error)]
pub enum PolicyRuntimeError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Optimization(#[from] crate::PolicyOptimizationError),
    #[error("invalid policy operation: {0}")]
    Invalid(String),
    #[error("promotion blocked: {0}")]
    PromotionBlocked(String),
    #[error("rollback blocked: {0}")]
    RollbackBlocked(String),
}

/// Active and selected policy context for one execution.
#[derive(Debug, Clone, PartialEq)]
pub struct ExecutionPolicyResolution {
    pub active: EngineeringPolicy,
    pub selected: EngineeringPolicy,
    pub source: PolicySelectionSource,
    pub experiment: Option<ExperimentMembership>,
    pub explanation: Vec<String>,
}

/// Installs the exact configured Phase 7 behavior once. Existing legacy runs
/// are never rewritten or linked to this policy.
pub async fn ensure_bootstrap_policy(
    store: &Store,
    config: &ForgeConfig,
) -> Result<EngineeringPolicy, PolicyRuntimeError> {
    if let Some(active) = store.active_policy(&config.repository.name).await? {
        return Ok(active);
    }
    let policy = EngineeringPolicy::bootstrap_from_config(store.next_policy_id().await?, config);
    policy
        .validate(&PolicyBounds::for_config(config))
        .map_err(|error| PolicyRuntimeError::Invalid(error.to_string()))?;
    let event = PolicyEvent {
        subject: PolicyEventSubject::Policy(policy.policy_id.clone()),
        seq: 1,
        timestamp: policy.created_at,
        payload: PolicyEventPayload::PolicyCreated {
            provenance: policy.provenance.as_str().to_string(),
            fingerprint: policy.fingerprint(),
        },
    };
    store.install_bootstrap_policy(&policy, &event).await?;
    Ok(policy)
}

/// Resolves an explicit override, deterministic canary assignment, or the
/// active policy in that order.
pub async fn resolve_execution_policy(
    store: &Store,
    config: &ForgeConfig,
    task_revision_id: &TaskRevisionId,
    manual_override: Option<&str>,
    now: DateTime<Utc>,
) -> Result<ExecutionPolicyResolution, PolicyRuntimeError> {
    let active = ensure_bootstrap_policy(store, config).await?;
    if let Some(manual) = manual_override {
        return Ok(ExecutionPolicyResolution {
            selected: active.clone(),
            active,
            source: PolicySelectionSource::ManualOverride,
            experiment: None,
            explanation: vec![format!(
                "explicit user choice `{manual}` overrides policy routing"
            )],
        });
    }

    if let Some(experiment) = store.active_policy_experiment(&active.repository).await? {
        if experiment.control_policy_id != active.policy_id {
            return Err(PolicyRuntimeError::Invalid(format!(
                "running experiment {} no longer has the active policy as control",
                experiment.experiment_id
            )));
        }
        let arm = experiment.arm_for(task_revision_id);
        let assignment = ExperimentAssignment {
            experiment_id: experiment.experiment_id.clone(),
            task_revision_id: task_revision_id.clone(),
            arm,
            assignment_version: experiment.assignment.version.clone(),
            assigned_at: now,
        };
        let arm = store.record_experiment_assignment(&assignment).await?;
        let (selected_id, source) = match arm {
            forge_core::ExperimentArm::Control => (
                &experiment.control_policy_id,
                PolicySelectionSource::CanaryControl,
            ),
            forge_core::ExperimentArm::Candidate => (
                &experiment.candidate_policy_id,
                PolicySelectionSource::CanaryCandidate,
            ),
        };
        let selected = store
            .policy_by_id(selected_id)
            .await?
            .ok_or_else(|| PolicyRuntimeError::Invalid(format!("missing policy {selected_id}")))?;
        return Ok(ExecutionPolicyResolution {
            active,
            selected,
            source,
            experiment: Some(ExperimentMembership {
                experiment_id: experiment.experiment_id.clone(),
                arm,
            }),
            explanation: vec![format!(
                "experiment {} deterministically assigned task revision {} to {arm}",
                experiment.experiment_id, task_revision_id
            )],
        });
    }

    Ok(ExecutionPolicyResolution {
        selected: active.clone(),
        active,
        source: PolicySelectionSource::ActivePolicy,
        experiment: None,
        explanation: vec!["active engineering policy governed execution".into()],
    })
}

pub async fn create_policy_experiment(
    store: &Store,
    repository: &str,
    proposal_id: &PolicyProposalId,
    candidate_share_percent: u32,
    budget: ExperimentBudget,
) -> Result<PolicyExperiment, PolicyRuntimeError> {
    if budget.max_tasks == 0 {
        return Err(PolicyRuntimeError::Invalid(
            "experiment max_tasks must be greater than zero".into(),
        ));
    }
    if store.active_policy_experiment(repository).await?.is_some() {
        return Err(PolicyRuntimeError::Invalid(
            "a policy experiment is already running for this repository".into(),
        ));
    }
    let proposal = store
        .policy_proposal_by_id(proposal_id)
        .await?
        .ok_or_else(|| PolicyRuntimeError::Invalid(format!("missing proposal {proposal_id}")))?;
    if proposal.repository != repository {
        return Err(PolicyRuntimeError::Invalid(
            "proposal belongs to another repository".into(),
        ));
    }
    if !matches!(
        proposal.recommendation,
        forge_core::ProposalRecommendation::CanaryTest
            | forge_core::ProposalRecommendation::ShadowTest
    ) {
        return Err(PolicyRuntimeError::Invalid(format!(
            "proposal recommendation `{}` does not call for an experiment",
            proposal.recommendation
        )));
    }
    let active = store
        .active_policy(repository)
        .await?
        .ok_or_else(|| PolicyRuntimeError::Invalid("repository has no active policy".into()))?;
    if active.policy_id != proposal.active_policy_id {
        return Err(PolicyRuntimeError::Invalid(
            "proposal is stale because its control policy is no longer active".into(),
        ));
    }
    let candidate = store
        .policy_by_id(&proposal.candidate_policy_id)
        .await?
        .ok_or_else(|| PolicyRuntimeError::Invalid("proposal candidate is missing".into()))?;
    if !matches!(
        candidate.status,
        PolicyStatus::Draft | PolicyStatus::Shadow | PolicyStatus::Canary
    ) {
        return Err(PolicyRuntimeError::Invalid(format!(
            "candidate status `{}` cannot enter an experiment",
            candidate.status
        )));
    }
    let experiment = PolicyExperiment {
        experiment_id: store.next_policy_experiment_id().await?,
        repository: repository.to_string(),
        control_policy_id: active.policy_id,
        candidate_policy_id: candidate.policy_id.clone(),
        assignment: AssignmentRule::new(candidate_share_percent),
        budget,
        status: PolicyExperimentStatus::Running,
        started_at: Utc::now(),
        concluded_at: None,
        proposal_id: Some(proposal_id.clone()),
    };
    if candidate.status != PolicyStatus::Canary {
        store
            .set_policy_status(&candidate.policy_id, PolicyStatus::Canary)
            .await?;
    }
    // Changing status first fails closed if startup is interrupted: an unused
    // canary controls nothing, while a running experiment never names a draft.
    store.insert_policy_experiment(&experiment).await?;
    let subject = PolicyEventSubject::Experiment(experiment.experiment_id.clone());
    store
        .append_policy_events(&[PolicyEvent {
            seq: store.next_policy_event_seq(&subject).await?,
            subject,
            timestamp: experiment.started_at,
            payload: PolicyEventPayload::PolicyExperimentStarted {
                control_policy_id: experiment.control_policy_id.clone(),
                candidate_policy_id: experiment.candidate_policy_id.clone(),
                candidate_share_percent: experiment.assignment.candidate_share_percent,
            },
        }])
        .await?;
    Ok(experiment)
}

pub async fn promote_proposal(
    store: &Store,
    repository: &str,
    proposal_id: &PolicyProposalId,
    bounds: &PolicyBounds,
    actor: &str,
) -> Result<PromotionGate, PolicyRuntimeError> {
    let proposal = store
        .policy_proposal_by_id(proposal_id)
        .await?
        .ok_or_else(|| PolicyRuntimeError::Invalid(format!("missing proposal {proposal_id}")))?;
    let active = store
        .active_policy(repository)
        .await?
        .ok_or_else(|| PolicyRuntimeError::Invalid("repository has no active policy".into()))?;
    let candidate = store
        .policy_by_id(&proposal.candidate_policy_id)
        .await?
        .ok_or_else(|| PolicyRuntimeError::Invalid("proposal candidate is missing".into()))?;
    if proposal.repository != repository
        || proposal.active_policy_id != active.policy_id
        || proposal.candidate_fingerprint != candidate.fingerprint()
        || candidate.parent_policy_id.as_ref() != Some(&active.policy_id)
    {
        return Err(PolicyRuntimeError::Invalid(
            "proposal scope, lineage, active policy, or fingerprint is stale".into(),
        ));
    }
    let experiments = store.policy_experiments(repository, u32::MAX).await?;
    if !experiments.iter().any(|experiment| {
        experiment.control_policy_id == active.policy_id
            && experiment.candidate_policy_id == candidate.policy_id
            && experiment.status == PolicyExperimentStatus::Concluded
    }) {
        return Err(PolicyRuntimeError::PromotionBlocked(
            "no concluded control/candidate experiment supports this promotion".into(),
        ));
    }
    let gate = evaluate_promotion(
        &proposal,
        &candidate,
        &active,
        bounds,
        &Approval::Explicit {
            actor: actor.to_string(),
        },
        false,
    );
    if gate.is_blocked() {
        return Err(PolicyRuntimeError::PromotionBlocked(
            gate.reasons().join("; "),
        ));
    }
    let subject = PolicyEventSubject::Policy(candidate.policy_id.clone());
    let event = PolicyEvent {
        seq: store.next_policy_event_seq(&subject).await?,
        subject,
        timestamp: Utc::now(),
        payload: PolicyEventPayload::PolicyPromoted {
            from_policy_id: active.policy_id.clone(),
            to_policy_id: candidate.policy_id.clone(),
            approved_by: actor.to_string(),
        },
    };
    store
        .promote_policy(
            repository,
            &active.policy_id,
            &candidate.policy_id,
            proposal_id,
            &event,
        )
        .await?;
    Ok(gate)
}

pub async fn rollback_policy(
    store: &Store,
    repository: &str,
    target: &PolicyId,
    reason: &str,
    actor: &str,
) -> Result<(), PolicyRuntimeError> {
    let active = store
        .active_policy(repository)
        .await?
        .ok_or_else(|| PolicyRuntimeError::Invalid("repository has no active policy".into()))?;
    let target_policy = store
        .policy_by_id(target)
        .await?
        .ok_or_else(|| PolicyRuntimeError::Invalid(format!("missing policy {target}")))?;
    let rollback = prepare_rollback(&active, &target_policy, reason, actor)
        .map_err(|error| PolicyRuntimeError::RollbackBlocked(error.to_string()))?;
    let subject = PolicyEventSubject::Policy(target.clone());
    let event = PolicyEvent {
        seq: store.next_policy_event_seq(&subject).await?,
        subject,
        timestamp: rollback.at,
        payload: PolicyEventPayload::PolicyRolledBack {
            from_policy_id: rollback.from_policy_id.clone(),
            to_policy_id: rollback.to_policy_id.clone(),
            reason: rollback.reason.clone(),
        },
    };
    store
        .rollback_policy(
            repository,
            &rollback.from_policy_id,
            &rollback.to_policy_id,
            &event,
        )
        .await?;
    Ok(())
}
