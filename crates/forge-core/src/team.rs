//! Provider-agnostic multi-agent execution contracts.
//!
//! A team coordinates ordinary Forge runs. It does not duplicate their full
//! evidence or invent organizational personas; plans contain task semantics,
//! dependency edges, assignments, artifacts, and reviews.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::agent::{AgentConfig, Capability};
use crate::events::EventPayload;
use crate::ids::{
    AgentId, RoutingDecisionId, RunId, TaskId, TeamArtifactId, TeamExecutionId, TeamNodeId,
};
use crate::integrity::EvaluationIntegrity;
use crate::result::{Evaluation, Verdict};
use crate::run::{ExecutionProvenance, PatchSummary, RunOutcome, SelectionSource};
use crate::task::{EngineeringTask, TaskRevisionId};

pub const TEAM_PLAN_VERSION: &str = "team-plan-v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanSourceKind {
    Explicit,
    Generated,
    Imported,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanProvenance {
    pub source: PlanSourceKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub planner_config: Option<AgentConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub planner_run_id: Option<RunId>,
}

impl PlanProvenance {
    pub fn explicit() -> Self {
        Self {
            source: PlanSourceKind::Explicit,
            planner_config: None,
            planner_run_id: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TeamExecutionType {
    Analysis,
    Implementation,
    Review,
    Integration,
    Verification,
}

impl TeamExecutionType {
    pub fn requires_agent(self) -> bool {
        matches!(self, Self::Analysis | Self::Implementation | Self::Review)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "strategy", rename_all = "snake_case")]
pub enum TeamAssignmentStrategy {
    Explicit { agent: AgentId },
    Auto,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TeamArtifactKind {
    Analysis,
    StructuredFindings,
    CandidatePatch,
    CandidateCommit,
    Evaluation,
    Review,
    Metrics,
    FileReference,
    Integration,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TeamPlanNode {
    #[serde(alias = "id")]
    pub node_id: TeamNodeId,
    pub objective: String,
    pub execution: TeamExecutionType,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<TeamNodeId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<TeamArtifactKind>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outputs: Vec<TeamArtifactKind>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub constraints: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[serde(alias = "capabilities")]
    pub required_capabilities: Vec<Capability>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignment: Option<TeamAssignmentStrategy>,
    #[serde(default = "default_required")]
    pub required: bool,
}

fn default_required() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeamEdge {
    pub from: TeamNodeId,
    pub to: TeamNodeId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeamPlan {
    pub plan_version: String,
    pub root_objective: String,
    pub nodes: Vec<TeamPlanNode>,
}

impl TeamPlan {
    pub fn new(root_objective: impl Into<String>, nodes: Vec<TeamPlanNode>) -> Self {
        Self {
            plan_version: TEAM_PLAN_VERSION.into(),
            root_objective: root_objective.into(),
            nodes,
        }
    }

    pub fn validate(mut self) -> Result<ValidatedTeamPlan, TeamPlanError> {
        if self.plan_version != TEAM_PLAN_VERSION {
            return Err(TeamPlanError::UnsupportedVersion(self.plan_version));
        }
        if self.root_objective.trim().is_empty() {
            return Err(TeamPlanError::EmptyRootObjective);
        }
        if self.nodes.is_empty() {
            return Err(TeamPlanError::NoNodes);
        }
        self.nodes
            .sort_by(|left, right| left.node_id.cmp(&right.node_id));
        let mut ids = BTreeSet::new();
        for node in &mut self.nodes {
            if !ids.insert(node.node_id.clone()) {
                return Err(TeamPlanError::DuplicateNode(node.node_id.clone()));
            }
            if node.objective.trim().is_empty() {
                return Err(TeamPlanError::EmptyNodeObjective(node.node_id.clone()));
            }
            if node.execution.requires_agent() && node.assignment.is_none() {
                return Err(TeamPlanError::MissingAssignment(node.node_id.clone()));
            }
            if !node.execution.requires_agent() && node.assignment.is_some() {
                return Err(TeamPlanError::UnexpectedAssignment(node.node_id.clone()));
            }
            for output in &node.outputs {
                let supported = match node.execution {
                    TeamExecutionType::Analysis => matches!(
                        output,
                        TeamArtifactKind::Analysis | TeamArtifactKind::StructuredFindings
                    ),
                    TeamExecutionType::Implementation => matches!(
                        output,
                        TeamArtifactKind::CandidatePatch
                            | TeamArtifactKind::CandidateCommit
                            | TeamArtifactKind::Evaluation
                            | TeamArtifactKind::Metrics
                    ),
                    TeamExecutionType::Review => *output == TeamArtifactKind::Review,
                    TeamExecutionType::Integration => *output == TeamArtifactKind::Integration,
                    TeamExecutionType::Verification => false,
                };
                if !supported {
                    return Err(TeamPlanError::UnsupportedOutput {
                        node: node.node_id.clone(),
                        execution: node.execution,
                        output: *output,
                    });
                }
            }
            node.depends_on.sort();
            let original_len = node.depends_on.len();
            node.depends_on.dedup();
            if node.depends_on.len() != original_len {
                return Err(TeamPlanError::DuplicateDependency(node.node_id.clone()));
            }
            node.inputs.sort();
            node.inputs.dedup();
            node.outputs.sort();
            node.outputs.dedup();
            node.required_capabilities.sort();
            node.required_capabilities.dedup();
        }
        for node in &self.nodes {
            for dependency in &node.depends_on {
                if !ids.contains(dependency) {
                    return Err(TeamPlanError::MissingDependency {
                        node: node.node_id.clone(),
                        dependency: dependency.clone(),
                    });
                }
                if dependency == &node.node_id {
                    return Err(TeamPlanError::Cycle(vec![node.node_id.clone()]));
                }
            }
            if !node.inputs.is_empty() && node.depends_on.is_empty() {
                return Err(TeamPlanError::InputsWithoutDependencies(
                    node.node_id.clone(),
                ));
            }
            for input in &node.inputs {
                let produced = node.depends_on.iter().any(|dependency| {
                    self.nodes
                        .iter()
                        .find(|candidate| &candidate.node_id == dependency)
                        .is_some_and(|candidate| candidate.outputs.contains(input))
                });
                if !produced {
                    return Err(TeamPlanError::MissingInputProducer {
                        node: node.node_id.clone(),
                        input: *input,
                    });
                }
            }
        }
        let order = topological_order(&self.nodes)?;
        let edges = self
            .nodes
            .iter()
            .flat_map(|node| {
                node.depends_on.iter().map(|dependency| TeamEdge {
                    from: dependency.clone(),
                    to: node.node_id.clone(),
                })
            })
            .collect::<Vec<_>>();
        let fingerprint = fingerprint(&self)?;
        Ok(ValidatedTeamPlan {
            plan: self,
            edges,
            topological_order: order,
            fingerprint,
        })
    }
}

fn topological_order(nodes: &[TeamPlanNode]) -> Result<Vec<TeamNodeId>, TeamPlanError> {
    let mut indegree = nodes
        .iter()
        .map(|node| (node.node_id.clone(), node.depends_on.len()))
        .collect::<BTreeMap<_, _>>();
    let mut outgoing = BTreeMap::<TeamNodeId, Vec<TeamNodeId>>::new();
    for node in nodes {
        for dependency in &node.depends_on {
            outgoing
                .entry(dependency.clone())
                .or_default()
                .push(node.node_id.clone());
        }
    }
    for dependents in outgoing.values_mut() {
        dependents.sort();
    }
    let mut ready = indegree
        .iter()
        .filter(|(_, count)| **count == 0)
        .map(|(id, _)| id.clone())
        .collect::<BTreeSet<_>>();
    let mut order = Vec::with_capacity(nodes.len());
    while let Some(next) = ready.pop_first() {
        order.push(next.clone());
        for dependent in outgoing.get(&next).into_iter().flatten() {
            let count = indegree
                .get_mut(dependent)
                .expect("validated dependent exists");
            *count -= 1;
            if *count == 0 {
                ready.insert(dependent.clone());
            }
        }
    }
    if order.len() != nodes.len() {
        let cycle = indegree
            .into_iter()
            .filter(|(_, count)| *count > 0)
            .map(|(id, _)| id)
            .collect();
        return Err(TeamPlanError::Cycle(cycle));
    }
    Ok(order)
}

fn fingerprint(plan: &TeamPlan) -> Result<String, TeamPlanError> {
    let bytes = serde_json::to_vec(plan)?;
    Ok(Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidatedTeamPlan {
    pub plan: TeamPlan,
    pub edges: Vec<TeamEdge>,
    pub topological_order: Vec<TeamNodeId>,
    pub fingerprint: String,
}

impl ValidatedTeamPlan {
    pub fn node(&self, id: &TeamNodeId) -> Option<&TeamPlanNode> {
        self.plan.nodes.iter().find(|node| &node.node_id == id)
    }

    pub fn terminal_nodes(&self) -> Vec<&TeamPlanNode> {
        let parents = self
            .edges
            .iter()
            .map(|edge| &edge.from)
            .collect::<BTreeSet<_>>();
        self.plan
            .nodes
            .iter()
            .filter(|node| !parents.contains(&node.node_id))
            .collect()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum TeamPlanError {
    #[error("team plan version `{0}` is not supported")]
    UnsupportedVersion(String),
    #[error("team plan root objective is empty")]
    EmptyRootObjective,
    #[error("team plan has no nodes")]
    NoNodes,
    #[error("team plan contains duplicate node `{0}`")]
    DuplicateNode(TeamNodeId),
    #[error("team node `{0}` has an empty objective")]
    EmptyNodeObjective(TeamNodeId),
    #[error("team node `{0}` requires an agent assignment")]
    MissingAssignment(TeamNodeId),
    #[error("deterministic team node `{0}` must not have an agent assignment")]
    UnexpectedAssignment(TeamNodeId),
    #[error("team node `{node}` of type {execution:?} cannot publish {output:?}")]
    UnsupportedOutput {
        node: TeamNodeId,
        execution: TeamExecutionType,
        output: TeamArtifactKind,
    },
    #[error("team node `{0}` repeats a dependency edge")]
    DuplicateDependency(TeamNodeId),
    #[error("team node `{0}` declares inputs but has no dependencies")]
    InputsWithoutDependencies(TeamNodeId),
    #[error("team node `{node}` requests input {input:?}, but no direct dependency declares it")]
    MissingInputProducer {
        node: TeamNodeId,
        input: TeamArtifactKind,
    },
    #[error("team node `{node}` depends on missing node `{dependency}`")]
    MissingDependency {
        node: TeamNodeId,
        dependency: TeamNodeId,
    },
    #[error("team plan contains a dependency cycle involving {0:?}")]
    Cycle(Vec<TeamNodeId>),
    #[error("team plan could not be serialized for fingerprinting")]
    Serialize(#[from] serde_json::Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TeamStatus {
    Planned,
    Running,
    Completed,
    CompletedWithFailures,
    Blocked,
    InfrastructureFailed,
    Cancelled,
}

impl TeamStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Planned => "planned",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::CompletedWithFailures => "completed_with_failures",
            Self::Blocked => "blocked",
            Self::InfrastructureFailed => "infrastructure_failed",
            Self::Cancelled => "cancelled",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TeamNodeStatus {
    Pending,
    Ready,
    Running,
    Succeeded,
    Failed,
    Blocked,
    AssignmentBlocked,
    Cancelled,
}

impl TeamNodeStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Ready => "ready",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Blocked => "blocked",
            Self::AssignmentBlocked => "assignment_blocked",
            Self::Cancelled => "cancelled",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TeamFailureKind {
    Engineering,
    AgentProcess,
    Infrastructure,
    Evaluation,
    Assignment,
    Integration,
    Review,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeTaskLineage {
    pub root_task_id: TaskId,
    pub root_task_revision_id: TaskRevisionId,
    pub team_execution_id: TeamExecutionId,
    pub node_id: TeamNodeId,
    pub node_task_id: TaskId,
    pub input_commit: String,
    pub input_artifact_ids: Vec<TeamArtifactId>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResolvedTeamAssignment {
    pub agent: AgentConfig,
    pub selection_source: SelectionSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routing_decision_id: Option<RoutingDecisionId>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TeamNodeExecution {
    pub node_id: TeamNodeId,
    pub status: TeamNodeStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<EngineeringTask>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lineage: Option<NodeTaskLineage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignment: Option<ResolvedTeamAssignment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routing_decision_id: Option<RoutingDecisionId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub run_ids: Vec<RunId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub input_artifact_ids: Vec<TeamArtifactId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub output_artifact_ids: Vec<TeamArtifactId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_commit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_commit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review: Option<ReviewResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_kind: Option<TeamFailureKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "storage", rename_all = "snake_case")]
pub enum TeamArtifactContent {
    InlineJson { value: serde_json::Value },
    Text { value: String },
    FileReference { path: String, sha256: String },
    Commit { commit: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TeamArtifact {
    pub artifact_id: TeamArtifactId,
    pub team_execution_id: TeamExecutionId,
    pub producer_node_id: TeamNodeId,
    pub kind: TeamArtifactKind,
    pub content: TeamArtifactContent,
    pub created_at: DateTime<Utc>,
    pub content_sha256: String,
}

impl TeamArtifact {
    pub fn new(
        artifact_id: TeamArtifactId,
        team_execution_id: TeamExecutionId,
        producer_node_id: TeamNodeId,
        kind: TeamArtifactKind,
        content: TeamArtifactContent,
        created_at: DateTime<Utc>,
    ) -> Result<Self, serde_json::Error> {
        let bytes = serde_json::to_vec(&content)?;
        let content_sha256 = Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        Ok(Self {
            artifact_id,
            team_execution_id,
            producer_node_id,
            kind,
            content,
            created_at,
            content_sha256,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewDecision {
    Approve,
    RequestChanges,
    Inconclusive,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewFinding {
    pub category: String,
    pub severity: String,
    pub explanation: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewResult {
    pub decision: ReviewDecision,
    #[serde(default)]
    pub findings: Vec<ReviewFinding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prose: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FinalCandidate {
    pub base_commit: String,
    pub integrated_commit: String,
    pub contributing_nodes: Vec<TeamNodeId>,
    pub contributing_runs: Vec<RunId>,
    pub patch: PatchSummary,
    pub lineage: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TeamFinalEvaluation {
    pub integrity: EvaluationIntegrity,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evaluation: Option<Evaluation>,
    pub verdict: Verdict,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TeamResourceSummary {
    pub agent_run_count: u64,
    pub failed_attempt_count: u64,
    pub warning_count: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_run_duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub known_cost_usd: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TeamOutcome {
    Passed,
    Failed,
    Inconclusive,
    Blocked,
    Errored,
}

impl TeamOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Passed => "pass",
            Self::Failed => "fail",
            Self::Inconclusive => "inconclusive",
            Self::Blocked => "blocked",
            Self::Errored => "error",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TeamComparisonRelation {
    TeamBetter,
    BaselineBetter,
    Tie,
    Incomparable,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TeamBaselineComparison {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline_run_id: Option<RunId>,
    pub correctness: TeamComparisonRelation,
    pub integrity: TeamComparisonRelation,
    pub runtime: TeamComparisonRelation,
    pub tokens: TeamComparisonRelation,
    pub known_cost: TeamComparisonRelation,
    pub patch_size: TeamComparisonRelation,
    pub warnings: TeamComparisonRelation,
    #[serde(default)]
    pub comparable_benchmarks: BTreeMap<String, TeamComparisonRelation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl TeamBaselineComparison {
    pub fn unavailable() -> Self {
        Self {
            baseline_run_id: None,
            correctness: TeamComparisonRelation::Unavailable,
            integrity: TeamComparisonRelation::Unavailable,
            runtime: TeamComparisonRelation::Unavailable,
            tokens: TeamComparisonRelation::Unavailable,
            known_cost: TeamComparisonRelation::Unavailable,
            patch_size: TeamComparisonRelation::Unavailable,
            warnings: TeamComparisonRelation::Unavailable,
            comparable_benchmarks: BTreeMap::new(),
            note: Some("single-agent baseline unavailable".into()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TeamExecution {
    pub team_execution_id: TeamExecutionId,
    pub root_task_id: TaskId,
    pub root_task_revision_id: TaskRevisionId,
    pub base_commit: String,
    pub plan: ValidatedTeamPlan,
    pub plan_provenance: PlanProvenance,
    pub execution_provenance: ExecutionProvenance,
    pub status: TeamStatus,
    pub nodes: Vec<TeamNodeExecution>,
    #[serde(default)]
    pub artifacts: Vec<TeamArtifact>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_candidate: Option<FinalCandidate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_evaluation: Option<TeamFinalEvaluation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<TeamOutcome>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline_comparison: Option<TeamBaselineComparison>,
    #[serde(default)]
    pub resources: TeamResourceSummary,
    pub created_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_reason: Option<String>,
}

impl TeamExecution {
    pub fn new(
        team_execution_id: TeamExecutionId,
        root_task_id: TaskId,
        root_task_revision_id: TaskRevisionId,
        base_commit: impl Into<String>,
        plan: ValidatedTeamPlan,
        plan_provenance: PlanProvenance,
    ) -> Self {
        let nodes = plan
            .plan
            .nodes
            .iter()
            .map(|node| TeamNodeExecution {
                node_id: node.node_id.clone(),
                status: TeamNodeStatus::Pending,
                task: None,
                lineage: None,
                assignment: None,
                routing_decision_id: None,
                run_ids: Vec::new(),
                input_artifact_ids: Vec::new(),
                output_artifact_ids: Vec::new(),
                input_commit: None,
                output_commit: None,
                review: None,
                failure_kind: None,
                failure_reason: None,
                started_at: None,
                finished_at: None,
            })
            .collect();
        Self {
            team_execution_id,
            root_task_id,
            root_task_revision_id,
            base_commit: base_commit.into(),
            plan,
            plan_provenance,
            execution_provenance: ExecutionProvenance::Unknown,
            status: TeamStatus::Planned,
            nodes,
            artifacts: Vec::new(),
            final_candidate: None,
            final_evaluation: None,
            outcome: None,
            baseline_comparison: None,
            resources: TeamResourceSummary::default(),
            created_at: Utc::now(),
            completed_at: None,
            failure_reason: None,
        }
    }

    pub fn node(&self, id: &TeamNodeId) -> Option<&TeamNodeExecution> {
        self.nodes.iter().find(|node| &node.node_id == id)
    }

    pub fn node_mut(&mut self, id: &TeamNodeId) -> Option<&mut TeamNodeExecution> {
        self.nodes.iter_mut().find(|node| &node.node_id == id)
    }

    pub fn run_ids(&self) -> Vec<RunId> {
        self.nodes
            .iter()
            .flat_map(|node| node.run_ids.iter().cloned())
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TeamEvent {
    pub team_execution_id: TeamExecutionId,
    pub seq: u64,
    pub timestamp: DateTime<Utc>,
    pub payload: TeamEventPayload,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TeamEventPayload {
    TeamStarted {
        task_id: TaskId,
        base_commit: String,
    },
    TeamPlanResolved {
        plan_fingerprint: String,
        node_count: u64,
    },
    NodeReady {
        node_id: TeamNodeId,
    },
    NodeStarted {
        node_id: TeamNodeId,
        input_commit: String,
    },
    NodeCompleted {
        node_id: TeamNodeId,
        run_id: Option<RunId>,
    },
    NodeFailed {
        node_id: TeamNodeId,
        reason: String,
    },
    NodeBlocked {
        node_id: TeamNodeId,
        reason: String,
    },
    ArtifactPublished {
        artifact_id: TeamArtifactId,
        node_id: TeamNodeId,
    },
    HandoffCompleted {
        from: TeamNodeId,
        to: TeamNodeId,
        artifact_count: u64,
    },
    ReviewCompleted {
        node_id: TeamNodeId,
        decision: ReviewDecision,
    },
    IntegrationStarted {
        candidate_count: u64,
    },
    IntegrationCompleted {
        commit: String,
    },
    FinalEvaluationStarted {
        commit: String,
    },
    FinalEvaluationCompleted {
        verdict: Verdict,
    },
    EvaluationLifecycle {
        event: EventPayload,
    },
    TeamCompleted {
        outcome: TeamOutcome,
    },
    TeamFailed {
        reason: String,
    },
}

/// Compact comparable single-agent evidence returned by the ledger.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SingleAgentBaseline {
    pub run_id: RunId,
    pub execution_provenance: ExecutionProvenance,
    pub outcome: RunOutcome,
    pub integrity: Option<EvaluationIntegrity>,
    pub runtime_ms: Option<u64>,
    pub total_tokens: Option<u64>,
    pub known_cost_usd: Option<f64>,
    pub patch_lines: Option<u64>,
    pub warning_count: u64,
    pub evaluation: Option<Evaluation>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: &str, depends_on: &[&str]) -> TeamPlanNode {
        TeamPlanNode {
            node_id: TeamNodeId::new(id).unwrap(),
            objective: format!("execute {id}"),
            execution: TeamExecutionType::Implementation,
            depends_on: depends_on
                .iter()
                .map(|dependency| TeamNodeId::new(*dependency).unwrap())
                .collect(),
            inputs: Vec::new(),
            outputs: vec![TeamArtifactKind::CandidateCommit],
            constraints: Vec::new(),
            required_capabilities: vec![Capability::EditFiles],
            assignment: Some(TeamAssignmentStrategy::Explicit {
                agent: AgentId::new("fake").unwrap(),
            }),
            required: true,
        }
    }

    #[test]
    fn linear_and_branching_dags_have_deterministic_order() {
        let linear = TeamPlan::new(
            "root",
            vec![node("c", &["b"]), node("a", &[]), node("b", &["a"])],
        )
        .validate()
        .unwrap();
        assert_eq!(
            linear
                .topological_order
                .iter()
                .map(TeamNodeId::as_str)
                .collect::<Vec<_>>(),
            vec!["a", "b", "c"]
        );

        let branching = TeamPlan::new(
            "root",
            vec![
                node("d", &["b", "c"]),
                node("c", &["a"]),
                node("b", &["a"]),
                node("a", &[]),
                node("isolated", &[]),
            ],
        )
        .validate()
        .unwrap();
        assert_eq!(
            branching
                .topological_order
                .iter()
                .map(TeamNodeId::as_str)
                .collect::<Vec<_>>(),
            vec!["a", "b", "c", "d", "isolated"]
        );
    }

    #[test]
    fn invalid_dags_fail_before_execution() {
        assert!(matches!(
            TeamPlan::new("root", vec![node("a", &[]), node("a", &[])])
                .validate()
                .unwrap_err(),
            TeamPlanError::DuplicateNode(_)
        ));
        assert!(matches!(
            TeamPlan::new("root", vec![node("a", &["missing"])])
                .validate()
                .unwrap_err(),
            TeamPlanError::MissingDependency { .. }
        ));
        assert!(matches!(
            TeamPlan::new("root", vec![node("a", &["b"]), node("b", &["a"])])
                .validate()
                .unwrap_err(),
            TeamPlanError::Cycle(_)
        ));
        assert!(matches!(
            TeamPlan::new("root", vec![node("a", &["b", "b"]), node("b", &[])])
                .validate()
                .unwrap_err(),
            TeamPlanError::DuplicateDependency(_)
        ));
        let mut input_without_dependency = node("a", &[]);
        input_without_dependency.inputs = vec![TeamArtifactKind::Review];
        assert!(matches!(
            TeamPlan::new("root", vec![input_without_dependency])
                .validate()
                .unwrap_err(),
            TeamPlanError::InputsWithoutDependencies(_)
        ));
        let mut missing_producer = node("b", &["a"]);
        missing_producer.inputs = vec![TeamArtifactKind::Review];
        assert!(matches!(
            TeamPlan::new("root", vec![node("a", &[]), missing_producer])
                .validate()
                .unwrap_err(),
            TeamPlanError::MissingInputProducer { .. }
        ));
    }

    #[test]
    fn plan_fingerprint_is_stable_across_input_order_and_changes_with_semantics() {
        let first = TeamPlan::new("root", vec![node("b", &["a"]), node("a", &[])])
            .validate()
            .unwrap();
        let second = TeamPlan::new("root", vec![node("a", &[]), node("b", &["a"])])
            .validate()
            .unwrap();
        assert_eq!(first.fingerprint, second.fingerprint);

        let mut changed = node("b", &["a"]);
        changed.objective = "different".into();
        let changed = TeamPlan::new("root", vec![node("a", &[]), changed])
            .validate()
            .unwrap();
        assert_ne!(first.fingerprint, changed.fingerprint);
    }
}
