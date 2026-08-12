//! Versioned, immutable engineering policy — the thing Phase 8 optimizes.
//!
//! ```text
//! observe → measure → learn → propose → validate → deploy cautiously
//!    ↑                                                      │
//!    └──────────────── retain or roll back ◀────────────────┘
//! ```
//!
//! # What "self-optimizing" means here
//!
//! Forge optimizes **how it engineers**: which agent to route to, how much
//! world-model context to supply, whether to run a team, what budgets to allow.
//! It does not optimize **how it judges**. Required evaluators, integrity
//! rules, protected paths, provenance, and the meaning of PASS are
//! [`FixedGuardrail`]s, and no policy can express a change to any of them.
//!
//! That separation is the whole safety argument of this phase, so it is
//! enforced structurally rather than by convention: an [`EngineeringPolicy`]
//! has no field capable of encoding a guardrail, every policy struct rejects
//! unknown fields, and [`EngineeringPolicy::validate`] refuses anything outside
//! its declared bounds.
//!
//! > Forge optimizes its engineering policy, not the truth criteria used to
//! > judge whether Forge succeeded.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config::ForgeConfig;
use crate::health::HealthDimensionKind;
use crate::ids::{PolicyId, PolicyProposalId};
use crate::result::Direction;
use crate::routing::ExplorationPolicy;

/// Schema of the persisted policy record.
pub const POLICY_SCHEMA_VERSION: &str = "policy-v1";
/// Identity of the objective evaluation semantics.
pub const POLICY_OBJECTIVE_VERSION: &str = "policy-objective-v1";
/// Identity of the baseline optimizer.
pub const POLICY_OPTIMIZER_VERSION: &str = "policy-baseline-v1";

// ------------------------------------------------------------------ guardrails

/// A property of Forge that policy may never change.
///
/// These are not conservative defaults or tunable knobs — they are the system
/// that decides whether Forge succeeded. An optimizer that could relax them
/// would be able to improve its apparent performance by weakening its own
/// judge, which is the single failure mode this phase exists to prevent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FixedGuardrail {
    /// Which evaluators must run and must pass.
    RequiredEvaluators,
    /// What counts as PASS, FAIL, or INCONCLUSIVE.
    EvaluationTruthBoundary,
    /// Protected evaluation inputs (Phase 0.5).
    ProtectedPaths,
    /// Workspace/patch integrity rules.
    IntegrityRules,
    /// Credential filtering and redaction.
    SecretHandling,
    /// Sandboxing and host access.
    HostSecurity,
    /// How evidence provenance is recorded.
    ProvenanceRules,
    /// Which observations count as evidence.
    EvidenceEligibility,
    /// Constraints the user declared on a task.
    UserDeclaredConstraints,
    /// Repository contracts and invariants.
    RepositoryContracts,
}

impl FixedGuardrail {
    pub const ALL: [FixedGuardrail; 10] = [
        Self::RequiredEvaluators,
        Self::EvaluationTruthBoundary,
        Self::ProtectedPaths,
        Self::IntegrityRules,
        Self::SecretHandling,
        Self::HostSecurity,
        Self::ProvenanceRules,
        Self::EvidenceEligibility,
        Self::UserDeclaredConstraints,
        Self::RepositoryContracts,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::RequiredEvaluators => "required_evaluators",
            Self::EvaluationTruthBoundary => "evaluation_truth_boundary",
            Self::ProtectedPaths => "protected_paths",
            Self::IntegrityRules => "integrity_rules",
            Self::SecretHandling => "secret_handling",
            Self::HostSecurity => "host_security",
            Self::ProvenanceRules => "provenance_rules",
            Self::EvidenceEligibility => "evidence_eligibility",
            Self::UserDeclaredConstraints => "user_declared_constraints",
            Self::RepositoryContracts => "repository_contracts",
        }
    }

    /// Why this guardrail exists, for the CLI and for the record.
    pub fn rationale(self) -> &'static str {
        match self {
            Self::RequiredEvaluators => "an optimizer must not delete the checks it is measured by",
            Self::EvaluationTruthBoundary => "PASS must mean the same thing under every policy",
            Self::ProtectedPaths => "evaluation inputs must not become optimization targets",
            Self::IntegrityRules => "a compromised measurement must stay compromised",
            Self::SecretHandling => "credential exposure is never a performance tradeoff",
            Self::HostSecurity => "containment is not a tunable parameter",
            Self::ProvenanceRules => "evidence must not be relabelled to suit a conclusion",
            Self::EvidenceEligibility => "inconvenient observations must not become ineligible",
            Self::UserDeclaredConstraints => "the user's constraints outrank the optimizer's goals",
            Self::RepositoryContracts => "invariants are not removed to make metrics improve",
        }
    }
}

impl std::fmt::Display for FixedGuardrail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The guardrails in force when a policy was created.
///
/// Recorded, never configured: a policy states which guardrails governed it so
/// a historical execution stays interpretable, and carries no ability to change
/// them. The set is always complete — a policy that omitted one would be a
/// policy claiming that guardrail did not apply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct GuardrailSet(BTreeSet<FixedGuardrail>);

impl GuardrailSet {
    /// Every guardrail. The only valid set.
    pub fn complete() -> Self {
        Self(FixedGuardrail::ALL.into_iter().collect())
    }

    pub fn contains(&self, guardrail: FixedGuardrail) -> bool {
        self.0.contains(&guardrail)
    }

    pub fn is_complete(&self) -> bool {
        FixedGuardrail::ALL
            .iter()
            .all(|guardrail| self.0.contains(guardrail))
    }

    pub fn iter(&self) -> impl Iterator<Item = &FixedGuardrail> {
        self.0.iter()
    }
}

impl Default for GuardrailSet {
    fn default() -> Self {
        Self::complete()
    }
}

/// A strategy dimension policy is allowed to change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OptimizableDimension {
    RoutingParameters,
    ContextStrategy,
    ExecutionStrategy,
    TeamStrategy,
    ReviewStrategy,
    ResourceBudgets,
    ExplorationStrategy,
}

impl OptimizableDimension {
    pub const ALL: [OptimizableDimension; 7] = [
        Self::RoutingParameters,
        Self::ContextStrategy,
        Self::ExecutionStrategy,
        Self::TeamStrategy,
        Self::ReviewStrategy,
        Self::ResourceBudgets,
        Self::ExplorationStrategy,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::RoutingParameters => "routing",
            Self::ContextStrategy => "context",
            Self::ExecutionStrategy => "execution",
            Self::TeamStrategy => "team",
            Self::ReviewStrategy => "review",
            Self::ResourceBudgets => "resources",
            Self::ExplorationStrategy => "exploration",
        }
    }
}

impl std::fmt::Display for OptimizableDimension {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// --------------------------------------------------------------- policy bounds

/// Hard maxima an optimizer may approach but never exceed.
///
/// Supplied by configuration, not by policy. An optimizer may lower a budget;
/// only a human raising these bounds can let it raise one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyBounds {
    pub max_world_facts: u32,
    pub max_timeout_secs: u64,
    pub max_retries: u32,
    pub max_parallel_team_nodes: u32,
    /// `None` means no cost ceiling is known — which is not the same as free.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_cost_usd: Option<f64>,
    pub max_review_nodes: u32,
}

impl Default for PolicyBounds {
    fn default() -> Self {
        Self {
            max_world_facts: 32,
            max_timeout_secs: 7_200,
            max_retries: 3,
            max_parallel_team_nodes: 8,
            max_cost_usd: None,
            max_review_nodes: 1,
        }
    }
}

impl PolicyBounds {
    /// Compile-time policy ceilings combined with the repository's existing
    /// hard execution limits.
    pub fn for_config(config: &ForgeConfig) -> Self {
        let configured_timeout = config
            .agents
            .values()
            .filter_map(|agent| agent.timeout_secs)
            .fold(config.defaults.timeout_secs, u64::max);
        Self {
            max_timeout_secs: configured_timeout,
            max_parallel_team_nodes: u32::try_from(config.team.max_parallel_nodes)
                .unwrap_or(u32::MAX),
            // Phase 7 had neither retries nor advisory review in the ordinary
            // run path, so a policy cannot claim those unimplemented actions.
            max_retries: 0,
            max_review_nodes: 0,
            ..Self::default()
        }
    }
}

// ------------------------------------------------------------ policy sections

