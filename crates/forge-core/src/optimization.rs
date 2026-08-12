//! Evidence, proposals, experiments, and decisions for policy optimization.
//!
//! ```text
//! persisted Phase 0–7 evidence
//!            │  fixed cutoff
//!            ▼
//! PolicyEvidenceSnapshot  (eligible + excluded, fingerprinted)
//!            │
//!            ▼
//!    PolicyProposal  ──▶ shadow / canary experiment ──▶ promotion gate
//!            │
//!            ▼
//!    PolicyDecision   attached to every policy-governed execution
//! ```
//!
//! Two disciplines run through everything here.
//!
//! **Nothing is discarded silently.** Every observation the optimizer saw is
//! either eligible or excluded *with a reason*. An optimizer that could quietly
//! drop inconvenient evidence would be able to improve its apparent
//! performance without improving anything.
//!
//! **Nothing is invented.** A shadow policy records what it would have chosen
//! and never what would have happened. Missing health is missing, unknown cost
//! is unknown, and an execution that did not occur has no outcome.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::health::AttributionLevel;
use crate::ids::{
    HealthSnapshotId, PolicyDecisionId, PolicyExperimentId, PolicyId, PolicyProposalId, RunId,
    WorldModelSnapshotId,
};
use crate::policy::{
    ApprovalRequirement, ObjectiveConstraint, ObjectiveKind, ObjectiveMetric, OptimizableDimension,
    OptimizationObjective, PolicyComparison,
};
use crate::result::Direction;
use crate::run::{ExecutionProvenance, RunOutcome};
use crate::task::TaskRevisionId;

/// Identity of the evidence-gathering semantics.
pub const POLICY_EVIDENCE_VERSION: &str = "policy-evidence-v1";
/// Identity of the deterministic experiment-assignment rule.
pub const POLICY_ASSIGNMENT_VERSION: &str = "policy-assignment-v1";

// ----------------------------------------------------------------- observation

/// How an execution came to use the strategy it used.
///
/// Preserved rather than flattened: evidence from a canary arm, a control arm,
/// an ordinary active-policy run, and a user override are not identically
/// sampled, and an optimizer that treated them as one pool would be learning
/// partly from its own selection choices.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationSource {
    /// Governed by the active policy in the ordinary way.
    ActivePolicy,
    /// Assigned to the candidate arm of a controlled experiment.
    CanaryCandidate,
    /// Assigned to the control arm of a controlled experiment.
    CanaryControl,
    /// The user named the strategy explicitly.
    ManualOverride,
    /// Recorded before Phase 8 existed; no policy governed it.
    Legacy,
}

impl ObservationSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ActivePolicy => "active_policy",
            Self::CanaryCandidate => "canary_candidate",
            Self::CanaryControl => "canary_control",
            Self::ManualOverride => "manual_override",
            Self::Legacy => "legacy",
        }
    }

    /// Whether the strategy was chosen by policy rather than by a person.
    pub fn is_policy_controlled(self) -> bool {
        matches!(
            self,
            Self::ActivePolicy | Self::CanaryCandidate | Self::CanaryControl
        )
    }
}

impl std::fmt::Display for ObservationSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One historical execution, in the shape optimization needs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PolicyObservation {
    pub run_id: RunId,
    pub task_revision_id: TaskRevisionId,
    /// The policy that governed it, when one did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy_id: Option<PolicyId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy_fingerprint: Option<String>,
    pub source: ObservationSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub experiment: Option<ExperimentMembership>,
    pub provenance: ExecutionProvenance,
    pub outcome: RunOutcome,
    /// Whether the evaluation's own inputs stayed intact.
    pub integrity_clean: bool,
    /// Agent configuration fingerprint, for comparability.
    pub config_fingerprint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_ms: Option<u64>,
    /// `None` means unknown, which is never zero.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patch_lines: Option<u64>,
    /// The repository state the run's evidence describes (Phase 7 semantics).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub measured_commit: Option<String>,
    pub observed_at: DateTime<Utc>,
}

impl PolicyObservation {
    /// Whether the run represents an engineering success.
    ///
    /// `Errored` is deliberately absent: Forge failing to carry a run through
    /// its pipeline is infrastructure trouble, not an engineering result, and
    /// counting it either way would corrupt the comparison.
    pub fn is_success(&self) -> bool {
        self.outcome == RunOutcome::Passed
    }

    pub fn is_infrastructure_failure(&self) -> bool {
        self.outcome == RunOutcome::Errored
    }
}

/// An observation's place in a controlled experiment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExperimentMembership {
    pub experiment_id: PolicyExperimentId,
    pub arm: ExperimentArm,
}

/// Which side of a controlled experiment an execution was on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExperimentArm {
    Control,
    Candidate,
}

impl ExperimentArm {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Control => "control",
            Self::Candidate => "candidate",
        }
    }
}

impl std::fmt::Display for ExperimentArm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Why an observation could not be used.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum EvidenceExclusion {
    WrongRepository,
    /// Governed by neither policy under comparison.
    PolicyMismatch,
    /// Agent or evaluation configuration is not comparable.
    IncomparableConfiguration {
        detail: String,
    },
    /// No policy identity, so it cannot be attributed to either arm.
    MissingPolicyIdentity,
    /// Created after the decision's evidence cutoff.
    PostCutoff,
    /// Provenance the comparison does not accept.
    DisallowedProvenance {
        provenance: ExecutionProvenance,
    },
    /// Forge could not carry the run through; says nothing about engineering.
    InfrastructureFailure,
    /// No nameable repository state, so health cannot be tied to it.
    MissingMeasuredCommit,
    /// The user chose the strategy, so it does not measure policy.
    ManualOverride,
    /// Outside the objective's observation window.
    OutsideObservationWindow,
    /// The resolver's explicit collection cap was reached.
    CollectionLimit,
}

