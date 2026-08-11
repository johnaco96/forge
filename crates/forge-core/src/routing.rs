//! Provider-agnostic contracts for evidence-based agent routing.
//!
//! Phase 4A deliberately defines inputs, evidence trust policy, readiness,
//! reproducibility, and future decision output without selecting an agent.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::agent::AgentConfig;
use crate::ids::{AgentId, ExperimentId, RoutingDecisionId, RunId, TaskId};
use crate::integrity::IntegrityStatus;
use crate::result::EvaluationSummary;
use crate::run::{
    AgentExecutionStatus, ExecutionProvenance, RunOutcome, RunStatus, SelectionSource, Usage,
};
use crate::task::{TaskClassification, TaskRevision, TaskRevisionId};

pub const ROUTING_CONTRACT_VERSION: &str = "routing-contract-v1";
pub const DEFAULT_EVIDENCE_POLICY_VERSION: &str = "routing-evidence-v1";

#[derive(Debug, thiserror::Error)]
pub enum RoutingContractError {
    #[error("routing requires at least one candidate agent")]
    NoCandidates,
    #[error("candidate agent `{0}` appears more than once")]
    DuplicateCandidate(AgentId),
    #[error("candidate configuration for `{candidate}` belongs to `{configured}`")]
    CandidateConfigMismatch {
        candidate: AgentId,
        configured: AgentId,
    },
    #[error("failed to serialize a routing evidence snapshot")]
    SnapshotSerialization(#[from] serde_json::Error),
}

/// Information that the roadmap anticipates but Forge cannot reliably know
/// before an agent runs. Recording absence prevents an estimator from being
/// mistaken for observed fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnavailableRoutingFeatureKind {
    ExpectedPatchLines,
    ExpectedCodeComplexity,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnavailableRoutingFeature {
    pub feature: UnavailableRoutingFeatureKind,
    pub reason: String,
}

/// Task facts available before the candidate run starts.
///
/// Actual patch size, runtime, token usage, cost, and evaluator results are
/// intentionally absent; those are targets or historical observations, not
/// features of the run being routed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoutingFeatures {
    pub task_id: TaskId,
    pub task_revision_id: TaskRevisionId,
    pub repository: String,
    pub classification: TaskClassification,
    pub components: Vec<String>,
    pub tags: Vec<String>,
    /// Deterministic normalized terms from the objective, never an LLM label.
    pub objective_terms: Vec<String>,
    pub unavailable: Vec<UnavailableRoutingFeature>,
}

impl RoutingFeatures {
    pub fn from_revision(revision: &TaskRevision) -> Self {
        let task = revision.task();
        let mut components = task.components.clone();
        components.sort();
        components.dedup();
        let mut tags = task.tags.clone();
        tags.sort();
        tags.dedup();
        let objective_terms = task
            .objective
            .split(|character: char| !character.is_alphanumeric())
            .filter(|term| term.chars().count() >= 3)
            .map(str::to_lowercase)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let unavailable = [
            UnavailableRoutingFeatureKind::ExpectedPatchLines,
            UnavailableRoutingFeatureKind::ExpectedCodeComplexity,
        ]
        .into_iter()
        .map(|feature| UnavailableRoutingFeature {
            feature,
            reason: "not reliably known before agent execution".into(),
        })
        .collect();
        Self {
            task_id: task.task_id.clone(),
            task_revision_id: revision.revision_id().clone(),
            repository: task.repository.clone(),
            classification: task.effective_classification(),
            components,
            tags,
            objective_terms,
            unavailable,
        }
    }
}

/// One registered, available, configured candidate resolved before routing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidateAgent {
    pub agent_id: AgentId,
    pub config: AgentConfig,
    pub config_fingerprint: String,
}

