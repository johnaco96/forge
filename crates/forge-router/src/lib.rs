//! Deterministic, provider-agnostic historical routing.
//!
//! Phase 4A resolves eligible candidate configurations and retrieves a stable,
//! policy-filtered evidence snapshot. Phase 4B scores that snapshot without
//! changing its trust boundary, persists the decision, and leaves execution to
//! the ordinary runner.

#![deny(rust_2018_idioms)]

use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use forge_core::agent::{AdapterStatus, AgentConfig, AgentDescriptor, Capability};
use forge_core::config::RoutingConfig;
use forge_core::ids::{AgentId, RoutingDecisionId};
use forge_core::routing::{
    AgentRoutingScore, CandidateAgent, CandidateAgentSet, DecisionSource, InfluentialRoutingRun,
    RoutingContractError, RoutingDecision, RoutingDecisionRecord, RoutingEvent,
    RoutingEventPayload, RoutingEvidence, RoutingExplanation, RoutingExplanationReason,
    RoutingPolicyConfiguration, RoutingReadiness, RoutingRequest, RoutingSuggestedAction,
    RoutingTarget,
};
use forge_store::{Store, StoreError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CandidateAvailability {
    Available,
    Unavailable { reason: String },
}

/// Caller-supplied current configuration and availability. Availability is an
/// explicit probe result; the router never guesses from an agent name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateRequest {
    pub config: AgentConfig,
    pub availability: CandidateAvailability,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CandidateRequirements {
    pub capabilities: BTreeSet<Capability>,
}

#[derive(Debug, thiserror::Error)]
pub enum RouterError {
    #[error("no currently available routing candidates")]
    NoAvailableCandidates,
    #[error("candidate agent `{0}` is not registered")]
    UnregisteredAgent(AgentId),
    #[error("candidate agent `{agent_id}` is unavailable: {reason}")]
    UnavailableAgent { agent_id: AgentId, reason: String },
    #[error("candidate agent `{agent_id}` lacks required capability `{capability:?}`")]
    IneligibleAgent {
        agent_id: AgentId,
        capability: Capability,
    },
    #[error(transparent)]
    Contract(#[from] RoutingContractError),
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// Resolves registered, implemented, currently available candidates without
/// hard-coding any provider identity.
pub fn resolve_candidates(
    registry: &[AgentDescriptor],
    requested: Vec<CandidateRequest>,
    requirements: &CandidateRequirements,
) -> Result<CandidateAgentSet, RouterError> {
    if requested.is_empty() {
        return Err(RouterError::NoAvailableCandidates);
    }
    let mut candidates = Vec::with_capacity(requested.len());
    for request in requested {
        let agent_id = request.config.agent_id.clone();
        let descriptor = registry
            .iter()
            .find(|descriptor| descriptor.agent_id == agent_id)
            .ok_or_else(|| RouterError::UnregisteredAgent(agent_id.clone()))?;
        if descriptor.adapter_status != AdapterStatus::Implemented {
            return Err(RouterError::UnavailableAgent {
                agent_id,
                reason: "adapter is not implemented".into(),
            });
        }
        if let CandidateAvailability::Unavailable { reason } = request.availability {
            return Err(RouterError::UnavailableAgent { agent_id, reason });
        }
        for capability in &requirements.capabilities {
            if !descriptor.capabilities.contains(capability) {
                return Err(RouterError::IneligibleAgent {
                    agent_id,
                    capability: capability.clone(),
                });
            }
        }
        candidates.push(CandidateAgent::new(agent_id, request.config)?);
    }
    CandidateAgentSet::new(candidates).map_err(Into::into)
}

/// Store-backed routing façade. It selects configurations but never executes
/// them; callers pass a selected adapter to the ordinary runner.
#[derive(Debug, Clone)]
pub struct RoutingContract {
    store: Store,
}

impl RoutingContract {
    pub fn new(store: Store) -> Self {
        Self { store }
    }

    pub async fn evidence(&self, request: &RoutingRequest) -> Result<RoutingEvidence, RouterError> {
        Ok(self.store.routing_evidence(request).await?)
    }

    /// Resolves evidence, makes one versioned decision, and durably records
    /// both the decision and its deterministic lifecycle events.
    pub async fn route(
        &self,
        request: &RoutingRequest,
        config: &RoutingConfig,
    ) -> Result<RoutingDecisionRecord, RouterError> {
        let evidence = self.evidence(request).await?;
        let decision_id = self.store.next_routing_decision_id().await?;
        let created_at = Utc::now();
        let record =
            BaselineRouter::decide(request, evidence, config, decision_id.clone(), created_at);
        self.store.save_routing_decision(&record).await?;
        self.store
            .append_routing_events(&routing_events(&record))
            .await?;
        Ok(record)
    }
}

pub const ROUTER_VERSION: &str = "historical-baseline-v1";
const INFLUENTIAL_RUN_LIMIT: usize = 5;

/// The first routing policy: similarity-weighted Beta/Bernoulli smoothing.
pub struct BaselineRouter;

impl BaselineRouter {
    pub fn decide(
        request: &RoutingRequest,
        mut evidence: RoutingEvidence,
        config: &RoutingConfig,
        decision_id: RoutingDecisionId,
        created_at: DateTime<Utc>,
    ) -> RoutingDecisionRecord {
        evidence.snapshot.set_routing_policy_version(ROUTER_VERSION);
        let scores = score_candidates(request, &evidence, config);
        let margin = (scores.len() >= 2).then(|| scores[0].routing_score - scores[1].routing_score);
        let mut reasons = explanation_reasons(&evidence, margin, config.minimum_score_margin);

        let decision = if scores.len() == 1 {
            reasons.push(RoutingExplanationReason::OnlyOneCandidateAvailable);
            insufficient(
                &evidence,
                scores.clone(),
                margin,
                reasons,
                RoutingSuggestedAction::SelectManually,
            )
        } else if !matches!(evidence.readiness, RoutingReadiness::Ready)
            || margin.is_none_or(|value| value < config.minimum_score_margin)
        {
            uncertain_decision(request, &evidence, scores.clone(), margin, reasons)
        } else if request.exploration_policy()
            == forge_core::routing::ExplorationPolicy::PeriodicCompetition
            && evidence.summary.resolved_runs > 0
            && evidence
                .summary
                .resolved_runs
                .is_multiple_of(config.periodic_competition_interval)
        {
            reasons.push(RoutingExplanationReason::PeriodicCompetition {
                resolved_observations: evidence.summary.resolved_runs,
                interval: config.periodic_competition_interval,
            });
            compete(&evidence, scores.clone(), margin, reasons)
        } else {
            RoutingDecision::Selected {
                agent: scores[0].agent.clone(),
                evidence_summary: evidence.summary.clone(),
                snapshot: evidence.snapshot.clone(),
                explanation: explanation(reasons),
                scores: scores.clone(),
                decision_margin: margin,
            }
        };
        let selected = match &decision {
            RoutingDecision::Selected { agent, .. } => Some(agent.clone()),
            _ => None,
        };
        RoutingDecisionRecord {
            decision_id,
            run_id: None,
            task_id: request.task_revision().task().task_id.clone(),
            task_revision_id: request.task_revision().revision_id().clone(),
            created_at,
            candidates: request.candidates().as_slice().to_vec(),
            selected,
            router_version: ROUTER_VERSION.into(),
            evidence_policy_version: request.evidence_policy().version.clone(),
            policy_configuration: RoutingPolicyConfiguration {
                prior_alpha: config.baseline.prior_alpha,
                prior_beta: config.baseline.prior_beta,
                minimum_score_margin: config.minimum_score_margin,
                periodic_competition_interval: config.periodic_competition_interval,
            },
            historical_cutoff: request.historical_cutoff(),
            evidence_fingerprint: evidence.snapshot.evidence_fingerprint.clone(),
            decision,
        }
    }
}

fn score_candidates(
    request: &RoutingRequest,
    evidence: &RoutingEvidence,
    config: &RoutingConfig,
) -> Vec<AgentRoutingScore> {
    let mut scores = request
        .candidates()
        .as_slice()
        .iter()
        .map(|candidate| {
            let records = evidence
                .eligible
                .iter()
                .filter(|record| record.agent_id == candidate.agent_id)
                .collect::<Vec<_>>();
            let positive_count = records
                .iter()
                .filter(|record| record.target == RoutingTarget::Positive)
                .count() as u64;
            let negative_count = records
                .iter()
                .filter(|record| record.target == RoutingTarget::Negative)
                .count() as u64;
            let unresolved_count = records.len() as u64 - positive_count - negative_count;
            let positive_weight: f64 = records
                .iter()
                .filter(|record| record.target == RoutingTarget::Positive)
                .map(|record| record.similarity_score)
                .sum();
            let negative_weight: f64 = records
                .iter()
                .filter(|record| record.target == RoutingTarget::Negative)
                .map(|record| record.similarity_score)
                .sum();
            let weighted = positive_weight + negative_weight;
            let prior = config.baseline.prior_alpha + config.baseline.prior_beta;
            let predicted = (positive_weight + config.baseline.prior_alpha) / (weighted + prior);
            let mut influential = records;
            influential.sort_by(|left, right| {
                right
                    .similarity_score
                    .total_cmp(&left.similarity_score)
                    .then_with(|| left.run_id.cmp(&right.run_id))
            });
            AgentRoutingScore {
                agent: candidate.clone(),
                predicted_success: predicted,
                routing_score: predicted,
                resolved_evidence_count: positive_count + negative_count,
                positive_count,
                negative_count,
                unresolved_count,
                weighted_similarity_evidence: weighted,
                evidence_strength: weighted / (weighted + prior),
                influential_runs: influential
                    .into_iter()
                    .take(INFLUENTIAL_RUN_LIMIT)
                    .map(|record| InfluentialRoutingRun {
                        run_id: record.run_id.clone(),
                        task_revision_id: record.task_revision_id.clone(),
                        target: record.target,
                        similarity_weight: record.similarity_score,
                        experiment_id: record.experiment_id.clone(),
                    })
                    .collect(),
            }
        })
        .collect::<Vec<_>>();
    scores.sort_by(|left, right| {
        right
            .routing_score
            .total_cmp(&left.routing_score)
            .then_with(|| left.agent.agent_id.cmp(&right.agent.agent_id))
    });
    scores
}

fn explanation_reasons(
    evidence: &RoutingEvidence,
    margin: Option<f64>,
    required_margin: f64,
) -> Vec<RoutingExplanationReason> {
    let mut reasons = vec![
        RoutingExplanationReason::EligibleEvidence {
            count: evidence.summary.eligible_runs,
        },
        RoutingExplanationReason::SimilarHistoricalTasks {
            revisions: evidence.summary.similar_task_revisions,
        },
    ];
    reasons.extend(evidence.summary.per_agent.iter().map(|count| {
        RoutingExplanationReason::AgentObservations {
            agent_id: count.agent_id.clone(),
            resolved: count.resolved,
        }
    }));
    reasons.extend(evidence.summary.excluded.iter().map(|count| {
        RoutingExplanationReason::ExcludedEvidence {
            reason: count.reason.clone(),
            count: count.count,
        }
    }));
    if let RoutingReadiness::InsufficientEvidence {
        reasons: missing, ..
    } = &evidence.readiness
    {
        reasons.extend(
            missing
                .iter()
                .cloned()
                .map(RoutingExplanationReason::InsufficientEvidence),
        );
    }
    if let Some(actual) = margin {
        reasons.push(RoutingExplanationReason::ScoreMargin {
            actual,
            required: required_margin,
        });
    }
    reasons
}

fn explanation(reasons: Vec<RoutingExplanationReason>) -> RoutingExplanation {
    RoutingExplanation {
        source: DecisionSource::HistoricalHeuristic,
        policy_version: ROUTER_VERSION.into(),
        reasons,
    }
}

fn uncertain_decision(
    request: &RoutingRequest,
    evidence: &RoutingEvidence,
    scores: Vec<AgentRoutingScore>,
    margin: Option<f64>,
    reasons: Vec<RoutingExplanationReason>,
) -> RoutingDecision {
    match request.exploration_policy() {
        forge_core::routing::ExplorationPolicy::None => insufficient(
            evidence,
            scores,
            margin,
            reasons,
            RoutingSuggestedAction::GatherLiveEvidence,
        ),
        forge_core::routing::ExplorationPolicy::CompeteWhenUncertain
        | forge_core::routing::ExplorationPolicy::PeriodicCompetition => {
            compete(evidence, scores, margin, reasons)
        }
    }
}

fn insufficient(
    evidence: &RoutingEvidence,
    scores: Vec<AgentRoutingScore>,
    margin: Option<f64>,
    reasons: Vec<RoutingExplanationReason>,
    suggested_action: RoutingSuggestedAction,
) -> RoutingDecision {
    RoutingDecision::InsufficientEvidence {
        evidence_summary: evidence.summary.clone(),
        snapshot: evidence.snapshot.clone(),
        explanation: explanation(reasons),
        suggested_action,
        scores,
        decision_margin: margin,
    }
}

fn compete(
    evidence: &RoutingEvidence,
    scores: Vec<AgentRoutingScore>,
    margin: Option<f64>,
    reasons: Vec<RoutingExplanationReason>,
) -> RoutingDecision {
    RoutingDecision::CompeteRecommended {
        evidence_summary: evidence.summary.clone(),
        snapshot: evidence.snapshot.clone(),
        explanation: explanation(reasons),
        scores,
        decision_margin: margin,
    }
}

fn routing_events(record: &RoutingDecisionRecord) -> Vec<RoutingEvent> {
    let summary = match &record.decision {
        RoutingDecision::Selected {
            evidence_summary, ..
        }
        | RoutingDecision::InsufficientEvidence {
            evidence_summary, ..
        }
        | RoutingDecision::CompeteRecommended {
            evidence_summary, ..
        } => evidence_summary,
    };
    let terminal = match &record.decision {
        RoutingDecision::Selected {
            agent,
            decision_margin,
            ..
        } => RoutingEventPayload::RoutingDecisionMade {
            selected_agent: agent.agent_id.clone(),
            margin: decision_margin.unwrap_or(0.0),
        },
        RoutingDecision::InsufficientEvidence { .. } => {
            RoutingEventPayload::RoutingInsufficientEvidence
        }
        RoutingDecision::CompeteRecommended { .. } => {
            RoutingEventPayload::RoutingCompetitionRecommended
        }
    };
    [
        RoutingEventPayload::RoutingStarted {
            candidate_count: record.candidates.len() as u64,
        },
        RoutingEventPayload::RoutingEvidenceResolved {
            eligible_runs: summary.eligible_runs,
            excluded_runs: summary.excluded.iter().map(|count| count.count).sum(),
            evidence_fingerprint: record.evidence_fingerprint.clone(),
        },
        terminal,
    ]
    .into_iter()
    .enumerate()
    .map(|(seq, payload)| RoutingEvent {
        decision_id: record.decision_id.clone(),
        seq: seq as u64 + 1,
        timestamp: record.created_at,
        payload,
    })
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_core::ids::{RunId, TaskId};
    use forge_core::routing::{
        AgentEvidenceCount, ExplorationPolicy, MinimumRoutingEvidence, RoutingEvidenceRecord,
        RoutingEvidenceSnapshot, RoutingEvidenceSummary, RoutingFeatures, UnresolvedRoutingTarget,
    };
    use forge_core::run::{
        AgentExecutionStatus, ExecutionProvenance, RunOutcome, RunStatus, SelectionSource, Usage,
    };
    use forge_core::task::{
        EngineeringTask, EvaluationSpec, TaskClassification, TaskMetadata, TaskRevision,
    };

    fn descriptor(
        id: &str,
        status: AdapterStatus,
        capabilities: Vec<Capability>,
    ) -> AgentDescriptor {
        AgentDescriptor {
            agent_id: AgentId::new(id).unwrap(),
            display_name: id.into(),
            harness: format!("{id}-harness"),
            executable: None,
            default_model: None,
            capabilities,
            adapter_status: status,
        }
    }

    fn request(id: &str, availability: CandidateAvailability) -> CandidateRequest {
        CandidateRequest {
            config: AgentConfig::new(AgentId::new(id).unwrap(), format!("{id}-harness")),
            availability,
        }
    }

    #[test]
    fn candidates_are_provider_agnostic_and_capability_checked() {
        let registry = vec![
            descriptor(
                "local-specialist",
                AdapterStatus::Implemented,
                vec![Capability::EditFiles, Capability::RunCommands],
            ),
            descriptor(
                "remote-worker",
                AdapterStatus::Implemented,
                vec![Capability::EditFiles, Capability::RunCommands],
            ),
        ];
        let requirements = CandidateRequirements {
            capabilities: BTreeSet::from([Capability::EditFiles, Capability::RunCommands]),
        };
        let candidates = resolve_candidates(
            &registry,
            vec![
                request("remote-worker", CandidateAvailability::Available),
                request("local-specialist", CandidateAvailability::Available),
            ],
            &requirements,
        )
        .unwrap();
        assert_eq!(
            candidates
                .agent_ids()
                .map(AgentId::as_str)
                .collect::<Vec<_>>(),
            vec!["local-specialist", "remote-worker"]
        );
    }

    #[test]
    fn unregistered_and_unavailable_candidates_are_rejected() {
        let registry = vec![descriptor(
            "registered",
            AdapterStatus::Implemented,
            Vec::new(),
        )];
        assert!(matches!(
            resolve_candidates(&registry, Vec::new(), &CandidateRequirements::default(),),
            Err(RouterError::NoAvailableCandidates)
        ));
        assert!(matches!(
            resolve_candidates(
                &registry,
                vec![request("missing", CandidateAvailability::Available)],
                &CandidateRequirements::default(),
            ),
            Err(RouterError::UnregisteredAgent(_))
        ));
        assert!(matches!(
            resolve_candidates(
                &registry,
                vec![request(
                    "registered",
                    CandidateAvailability::Unavailable {
                        reason: "not configured".into()
                    }
                )],
                &CandidateRequirements::default(),
            ),
            Err(RouterError::UnavailableAgent { .. })
        ));
    }

    fn revision() -> TaskRevision {
        TaskRevision::snapshot(EngineeringTask {
            task_id: TaskId::sequential(1042),
            repository: "forge".into(),
            objective: "Repair concurrent queue ordering".into(),
            constraints: Vec::new(),
            evaluation: EvaluationSpec::default(),
            protection: Default::default(),
            metadata: TaskMetadata::default(),
            classification: TaskClassification {
                category: Some("debugging".into()),
                language: Some("rust".into()),
                domain: Some("concurrency".into()),
                difficulty: Some("medium".into()),
            },
            components: vec!["scheduler".into()],
            tags: vec!["race".into()],
        })
        .unwrap()
    }

    fn routing_request(ids: &[&str], exploration: ExplorationPolicy) -> RoutingRequest {
        let candidates = CandidateAgentSet::new(
            ids.iter()
                .map(|id| {
                    let agent_id = AgentId::new(*id).unwrap();
                    CandidateAgent::new(
                        agent_id.clone(),
                        AgentConfig::new(agent_id, format!("{id}-harness")),
                    )
                    .unwrap()
                })
                .collect(),
        )
        .unwrap();
        RoutingRequest::new(
            revision(),
            candidates,
            forge_core::RoutingEvidencePolicy::default(),
            MinimumRoutingEvidence {
                total: 2,
                per_agent: 1,
            },
            exploration,
            DateTime::parse_from_rfc3339("2026-01-02T00:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        )
    }

    fn evidence(
        request: &RoutingRequest,
        observations: &[(&str, RoutingTarget, f64)],
        ready: bool,
    ) -> RoutingEvidence {
        let eligible = observations
            .iter()
            .enumerate()
            .map(|(index, (agent, target, similarity))| {
                let candidate = request
                    .candidates()
                    .as_slice()
                    .iter()
                    .find(|candidate| candidate.agent_id.as_str() == *agent)
                    .unwrap();
                RoutingEvidenceRecord {
                    run_id: RunId::sequential(index as u64 + 1),
                    task_revision_id: request.task_revision().revision_id().clone(),
                    agent_id: candidate.agent_id.clone(),
                    agent_config: candidate.config.clone(),
                    config_fingerprint: candidate.config_fingerprint.clone(),
                    features: RoutingFeatures::from_revision(request.task_revision()),
                    similarity_score: *similarity,
                    similarity_reasons: vec!["test".into()],
                    run_status: RunStatus::Completed,
                    agent_status: AgentExecutionStatus::Completed,
                    outcome: match target {
                        RoutingTarget::Positive => RunOutcome::Passed,
                        RoutingTarget::Negative => RunOutcome::Failed,
                        RoutingTarget::Unresolved(UnresolvedRoutingTarget::Inconclusive) => {
                            RunOutcome::Inconclusive
                        }
                        RoutingTarget::Unresolved(UnresolvedRoutingTarget::NoChange) => {
                            RunOutcome::NoChange
                        }
                    },
                    target: *target,
                    integrity: None,
                    evaluator_summary: None,
                    agent_runtime_ms: Some(1),
                    provider_reported_usage: Usage::default(),
                    known_cost_usd: None,
                    provenance: ExecutionProvenance::Live,
                    selection_source: SelectionSource::Manual,
                    experiment_id: None,
                    created_at: request.historical_cutoff(),
                }
            })
            .collect::<Vec<_>>();
        let per_agent = request
            .candidates()
            .agent_ids()
            .map(|id| {
                let records = eligible.iter().filter(|record| &record.agent_id == id);
                let targets = records.map(|record| record.target).collect::<Vec<_>>();
                let positive = targets
                    .iter()
                    .filter(|target| **target == RoutingTarget::Positive)
                    .count() as u64;
                let negative = targets
                    .iter()
                    .filter(|target| **target == RoutingTarget::Negative)
                    .count() as u64;
                AgentEvidenceCount {
                    agent_id: id.clone(),
                    eligible: targets.len() as u64,
                    resolved: positive + negative,
                    positive,
                    negative,
                    inconclusive: targets
                        .iter()
                        .filter(|target| {
                            **target
                                == RoutingTarget::Unresolved(UnresolvedRoutingTarget::Inconclusive)
                        })
                        .count() as u64,
                    no_change: targets
                        .iter()
                        .filter(|target| {
                            **target == RoutingTarget::Unresolved(UnresolvedRoutingTarget::NoChange)
                        })
                        .count() as u64,
                }
            })
            .collect::<Vec<_>>();
        let summary = RoutingEvidenceSummary {
            historical_runs_found: eligible.len() as u64,
            eligible_runs: eligible.len() as u64,
            resolved_runs: eligible
                .iter()
                .filter(|run| run.target.is_resolved())
                .count() as u64,
            similar_task_revisions: 1,
            excluded: Vec::new(),
            per_agent,
        };
        RoutingEvidence {
            snapshot: RoutingEvidenceSnapshot::build(request, &eligible, &[]).unwrap(),
            eligible,
            excluded: Vec::new(),
            readiness: if ready {
                RoutingReadiness::Ready
            } else {
                RoutingReadiness::InsufficientEvidence {
                    reasons: Vec::new(),
                    eligible_runs: summary.eligible_runs,
                    resolved_runs: summary.resolved_runs,
                    required_runs: request.minimum_evidence().total,
                }
            },
            summary,
        }
    }

    fn decide(
        request: &RoutingRequest,
        evidence: RoutingEvidence,
        config: &RoutingConfig,
    ) -> RoutingDecisionRecord {
        BaselineRouter::decide(
            request,
            evidence,
            config,
            RoutingDecisionId::sequential(1),
            request.historical_cutoff(),
        )
    }

    #[test]
    fn sufficient_weighted_history_selects_the_stronger_provider_agnostic_candidate() {
        let request = routing_request(
            &["claude", "codex", "local-specialist"],
            ExplorationPolicy::None,
        );
        let observations = [
            ("codex", RoutingTarget::Positive, 1.0),
            ("codex", RoutingTarget::Positive, 1.0),
            ("codex", RoutingTarget::Negative, 0.4),
            ("claude", RoutingTarget::Positive, 0.4),
            ("claude", RoutingTarget::Negative, 1.0),
            ("local-specialist", RoutingTarget::Positive, 0.4),
            ("local-specialist", RoutingTarget::Negative, 1.0),
            (
                "codex",
                RoutingTarget::Unresolved(UnresolvedRoutingTarget::Inconclusive),
                1.0,
            ),
            (
                "claude",
                RoutingTarget::Unresolved(UnresolvedRoutingTarget::NoChange),
                1.0,
            ),
        ];
        let record = decide(
            &request,
            evidence(&request, &observations, true),
            &RoutingConfig::default(),
        );
        assert!(matches!(
            record.decision,
            RoutingDecision::Selected { ref agent, .. } if agent.agent_id.as_str() == "codex"
        ));
        assert_eq!(record.router_version, ROUTER_VERSION);
    }

    #[test]
    fn conservative_prior_and_similarity_weighting_are_visible_in_scores() {
        let request = routing_request(&["alpha", "beta"], ExplorationPolicy::None);
        let observations = [
            ("alpha", RoutingTarget::Positive, 1.0),
            ("beta", RoutingTarget::Positive, 0.2),
            ("beta", RoutingTarget::Negative, 1.0),
        ];
        let record = decide(
            &request,
            evidence(&request, &observations, true),
            &RoutingConfig::default(),
        );
        let alpha = record
            .decision
            .scores()
            .iter()
            .find(|score| score.agent.agent_id.as_str() == "alpha")
            .unwrap();
        assert!((alpha.predicted_success - (2.0 / 3.0)).abs() < 1e-12);
        assert!(alpha.predicted_success < 1.0);
        assert_eq!(alpha.influential_runs.len(), 1);
    }

    #[test]
    fn readiness_margin_and_exploration_policies_do_not_force_a_choice() {
        let observations = [
            ("alpha", RoutingTarget::Positive, 1.0),
            ("beta", RoutingTarget::Positive, 1.0),
        ];
        let compete_request =
            routing_request(&["alpha", "beta"], ExplorationPolicy::CompeteWhenUncertain);
        let compete = decide(
            &compete_request,
            evidence(&compete_request, &observations, true),
            &RoutingConfig::default(),
        );
        assert!(matches!(
            compete.decision,
            RoutingDecision::CompeteRecommended { .. }
        ));

        let none_request = routing_request(&["alpha", "beta"], ExplorationPolicy::None);
        let insufficient = decide(
            &none_request,
            evidence(&none_request, &observations, false),
            &RoutingConfig::default(),
        );
        assert!(matches!(
            insufficient.decision,
            RoutingDecision::InsufficientEvidence { .. }
        ));
    }

    #[test]
    fn one_candidate_requires_explicit_selection_and_periodic_policy_is_deterministic() {
        let one = routing_request(&["alpha"], ExplorationPolicy::None);
        let one_result = decide(
            &one,
            evidence(&one, &[("alpha", RoutingTarget::Positive, 1.0)], true),
            &RoutingConfig::default(),
        );
        assert!(matches!(
            one_result.decision,
            RoutingDecision::InsufficientEvidence { .. }
        ));

        let periodic = routing_request(&["alpha", "beta"], ExplorationPolicy::PeriodicCompetition);
        let observations = (0..10)
            .map(|index| {
                if index < 5 {
                    ("alpha", RoutingTarget::Positive, 1.0)
                } else if index < 8 {
                    ("beta", RoutingTarget::Negative, 1.0)
                } else {
                    ("beta", RoutingTarget::Positive, 1.0)
                }
            })
            .collect::<Vec<_>>();
        let result = decide(
            &periodic,
            evidence(&periodic, &observations, true),
            &RoutingConfig::default(),
        );
        assert!(matches!(
            result.decision,
            RoutingDecision::CompeteRecommended { .. }
        ));
    }

    #[test]
    fn identical_snapshot_and_cutoff_recompute_identically() {
        let request = routing_request(&["alpha", "beta"], ExplorationPolicy::None);
        let history = [
            ("alpha", RoutingTarget::Positive, 1.0),
            ("beta", RoutingTarget::Negative, 1.0),
        ];
        let evidence = evidence(&request, &history, true);
        let first = decide(&request, evidence.clone(), &RoutingConfig::default());
        let second = decide(&request, evidence, &RoutingConfig::default());
        assert_eq!(first, second);
        assert_eq!(
            first.decision.snapshot().routing_policy_version.as_deref(),
            Some(ROUTER_VERSION)
        );
    }

    #[tokio::test]
    async fn route_persists_decision_and_structured_events() {
        let store = Store::open_in_memory().await.unwrap();
        let request = routing_request(&["alpha", "beta"], ExplorationPolicy::CompeteWhenUncertain);
        store
            .upsert_task(request.task_revision().task())
            .await
            .unwrap();
        let record = RoutingContract::new(store.clone())
            .route(&request, &RoutingConfig::default())
            .await
            .unwrap();
        assert_eq!(
            store
                .load_routing_decision(&record.decision_id)
                .await
                .unwrap(),
            Some(record.clone())
        );
        let events = store.routing_events_for(&record.decision_id).await.unwrap();
        assert_eq!(events.len(), 3);
        assert!(matches!(
            events.last().unwrap().payload,
            RoutingEventPayload::RoutingCompetitionRecommended
        ));
    }
}