/// Routing parameters. Configures the Phase 4 router; never replaces it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoutingPolicySettings {
    /// Whether the learned router chooses, or the configured default agent does.
    pub use_learned_routing: bool,
    pub minimum_total_evidence: u64,
    pub minimum_agent_evidence: u64,
    /// Score margin the leader must clear before the router commits.
    pub minimum_score_margin: f64,
    /// Which Phase 4 evidence-policy version to route under.
    pub evidence_policy_version: String,
}

impl Default for RoutingPolicySettings {
    fn default() -> Self {
        Self {
            use_learned_routing: true,
            minimum_total_evidence: 5,
            minimum_agent_evidence: 2,
            minimum_score_margin: 0.05,
            evidence_policy_version: crate::routing::DEFAULT_EVIDENCE_POLICY_VERSION.to_string(),
        }
    }
}

/// How much repository context an agent is given.
///
/// Bounded by construction: there is no "everything" option, because dumping a
/// world model into a prompt is not a strategy.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextPolicy {
    /// Upper bound on world-model facts supplied.
    pub max_world_facts: u32,
    /// Whether known failure modes are included alongside structural facts.
    pub include_failure_history: bool,
    /// Named, versioned selection strategy.
    pub selection_strategy: ContextSelectionStrategy,
}

impl Default for ContextPolicy {
    fn default() -> Self {
        Self {
            max_world_facts: 8,
            include_failure_history: false,
            selection_strategy: ContextSelectionStrategy::TaskRelevanceV1,
        }
    }
}

/// Deterministic context-selection strategies.
///
/// Every variant names a concrete, versioned algorithm. Nothing here lets a
/// model silently decide what context to supply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextSelectionStrategy {
    /// No world-model context.
    None,
    /// Phase 6 task-relevance selection.
    TaskRelevanceV1,
    /// Task relevance plus the components the task names.
    TaskRelevanceWithComponentsV1,
}

impl ContextSelectionStrategy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::TaskRelevanceV1 => "task-relevance-v1",
            Self::TaskRelevanceWithComponentsV1 => "task-relevance-with-components-v1",
        }
    }
}

/// Whether a task runs as one agent or a team.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionStrategy {
    /// One agent, chosen by the routing settings.
    SingleAgent,
    /// The Phase 5 team path.
    Team,
    /// Single agent unless the task's own definition calls for a team.
    TaskDirected,
}

impl ExecutionStrategy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SingleAgent => "single-agent",
            Self::Team => "team",
            Self::TaskDirected => "task-directed",
        }
    }
}

/// Bounded team shapes, reusing Phase 5 plans.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TeamPolicySettings {
    pub plan_template: TeamPlanTemplate,
    pub max_parallel_nodes: u32,
    pub stop_on_required_node_failure: bool,
}

impl Default for TeamPolicySettings {
    fn default() -> Self {
        Self {
            plan_template: TeamPlanTemplate::ImplementationOnly,
            max_parallel_nodes: 2,
            stop_on_required_node_failure: true,
        }
    }
}

/// A fixed catalogue of team shapes.
///
/// Deliberately enumerated rather than generated: arbitrary organizational
/// hierarchies are not an optimization space Phase 8 opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TeamPlanTemplate {
    ImplementationOnly,
    ImplementationThenReview,
    AnalysisImplementationReview,
}

impl TeamPlanTemplate {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ImplementationOnly => "implementation",
            Self::ImplementationThenReview => "implementation-review",
            Self::AnalysisImplementationReview => "analysis-implementation-review",
        }
    }

    /// Review nodes this template adds.
    pub fn review_nodes(self) -> u32 {
        match self {
            Self::ImplementationOnly => 0,
            Self::ImplementationThenReview | Self::AnalysisImplementationReview => 1,
        }
    }
}

/// Optional advisory review.
///
/// Advisory only. A reviewer can never overturn a deterministic evaluator
/// result, and no policy can make it authoritative — that would move the truth
/// boundary, which is a [`FixedGuardrail`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewPolicy {
    /// Whether an extra advisory review node runs.
    pub advisory_review_enabled: bool,
    /// How many advisory reviewers, bounded by [`PolicyBounds::max_review_nodes`].
    pub advisory_review_nodes: u32,
}

/// Execution budgets.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourcePolicy {
    pub timeout_secs: u64,
    pub max_retries: u32,
    /// Ceiling on known monetary cost per task, when cost is known at all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_cost_usd: Option<f64>,
}

impl Default for ResourcePolicy {
    fn default() -> Self {
        Self {
            timeout_secs: 3_600,
            max_retries: 0,
            max_cost_usd: None,
        }
    }
}

/// How much Forge spends learning rather than delivering.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExplorationPolicySettings {
    pub policy: ExplorationPolicy,
    /// Hard ceiling on extra runs an experiment may spend. Never unbounded.
    pub max_extra_runs: u32,
    /// Ceiling on additional known cost, when cost is known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_extra_cost_usd: Option<f64>,
}

impl Default for ExplorationPolicySettings {
    fn default() -> Self {
        Self {
            policy: ExplorationPolicy::CompeteWhenUncertain,
            max_extra_runs: 4,
            max_extra_cost_usd: None,
        }
    }
}

// ------------------------------------------------------------------ objectives

/// Something a policy is judged on.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "metric", rename_all = "snake_case")]
pub enum ObjectiveMetric {
    /// Share of comparable executions whose outcome passed.
    TaskSuccessRate,
    /// Share whose evaluation inputs stayed clean.
    IntegrityCleanRate,
    /// Wall-clock time per execution.
    Runtime,
    /// Known monetary cost per execution.
    Cost,
    /// Tokens consumed per execution.
    TokenUsage,
    /// Share of executions Forge could not carry through.
    InfrastructureFailureRate,
    /// Lines changed per accepted candidate.
    PatchSize,
    /// A Phase 7 longitudinal dimension.
    RepositoryHealth { dimension: HealthDimensionKind },
}

impl ObjectiveMetric {
    pub fn as_str(&self) -> String {
        match self {
            Self::TaskSuccessRate => "task_success_rate".to_string(),
            Self::IntegrityCleanRate => "integrity_clean_rate".to_string(),
            Self::Runtime => "runtime".to_string(),
            Self::Cost => "cost".to_string(),
            Self::TokenUsage => "token_usage".to_string(),
            Self::InfrastructureFailureRate => "infrastructure_failure_rate".to_string(),
            Self::PatchSize => "patch_size".to_string(),
            Self::RepositoryHealth { dimension } => format!("health.{dimension}"),
        }
    }

    /// Whether the metric is a long-term repository outcome, which may not be
    /// observable at the time a policy's tasks finish.
    pub fn is_longitudinal(&self) -> bool {
        matches!(self, Self::RepositoryHealth { .. })
    }
}

impl std::fmt::Display for ObjectiveMetric {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.as_str())
    }
}

/// Whether a term must hold, or is merely preferred.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum ObjectiveKind {
    /// Must hold. Dominates every soft preference, always.
    Hard {
        /// The bound the metric must satisfy.
        constraint: ObjectiveConstraint,
    },
    /// Preferred. Lower priority numbers are considered first.
    Soft {
        priority: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        weight: Option<f64>,
    },
}

impl ObjectiveKind {
    pub fn is_hard(&self) -> bool {
        matches!(self, Self::Hard { .. })
    }
}

/// A bound a hard objective imposes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "constraint", rename_all = "snake_case")]
pub enum ObjectiveConstraint {
    /// Must be at least this value.
    AtLeast { value: f64 },
    /// Must be at most this value.
    AtMost { value: f64 },
    /// Must not worsen by more than this percentage against the comparison
    /// target.
    NonRegression { tolerance_percent: f64 },
}

impl ObjectiveConstraint {
    /// Whether an observed value (and optional baseline) satisfies the bound.
    ///
    /// A missing observation never satisfies a hard constraint: "we did not
    /// measure it" is not "it held".
    pub fn is_satisfied(
        &self,
        value: Option<f64>,
        baseline: Option<f64>,
        direction: Direction,
    ) -> bool {
        let Some(value) = value else {
            return false;
        };
        match self {
            Self::AtLeast { value: bound } => value >= *bound,
            Self::AtMost { value: bound } => value <= *bound,
            Self::NonRegression { tolerance_percent } => {
                let Some(baseline) = baseline else {
                    return false;
                };
                if baseline == 0.0 {
                    return match direction {
                        Direction::HigherIsBetter => value >= baseline,
                        Direction::LowerIsBetter => value <= baseline,
                        Direction::Neutral => true,
                    };
                }
                let change = (value - baseline) / baseline.abs() * 100.0;
                match direction {
                    Direction::HigherIsBetter => change >= -tolerance_percent,
                    Direction::LowerIsBetter => change <= *tolerance_percent,
                    Direction::Neutral => true,
                }
            }
        }
    }
}