impl CandidateAgent {
    pub fn new(agent_id: AgentId, config: AgentConfig) -> Result<Self, RoutingContractError> {
        if agent_id != config.agent_id {
            return Err(RoutingContractError::CandidateConfigMismatch {
                candidate: agent_id,
                configured: config.agent_id,
            });
        }
        let config_fingerprint = config.fingerprint();
        Ok(Self {
            agent_id,
            config,
            config_fingerprint,
        })
    }
}

/// A deterministic, duplicate-free candidate set. It contains only candidates
/// already proven registered, available, configured, and eligible by the
/// resolver at the `forge-router` boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidateAgentSet(Vec<CandidateAgent>);

impl CandidateAgentSet {
    pub fn new(mut candidates: Vec<CandidateAgent>) -> Result<Self, RoutingContractError> {
        if candidates.is_empty() {
            return Err(RoutingContractError::NoCandidates);
        }
        candidates.sort_by(|left, right| {
            left.agent_id
                .cmp(&right.agent_id)
                .then_with(|| left.config_fingerprint.cmp(&right.config_fingerprint))
        });
        let mut seen = BTreeSet::new();
        for candidate in &candidates {
            if !seen.insert(candidate.agent_id.clone()) {
                return Err(RoutingContractError::DuplicateCandidate(
                    candidate.agent_id.clone(),
                ));
            }
        }
        Ok(Self(candidates))
    }

    pub fn as_slice(&self) -> &[CandidateAgent] {
        &self.0
    }