impl EvidenceExclusion {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::WrongRepository => "wrong_repository",
            Self::PolicyMismatch => "policy_mismatch",
            Self::IncomparableConfiguration { .. } => "incomparable_configuration",
            Self::MissingPolicyIdentity => "missing_policy_identity",
            Self::PostCutoff => "post_cutoff",
            Self::DisallowedProvenance { .. } => "disallowed_provenance",
            Self::InfrastructureFailure => "infrastructure_failure",
            Self::MissingMeasuredCommit => "missing_measured_commit",
            Self::ManualOverride => "manual_override",
            Self::OutsideObservationWindow => "outside_observation_window",
            Self::CollectionLimit => "collection_limit",
        }
    }

    pub fn describe(&self) -> String {
        match self {
            Self::WrongRepository => "belongs to another repository".into(),
            Self::PolicyMismatch => "governed by neither compared policy".into(),
            Self::IncomparableConfiguration { detail } => {
                format!("configuration is not comparable: {detail}")
            }
            Self::MissingPolicyIdentity => "no policy identity recorded".into(),
            Self::PostCutoff => "created after the evidence cutoff".into(),
            Self::DisallowedProvenance { provenance } => {
                format!("provenance `{}` is not accepted here", provenance.as_str())
            }
            Self::InfrastructureFailure => {
                "Forge could not complete the run; not an engineering outcome".into()
            }
            Self::MissingMeasuredCommit => "no measured repository state".into(),
            Self::ManualOverride => {
                "the user chose the strategy, so it does not measure policy".into()
            }
            Self::OutsideObservationWindow => "outside the objective's observation window".into(),
            Self::CollectionLimit => "outside the resolver's explicit collection limit".into(),
        }
    }
}

/// An observation and why it was left out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExcludedObservation {
    pub run_id: RunId,
    pub exclusion: EvidenceExclusion,
}

/// A health snapshot available to the decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthEvidenceRef {
    pub health_snapshot_id: HealthSnapshotId,
    pub commit: String,
    pub observed_at: DateTime<Utc>,
}

// ------------------------------------------------------------------- snapshot

/// The complete, immutable evidence a proposal was computed from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PolicyEvidenceSnapshot {
    pub repository: String,
    /// Nothing after this instant may influence the result.
    pub cutoff: DateTime<Utc>,
    pub active_policy_id: PolicyId,
    pub active_policy_fingerprint: String,
    pub candidate_policy_fingerprints: Vec<String>,
    pub eligible: Vec<PolicyObservation>,
    pub excluded: Vec<ExcludedObservation>,
    pub health: Vec<HealthEvidenceRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub world_model_snapshot_id: Option<WorldModelSnapshotId>,
    pub evidence_version: String,
    pub observation_window_days: u32,
}

impl PolicyEvidenceSnapshot {
    /// Deterministic identity of the evidence set and its configuration.
    ///
    /// Recomputing a proposal from the same inputs must reproduce this exactly,
    /// which is what makes a historical recommendation auditable rather than
    /// merely recorded.
    pub fn fingerprint(&self) -> String {
        let mut digest = Sha256::new();
        let mut field = |value: &str| {
            digest.update(value.as_bytes());
            digest.update([0x1f]);
        };
        field(&self.repository);
        field(&self.cutoff.to_rfc3339());
        field(self.active_policy_id.as_str());
        field(&self.active_policy_fingerprint);
        for candidate in &self.candidate_policy_fingerprints {
            field(candidate);
        }
        field(&self.evidence_version);
        field(&self.observation_window_days.to_string());
        field(
            self.world_model_snapshot_id
                .as_ref()
                .map(WorldModelSnapshotId::as_str)
                .unwrap_or("none"),
        );

        // Eligible and excluded ids both matter: two runs with the same
        // eligible set but different exclusions were computed from different
        // evidence and must not share a fingerprint.
        for observation in &self.eligible {
            field(observation.run_id.as_str());
            field(observation.task_revision_id.as_str());
            field(
                observation
                    .policy_id
                    .as_ref()
                    .map(PolicyId::as_str)
                    .unwrap_or("none"),
            );
            field(observation.source.as_str());
            field(observation.policy_fingerprint.as_deref().unwrap_or("none"));
            if let Some(experiment) = &observation.experiment {
                field(experiment.experiment_id.as_str());
                field(experiment.arm.as_str());
            } else {
                field("no-experiment");
            }
            field(observation.provenance.as_str());
            field(observation.outcome.as_str());
            field(if observation.integrity_clean {
                "integrity-clean"
            } else {
                "integrity-not-clean"
            });
            field(&observation.config_fingerprint);
            field(&optional_u64(observation.runtime_ms));
            field(&optional_f64(observation.cost_usd));
            field(&optional_u64(observation.tokens));
            field(&optional_u64(observation.patch_lines));
            field(observation.measured_commit.as_deref().unwrap_or("none"));
            field(&observation.observed_at.to_rfc3339());
        }
        for excluded in &self.excluded {
            field(excluded.run_id.as_str());
            field(excluded.exclusion.as_str());
            field(&excluded.exclusion.describe());
        }
        for health in &self.health {
            field(health.health_snapshot_id.as_str());
            field(&health.commit);
            field(&health.observed_at.to_rfc3339());
        }
        format!("{:x}", digest.finalize())[..32].to_string()
    }

    /// Eligible observations governed by one policy fingerprint.
    pub fn observations_for(&self, fingerprint: &str) -> Vec<&PolicyObservation> {
        self.eligible
            .iter()
            .filter(|observation| observation.policy_fingerprint.as_deref() == Some(fingerprint))
            .collect()
    }

    /// Eligible observations on one experiment arm.
    pub fn observations_on_arm(&self, arm: ExperimentArm) -> Vec<&PolicyObservation> {
        self.eligible
            .iter()
            .filter(|observation| {
                observation
                    .experiment
                    .as_ref()
                    .is_some_and(|membership| membership.arm == arm)
            })
            .collect()
    }

    /// How many observations came from each source.
    pub fn source_breakdown(&self) -> BTreeMap<ObservationSource, u64> {
        let mut counts = BTreeMap::new();
        for observation in &self.eligible {
            *counts.entry(observation.source).or_default() += 1;
        }
        counts
    }

    /// How many observations were excluded for each reason.
    pub fn exclusion_breakdown(&self) -> BTreeMap<String, u64> {
        let mut counts: BTreeMap<String, u64> = BTreeMap::new();
        for excluded in &self.excluded {
            *counts
                .entry(excluded.exclusion.as_str().to_string())
                .or_default() += 1;
        }
        counts
    }
}

fn optional_u64(value: Option<u64>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "none".to_string())
}

fn optional_f64(value: Option<f64>) -> String {
    value
        .map(|value| value.to_bits().to_string())
        .unwrap_or_else(|| "none".to_string())
}

// ------------------------------------------------------------------- outcomes

/// Aggregated raw counts for one policy arm.
///
/// Counts stay raw. Rates are derived on demand and are `None` when the
/// denominator is zero, because a rate over no observations is not a small
/// number — it is not a measurement.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PolicyOutcomeSummary {
    pub observations: u64,
    pub passed: u64,
    pub failed: u64,
    pub inconclusive: u64,
    pub no_change: u64,
    pub integrity_clean: u64,
    /// Excluded from every rate; recorded so the exclusion is visible.
    pub infrastructure_failures: u64,
    pub runtime_ms_total: u64,
    pub runtime_observations: u64,
    pub cost_usd_total: f64,
    pub cost_observations: u64,
    pub tokens_total: u64,
    pub token_observations: u64,
    pub patch_lines_total: u64,
    pub patch_observations: u64,
}