/// One thing a policy is optimized for or constrained by.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObjectiveTerm {
    pub metric: ObjectiveMetric,
    pub direction: Direction,
    pub kind: ObjectiveKind,
}

impl ObjectiveTerm {
    pub fn hard(
        metric: ObjectiveMetric,
        direction: Direction,
        constraint: ObjectiveConstraint,
    ) -> Self {
        Self {
            metric,
            direction,
            kind: ObjectiveKind::Hard { constraint },
        }
    }

    pub fn soft(metric: ObjectiveMetric, direction: Direction, priority: u32) -> Self {
        Self {
            metric,
            direction,
            kind: ObjectiveKind::Soft {
                priority,
                weight: None,
            },
        }
    }
}

/// How much evidence a conclusion needs before it may be drawn.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MinimumEvidence {
    pub observations: u64,
    /// Comparable observations per policy arm.
    pub comparable_observations_per_arm: u64,
    /// Health snapshots required before a longitudinal term may be concluded.
    pub health_snapshots: u64,
    /// Improvement a soft objective must show before it counts, as a
    /// percentage.
    pub minimum_improvement_percent: f64,
}

impl Default for MinimumEvidence {
    fn default() -> Self {
        // Conservative: enough that one lucky run cannot move policy.
        Self {
            observations: 20,
            comparable_observations_per_arm: 8,
            health_snapshots: 3,
            minimum_improvement_percent: 2.0,
        }
    }
}

/// The full objective definition a policy is judged against.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OptimizationObjective {
    pub version: String,
    pub terms: Vec<ObjectiveTerm>,
    /// How far back evidence is gathered, in days.
    pub observation_window_days: u32,
    pub minimum_evidence: MinimumEvidence,
}

impl OptimizationObjective {
    /// The conservative default: correctness and integrity are hard, speed and
    /// cost are merely preferred, and repository health must not degrade
    /// materially.
    pub fn conservative_default() -> Self {
        Self {
            version: POLICY_OBJECTIVE_VERSION.to_string(),
            terms: vec![
                ObjectiveTerm::hard(
                    ObjectiveMetric::TaskSuccessRate,
                    Direction::HigherIsBetter,
                    ObjectiveConstraint::NonRegression {
                        tolerance_percent: 0.0,
                    },
                ),
                ObjectiveTerm::hard(
                    ObjectiveMetric::IntegrityCleanRate,
                    Direction::HigherIsBetter,
                    ObjectiveConstraint::AtLeast { value: 1.0 },
                ),
                ObjectiveTerm::hard(
                    ObjectiveMetric::RepositoryHealth {
                        dimension: HealthDimensionKind::Security,
                    },
                    Direction::HigherIsBetter,
                    ObjectiveConstraint::NonRegression {
                        tolerance_percent: 0.0,
                    },
                ),
                ObjectiveTerm::soft(ObjectiveMetric::Runtime, Direction::LowerIsBetter, 1),
                ObjectiveTerm::soft(ObjectiveMetric::Cost, Direction::LowerIsBetter, 2),
                ObjectiveTerm::soft(
                    ObjectiveMetric::RepositoryHealth {
                        dimension: HealthDimensionKind::Complexity,
                    },
                    Direction::LowerIsBetter,
                    3,
                ),
            ],
            observation_window_days: 30,
            minimum_evidence: MinimumEvidence::default(),
        }
    }

    pub fn hard_terms(&self) -> impl Iterator<Item = &ObjectiveTerm> {
        self.terms.iter().filter(|term| term.kind.is_hard())
    }

    pub fn soft_terms(&self) -> impl Iterator<Item = &ObjectiveTerm> {
        self.terms.iter().filter(|term| !term.kind.is_hard())
    }

    /// Whether any term depends on longitudinal health evidence.
    pub fn has_longitudinal_terms(&self) -> bool {
        self.terms.iter().any(|term| term.metric.is_longitudinal())
    }

    fn validate(&self) -> Result<(), PolicyError> {
        if self.version.trim().is_empty() {
            return Err(PolicyError::InvalidObjective("version is empty".into()));
        }
        if self.terms.is_empty() {
            return Err(PolicyError::InvalidObjective(
                "an objective with no terms cannot judge anything".into(),
            ));
        }
        if !self.terms.iter().any(|term| term.kind.is_hard()) {
            return Err(PolicyError::InvalidObjective(
                "at least one hard constraint is required; an objective of pure preferences \
                 can be satisfied by degrading correctness"
                    .into(),
            ));
        }
        let mut seen = BTreeSet::new();
        for term in &self.terms {
            if !seen.insert(term.metric.as_str()) {
                return Err(PolicyError::InvalidObjective(format!(
                    "metric `{}` appears more than once",
                    term.metric
                )));
            }
            if let ObjectiveKind::Soft {
                weight: Some(weight),
                ..
            } = &term.kind
                && (!weight.is_finite() || *weight < 0.0)
            {
                return Err(PolicyError::InvalidObjective(format!(
                    "weight for `{}` must be a finite non-negative number",
                    term.metric
                )));
            }
        }
        if self.observation_window_days == 0 {
            return Err(PolicyError::InvalidObjective(
                "observation window must be at least one day".into(),
            ));
        }
        Ok(())
    }
}

// --------------------------------------------------------- multi-objective

/// How a candidate compares to a baseline across every objective.
///
/// Deliberately not a score. "Faster but more complex" is a real answer and a
/// number that averages it away is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyComparison {
    /// Better or equal on every measured objective, better on at least one.
    Dominates,
    /// Worse or equal on every measured objective, worse on at least one.
    Dominated,
    /// Better on some, worse on others.
    Tradeoff,
    /// Measured, and indistinguishable.
    Equivalent,
    /// A hard constraint failed; preferences are not consulted.
    ConstraintViolated,
    /// Not enough comparable evidence to say anything.
    InsufficientEvidence,
}

impl PolicyComparison {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Dominates => "dominates",
            Self::Dominated => "dominated",
            Self::Tradeoff => "tradeoff",
            Self::Equivalent => "equivalent",
            Self::ConstraintViolated => "constraint-violated",
            Self::InsufficientEvidence => "insufficient-evidence",
        }
    }

    /// Whether this comparison could ever support promotion.
    pub fn is_promotable(self) -> bool {
        matches!(self, Self::Dominates | Self::Tradeoff)
    }
}

impl std::fmt::Display for PolicyComparison {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// -------------------------------------------------------------------- lifecycle

/// Where a policy is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyStatus {
    /// Created, governing nothing.
    Draft,
    /// Making decisions that are recorded but do not control execution.
    Shadow,
    /// Governing a bounded, deterministic subset of tasks.
    Canary,
    /// Governing execution.
    Active,
    /// Evaluated and refused. Retained for the record.
    Rejected,
    /// Replaced by a descendant policy.
    Superseded,
    /// Was active, then reverted.
    RolledBack,
    /// Withdrawn from use without being replaced.
    Retired,
}

impl PolicyStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Shadow => "shadow",
            Self::Canary => "canary",
            Self::Active => "active",
            Self::Rejected => "rejected",
            Self::Superseded => "superseded",
            Self::RolledBack => "rolled_back",
            Self::Retired => "retired",
        }
    }

    /// Whether the policy is governing any real execution.
    pub fn governs_execution(self) -> bool {
        matches!(self, Self::Canary | Self::Active)
    }

    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Rejected | Self::Superseded | Self::RolledBack | Self::Retired
        )
    }

    /// Transitions the lifecycle permits.
    ///
    /// A `Draft` can never become `Active` directly: promotion is an explicit
    /// act that requires evidence, and a policy that skipped testing must not
    /// be able to reach production by a single status write.
    pub fn can_transition_to(self, next: PolicyStatus) -> bool {
        use PolicyStatus::*;
        if self.is_terminal() {
            return false;
        }
        matches!(
            (self, next),
            (Draft, Shadow)
                | (Draft, Canary)
                | (Draft, Rejected)
                | (Draft, Retired)
                | (Shadow, Canary)
                | (Shadow, Rejected)
                | (Shadow, Retired)
                | (Canary, Active)
                | (Canary, Rejected)
                | (Canary, Retired)
                | (Active, Superseded)
                | (Active, RolledBack)
                | (Active, Retired)
        )
    }
}

