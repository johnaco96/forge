//! Forge's core domain model.
//!
//! This crate defines *what Forge knows about*: tasks, agents, runs, events,
//! workspaces, and evaluations. It deliberately depends on no agent, no
//! database, and no execution mechanism, so those can all be replaced without
//! touching the vocabulary the rest of the system shares.
//!
//! The shape of a run:
//!
//! ```text
//! EngineeringTask ──▶ AgentRun ──▶ Event stream
//!                        │
//!                        ├──▶ Workspace   (isolated checkout)
//!                        ├──▶ PatchSummary (what changed)
//!                        └──▶ Evaluation  (Forge's own judgment)
//! ```

#![deny(rust_2018_idioms)]

pub mod agent;
pub mod config;
pub mod events;
pub mod experiment;
pub mod ids;
pub mod integrity;
pub mod patch;
pub mod result;
pub mod routing;
pub mod run;
pub mod security;
pub mod task;
pub mod team;
pub mod workspace;
pub mod world;

pub use agent::{AdapterStatus, AgentConfig, AgentDescriptor, Capability};
pub use config::{
    AgentSettings, BaselineRoutingConfig, CONFIG_FILE, ConfigError, FORGE_DIR, ForgeConfig, Layout,
    RoutingConfig, TeamConfig, WorldModelConfig,
};
pub use events::{EvaluationSubject, Event, EventPayload, EventSink, NullSink, RecordingSink};
pub use experiment::{
    Comparison, ComparisonInput, ComparisonKey, ComparisonRelation, DimensionComparison,
    Experiment, ExperimentEvent, ExperimentEventPayload, ExperimentRecordingSink, ExperimentStatus,
    PairwiseComparison,
};
pub use ids::{
    AgentId, ExperimentId, IdError, RoutingDecisionId, RunId, TaskId, TeamArtifactId,
    TeamExecutionId, TeamNodeId, WorldModelFactId, WorldModelSnapshotId,
};
pub use integrity::{
    CompiledProtection, EvaluationIntegrity, IntegrityStatus, ProtectionError, ProtectionPolicy,
};
pub use patch::{
    CandidatePatch, ChangeKind, DeltaEntry, ExcludedEntry, ExclusionReason, PatchPolicy,
    PatchWarning, WarningKind, WorkspaceDelta,
};
pub use result::{
    BenchmarkMetrics, CheckResult, Dimension, Direction, Evaluation, EvaluationSummary,
    EvaluatorExecutionStatus, EvaluatorKind, EvaluatorSummary, Metric, MetricName, MetricNameError,
    MetricValue, Score, ScoreError, Verdict,
};
pub use routing::{
    AgentEvidenceCount, AgentRoutingScore, CandidateAgent, CandidateAgentSet, DecisionSource,
    EvidenceExclusionCount, EvidenceExclusionReason, EvidencePolicyVersion,
    ExcludedRoutingEvidence, ExplorationPolicy, InfluentialRoutingRun, MinimumRoutingEvidence,
    RoutingContractError, RoutingDecision, RoutingDecisionKind, RoutingDecisionRecord,
    RoutingEvent, RoutingEventPayload, RoutingEvidence, RoutingEvidencePolicy,
    RoutingEvidenceRecord, RoutingEvidenceSnapshot, RoutingEvidenceSummary, RoutingExplanation,
    RoutingExplanationReason, RoutingFeatures, RoutingPolicyConfiguration, RoutingReadiness,
    RoutingReadinessReason, RoutingRequest, RoutingSuggestedAction, RoutingTarget,
    UnavailableRoutingFeature, UnavailableRoutingFeatureKind, UnresolvedRoutingTarget,
};
pub use run::{
    AgentExecution, AgentExecutionStatus, AgentRun, ExecutionProvenance, PatchSummary,
    RunArtifacts, RunError, RunOutcome, RunStatus, SelectionSource, Usage,
};
pub use security::{AgentSecurity, HostContainment, SecurityPosture, WorkspaceIsolation};
pub use task::{
    BenchmarkSpec, CommandSpec, EngineeringTask, EvaluationSpec, NamedCommand, TaskClassification,
    TaskError, TaskMetadata, TaskRevision, TaskRevisionError, TaskRevisionId,
};
pub use team::{
    FinalCandidate, NodeTaskLineage, PlanProvenance, PlanSourceKind, ResolvedTeamAssignment,
    ReviewDecision, ReviewFinding, ReviewResult, SingleAgentBaseline, TEAM_PLAN_VERSION,
    TeamArtifact, TeamArtifactContent, TeamArtifactKind, TeamAssignmentStrategy,
    TeamBaselineComparison, TeamComparisonRelation, TeamEdge, TeamEvent, TeamEventPayload,
    TeamExecution, TeamExecutionType, TeamFailureKind, TeamFinalEvaluation, TeamNodeExecution,
    TeamNodeStatus, TeamOutcome, TeamPlan, TeamPlanError, TeamPlanNode, TeamResourceSummary,
    TeamStatus, ValidatedTeamPlan,
};
pub use workspace::{Workspace, WorkspaceKind};
pub use world::*;