impl PolicyOutcomeSummary {
    pub fn from_observations(observations: &[&PolicyObservation]) -> Self {
        let mut summary = Self::default();
        for observation in observations {
            if observation.is_infrastructure_failure() {
                summary.infrastructure_failures += 1;
                continue;
            }
            summary.observations += 1;
            match observation.outcome {
                RunOutcome::Passed => summary.passed += 1,
                RunOutcome::Failed => summary.failed += 1,
                RunOutcome::Inconclusive => summary.inconclusive += 1,
                RunOutcome::NoChange => summary.no_change += 1,
                RunOutcome::Errored => unreachable!("filtered above"),
            }
            if observation.integrity_clean {
                summary.integrity_clean += 1;
            }
            if let Some(runtime) = observation.runtime_ms {
                summary.runtime_ms_total += runtime;
                summary.runtime_observations += 1;
            }
            if let Some(cost) = observation.cost_usd {
                summary.cost_usd_total += cost;
                summary.cost_observations += 1;
            }
            if let Some(tokens) = observation.tokens {
                summary.tokens_total += tokens;
                summary.token_observations += 1;
            }
            if let Some(lines) = observation.patch_lines {
                summary.patch_lines_total += lines;
                summary.patch_observations += 1;
            }
        }
        summary
    }

    pub fn success_rate(&self) -> Option<f64> {
        (self.observations > 0).then(|| self.passed as f64 / self.observations as f64)
    }

    pub fn integrity_clean_rate(&self) -> Option<f64> {
        (self.observations > 0).then(|| self.integrity_clean as f64 / self.observations as f64)
    }

    pub fn mean_runtime_ms(&self) -> Option<f64> {
        (self.runtime_observations > 0)
            .then(|| self.runtime_ms_total as f64 / self.runtime_observations as f64)
    }

    /// Mean known cost. `None` when no observation reported a cost — unknown
    /// cost is never zero cost.
    pub fn mean_cost_usd(&self) -> Option<f64> {
        (self.cost_observations > 0).then(|| self.cost_usd_total / self.cost_observations as f64)
    }

    pub fn mean_tokens(&self) -> Option<f64> {
        (self.token_observations > 0)
            .then(|| self.tokens_total as f64 / self.token_observations as f64)
    }

    pub fn mean_patch_lines(&self) -> Option<f64> {
        (self.patch_observations > 0)
            .then(|| self.patch_lines_total as f64 / self.patch_observations as f64)
    }

    /// Share of attempted executions Forge could not complete.
    pub fn infrastructure_failure_rate(&self) -> Option<f64> {
        let attempted = self.observations + self.infrastructure_failures;
        (attempted > 0).then(|| self.infrastructure_failures as f64 / attempted as f64)
    }

    /// The value of one objective metric, or `None` if unmeasured.
    pub fn value_for(&self, metric: &ObjectiveMetric) -> Option<f64> {
        match metric {
            ObjectiveMetric::TaskSuccessRate => self.success_rate(),
            ObjectiveMetric::IntegrityCleanRate => self.integrity_clean_rate(),
            ObjectiveMetric::Runtime => self.mean_runtime_ms(),
            ObjectiveMetric::Cost => self.mean_cost_usd(),
            ObjectiveMetric::TokenUsage => self.mean_tokens(),
            ObjectiveMetric::InfrastructureFailureRate => self.infrastructure_failure_rate(),
            ObjectiveMetric::PatchSize => self.mean_patch_lines(),
            // Longitudinal metrics do not come from run aggregation.
            ObjectiveMetric::RepositoryHealth { .. } => None,
        }
    }
}

// ----------------------------------------------------------------- objectives

/// How one objective term came out.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ObjectiveOutcome {
    pub metric: ObjectiveMetric,
    pub direction: Direction,
    pub is_hard: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub percent_change: Option<f64>,
    /// `None` when the term could not be measured on both sides.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate_better: Option<bool>,
    /// Why the term could not be evaluated, when it could not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unmeasured_reason: Option<String>,
}

impl ObjectiveOutcome {
    /// Whether both sides produced a value.
    ///
    /// Not the same as having moved: a metric measured on both sides and found
    /// unchanged is measured, and reporting it as unmeasured would hide a real
    /// observation.
    pub fn is_measured(&self) -> bool {
        self.baseline.is_some() && self.candidate.is_some()
    }

    pub fn describe(&self) -> String {
        match (self.baseline, self.candidate) {
            (Some(baseline), Some(candidate)) => {
                let percent = self
                    .percent_change
                    .map(|p| format!("  ({}{:.1}%)", if p >= 0.0 { "+" } else { "" }, p))
                    .unwrap_or_default();
                format!("{baseline:.4} → {candidate:.4}{percent}")
            }
            _ => self
                .unmeasured_reason
                .clone()
                .unwrap_or_else(|| "not measured".to_string()),
        }
    }
}

/// Whether a hard constraint held.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConstraintResult {
    pub metric: ObjectiveMetric,
    pub constraint: ObjectiveConstraint,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline: Option<f64>,
    pub satisfied: bool,
    pub detail: String,
}

// ------------------------------------------------------------------ proposal

/// What the optimizer recommends doing with a candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProposalRecommendation {
    /// The candidate is worse, or violates a hard constraint.
    Reject,
    /// Worth observing without letting it control anything.
    ShadowTest,
    /// Worth a bounded controlled experiment.
    CanaryTest,
    /// Evidence supports activating it.
    Promote,
    /// Not enough comparable evidence to say.
    InsufficientEvidence,
    /// Short-term evidence is favourable, but a required long-term health
    /// observation has not completed.
    HealthObservationPending,
}

impl ProposalRecommendation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Reject => "reject",
            Self::ShadowTest => "shadow-test",
            Self::CanaryTest => "canary-test",
            Self::Promote => "promote",
            Self::InsufficientEvidence => "insufficient-evidence",
            Self::HealthObservationPending => "health-observation-pending",
        }
    }

    /// Whether the recommendation alone could justify activation.
    pub fn permits_promotion(self) -> bool {
        self == Self::Promote
    }
}

impl std::fmt::Display for ProposalRecommendation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How much the evidence supports the conclusion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceStrength {
    None,
    Weak,
    Moderate,
    Strong,
}