impl std::fmt::Display for PolicyStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How a policy came to exist. Recorded at creation, never inferred later.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyProvenance {
    /// Written by a person or by configuration.
    UserDeclared,
    /// Brought in from outside this ledger.
    Imported,
    /// Produced by the optimizer.
    OptimizerProposed,
    /// Promoted after a controlled experiment.
    ExperimentPromoted,
    /// Created by reverting to an earlier policy.
    Rollback,
    /// Created by Phase 8 installation to preserve existing behavior.
    Bootstrap,
}

impl PolicyProvenance {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UserDeclared => "user_declared",
            Self::Imported => "imported",
            Self::OptimizerProposed => "optimizer_proposed",
            Self::ExperimentPromoted => "experiment_promoted",
            Self::Rollback => "rollback",
            Self::Bootstrap => "bootstrap",
        }
    }
}

/// Whether a change may be applied without a human.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalRequirement {
    /// A bounded parameter change, if automatic promotion is enabled at all.
    AutomaticAllowed,
    /// Needs an explicit human command.
    ApprovalRequired,
    /// Cannot be done by any means short of changing Forge itself.
    Forbidden,
}

impl ApprovalRequirement {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AutomaticAllowed => "automatic-allowed",
            Self::ApprovalRequired => "approval-required",
            Self::Forbidden => "forbidden",
        }
    }

    /// What approval a change to `dimension` needs.
    ///
    /// Only routing and resource *parameters* are ever eligible for automatic
    /// promotion, and only within bounds. Anything that changes the shape of
    /// execution — how many agents run, what context they see, whether review
    /// happens — requires a person.
    pub fn for_dimension(dimension: OptimizableDimension) -> Self {
        match dimension {
            OptimizableDimension::RoutingParameters | OptimizableDimension::ResourceBudgets => {
                Self::AutomaticAllowed
            }
            OptimizableDimension::ContextStrategy
            | OptimizableDimension::ExecutionStrategy
            | OptimizableDimension::TeamStrategy
            | OptimizableDimension::ReviewStrategy
            | OptimizableDimension::ExplorationStrategy => Self::ApprovalRequired,
        }
    }

    /// The strictest requirement across a set of changed dimensions.
    pub fn strictest(dimensions: &[OptimizableDimension]) -> Self {
        dimensions
            .iter()
            .map(|dimension| Self::for_dimension(*dimension))
            .max()
            .unwrap_or(Self::AutomaticAllowed)
    }
}

impl std::fmt::Display for ApprovalRequirement {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// ----------------------------------------------------------------------- policy

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum PolicyError {
    #[error("policy repository scope must not be empty")]
    EmptyRepository,
    #[error("unsupported policy schema `{0}`")]
    UnsupportedSchema(String),
    #[error("invalid objective: {0}")]
    InvalidObjective(String),
    #[error(
        "policy sets {setting} to {value}, above the configured maximum of {maximum}. \
         An optimizer may lower a budget; raising this one is a human decision."
    )]
    ExceedsBounds {
        setting: &'static str,
        value: String,
        maximum: String,
    },
    #[error("invalid policy setting: {0}")]
    InvalidSetting(String),
    #[error(
        "policy omits guardrail `{0}`. Guardrails are recorded, not configured; \
         a policy cannot declare one inapplicable."
    )]
    IncompleteGuardrails(FixedGuardrail),
    #[error("invalid policy transition: {from} -> {to}")]
    InvalidTransition {
        from: PolicyStatus,
        to: PolicyStatus,
    },
}

/// An immutable, versioned engineering strategy.
///
/// Contains only optimizable dimensions. There is deliberately no field for
/// required evaluators, protected paths, provenance rules, evidence
/// eligibility, or the meaning of PASS — those are [`FixedGuardrail`]s, and
/// their absence from this type is what makes "the optimizer cannot weaken its
/// own judge" a structural property rather than a promise.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineeringPolicy {
    pub policy_id: PolicyId,
    pub schema_version: String,
    /// Repository this policy governs. Policies are repository-scoped.
    pub repository: String,
    pub created_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_policy_id: Option<PolicyId>,
    pub status: PolicyStatus,
    pub provenance: PolicyProvenance,
    /// Guardrails in force. Always complete; recorded for interpretability.
    pub guardrails: GuardrailSet,

    pub routing: RoutingPolicySettings,
    pub context: ContextPolicy,
    pub execution: ExecutionStrategy,
    pub team: TeamPolicySettings,
    pub review: ReviewPolicy,
    pub resources: ResourcePolicy,
    pub exploration: ExplorationPolicySettings,

    pub objective: OptimizationObjective,
    /// The optimizer version that produced this policy, when one did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub optimizer_version: Option<String>,
    /// The proposal this policy came from, when it came from one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposal_id: Option<PolicyProposalId>,
}

impl EngineeringPolicy {
    /// The policy that reproduces existing Phase 0–7 behavior.
    ///
    /// Installing Phase 8 must not change how anything executes, so the
    /// bootstrap policy is exactly the prior configured-default routing,
    /// context, single-run, resource, and exploration behavior.
    pub fn bootstrap(policy_id: PolicyId, repository: impl Into<String>) -> Self {
        let repository = repository.into();
        Self::bootstrap_from_config(policy_id, &ForgeConfig::default_for(repository.clone()))
    }

    /// The immutable policy snapshot that exactly describes the configured
    /// Phase 7 execution path.
    pub fn bootstrap_from_config(policy_id: PolicyId, config: &ForgeConfig) -> Self {
        let world_enabled = config.world_model.enabled;
        let configured_timeout = config
            .agents
            .values()
            .filter_map(|agent| agent.timeout_secs)
            .fold(config.defaults.timeout_secs, u64::max);
        Self {
            policy_id,
            schema_version: POLICY_SCHEMA_VERSION.to_string(),
            repository: config.repository.name.clone(),
            created_at: Utc::now(),
            parent_policy_id: None,
            status: PolicyStatus::Active,
            provenance: PolicyProvenance::Bootstrap,
            guardrails: GuardrailSet::complete(),
            routing: RoutingPolicySettings {
                use_learned_routing: false,
                minimum_total_evidence: config.routing.minimum_total_evidence,
                minimum_agent_evidence: config.routing.minimum_agent_evidence,
                minimum_score_margin: config.routing.minimum_score_margin,
                evidence_policy_version: crate::routing::DEFAULT_EVIDENCE_POLICY_VERSION
                    .to_string(),
            },
            context: ContextPolicy {
                max_world_facts: if world_enabled { 12 } else { 0 },
                include_failure_history: world_enabled && config.world_model.history,
                selection_strategy: if world_enabled {
                    ContextSelectionStrategy::TaskRelevanceWithComponentsV1
                } else {
                    ContextSelectionStrategy::None
                },
            },
            execution: ExecutionStrategy::SingleAgent,
            team: TeamPolicySettings {
                plan_template: TeamPlanTemplate::ImplementationOnly,
                max_parallel_nodes: u32::try_from(config.team.max_parallel_nodes)
                    .unwrap_or(u32::MAX),
                stop_on_required_node_failure: config.team.stop_on_required_node_failure,
            },
            review: ReviewPolicy::default(),
            resources: ResourcePolicy {
                timeout_secs: configured_timeout,
                max_retries: 0,
                max_cost_usd: None,
            },
            exploration: ExplorationPolicySettings {
                policy: config.routing.exploration_policy,
                ..ExplorationPolicySettings::default()
            },
            objective: OptimizationObjective::conservative_default(),
            optimizer_version: None,
            proposal_id: None,
        }
    }