    pub fn agent_ids(&self) -> impl Iterator<Item = &AgentId> {
        self.0.iter().map(|candidate| &candidate.agent_id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EvidencePolicyVersion(pub String);

/// Trust filters applied before historical evidence reaches a router.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoutingEvidencePolicy {
    pub version: EvidencePolicyVersion,
    pub allowed_provenance: BTreeSet<ExecutionProvenance>,
    pub require_completed_run: bool,
    pub require_execution_record: bool,
    pub require_acceptable_integrity: bool,
    pub exclude_evaluator_infrastructure_errors: bool,
    /// Repository equality contributes 0.20 in Forge's transparent similarity
    /// model, so this default retains repository history without pretending it
    /// is more comparable than it is.
    pub minimum_similarity_score: f64,
}

impl Default for RoutingEvidencePolicy {
    fn default() -> Self {
        Self {
            version: EvidencePolicyVersion(DEFAULT_EVIDENCE_POLICY_VERSION.into()),
            allowed_provenance: BTreeSet::from([ExecutionProvenance::Live]),
            require_completed_run: true,
            require_execution_record: true,
            require_acceptable_integrity: true,
            exclude_evaluator_infrastructure_errors: true,
            minimum_similarity_score: 0.20,
        }
    }
}

/// Simple readiness thresholds. Only resolved PASS/FAIL targets satisfy these
/// minima; unresolved evidence remains visible but cannot manufacture
/// predictive sample size.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MinimumRoutingEvidence {
    pub total: u64,
    pub per_agent: u64,
}

impl Default for MinimumRoutingEvidence {
    fn default() -> Self {
        Self {
            total: 10,
            per_agent: 3,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExplorationPolicy {
    None,
    #[default]
    CompeteWhenUncertain,
    PeriodicCompetition,
}

/// Exact immutable input to the future router.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoutingRequest {
    task_revision: TaskRevision,
    features: RoutingFeatures,
    candidates: CandidateAgentSet,
    evidence_policy: RoutingEvidencePolicy,
    minimum_evidence: MinimumRoutingEvidence,
    exploration_policy: ExplorationPolicy,
    historical_cutoff: DateTime<Utc>,
}

impl RoutingRequest {
    pub fn new(
        task_revision: TaskRevision,
        candidates: CandidateAgentSet,
        evidence_policy: RoutingEvidencePolicy,
        minimum_evidence: MinimumRoutingEvidence,
        exploration_policy: ExplorationPolicy,
        historical_cutoff: DateTime<Utc>,
    ) -> Self {
        let features = RoutingFeatures::from_revision(&task_revision);
        Self {
            task_revision,
            features,
            candidates,
            evidence_policy,
            minimum_evidence,
            exploration_policy,
            historical_cutoff,
        }
    }

    pub fn task_revision(&self) -> &TaskRevision {
        &self.task_revision
    }

    pub fn features(&self) -> &RoutingFeatures {
        &self.features
    }

    pub fn candidates(&self) -> &CandidateAgentSet {
        &self.candidates
    }

    pub fn evidence_policy(&self) -> &RoutingEvidencePolicy {
        &self.evidence_policy
    }

    pub fn minimum_evidence(&self) -> MinimumRoutingEvidence {
        self.minimum_evidence
    }

    pub fn exploration_policy(&self) -> ExplorationPolicy {
        self.exploration_policy
    }

    pub fn historical_cutoff(&self) -> DateTime<Utc> {
        self.historical_cutoff
    }
}

/// Historical outcome target exposed to a future router.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "class", content = "outcome", rename_all = "snake_case")]
pub enum RoutingTarget {
    Positive,
    Negative,
    Unresolved(UnresolvedRoutingTarget),
}

impl RoutingTarget {
    pub fn from_outcome(outcome: RunOutcome) -> Option<Self> {
        match outcome {
            RunOutcome::Passed => Some(Self::Positive),
            RunOutcome::Failed => Some(Self::Negative),
            RunOutcome::Inconclusive => {
                Some(Self::Unresolved(UnresolvedRoutingTarget::Inconclusive))
            }
            RunOutcome::NoChange => Some(Self::Unresolved(UnresolvedRoutingTarget::NoChange)),
            RunOutcome::Errored => None,
        }
    }

    pub fn is_resolved(self) -> bool {
        matches!(self, Self::Positive | Self::Negative)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnresolvedRoutingTarget {
    Inconclusive,
    NoChange,
}

/// A compact historical observation; large logs and patch content remain in
/// the ledger and never cross this analytical boundary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoutingEvidenceRecord {
    pub run_id: RunId,
    pub task_revision_id: TaskRevisionId,
    pub agent_id: AgentId,
    pub agent_config: AgentConfig,
    pub config_fingerprint: String,
    pub features: RoutingFeatures,
    pub similarity_score: f64,
    pub similarity_reasons: Vec<String>,
    pub run_status: RunStatus,
    pub agent_status: AgentExecutionStatus,
    pub outcome: RunOutcome,
    pub target: RoutingTarget,
    pub integrity: Option<IntegrityStatus>,
    pub evaluator_summary: Option<EvaluationSummary>,
    pub agent_runtime_ms: Option<u64>,
    pub provider_reported_usage: Usage,
    pub known_cost_usd: Option<f64>,
    pub provenance: ExecutionProvenance,
    pub selection_source: SelectionSource,
    pub experiment_id: Option<ExperimentId>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum EvidenceExclusionReason {
    SyntheticProvenance,
    UnknownProvenance,
    ImportedProvenance,
    ProvenanceNotAllowed {
        provenance: ExecutionProvenance,
    },
    IncompleteRun {
        status: RunStatus,
    },
    InfrastructureFailure,
    MissingExecution,
    MissingOutcome,
    MissingIntegrity,
    IntegrityViolation {
        status: IntegrityStatus,
    },
    MissingEvaluation,
    EvaluatorInfrastructureFailure,
    InsufficientSimilarity,
    CandidateConfigurationMismatch {
        agent_id: AgentId,
        config_fingerprint: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExcludedRoutingEvidence {
    pub run_id: RunId,
    pub reason: EvidenceExclusionReason,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceExclusionCount {
    pub reason: EvidenceExclusionReason,
    pub count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentEvidenceCount {
    pub agent_id: AgentId,
    pub eligible: u64,
    pub resolved: u64,
    pub positive: u64,
    pub negative: u64,
    pub inconclusive: u64,
    pub no_change: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum RoutingReadinessReason {
    NoEligibleLiveHistory,
    NoComparableHistoricalTasks,
    OnlyOneCandidateHasResolvedEvidence,
    InsufficientTotalEvidence {
        available: u64,
        required: u64,
    },
    InsufficientAgentEvidence {
        agent_id: AgentId,
        available: u64,
        required: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum RoutingReadiness {
    Ready,
    InsufficientEvidence {
        reasons: Vec<RoutingReadinessReason>,
        eligible_runs: u64,
        resolved_runs: u64,
        required_runs: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoutingEvidenceSummary {
    pub historical_runs_found: u64,
    pub eligible_runs: u64,
    pub resolved_runs: u64,
    pub similar_task_revisions: u64,
    pub excluded: Vec<EvidenceExclusionCount>,
    pub per_agent: Vec<AgentEvidenceCount>,
}

/// The exact evidence boundary behind a future decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoutingEvidenceSnapshot {
    pub routing_contract_version: String,
    /// `None` in Phase 4A because no selection policy is implemented yet.
    pub routing_policy_version: Option<String>,
    pub evidence_policy_version: EvidencePolicyVersion,
    pub task_revision_id: TaskRevisionId,
    pub candidate_config_fingerprints: BTreeMap<AgentId, String>,
    pub historical_cutoff: DateTime<Utc>,
    pub minimum_evidence: MinimumRoutingEvidence,
    pub eligible_run_ids: Vec<RunId>,
    pub evidence_fingerprint: String,
}

impl RoutingEvidenceSnapshot {
    pub fn build(
        request: &RoutingRequest,
        eligible: &[RoutingEvidenceRecord],
        excluded: &[ExcludedRoutingEvidence],
    ) -> Result<Self, RoutingContractError> {
        #[derive(Serialize)]
        struct FingerprintInput<'a> {
            contract: &'static str,
            request: &'a RoutingRequest,
            eligible: &'a [RoutingEvidenceRecord],
            excluded: &'a [ExcludedRoutingEvidence],
        }
        let bytes = serde_json::to_vec(&FingerprintInput {
            contract: ROUTING_CONTRACT_VERSION,
            request,
            eligible,
            excluded,
        })?;
        let digest = Sha256::digest(bytes);
        let evidence_fingerprint = digest.iter().map(|byte| format!("{byte:02x}")).collect();
        Ok(Self {
            routing_contract_version: ROUTING_CONTRACT_VERSION.into(),
            routing_policy_version: None,
            evidence_policy_version: request.evidence_policy.version.clone(),
            task_revision_id: request.task_revision.revision_id().clone(),
            candidate_config_fingerprints: request
                .candidates
                .as_slice()
                .iter()
                .map(|candidate| {
                    (
                        candidate.agent_id.clone(),
                        candidate.config_fingerprint.clone(),
                    )
                })
                .collect(),
            historical_cutoff: request.historical_cutoff,
            minimum_evidence: request.minimum_evidence,
            eligible_run_ids: eligible
                .iter()
                .map(|record| record.run_id.clone())
                .collect(),
            evidence_fingerprint,
        })
    }

    pub fn set_routing_policy_version(&mut self, version: impl Into<String>) {
        self.routing_policy_version = Some(version.into());
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoutingEvidence {
    pub eligible: Vec<RoutingEvidenceRecord>,
    pub excluded: Vec<ExcludedRoutingEvidence>,
    pub summary: RoutingEvidenceSummary,
    pub readiness: RoutingReadiness,
    pub snapshot: RoutingEvidenceSnapshot,
}

/// Structured explanation inputs. Rendering these is deterministic and does
/// not require an LLM.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RoutingExplanationReason {
    EligibleEvidence {
        count: u64,
    },
    SimilarHistoricalTasks {
        revisions: u64,
    },
    AgentObservations {
        agent_id: AgentId,
        resolved: u64,
    },
    ExcludedEvidence {
        reason: EvidenceExclusionReason,
        count: u64,
    },
    InsufficientEvidence(RoutingReadinessReason),
    ScoreMargin {
        actual: f64,
        required: f64,
    },
    OnlyOneCandidateAvailable,
    PeriodicCompetition {
        resolved_observations: u64,
        interval: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionSource {
    HistoricalHeuristic,
    LearnedModel,
    ManualPolicy,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoutingExplanation {
    pub source: DecisionSource,
    pub policy_version: String,
    pub reasons: Vec<RoutingExplanationReason>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InfluentialRoutingRun {
    pub run_id: RunId,
    pub task_revision_id: TaskRevisionId,
    pub target: RoutingTarget,
    pub similarity_weight: f64,
    pub experiment_id: Option<ExperimentId>,
}

/// A score for one exact currently configured candidate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentRoutingScore {
    pub agent: CandidateAgent,
    pub predicted_success: f64,
    pub routing_score: f64,
    pub resolved_evidence_count: u64,
    pub positive_count: u64,
    pub negative_count: u64,
    pub unresolved_count: u64,
    pub weighted_similarity_evidence: f64,
    pub evidence_strength: f64,
    pub influential_runs: Vec<InfluentialRoutingRun>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoutingPolicyConfiguration {
    pub prior_alpha: f64,
    pub prior_beta: f64,
    pub minimum_score_margin: f64,
    pub periodic_competition_interval: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoutingDecisionKind {
    Selected,
    InsufficientEvidence,
    CompeteRecommended,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoutingSuggestedAction {
    GatherLiveEvidence,
    Compete,
    SelectManually,
}

/// Output contract for a future router. Phase 4A never constructs `Selected`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum RoutingDecision {
    Selected {
        agent: CandidateAgent,
        evidence_summary: RoutingEvidenceSummary,
        snapshot: RoutingEvidenceSnapshot,
        explanation: RoutingExplanation,
        scores: Vec<AgentRoutingScore>,
        decision_margin: Option<f64>,
    },
    InsufficientEvidence {
        evidence_summary: RoutingEvidenceSummary,
        snapshot: RoutingEvidenceSnapshot,
        explanation: RoutingExplanation,
        suggested_action: RoutingSuggestedAction,
        scores: Vec<AgentRoutingScore>,
        decision_margin: Option<f64>,
    },
    CompeteRecommended {
        evidence_summary: RoutingEvidenceSummary,
        snapshot: RoutingEvidenceSnapshot,
        explanation: RoutingExplanation,
        scores: Vec<AgentRoutingScore>,
        decision_margin: Option<f64>,
    },
}

impl RoutingDecision {
    pub fn kind(&self) -> RoutingDecisionKind {
        match self {
            Self::Selected { .. } => RoutingDecisionKind::Selected,
            Self::InsufficientEvidence { .. } => RoutingDecisionKind::InsufficientEvidence,
            Self::CompeteRecommended { .. } => RoutingDecisionKind::CompeteRecommended,
        }
    }

    pub fn snapshot(&self) -> &RoutingEvidenceSnapshot {
        match self {
            Self::Selected { snapshot, .. }
            | Self::InsufficientEvidence { snapshot, .. }
            | Self::CompeteRecommended { snapshot, .. } => snapshot,
        }
    }

    pub fn scores(&self) -> &[AgentRoutingScore] {
        match self {
            Self::Selected { scores, .. }
            | Self::InsufficientEvidence { scores, .. }
            | Self::CompeteRecommended { scores, .. } => scores,
        }
    }
}

/// Complete durable answer to “why did Forge choose this at that time?”.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoutingDecisionRecord {
    pub decision_id: RoutingDecisionId,
    pub run_id: Option<RunId>,
    pub task_id: TaskId,
    pub task_revision_id: TaskRevisionId,
    pub created_at: DateTime<Utc>,
    pub candidates: Vec<CandidateAgent>,
    pub selected: Option<CandidateAgent>,
    pub router_version: String,
    pub evidence_policy_version: EvidencePolicyVersion,
    pub policy_configuration: RoutingPolicyConfiguration,
    pub historical_cutoff: DateTime<Utc>,
    pub evidence_fingerprint: String,
    pub decision: RoutingDecision,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoutingEvent {
    pub decision_id: RoutingDecisionId,
    pub seq: u64,
    pub timestamp: DateTime<Utc>,
    pub payload: RoutingEventPayload,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RoutingEventPayload {
    RoutingStarted {
        candidate_count: u64,
    },
    RoutingEvidenceResolved {
        eligible_runs: u64,
        excluded_runs: u64,
        evidence_fingerprint: String,
    },
    RoutingDecisionMade {
        selected_agent: AgentId,
        margin: f64,
    },
    RoutingInsufficientEvidence,
    RoutingCompetitionRecommended,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::AgentConfig;
    use crate::ids::{AgentId, TaskId};
    use crate::task::{EngineeringTask, EvaluationSpec, TaskClassification, TaskMetadata};

    fn revision() -> TaskRevision {
        TaskRevision::snapshot(EngineeringTask {
            task_id: TaskId::sequential(1042),
            repository: "forge".into(),
            objective: "Repair concurrent queue wakeup ordering".into(),
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

    #[test]
    fn features_contain_only_pre_run_facts_and_explicit_absences() {
        let features = RoutingFeatures::from_revision(&revision());
        let json = serde_json::to_value(&features).unwrap();
        for leaked in ["runtime", "tokens", "cost", "actual_patch", "evaluation"] {
            assert!(json.get(leaked).is_none(), "leaked post-run key `{leaked}`");
        }
        assert_eq!(
            features.classification.category.as_deref(),
            Some("debugging")
        );
        assert!(features.unavailable.iter().any(|feature| {
            feature.feature == UnavailableRoutingFeatureKind::ExpectedPatchLines
        }));
    }

    #[test]
    fn candidate_sets_are_provider_agnostic_sorted_and_unique() {
        let local = AgentId::new("local-specialist").unwrap();
        let remote = AgentId::new("remote-worker").unwrap();
        let set = CandidateAgentSet::new(vec![
            CandidateAgent::new(
                remote.clone(),
                AgentConfig::new(remote.clone(), "remote-harness"),
            )
            .unwrap(),
            CandidateAgent::new(
                local.clone(),
                AgentConfig::new(local.clone(), "local-harness"),
            )
            .unwrap(),
        ])
        .unwrap();
        assert_eq!(
            set.agent_ids().map(AgentId::as_str).collect::<Vec<_>>(),
            vec!["local-specialist", "remote-worker"]
        );

        let duplicate =
            CandidateAgent::new(local.clone(), AgentConfig::new(local, "another-config")).unwrap();
        assert!(matches!(
            CandidateAgentSet::new(vec![set.as_slice()[0].clone(), duplicate]),
            Err(RoutingContractError::DuplicateCandidate(_))
        ));
    }

    #[test]
    fn routing_request_owns_an_immutable_task_snapshot() {
        let revision = revision();
        let id = AgentId::new("local").unwrap();
        let candidates = CandidateAgentSet::new(vec![
            CandidateAgent::new(id.clone(), AgentConfig::new(id, "local")).unwrap(),
        ])
        .unwrap();
        let request = RoutingRequest::new(
            revision.clone(),
            candidates,
            RoutingEvidencePolicy::default(),
            MinimumRoutingEvidence::default(),
            ExplorationPolicy::default(),
            Utc::now(),
        );
        assert_eq!(request.task_revision(), &revision);
        assert_eq!(
            request.features().task_revision_id,
            revision.revision_id().clone()
        );
    }
}