impl EvidenceStrength {
    /// Derived from the smaller arm, because a comparison is only as strong as
    /// its weaker side.
    pub fn from_counts(control: u64, candidate: u64, minimum_per_arm: u64) -> Self {
        let smaller = control.min(candidate);
        if smaller == 0 {
            Self::None
        } else if smaller < minimum_per_arm {
            Self::Weak
        } else if smaller < minimum_per_arm * 3 {
            Self::Moderate
        } else {
            Self::Strong
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Weak => "weak",
            Self::Moderate => "moderate",
            Self::Strong => "strong",
        }
    }
}

impl std::fmt::Display for EvidenceStrength {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// An immutable, evidence-backed suggestion.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PolicyProposal {
    pub proposal_id: PolicyProposalId,
    pub repository: String,
    pub created_at: DateTime<Utc>,
    pub active_policy_id: PolicyId,
    pub candidate_policy_id: PolicyId,
    pub candidate_fingerprint: String,
    pub changed_dimensions: Vec<OptimizableDimension>,
    pub changes: Vec<String>,
    pub objective: OptimizationObjective,
    pub cutoff: DateTime<Utc>,
    pub evidence_fingerprint: String,
    pub eligible_observations: u64,
    pub excluded_observations: u64,
    pub control_summary: PolicyOutcomeSummary,
    pub candidate_summary: PolicyOutcomeSummary,
    pub constraint_results: Vec<ConstraintResult>,
    pub objective_outcomes: Vec<ObjectiveOutcome>,
    pub comparison: PolicyComparison,
    pub evidence_strength: EvidenceStrength,
    pub recommendation: ProposalRecommendation,
    pub approval_requirement: ApprovalRequirement,
    pub explanation: Vec<String>,
    pub optimizer_version: String,
    pub evidence_version: String,
}

impl PolicyProposal {
    /// Hard constraints that failed.
    pub fn violated_constraints(&self) -> Vec<&ConstraintResult> {
        self.constraint_results
            .iter()
            .filter(|result| !result.satisfied)
            .collect()
    }

    pub fn satisfies_hard_constraints(&self) -> bool {
        self.constraint_results
            .iter()
            .all(|result| result.satisfied)
    }
}

// ------------------------------------------------------------------ decisions

/// Why an execution used the strategy it used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicySelectionSource {
    /// The active policy governed the execution.
    ActivePolicy,
    /// A canary experiment assigned it to the candidate policy.
    CanaryCandidate,
    /// A canary experiment assigned it to the control policy.
    CanaryControl,
    /// The user named the strategy; policy did not choose it.
    ManualOverride,
    /// A shadow policy recorded what it would have chosen, controlling nothing.
    Shadow,
    /// No policy was resolvable; documented legacy behaviour applied.
    NoPolicy,
}

impl PolicySelectionSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ActivePolicy => "active_policy",
            Self::CanaryCandidate => "canary_candidate",
            Self::CanaryControl => "canary_control",
            Self::ManualOverride => "manual_override",
            Self::Shadow => "shadow",
            Self::NoPolicy => "no_policy",
        }
    }

    /// Whether the policy actually determined what ran.
    pub fn policy_controlled_execution(self) -> bool {
        matches!(
            self,
            Self::ActivePolicy | Self::CanaryCandidate | Self::CanaryControl
        )
    }

    /// The observation source this decision produces.
    pub fn observation_source(self) -> ObservationSource {
        match self {
            Self::ActivePolicy => ObservationSource::ActivePolicy,
            Self::CanaryCandidate => ObservationSource::CanaryCandidate,
            Self::CanaryControl => ObservationSource::CanaryControl,
            Self::ManualOverride => ObservationSource::ManualOverride,
            // A shadow decision governs nothing, so it never becomes an
            // observation of policy performance.
            Self::Shadow | Self::NoPolicy => ObservationSource::Legacy,
        }
    }
}

impl std::fmt::Display for PolicySelectionSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The durable answer to "why did Forge use this strategy here?".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PolicyDecision {
    pub decision_id: PolicyDecisionId,
    pub repository: String,
    pub created_at: DateTime<Utc>,
    pub task_revision_id: TaskRevisionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_commit: Option<String>,
    /// The policy in force at decision time.
    pub active_policy_id: PolicyId,
    /// The policy that actually governed this execution — the same as the
    /// active policy unless a canary or shadow applied.
    pub selected_policy_id: PolicyId,
    pub policy_fingerprint: String,
    pub source: PolicySelectionSource,
    /// What the user demanded, when they demanded anything.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manual_override: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub experiment: Option<ExperimentMembership>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub world_model_snapshot_id: Option<WorldModelSnapshotId>,
    /// Exact facts supplied as context, when any were.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub context_fact_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health_snapshot_id: Option<HealthSnapshotId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_cutoff: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_fingerprint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub optimizer_version: Option<String>,
    pub explanation: Vec<String>,
}

impl PolicyDecision {
    /// Whether the record honestly claims the policy chose the strategy.
    ///
    /// A decision that recorded a manual override *and* claimed policy control
    /// would be a lie about who decided, so the two can never coexist.
    pub fn is_honest(&self) -> bool {
        match self.source {
            PolicySelectionSource::ManualOverride => self.manual_override.is_some(),
            source => !source.policy_controlled_execution() || self.manual_override.is_none(),
        }
    }
}

// ---------------------------------------------------------------- experiments

/// A bounded control-versus-candidate policy trial.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PolicyExperiment {
    pub experiment_id: PolicyExperimentId,
    pub repository: String,
    pub control_policy_id: PolicyId,
    pub candidate_policy_id: PolicyId,
    pub assignment: AssignmentRule,
    pub budget: ExperimentBudget,
    pub status: PolicyExperimentStatus,
    pub started_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub concluded_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposal_id: Option<PolicyProposalId>,
}

impl PolicyExperiment {
    /// Which arm a task revision belongs to.
    pub fn arm_for(&self, task_revision_id: &TaskRevisionId) -> ExperimentArm {
        self.assignment
            .arm_for(&self.experiment_id, task_revision_id)
    }

    pub fn is_open(&self) -> bool {
        self.status == PolicyExperimentStatus::Running
    }
}

/// Deterministic assignment.
///
/// Hash-based rather than random: the same task revision in the same experiment
/// must always land on the same arm, or the experiment could not be reproduced
/// or audited.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssignmentRule {
    pub version: String,
    /// Percentage of eligible task revisions given to the candidate, 0–100.
    pub candidate_share_percent: u32,
}

impl AssignmentRule {
    pub fn new(candidate_share_percent: u32) -> Self {
        Self {
            version: POLICY_ASSIGNMENT_VERSION.to_string(),
            candidate_share_percent: candidate_share_percent.min(100),
        }
    }