    /// Rejects any policy that is out of bounds, malformed, or that claims a
    /// guardrail does not apply.
    pub fn validate(&self, bounds: &PolicyBounds) -> Result<(), PolicyError> {
        if self.repository.trim().is_empty() {
            return Err(PolicyError::EmptyRepository);
        }
        if self.schema_version != POLICY_SCHEMA_VERSION {
            return Err(PolicyError::UnsupportedSchema(self.schema_version.clone()));
        }

        // Guardrails are recorded, never negotiated.
        for guardrail in FixedGuardrail::ALL {
            if !self.guardrails.contains(guardrail) {
                return Err(PolicyError::IncompleteGuardrails(guardrail));
            }
        }

        self.objective.validate()?;

        if self.context.max_world_facts > bounds.max_world_facts {
            return Err(PolicyError::ExceedsBounds {
                setting: "context.max_world_facts",
                value: self.context.max_world_facts.to_string(),
                maximum: bounds.max_world_facts.to_string(),
            });
        }
        if self.resources.timeout_secs > bounds.max_timeout_secs {
            return Err(PolicyError::ExceedsBounds {
                setting: "resources.timeout_secs",
                value: self.resources.timeout_secs.to_string(),
                maximum: bounds.max_timeout_secs.to_string(),
            });
        }
        if self.resources.max_retries > bounds.max_retries {
            return Err(PolicyError::ExceedsBounds {
                setting: "resources.max_retries",
                value: self.resources.max_retries.to_string(),
                maximum: bounds.max_retries.to_string(),
            });
        }
        if self.team.max_parallel_nodes > bounds.max_parallel_team_nodes {
            return Err(PolicyError::ExceedsBounds {
                setting: "team.max_parallel_nodes",
                value: self.team.max_parallel_nodes.to_string(),
                maximum: bounds.max_parallel_team_nodes.to_string(),
            });
        }
        if self.review.advisory_review_nodes > bounds.max_review_nodes {
            return Err(PolicyError::ExceedsBounds {
                setting: "review.advisory_review_nodes",
                value: self.review.advisory_review_nodes.to_string(),
                maximum: bounds.max_review_nodes.to_string(),
            });
        }
        match (self.resources.max_cost_usd, bounds.max_cost_usd) {
            (Some(policy_cost), Some(bound)) if policy_cost > bound => {
                return Err(PolicyError::ExceedsBounds {
                    setting: "resources.max_cost_usd",
                    value: policy_cost.to_string(),
                    maximum: bound.to_string(),
                });
            }
            _ => {}
        }

        if self.resources.timeout_secs == 0 {
            return Err(PolicyError::InvalidSetting(
                "resources.timeout_secs must be greater than zero".into(),
            ));
        }
        if !(0.0..=1.0).contains(&self.routing.minimum_score_margin)
            || !self.routing.minimum_score_margin.is_finite()
        {
            return Err(PolicyError::InvalidSetting(
                "routing.minimum_score_margin must be between 0.0 and 1.0".into(),
            ));
        }
        if self.routing.evidence_policy_version.trim().is_empty() {
            return Err(PolicyError::InvalidSetting(
                "routing.evidence_policy_version must name a versioned evidence policy".into(),
            ));
        }
        if self.review.advisory_review_enabled && self.review.advisory_review_nodes == 0 {
            return Err(PolicyError::InvalidSetting(
                "review is enabled with zero reviewers".into(),
            ));
        }
        if self.team.max_parallel_nodes == 0 {
            return Err(PolicyError::InvalidSetting(
                "team.max_parallel_nodes must be at least one".into(),
            ));
        }
        Ok(())
    }

    /// Deterministic identity over everything that can change execution.
    ///
    /// Two policies that would behave differently must not share a
    /// fingerprint, so every setting is written with explicit separators and
    /// the schema, objective, scope, and parent are included.
    pub fn fingerprint(&self) -> String {
        let mut digest = Sha256::new();
        let field = |value: &str, digest: &mut Sha256| {
            digest.update(value.as_bytes());
            digest.update([0x1f]);
        };

        field(&self.schema_version, &mut digest);
        field(&self.repository, &mut digest);
        field(
            self.parent_policy_id
                .as_ref()
                .map(|id| id.as_str())
                .unwrap_or(""),
            &mut digest,
        );

        // Routing
        field(&self.routing.use_learned_routing.to_string(), &mut digest);
        field(
            &self.routing.minimum_total_evidence.to_string(),
            &mut digest,
        );
        field(
            &self.routing.minimum_agent_evidence.to_string(),
            &mut digest,
        );
        field(
            &self.routing.minimum_score_margin.to_bits().to_string(),
            &mut digest,
        );
        field(&self.routing.evidence_policy_version, &mut digest);

        // Context
        field(&self.context.max_world_facts.to_string(), &mut digest);
        field(
            &self.context.include_failure_history.to_string(),
            &mut digest,
        );
        field(self.context.selection_strategy.as_str(), &mut digest);

        // Execution / team / review
        field(self.execution.as_str(), &mut digest);
        field(self.team.plan_template.as_str(), &mut digest);
        field(&self.team.max_parallel_nodes.to_string(), &mut digest);
        field(
            &self.team.stop_on_required_node_failure.to_string(),
            &mut digest,
        );
        field(
            &self.review.advisory_review_enabled.to_string(),
            &mut digest,
        );
        field(&self.review.advisory_review_nodes.to_string(), &mut digest);

        // Resources / exploration
        field(&self.resources.timeout_secs.to_string(), &mut digest);
        field(&self.resources.max_retries.to_string(), &mut digest);
        field(&optional_float(self.resources.max_cost_usd), &mut digest);
        field(&format!("{:?}", self.exploration.policy), &mut digest);
        field(&self.exploration.max_extra_runs.to_string(), &mut digest);
        field(
            &optional_float(self.exploration.max_extra_cost_usd),
            &mut digest,
        );

        // Objective
        field(&self.objective.version, &mut digest);
        field(
            &self.objective.observation_window_days.to_string(),
            &mut digest,
        );
        field(
            &self.objective.minimum_evidence.observations.to_string(),
            &mut digest,
        );
        field(
            &self
                .objective
                .minimum_evidence
                .comparable_observations_per_arm
                .to_string(),
            &mut digest,
        );
        field(
            &self.objective.minimum_evidence.health_snapshots.to_string(),
            &mut digest,
        );
        field(
            &self
                .objective
                .minimum_evidence
                .minimum_improvement_percent
                .to_bits()
                .to_string(),
            &mut digest,
        );
        for term in &self.objective.terms {
            field(&term.metric.as_str(), &mut digest);
            field(term.direction.as_str(), &mut digest);
            field(&format!("{:?}", term.kind), &mut digest);
        }

        // Guardrails, so a record that somehow omitted one is distinguishable.
        for guardrail in self.guardrails.iter() {
            field(guardrail.as_str(), &mut digest);
        }

        format!("{:x}", digest.finalize())[..32].to_string()
    }

    /// Advances the lifecycle.
    pub fn transition_to(&mut self, next: PolicyStatus) -> Result<(), PolicyError> {
        if !self.status.can_transition_to(next) {
            return Err(PolicyError::InvalidTransition {
                from: self.status,
                to: next,
            });
        }
        self.status = next;
        Ok(())
    }

    /// Which optimizable dimensions differ from another policy.
    ///
    /// Used to decide what approval a change needs, so it must not miss a
    /// dimension: anything not compared here would be promotable without the
    /// approval its dimension requires.
    pub fn changed_dimensions(&self, other: &Self) -> Vec<OptimizableDimension> {
        let mut changed = Vec::new();
        if self.routing != other.routing {
            changed.push(OptimizableDimension::RoutingParameters);
        }
        if self.context != other.context {
            changed.push(OptimizableDimension::ContextStrategy);
        }
        if self.execution != other.execution {
            changed.push(OptimizableDimension::ExecutionStrategy);
        }
        if self.team != other.team {
            changed.push(OptimizableDimension::TeamStrategy);
        }
        if self.review != other.review {
            changed.push(OptimizableDimension::ReviewStrategy);
        }
        if self.resources != other.resources {
            changed.push(OptimizableDimension::ResourceBudgets);
        }
        if self.exploration != other.exploration {
            changed.push(OptimizableDimension::ExplorationStrategy);
        }
        changed
    }