    /// The arm for one task revision, from a stable hash.
    pub fn arm_for(
        &self,
        experiment_id: &PolicyExperimentId,
        task_revision_id: &TaskRevisionId,
    ) -> ExperimentArm {
        let mut digest = Sha256::new();
        digest.update(self.version.as_bytes());
        digest.update([0x1f]);
        digest.update(experiment_id.as_str().as_bytes());
        digest.update([0x1f]);
        digest.update(task_revision_id.as_str().as_bytes());
        let hash = digest.finalize();
        let bucket = u32::from(hash[0]) % 100;
        if bucket < self.candidate_share_percent {
            ExperimentArm::Candidate
        } else {
            ExperimentArm::Control
        }
    }
}

/// Hard limits on what an experiment may spend.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExperimentBudget {
    pub max_tasks: u32,
    pub max_extra_runs: u32,
    /// `None` means no cost ceiling is known — never that cost is zero.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_extra_cost_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
}

impl Default for ExperimentBudget {
    fn default() -> Self {
        Self {
            max_tasks: 20,
            max_extra_runs: 4,
            max_extra_cost_usd: None,
            expires_at: None,
        }
    }
}

impl ExperimentBudget {
    /// Whether the experiment may accept another task.
    pub fn permits(&self, tasks_so_far: u32, extra_runs_so_far: u32, now: DateTime<Utc>) -> bool {
        if let Some(expiry) = self.expires_at
            && now >= expiry
        {
            return false;
        }
        tasks_so_far < self.max_tasks && extra_runs_so_far <= self.max_extra_runs
    }

    /// Whether a known additional cost would breach the ceiling.
    ///
    /// Unknown cost is not treated as free: a `None` observation cannot be
    /// shown to fit, so it does not extend the budget.
    pub fn permits_cost(&self, spent: Option<f64>, additional: Option<f64>) -> bool {
        match (self.max_extra_cost_usd, spent, additional) {
            (None, _, _) => true,
            (Some(ceiling), Some(spent), Some(additional)) => spent + additional <= ceiling,
            (Some(_), _, _) => false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyExperimentStatus {
    Running,
    /// Execution finished; long-term health may still be accruing.
    ExecutionComplete,
    /// Everything the objective needs has been observed.
    Concluded,
    Cancelled,
}

impl PolicyExperimentStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::ExecutionComplete => "execution_complete",
            Self::Concluded => "concluded",
            Self::Cancelled => "cancelled",
        }
    }
}

impl std::fmt::Display for PolicyExperimentStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One task revision's assignment, persisted so it is auditable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExperimentAssignment {
    pub experiment_id: PolicyExperimentId,
    pub task_revision_id: TaskRevisionId,
    pub arm: ExperimentArm,
    pub assignment_version: String,
    pub assigned_at: DateTime<Utc>,
}

// ------------------------------------------------------------------- shadow

/// What a shadow policy would have chosen.
///
/// Records a decision, never an outcome. The shadow-selected strategy did not
/// execute, so nothing is known about how it would have fared, and inventing a
/// counterfactual result would be the most seductive way to make an optimizer
/// look good.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShadowDecision {
    pub decision_id: PolicyDecisionId,
    pub repository: String,
    pub task_revision_id: TaskRevisionId,
    pub shadow_policy_id: PolicyId,
    pub shadow_policy_fingerprint: String,
    pub active_policy_id: PolicyId,
    /// What the active policy actually did.
    pub actual_selection: String,
    /// What the shadow policy would have done.
    pub shadow_selection: String,
    pub agreed: bool,
    pub created_at: DateTime<Utc>,
}

impl ShadowDecision {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        decision_id: PolicyDecisionId,
        repository: impl Into<String>,
        task_revision_id: TaskRevisionId,
        shadow_policy_id: PolicyId,
        shadow_policy_fingerprint: impl Into<String>,
        active_policy_id: PolicyId,
        actual_selection: impl Into<String>,
        shadow_selection: impl Into<String>,
    ) -> Self {
        let actual_selection = actual_selection.into();
        let shadow_selection = shadow_selection.into();
        Self {
            decision_id,
            repository: repository.into(),
            task_revision_id,
            shadow_policy_id,
            shadow_policy_fingerprint: shadow_policy_fingerprint.into(),
            active_policy_id,
            agreed: actual_selection == shadow_selection,
            actual_selection,
            shadow_selection,
            created_at: Utc::now(),
        }
    }
}

// -------------------------------------------------------------------- events

/// What a policy event is about.
///
/// Typed subjects, following Phases 5–7. A `RunId` would be the wrong subject:
/// these events are about policy, not about any one execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "subject", content = "id", rename_all = "snake_case")]
pub enum PolicyEventSubject {
    Policy(PolicyId),
    Proposal(PolicyProposalId),
    Experiment(PolicyExperimentId),
}