    /// A human-readable description of what changed, for proposals.
    pub fn describe_changes(&self, other: &Self) -> Vec<String> {
        let mut changes = Vec::new();
        if self.context.max_world_facts != other.context.max_world_facts {
            changes.push(format!(
                "context.max_world_facts {} → {}",
                other.context.max_world_facts, self.context.max_world_facts
            ));
        }
        if self.context.selection_strategy != other.context.selection_strategy {
            changes.push(format!(
                "context.selection_strategy {} → {}",
                other.context.selection_strategy.as_str(),
                self.context.selection_strategy.as_str()
            ));
        }
        if self.context.include_failure_history != other.context.include_failure_history {
            changes.push(format!(
                "context.include_failure_history {} → {}",
                other.context.include_failure_history, self.context.include_failure_history
            ));
        }
        if self.routing.minimum_score_margin != other.routing.minimum_score_margin {
            changes.push(format!(
                "routing.minimum_score_margin {} → {}",
                other.routing.minimum_score_margin, self.routing.minimum_score_margin
            ));
        }
        if self.routing.use_learned_routing != other.routing.use_learned_routing {
            changes.push(format!(
                "routing.use_learned_routing {} → {}",
                other.routing.use_learned_routing, self.routing.use_learned_routing
            ));
        }
        if self.execution != other.execution {
            changes.push(format!(
                "execution {} → {}",
                other.execution.as_str(),
                self.execution.as_str()
            ));
        }
        if self.team.plan_template != other.team.plan_template {
            changes.push(format!(
                "team.plan_template {} → {}",
                other.team.plan_template.as_str(),
                self.team.plan_template.as_str()
            ));
        }
        if self.review != other.review {
            changes.push(format!(
                "review.advisory_review_enabled {} → {}",
                other.review.advisory_review_enabled, self.review.advisory_review_enabled
            ));
        }
        if self.resources.timeout_secs != other.resources.timeout_secs {
            changes.push(format!(
                "resources.timeout_secs {} → {}",
                other.resources.timeout_secs, self.resources.timeout_secs
            ));
        }
        if self.resources.max_retries != other.resources.max_retries {
            changes.push(format!(
                "resources.max_retries {} → {}",
                other.resources.max_retries, self.resources.max_retries
            ));
        }
        changes
    }

    /// The approval a change from `parent` to this policy requires.
    pub fn approval_requirement(&self, parent: &Self) -> ApprovalRequirement {
        ApprovalRequirement::strictest(&self.changed_dimensions(parent))
    }

    /// Settings a report can display without knowing the type.
    pub fn display_rows(&self) -> Vec<(&'static str, String)> {
        vec![
            (
                "Routing",
                format!(
                    "{} margin {:.2}",
                    if self.routing.use_learned_routing {
                        "learned"
                    } else {
                        "configured default"
                    },
                    self.routing.minimum_score_margin
                ),
            ),
            (
                "Context",
                format!(
                    "{} max {} facts{}",
                    self.context.selection_strategy.as_str(),
                    self.context.max_world_facts,
                    if self.context.include_failure_history {
                        " + failure history"
                    } else {
                        ""
                    }
                ),
            ),
            ("Execution", self.execution.as_str().to_string()),
            (
                "Team",
                format!(
                    "{} max {} parallel",
                    self.team.plan_template.as_str(),
                    self.team.max_parallel_nodes
                ),
            ),
            (
                "Review",
                if self.review.advisory_review_enabled {
                    format!("{} advisory reviewer(s)", self.review.advisory_review_nodes)
                } else {
                    "none".to_string()
                },
            ),
            (
                "Resources",
                format!(
                    "timeout {}s, {} retries{}",
                    self.resources.timeout_secs,
                    self.resources.max_retries,
                    self.resources
                        .max_cost_usd
                        .map(|cost| format!(", max ${cost:.2}"))
                        .unwrap_or_default()
                ),
            ),
            (
                "Exploration",
                format!(
                    "{:?}, max {} extra runs",
                    self.exploration.policy, self.exploration.max_extra_runs
                ),
            ),
        ]
    }
}

fn optional_float(value: Option<f64>) -> String {
    value
        .map(|value| value.to_bits().to_string())
        .unwrap_or_else(|| "none".to_string())
}