impl PolicyEventSubject {
    pub fn id(&self) -> &str {
        match self {
            Self::Policy(id) => id.as_str(),
            Self::Proposal(id) => id.as_str(),
            Self::Experiment(id) => id.as_str(),
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Self::Policy(_) => "policy",
            Self::Proposal(_) => "proposal",
            Self::Experiment(_) => "experiment",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PolicyEvent {
    pub subject: PolicyEventSubject,
    pub seq: u64,
    pub timestamp: DateTime<Utc>,
    pub payload: PolicyEventPayload,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PolicyEventPayload {
    PolicyCreated {
        provenance: String,
        fingerprint: String,
    },
    PolicyProposalCreated {
        candidate_policy_id: PolicyId,
        recommendation: ProposalRecommendation,
        evidence_fingerprint: String,
    },
    ShadowDecisionRecorded {
        shadow_policy_id: PolicyId,
        agreed: bool,
    },
    PolicyExperimentStarted {
        control_policy_id: PolicyId,
        candidate_policy_id: PolicyId,
        candidate_share_percent: u32,
    },
    PolicyExperimentAssigned {
        task_revision_id: TaskRevisionId,
        arm: ExperimentArm,
    },
    PolicyExperimentObservationAdded {
        run_id: RunId,
        arm: ExperimentArm,
    },
    PolicyPromotionRecommended {
        candidate_policy_id: PolicyId,
        approval: ApprovalRequirement,
    },
    PolicyPromoted {
        from_policy_id: PolicyId,
        to_policy_id: PolicyId,
        approved_by: String,
    },
    PolicyRejected {
        reason: String,
    },
    PolicyRollbackRecommended {
        reason: String,
        attribution: AttributionLevel,
    },
    PolicyRolledBack {
        from_policy_id: PolicyId,
        to_policy_id: PolicyId,
        reason: String,
    },
    PolicyRetired {
        reason: String,
    },
}

impl PolicyEventPayload {
    pub fn event_type(&self) -> &'static str {
        match self {
            Self::PolicyCreated { .. } => "PolicyCreated",
            Self::PolicyProposalCreated { .. } => "PolicyProposalCreated",
            Self::ShadowDecisionRecorded { .. } => "ShadowDecisionRecorded",
            Self::PolicyExperimentStarted { .. } => "PolicyExperimentStarted",
            Self::PolicyExperimentAssigned { .. } => "PolicyExperimentAssigned",
            Self::PolicyExperimentObservationAdded { .. } => "PolicyExperimentObservationAdded",
            Self::PolicyPromotionRecommended { .. } => "PolicyPromotionRecommended",
            Self::PolicyPromoted { .. } => "PolicyPromoted",
            Self::PolicyRejected { .. } => "PolicyRejected",
            Self::PolicyRollbackRecommended { .. } => "PolicyRollbackRecommended",
            Self::PolicyRolledBack { .. } => "PolicyRolledBack",
            Self::PolicyRetired { .. } => "PolicyRetired",
        }
    }
}

/// Convenience helper mirroring the objective term shape.
pub fn objective_is_hard(kind: &ObjectiveKind) -> bool {
    kind.is_hard()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::TaskId;
    use crate::policy::{EngineeringPolicy, ObjectiveMetric};

    fn revision(seed: &str) -> TaskRevisionId {
        TaskRevisionId::for_definition(seed)
    }

    fn observation(
        run: u64,
        outcome: RunOutcome,
        source: ObservationSource,
        fingerprint: &str,
    ) -> PolicyObservation {
        PolicyObservation {
            run_id: RunId::sequential(run),
            task_revision_id: revision(&format!("task-{run}")),
            policy_id: Some(PolicyId::sequential(1)),
            policy_fingerprint: Some(fingerprint.to_string()),
            source,
            experiment: None,
            provenance: ExecutionProvenance::Live,
            outcome,
            integrity_clean: true,
            config_fingerprint: "cfg".into(),
            runtime_ms: Some(1_000),
            cost_usd: Some(0.10),
            tokens: Some(1_000),
            patch_lines: Some(20),
            measured_commit: Some("a".repeat(40)),
            observed_at: Utc::now(),
        }
    }

    fn snapshot(eligible: Vec<PolicyObservation>) -> PolicyEvidenceSnapshot {
        PolicyEvidenceSnapshot {
            repository: "forge".into(),
            cutoff: Utc::now(),
            active_policy_id: PolicyId::sequential(1),
            active_policy_fingerprint: "fp-active".into(),
            candidate_policy_fingerprints: vec!["fp-candidate".into()],
            eligible,
            excluded: Vec::new(),
            health: Vec::new(),
            world_model_snapshot_id: None,
            evidence_version: POLICY_EVIDENCE_VERSION.into(),
            observation_window_days: 30,
        }
    }

    // ------------------------------------------------------------ evidence

    #[test]
    fn infrastructure_failures_never_become_engineering_failures() {
        // They leave the numerator and the denominator alike.
        let observations = [
            observation(1, RunOutcome::Passed, ObservationSource::ActivePolicy, "fp"),
            observation(
                2,
                RunOutcome::Errored,
                ObservationSource::ActivePolicy,
                "fp",
            ),
        ];
        let refs: Vec<&PolicyObservation> = observations.iter().collect();
        let summary = PolicyOutcomeSummary::from_observations(&refs);

        assert_eq!(summary.observations, 1);
        assert_eq!(summary.passed, 1);
        assert_eq!(summary.infrastructure_failures, 1);
        assert_eq!(summary.success_rate(), Some(1.0));
        assert_eq!(summary.infrastructure_failure_rate(), Some(0.5));
    }

    #[test]
    fn unknown_cost_is_not_zero_cost() {
        let mut without_cost =
            observation(1, RunOutcome::Passed, ObservationSource::ActivePolicy, "fp");
        without_cost.cost_usd = None;
        let refs = vec![&without_cost];
        let summary = PolicyOutcomeSummary::from_observations(&refs);

        assert_eq!(summary.cost_observations, 0);
        assert_eq!(summary.mean_cost_usd(), None);
        assert_eq!(summary.value_for(&ObjectiveMetric::Cost), None);
    }

    #[test]
    fn a_rate_over_no_observations_is_not_a_measurement() {
        let summary = PolicyOutcomeSummary::default();
        assert_eq!(summary.success_rate(), None);
        assert_eq!(summary.integrity_clean_rate(), None);
        assert_eq!(summary.mean_runtime_ms(), None);
    }

    #[test]
    fn longitudinal_metrics_do_not_come_from_run_aggregation() {
        let summary = PolicyOutcomeSummary::default();
        assert_eq!(
            summary.value_for(&ObjectiveMetric::RepositoryHealth {
                dimension: crate::health::HealthDimensionKind::Complexity
            }),
            None
        );
    }

    #[test]
    fn selection_sources_stay_distinguishable() {
        let evidence = snapshot(vec![
            observation(1, RunOutcome::Passed, ObservationSource::ActivePolicy, "fp"),
            observation(
                2,
                RunOutcome::Passed,
                ObservationSource::CanaryCandidate,
                "fp",
            ),
            observation(
                3,
                RunOutcome::Passed,
                ObservationSource::CanaryControl,
                "fp",
            ),
            observation(
                4,
                RunOutcome::Passed,
                ObservationSource::ManualOverride,
                "fp",
            ),
        ]);
        let breakdown = evidence.source_breakdown();

        assert_eq!(breakdown[&ObservationSource::ActivePolicy], 1);
        assert_eq!(breakdown[&ObservationSource::CanaryCandidate], 1);
        assert_eq!(breakdown[&ObservationSource::CanaryControl], 1);
        assert_eq!(breakdown[&ObservationSource::ManualOverride], 1);
        assert!(!ObservationSource::ManualOverride.is_policy_controlled());
    }

    #[test]
    fn exclusions_are_counted_and_explained_rather_than_dropped() {
        let mut evidence = snapshot(vec![]);
        evidence.excluded = vec![
            ExcludedObservation {
                run_id: RunId::sequential(9),
                exclusion: EvidenceExclusion::PostCutoff,
            },
            ExcludedObservation {
                run_id: RunId::sequential(10),
                exclusion: EvidenceExclusion::InfrastructureFailure,
            },
            ExcludedObservation {
                run_id: RunId::sequential(11),
                exclusion: EvidenceExclusion::DisallowedProvenance {
                    provenance: ExecutionProvenance::Synthetic,
                },
            },
        ];

        let breakdown = evidence.exclusion_breakdown();
        assert_eq!(breakdown["post_cutoff"], 1);
        assert_eq!(breakdown["infrastructure_failure"], 1);
        assert_eq!(breakdown["disallowed_provenance"], 1);
        assert!(
            evidence.excluded[2]
                .exclusion
                .describe()
                .contains("synthetic")
        );
    }

    // --------------------------------------------------------- fingerprint

    #[test]
    fn the_same_evidence_produces_the_same_fingerprint() {
        let evidence = snapshot(vec![observation(
            1,
            RunOutcome::Passed,
            ObservationSource::ActivePolicy,
            "fp",
        )]);
        let first = evidence.fingerprint();
        for _ in 0..3 {
            assert_eq!(evidence.fingerprint(), first);
        }
    }

    #[test]
    fn different_evidence_produces_a_different_fingerprint() {
        let base = snapshot(vec![observation(
            1,
            RunOutcome::Passed,
            ObservationSource::ActivePolicy,
            "fp",
        )]);
        let baseline = base.fingerprint();

        let mut more_eligible = base.clone();
        more_eligible.eligible.push(observation(
            2,
            RunOutcome::Failed,
            ObservationSource::ActivePolicy,
            "fp",
        ));
        assert_ne!(more_eligible.fingerprint(), baseline);

        // An exclusion changes the evidence even though eligibility did not.
        let mut with_exclusion = base.clone();
        with_exclusion.excluded.push(ExcludedObservation {
            run_id: RunId::sequential(3),
            exclusion: EvidenceExclusion::PostCutoff,
        });
        assert_ne!(with_exclusion.fingerprint(), baseline);

        let mut later_cutoff = base.clone();
        later_cutoff.cutoff = base.cutoff + chrono::TimeDelta::try_hours(1).unwrap();
        assert_ne!(later_cutoff.fingerprint(), baseline);
    }

    // ----------------------------------------------------------- assignment

    #[test]
    fn experiment_assignment_is_deterministic() {
        let rule = AssignmentRule::new(50);
        let experiment = PolicyExperimentId::sequential(1);
        let task = revision("stable-task");

        let first = rule.arm_for(&experiment, &task);
        for _ in 0..10 {
            assert_eq!(rule.arm_for(&experiment, &task), first);
        }
    }

    #[test]
    fn assignment_depends_on_the_experiment_as_well_as_the_task() {
        let rule = AssignmentRule::new(50);
        let task = revision("stable-task");
        let arms: Vec<ExperimentArm> = (1..=8)
            .map(|n| rule.arm_for(&PolicyExperimentId::sequential(n), &task))
            .collect();
        // The same task must not be pinned to one arm across every experiment.
        assert!(arms.iter().any(|arm| *arm != arms[0]));
    }

    #[test]
    fn a_zero_share_assigns_everything_to_control() {
        let rule = AssignmentRule::new(0);
        let experiment = PolicyExperimentId::sequential(1);
        for n in 0..25 {
            assert_eq!(
                rule.arm_for(&experiment, &revision(&format!("task-{n}"))),
                ExperimentArm::Control
            );
        }
    }

    #[test]
    fn a_full_share_assigns_everything_to_the_candidate() {
        let rule = AssignmentRule::new(100);
        let experiment = PolicyExperimentId::sequential(1);
        for n in 0..25 {
            assert_eq!(
                rule.arm_for(&experiment, &revision(&format!("task-{n}"))),
                ExperimentArm::Candidate
            );
        }
    }

    #[test]
    fn a_share_splits_tasks_across_both_arms() {
        let rule = AssignmentRule::new(50);
        let experiment = PolicyExperimentId::sequential(1);
        let arms: Vec<ExperimentArm> = (0..60)
            .map(|n| rule.arm_for(&experiment, &revision(&format!("task-{n}"))))
            .collect();
        assert!(arms.contains(&ExperimentArm::Candidate));
        assert!(arms.contains(&ExperimentArm::Control));
    }

    // --------------------------------------------------------------- budget

    #[test]
    fn an_experiment_budget_is_a_hard_ceiling() {
        let budget = ExperimentBudget {
            max_tasks: 3,
            max_extra_runs: 2,
            max_extra_cost_usd: Some(1.0),
            expires_at: None,
        };
        let now = Utc::now();
        assert!(budget.permits(2, 1, now));
        assert!(!budget.permits(3, 1, now));
        assert!(!budget.permits(0, 3, now));
    }

    #[test]
    fn an_expired_experiment_accepts_nothing() {
        let now = Utc::now();
        let budget = ExperimentBudget {
            expires_at: Some(now - chrono::TimeDelta::try_hours(1).unwrap()),
            ..ExperimentBudget::default()
        };
        assert!(!budget.permits(0, 0, now));
    }

    #[test]
    fn unknown_cost_does_not_silently_fit_a_budget() {
        let budget = ExperimentBudget {
            max_extra_cost_usd: Some(1.0),
            ..ExperimentBudget::default()
        };
        // Known spend plus known addition is checked.
        assert!(budget.permits_cost(Some(0.5), Some(0.4)));
        assert!(!budget.permits_cost(Some(0.9), Some(0.4)));
        // Unknown spend or addition never silently fits a known ceiling.
        assert!(!budget.permits_cost(None, Some(0.1)));
        assert!(!budget.permits_cost(Some(0.5), None));
        assert!(!budget.permits_cost(Some(1.5), None));
    }

    // -------------------------------------------------------------- shadow

    #[test]
    fn a_shadow_decision_records_a_choice_and_never_an_outcome() {
        let shadow = ShadowDecision::new(
            PolicyDecisionId::sequential(1),
            "forge",
            revision("task"),
            PolicyId::sequential(2),
            "fp-shadow",
            PolicyId::sequential(1),
            "codex",
            "claude",
        );

        assert!(!shadow.agreed);
        assert_eq!(shadow.actual_selection, "codex");
        assert_eq!(shadow.shadow_selection, "claude");

        // There is deliberately no outcome field to populate.
        let json = serde_json::to_value(&shadow).unwrap();
        let object = json.as_object().unwrap();
        for forbidden in [
            "outcome",
            "verdict",
            "passed",
            "would_have_passed",
            "success",
        ] {
            assert!(
                !object.contains_key(forbidden),
                "shadow decision exposes `{forbidden}`, inviting a fabricated counterfactual"
            );
        }
    }

    #[test]
    fn a_shadow_decision_that_agrees_is_marked_as_such() {
        let shadow = ShadowDecision::new(
            PolicyDecisionId::sequential(1),
            "forge",
            revision("task"),
            PolicyId::sequential(2),
            "fp",
            PolicyId::sequential(1),
            "claude",
            "claude",
        );
        assert!(shadow.agreed);
    }

    #[test]
    fn a_shadow_selection_never_counts_as_a_policy_observation() {
        assert_eq!(
            PolicySelectionSource::Shadow.observation_source(),
            ObservationSource::Legacy
        );
        assert!(!PolicySelectionSource::Shadow.policy_controlled_execution());
    }

    // ------------------------------------------------------------ decisions

    #[test]
    fn a_decision_cannot_claim_policy_control_and_a_manual_override_at_once() {
        let mut decision = PolicyDecision {
            decision_id: PolicyDecisionId::sequential(1),
            repository: "forge".into(),
            created_at: Utc::now(),
            task_revision_id: revision("task"),
            base_commit: None,
            active_policy_id: PolicyId::sequential(1),
            selected_policy_id: PolicyId::sequential(1),
            policy_fingerprint: "fp".into(),
            source: PolicySelectionSource::ManualOverride,
            manual_override: Some("claude".into()),
            experiment: None,
            world_model_snapshot_id: None,
            context_fact_ids: Vec::new(),
            health_snapshot_id: None,
            evidence_cutoff: None,
            evidence_fingerprint: None,
            optimizer_version: None,
            explanation: vec!["user named the agent".into()],
        };
        assert!(decision.is_honest());

        // Claiming the policy chose it while an override was recorded is a lie.
        decision.source = PolicySelectionSource::ActivePolicy;
        assert!(!decision.is_honest());
    }

    #[test]
    fn selection_sources_map_to_observation_sources() {
        assert_eq!(
            PolicySelectionSource::CanaryCandidate.observation_source(),
            ObservationSource::CanaryCandidate
        );
        assert_eq!(
            PolicySelectionSource::CanaryControl.observation_source(),
            ObservationSource::CanaryControl
        );
        assert_eq!(
            PolicySelectionSource::ManualOverride.observation_source(),
            ObservationSource::ManualOverride
        );
    }

    // -------------------------------------------------- evidence strength

    #[test]
    fn evidence_strength_follows_the_weaker_arm() {
        // 100 observations on one side and none on the other prove nothing.
        assert_eq!(
            EvidenceStrength::from_counts(100, 0, 8),
            EvidenceStrength::None
        );
        assert_eq!(
            EvidenceStrength::from_counts(100, 3, 8),
            EvidenceStrength::Weak
        );
        assert_eq!(
            EvidenceStrength::from_counts(100, 10, 8),
            EvidenceStrength::Moderate
        );
        assert_eq!(
            EvidenceStrength::from_counts(100, 40, 8),
            EvidenceStrength::Strong
        );
    }

    // -------------------------------------------------------------- events

    #[test]
    fn policy_events_use_typed_policy_subjects() {
        let event = PolicyEvent {
            subject: PolicyEventSubject::Proposal(PolicyProposalId::sequential(12)),
            seq: 1,
            timestamp: Utc::now(),
            payload: PolicyEventPayload::PolicyProposalCreated {
                candidate_policy_id: PolicyId::sequential(5),
                recommendation: ProposalRecommendation::CanaryTest,
                evidence_fingerprint: "ef".into(),
            },
        };
        assert_eq!(event.subject.kind(), "proposal");
        assert_eq!(event.subject.id(), "PP-0012");
        assert_eq!(event.payload.event_type(), "PolicyProposalCreated");

        let json = serde_json::to_string(&event).unwrap();
        assert!(!json.contains("run_id"), "policy events are not run events");
        assert_eq!(serde_json::from_str::<PolicyEvent>(&json).unwrap(), event);
    }

    #[test]
    fn recommendations_gate_promotion() {
        assert!(ProposalRecommendation::Promote.permits_promotion());
        for recommendation in [
            ProposalRecommendation::Reject,
            ProposalRecommendation::ShadowTest,
            ProposalRecommendation::CanaryTest,
            ProposalRecommendation::InsufficientEvidence,
            ProposalRecommendation::HealthObservationPending,
        ] {
            assert!(!recommendation.permits_promotion(), "{recommendation}");
        }
    }

    #[test]
    fn evidence_snapshots_round_trip() {
        let evidence = snapshot(vec![observation(
            1,
            RunOutcome::Passed,
            ObservationSource::ActivePolicy,
            "fp",
        )]);
        let json = serde_json::to_string(&evidence).unwrap();
        let back: PolicyEvidenceSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(back, evidence);
        assert_eq!(back.fingerprint(), evidence.fingerprint());
    }

    #[test]
    fn observations_can_be_selected_by_policy_and_by_arm() {
        let mut candidate = observation(
            2,
            RunOutcome::Passed,
            ObservationSource::CanaryCandidate,
            "fp-candidate",
        );
        candidate.experiment = Some(ExperimentMembership {
            experiment_id: PolicyExperimentId::sequential(1),
            arm: ExperimentArm::Candidate,
        });
        let evidence = snapshot(vec![
            observation(
                1,
                RunOutcome::Passed,
                ObservationSource::ActivePolicy,
                "fp-active",
            ),
            candidate,
        ]);

        assert_eq!(evidence.observations_for("fp-active").len(), 1);
        assert_eq!(evidence.observations_for("fp-candidate").len(), 1);
        assert_eq!(
            evidence.observations_on_arm(ExperimentArm::Candidate).len(),
            1
        );
        assert_eq!(
            evidence.observations_on_arm(ExperimentArm::Control).len(),
            0
        );
    }

    #[test]
    fn a_bootstrap_policy_can_seed_an_experiment() {
        // Sanity: the experiment model composes with the policy model.
        let control = EngineeringPolicy::bootstrap(PolicyId::sequential(1), "forge");
        let mut candidate = control.clone();
        candidate.policy_id = PolicyId::sequential(2);
        candidate.context.max_world_facts = 12;

        let experiment = PolicyExperiment {
            experiment_id: PolicyExperimentId::sequential(1),
            repository: "forge".into(),
            control_policy_id: control.policy_id.clone(),
            candidate_policy_id: candidate.policy_id.clone(),
            assignment: AssignmentRule::new(50),
            budget: ExperimentBudget::default(),
            status: PolicyExperimentStatus::Running,
            started_at: Utc::now(),
            concluded_at: None,
            proposal_id: None,
        };
        assert!(experiment.is_open());
        let _ = experiment.arm_for(&revision("task"));
        let _ = TaskId::sequential(1);
    }
}