/// A policy's settings expressed as flat key/value pairs.
///
/// Used by validation tests and by the CLI. Deliberately derived from the typed
/// policy rather than being a parallel representation that could drift.
pub fn policy_settings(policy: &EngineeringPolicy) -> BTreeMap<String, String> {
    BTreeMap::from([
        (
            "routing.use_learned_routing".into(),
            policy.routing.use_learned_routing.to_string(),
        ),
        (
            "routing.minimum_score_margin".into(),
            policy.routing.minimum_score_margin.to_string(),
        ),
        (
            "context.max_world_facts".into(),
            policy.context.max_world_facts.to_string(),
        ),
        (
            "context.selection_strategy".into(),
            policy.context.selection_strategy.as_str().to_string(),
        ),
        ("execution".into(), policy.execution.as_str().to_string()),
        (
            "team.plan_template".into(),
            policy.team.plan_template.as_str().to_string(),
        ),
        (
            "review.advisory_review_enabled".into(),
            policy.review.advisory_review_enabled.to_string(),
        ),
        (
            "resources.timeout_secs".into(),
            policy.resources.timeout_secs.to_string(),
        ),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A single-field edit to a policy, used to prove coverage.
    type PolicyMutation = Box<dyn Fn(&mut EngineeringPolicy)>;

    fn policy() -> EngineeringPolicy {
        EngineeringPolicy::bootstrap(PolicyId::sequential(1), "forge")
    }

    // ------------------------------------------------------- safety boundary

    /// The central Phase 8 invariant, asserted structurally.
    #[test]
    fn a_policy_has_no_field_capable_of_expressing_a_guardrail() {
        // Serializing a policy must not produce any key that names a guardrail
        // concept. If someone later adds one, this fails loudly.
        let json = serde_json::to_value(policy()).unwrap();
        let object = json.as_object().unwrap();

        for forbidden in [
            "required_evaluators",
            "evaluators",
            "protected_paths",
            "protection",
            "integrity",
            "secrets",
            "provenance_rules",
            "evidence_eligibility",
            "pass_threshold",
            "verdict",
            "truth",
            "security_policy",
        ] {
            assert!(
                !object.contains_key(forbidden),
                "policy exposes `{forbidden}`, which would let an optimizer weaken its own judge"
            );
        }
    }

    #[test]
    fn unknown_policy_knobs_are_rejected_rather_than_ignored() {
        // A policy carrying a setting Forge does not understand is not a
        // policy Forge can honour.
        let mut json = serde_json::to_value(policy()).unwrap();
        json.as_object_mut()
            .unwrap()
            .insert("required_evaluators".into(), serde_json::json!([]));

        let error = serde_json::from_value::<EngineeringPolicy>(json).unwrap_err();
        assert!(error.to_string().contains("unknown field"), "{error}");
    }

    #[test]
    fn every_guardrail_must_be_recorded_as_in_force() {
        let mut policy = policy();
        // Simulate a policy that tried to drop a guardrail.
        let reduced: BTreeSet<FixedGuardrail> = FixedGuardrail::ALL
            .into_iter()
            .filter(|guardrail| *guardrail != FixedGuardrail::RequiredEvaluators)
            .collect();
        policy.guardrails = GuardrailSet(reduced);

        let error = policy.validate(&PolicyBounds::default()).unwrap_err();
        assert_eq!(
            error,
            PolicyError::IncompleteGuardrails(FixedGuardrail::RequiredEvaluators)
        );
    }

    #[test]
    fn the_complete_guardrail_set_covers_every_guardrail() {
        let set = GuardrailSet::complete();
        assert!(set.is_complete());
        for guardrail in FixedGuardrail::ALL {
            assert!(set.contains(guardrail), "{guardrail} missing");
            // Each guardrail explains itself, so a refusal can be reasoned about.
            assert!(!guardrail.rationale().is_empty());
        }
    }

    #[test]
    fn guardrails_and_optimizable_dimensions_do_not_overlap() {
        // Nothing may be both a safety property and a tuning knob.
        let guardrails: BTreeSet<&str> = FixedGuardrail::ALL.iter().map(|g| g.as_str()).collect();
        let optimizable: BTreeSet<&str> = OptimizableDimension::ALL
            .iter()
            .map(|dimension| dimension.as_str())
            .collect();
        assert!(guardrails.is_disjoint(&optimizable));
    }

    // --------------------------------------------------------------- bounds

    #[test]
    fn a_policy_may_not_exceed_a_configured_maximum() {
        let bounds = PolicyBounds::default();
        // Values copied out so the mutations own everything they touch.
        let (facts, timeout, retries, nodes, reviewers) = (
            bounds.max_world_facts + 1,
            bounds.max_timeout_secs + 1,
            bounds.max_retries + 1,
            bounds.max_parallel_team_nodes + 1,
            bounds.max_review_nodes + 1,
        );

        let cases: Vec<(PolicyMutation, &str)> = vec![
            (
                Box::new(move |policy: &mut EngineeringPolicy| {
                    policy.context.max_world_facts = facts
                }),
                "context.max_world_facts",
            ),
            (
                Box::new(move |policy: &mut EngineeringPolicy| {
                    policy.resources.timeout_secs = timeout
                }),
                "resources.timeout_secs",
            ),
            (
                Box::new(move |policy: &mut EngineeringPolicy| {
                    policy.resources.max_retries = retries
                }),
                "resources.max_retries",
            ),
            (
                Box::new(move |policy: &mut EngineeringPolicy| {
                    policy.team.max_parallel_nodes = nodes
                }),
                "team.max_parallel_nodes",
            ),
            (
                Box::new(move |policy: &mut EngineeringPolicy| {
                    policy.review.advisory_review_nodes = reviewers
                }),
                "review.advisory_review_nodes",
            ),
        ];

        for (mutate, setting) in cases {
            let mut policy = policy();
            mutate(&mut policy);
            let error = policy.validate(&bounds).unwrap_err();
            assert!(
                matches!(&error, PolicyError::ExceedsBounds { setting: s, .. } if *s == setting),
                "expected {setting} to be refused, got {error}"
            );
        }
    }

    #[test]
    fn lowering_a_budget_is_always_allowed() {
        // An optimizer may spend less; only a human may authorize more.
        let mut policy = policy();
        policy.resources.timeout_secs = 60;
        policy.context.max_world_facts = 2;
        policy.validate(&PolicyBounds::default()).unwrap();
    }

    #[test]
    fn a_cost_ceiling_above_the_configured_maximum_is_refused() {
        let bounds = PolicyBounds {
            max_cost_usd: Some(5.0),
            ..PolicyBounds::default()
        };
        let mut policy = policy();
        policy.resources.max_cost_usd = Some(50.0);
        assert!(matches!(
            policy.validate(&bounds),
            Err(PolicyError::ExceedsBounds { .. })
        ));
    }

    #[test]
    fn malformed_settings_are_refused() {
        let bounds = PolicyBounds::default();

        let mut zero_timeout = policy();
        zero_timeout.resources.timeout_secs = 0;
        assert!(zero_timeout.validate(&bounds).is_err());

        let mut bad_margin = policy();
        bad_margin.routing.minimum_score_margin = 1.5;
        assert!(bad_margin.validate(&bounds).is_err());

        let mut review_without_reviewers = policy();
        review_without_reviewers.review.advisory_review_enabled = true;
        assert!(review_without_reviewers.validate(&bounds).is_err());

        let mut no_nodes = policy();
        no_nodes.team.max_parallel_nodes = 0;
        assert!(no_nodes.validate(&bounds).is_err());
    }

    // ------------------------------------------------------------ objectives

    #[test]
    fn the_default_objective_makes_correctness_and_integrity_hard() {
        let objective = OptimizationObjective::conservative_default();
        objective.validate().unwrap();

        let hard: Vec<String> = objective
            .hard_terms()
            .map(|term| term.metric.as_str())
            .collect();
        assert!(hard.contains(&"task_success_rate".to_string()));
        assert!(hard.contains(&"integrity_clean_rate".to_string()));
        assert!(hard.contains(&"health.security".to_string()));

        // Speed and cost are preferences, never licences.
        let soft: Vec<String> = objective
            .soft_terms()
            .map(|term| term.metric.as_str())
            .collect();
        assert!(soft.contains(&"runtime".to_string()));
        assert!(soft.contains(&"cost".to_string()));
    }

    #[test]
    fn an_objective_of_pure_preferences_is_rejected() {
        // Without a hard constraint, "faster" could be satisfied by breaking
        // everything quickly.
        let objective = OptimizationObjective {
            version: POLICY_OBJECTIVE_VERSION.into(),
            terms: vec![ObjectiveTerm::soft(
                ObjectiveMetric::Runtime,
                Direction::LowerIsBetter,
                1,
            )],
            observation_window_days: 30,
            minimum_evidence: MinimumEvidence::default(),
        };
        let error = objective.validate().unwrap_err();
        assert!(error.to_string().contains("hard constraint"), "{error}");
    }

    #[test]
    fn a_duplicated_objective_metric_is_rejected() {
        let mut objective = OptimizationObjective::conservative_default();
        objective.terms.push(ObjectiveTerm::soft(
            ObjectiveMetric::Runtime,
            Direction::LowerIsBetter,
            9,
        ));
        assert!(objective.validate().is_err());
    }

    #[test]
    fn an_unmeasured_hard_constraint_is_not_satisfied() {
        // "We did not measure it" is never "it held".
        let constraint = ObjectiveConstraint::AtLeast { value: 1.0 };
        assert!(!constraint.is_satisfied(None, None, Direction::HigherIsBetter));
        assert!(constraint.is_satisfied(Some(1.0), None, Direction::HigherIsBetter));
    }

    #[test]
    fn non_regression_respects_the_metrics_own_direction() {
        let constraint = ObjectiveConstraint::NonRegression {
            tolerance_percent: 5.0,
        };
        // Higher is better: a 10% drop violates.
        assert!(!constraint.is_satisfied(Some(90.0), Some(100.0), Direction::HigherIsBetter));
        assert!(constraint.is_satisfied(Some(98.0), Some(100.0), Direction::HigherIsBetter));
        // Lower is better: a 10% rise violates.
        assert!(!constraint.is_satisfied(Some(110.0), Some(100.0), Direction::LowerIsBetter));
        assert!(constraint.is_satisfied(Some(102.0), Some(100.0), Direction::LowerIsBetter));
    }

    #[test]
    fn non_regression_without_a_baseline_is_not_satisfied() {
        let constraint = ObjectiveConstraint::NonRegression {
            tolerance_percent: 5.0,
        };
        assert!(!constraint.is_satisfied(Some(100.0), None, Direction::HigherIsBetter));
    }

    #[test]
    fn longitudinal_terms_are_identifiable() {
        assert!(
            ObjectiveMetric::RepositoryHealth {
                dimension: HealthDimensionKind::Complexity
            }
            .is_longitudinal()
        );
        assert!(!ObjectiveMetric::Runtime.is_longitudinal());
        assert!(OptimizationObjective::conservative_default().has_longitudinal_terms());
    }

    // ------------------------------------------------------------- lifecycle

    #[test]
    fn a_draft_can_never_become_active_directly() {
        // Promotion must pass through testing; a single status write must not
        // put an untested policy into production.
        assert!(!PolicyStatus::Draft.can_transition_to(PolicyStatus::Active));
        assert!(PolicyStatus::Draft.can_transition_to(PolicyStatus::Shadow));
        assert!(PolicyStatus::Draft.can_transition_to(PolicyStatus::Canary));
        assert!(PolicyStatus::Canary.can_transition_to(PolicyStatus::Active));
    }

    #[test]
    fn rejected_and_rolled_back_policies_are_terminal_but_retained() {
        for status in [
            PolicyStatus::Rejected,
            PolicyStatus::Superseded,
            PolicyStatus::RolledBack,
            PolicyStatus::Retired,
        ] {
            assert!(status.is_terminal());
            for next in [
                PolicyStatus::Active,
                PolicyStatus::Canary,
                PolicyStatus::Draft,
            ] {
                assert!(!status.can_transition_to(next), "{status} -> {next}");
            }
        }
    }

    #[test]
    fn only_canary_and_active_govern_execution() {
        assert!(PolicyStatus::Active.governs_execution());
        assert!(PolicyStatus::Canary.governs_execution());
        assert!(!PolicyStatus::Shadow.governs_execution());
        assert!(!PolicyStatus::Draft.governs_execution());
        assert!(!PolicyStatus::Rejected.governs_execution());
    }

    #[test]
    fn transitions_are_validated_on_the_policy() {
        let mut policy = policy();
        policy.status = PolicyStatus::Draft;
        assert!(policy.transition_to(PolicyStatus::Active).is_err());
        policy.transition_to(PolicyStatus::Shadow).unwrap();
        policy.transition_to(PolicyStatus::Canary).unwrap();
        policy.transition_to(PolicyStatus::Active).unwrap();
        assert_eq!(policy.status, PolicyStatus::Active);
    }

    // ----------------------------------------------------------- fingerprint

    #[test]
    fn identical_policies_share_a_fingerprint() {
        let a = policy();
        let mut b = policy();
        // Identity and timestamps are not behaviour.
        b.policy_id = PolicyId::sequential(99);
        b.created_at = Utc::now();
        b.status = PolicyStatus::Canary;
        assert_eq!(a.fingerprint(), b.fingerprint());
    }

    #[test]
    fn every_behavioural_setting_changes_the_fingerprint() {
        let baseline = policy().fingerprint();
        let mutations: Vec<PolicyMutation> = vec![
            Box::new(|p| p.routing.use_learned_routing = true),
            Box::new(|p| p.routing.minimum_total_evidence = 99),
            Box::new(|p| p.routing.minimum_agent_evidence = 99),
            Box::new(|p| p.routing.minimum_score_margin = 0.5),
            Box::new(|p| p.routing.evidence_policy_version = "other".into()),
            Box::new(|p| p.context.max_world_facts = 8),
            Box::new(|p| p.context.include_failure_history = false),
            Box::new(|p| p.context.selection_strategy = ContextSelectionStrategy::None),
            Box::new(|p| p.execution = ExecutionStrategy::Team),
            Box::new(|p| p.team.plan_template = TeamPlanTemplate::ImplementationThenReview),
            Box::new(|p| p.team.max_parallel_nodes = 4),
            Box::new(|p| p.team.stop_on_required_node_failure = true),
            Box::new(|p| p.review.advisory_review_enabled = true),
            Box::new(|p| p.review.advisory_review_nodes = 1),
            Box::new(|p| p.resources.timeout_secs = 60),
            Box::new(|p| p.resources.max_retries = 2),
            Box::new(|p| p.resources.max_cost_usd = Some(1.0)),
            Box::new(|p| p.exploration.policy = ExplorationPolicy::None),
            Box::new(|p| p.exploration.max_extra_runs = 9),
            Box::new(|p| p.objective.observation_window_days = 7),
            Box::new(|p| p.objective.minimum_evidence.observations = 999),
            Box::new(|p| p.repository = "other".into()),
            Box::new(|p| p.parent_policy_id = Some(PolicyId::sequential(7))),
        ];

        for (index, mutate) in mutations.iter().enumerate() {
            let mut candidate = policy();
            mutate(&mut candidate);
            assert_ne!(
                candidate.fingerprint(),
                baseline,
                "mutation {index} did not change the fingerprint; two policies that \
                 behave differently would share an identity"
            );
        }
    }

    #[test]
    fn fingerprints_are_not_confused_by_field_boundaries() {
        // Without separators, ("ab", "c") and ("a", "bc") could collide.
        let mut a = policy();
        a.repository = "ab".into();
        a.routing.evidence_policy_version = "c".into();
        let mut b = policy();
        b.repository = "a".into();
        b.routing.evidence_policy_version = "bc".into();
        assert_ne!(a.fingerprint(), b.fingerprint());
    }

    #[test]
    fn fingerprints_are_stable_across_repeated_computation() {
        let policy = policy();
        let first = policy.fingerprint();
        for _ in 0..5 {
            assert_eq!(policy.fingerprint(), first);
        }
    }

    // -------------------------------------------------------------- approval

    #[test]
    fn changing_execution_shape_requires_human_approval() {
        let parent = policy();
        let mut candidate = policy();
        candidate.execution = ExecutionStrategy::Team;

        assert_eq!(
            candidate.changed_dimensions(&parent),
            vec![OptimizableDimension::ExecutionStrategy]
        );
        assert_eq!(
            candidate.approval_requirement(&parent),
            ApprovalRequirement::ApprovalRequired
        );
    }

    #[test]
    fn a_bounded_parameter_change_may_be_automatic() {
        let parent = policy();
        let mut candidate = policy();
        candidate.routing.minimum_score_margin = 0.08;
        assert_eq!(
            candidate.approval_requirement(&parent),
            ApprovalRequirement::AutomaticAllowed
        );
    }

    #[test]
    fn the_strictest_requirement_across_changed_dimensions_wins() {
        let parent = policy();
        let mut candidate = policy();
        candidate.routing.minimum_score_margin = 0.08; // automatic
        candidate.context.max_world_facts = 8; // approval required

        let dimensions = candidate.changed_dimensions(&parent);
        assert_eq!(dimensions.len(), 2);
        assert_eq!(
            candidate.approval_requirement(&parent),
            ApprovalRequirement::ApprovalRequired
        );
    }

    #[test]
    fn every_optimizable_dimension_is_detected_by_the_change_comparison() {
        // A dimension missed here would be promotable without its approval.
        let parent = policy();
        let mutations: Vec<(PolicyMutation, OptimizableDimension)> = vec![
            (
                Box::new(|p: &mut EngineeringPolicy| p.routing.minimum_agent_evidence = 9),
                OptimizableDimension::RoutingParameters,
            ),
            (
                Box::new(|p: &mut EngineeringPolicy| p.context.max_world_facts = 8),
                OptimizableDimension::ContextStrategy,
            ),
            (
                Box::new(|p: &mut EngineeringPolicy| p.execution = ExecutionStrategy::Team),
                OptimizableDimension::ExecutionStrategy,
            ),
            (
                Box::new(|p: &mut EngineeringPolicy| p.team.max_parallel_nodes = 4),
                OptimizableDimension::TeamStrategy,
            ),
            (
                Box::new(|p: &mut EngineeringPolicy| p.review.advisory_review_enabled = true),
                OptimizableDimension::ReviewStrategy,
            ),
            (
                Box::new(|p: &mut EngineeringPolicy| p.resources.timeout_secs = 60),
                OptimizableDimension::ResourceBudgets,
            ),
            (
                Box::new(|p: &mut EngineeringPolicy| p.exploration.max_extra_runs = 9),
                OptimizableDimension::ExplorationStrategy,
            ),
        ];

        assert_eq!(mutations.len(), OptimizableDimension::ALL.len());
        for (mutate, expected) in mutations {
            let mut candidate = policy();
            mutate(&mut candidate);
            assert_eq!(candidate.changed_dimensions(&parent), vec![expected]);
        }
    }

    #[test]
    fn changes_describe_themselves_for_a_proposal() {
        let parent = policy();
        let mut candidate = policy();
        candidate.context.max_world_facts = 8;

        let changes = candidate.describe_changes(&parent);
        assert_eq!(changes, vec!["context.max_world_facts 12 → 8"]);
    }

    // ----------------------------------------------------------- comparison

    #[test]
    fn only_dominating_or_tradeoff_comparisons_can_support_promotion() {
        assert!(PolicyComparison::Dominates.is_promotable());
        assert!(PolicyComparison::Tradeoff.is_promotable());
        for comparison in [
            PolicyComparison::Dominated,
            PolicyComparison::Equivalent,
            PolicyComparison::ConstraintViolated,
            PolicyComparison::InsufficientEvidence,
        ] {
            assert!(!comparison.is_promotable(), "{comparison}");
        }
    }

    // ------------------------------------------------------------ bootstrap

    #[test]
    fn the_bootstrap_policy_is_valid_and_preserves_existing_behaviour() {
        let policy = EngineeringPolicy::bootstrap(PolicyId::sequential(1), "forge");
        policy.validate(&PolicyBounds::default()).unwrap();

        assert_eq!(policy.provenance, PolicyProvenance::Bootstrap);
        assert_eq!(policy.status, PolicyStatus::Active);
        assert!(policy.parent_policy_id.is_none());
        // Installing Phase 8 must not change how anything runs.
        assert!(!policy.routing.use_learned_routing);
        assert_eq!(policy.context.max_world_facts, 12);
        assert_eq!(policy.execution, ExecutionStrategy::SingleAgent);
        assert!(!policy.review.advisory_review_enabled);
        assert_eq!(
            policy.team.plan_template,
            TeamPlanTemplate::ImplementationOnly
        );
    }

    #[test]
    fn policies_round_trip() {
        let policy = policy();
        let json = serde_json::to_string(&policy).unwrap();
        let back: EngineeringPolicy = serde_json::from_str(&json).unwrap();
        assert_eq!(back, policy);
        assert_eq!(back.fingerprint(), policy.fingerprint());
    }

    #[test]
    fn display_rows_cover_every_optimizable_dimension() {
        let rows = policy().display_rows();
        assert_eq!(rows.len(), OptimizableDimension::ALL.len());
        assert!(!policy_settings(&policy()).is_empty());
    }
}
