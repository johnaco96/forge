use std::collections::{BTreeSet, HashSet};
use std::sync::Mutex;
use std::time::Duration;

use chrono::Utc;
use forge_agent::AgentRegistry;
use forge_core::agent::{AdapterStatus, Capability};
use forge_core::config::{ForgeConfig, Layout};
use forge_core::events::{EvaluationSubject, EventPayload, EventSink};
use forge_core::ids::{AgentId, TaskId, TeamNodeId};
use forge_core::patch::PatchPolicy;
use forge_core::result::{Direction, Verdict};
use forge_core::routing::{RoutingDecision, RoutingEvidencePolicy, RoutingRequest};
use forge_core::run::{
    AgentExecutionStatus, ExecutionProvenance, PatchSummary, RunOutcome, SelectionSource,
};
use forge_core::task::{EngineeringTask, EvaluationSpec, TaskRevision};
use forge_core::team::{
    FinalCandidate, NodeTaskLineage, PlanProvenance, ResolvedTeamAssignment, ReviewDecision,
    ReviewResult, SingleAgentBaseline, TeamArtifact, TeamArtifactContent, TeamArtifactKind,
    TeamBaselineComparison, TeamComparisonRelation, TeamEvent, TeamEventPayload, TeamExecution,
    TeamExecutionType, TeamFailureKind, TeamFinalEvaluation, TeamNodeStatus, TeamOutcome, TeamPlan,
    TeamPlanNode, TeamResourceSummary, TeamStatus,
};
use forge_core::workspace::{Workspace, WorkspaceKind};
use forge_eval::{EvalContext, EvaluationEngine, EvaluationPlan};
use forge_executor::{EnvPolicy, ProcessRunner};
use forge_git::{
    Repository, WorktreeManager, cached_patch, stage_candidate_patch, workspace_delta,
};
use forge_router::{
    CandidateAvailability, CandidateRequest, CandidateRequirements, ROUTER_VERSION,
    RoutingContract, resolve_candidates,
};
use forge_runner::{RunReport, RunRequest, Runner};
use forge_store::Store;

use crate::{TeamError, TeamResult};

#[derive(Debug, Clone)]
pub struct TeamRequest {
    pub task: EngineeringTask,
    pub plan: TeamPlan,
    pub plan_provenance: PlanProvenance,
    pub base_rev: Option<String>,
    pub timeout: Option<Duration>,
    pub keep_workspace: Option<bool>,
}

impl TeamRequest {
    pub fn explicit(task: EngineeringTask, plan: TeamPlan) -> Self {
        Self {
            task,
            plan,
            plan_provenance: PlanProvenance::explicit(),
            base_rev: None,
            timeout: None,
            keep_workspace: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct TeamReport {
    pub team: TeamExecution,
    pub events_recorded: usize,
    pub execution_strategy: &'static str,
}

#[derive(Debug, Default)]
struct EvaluationLifecycleSink {
    events: Mutex<Vec<EventPayload>>,
}

impl EvaluationLifecycleSink {
    fn drain(&self) -> Vec<EventPayload> {
        std::mem::take(&mut *self.events.lock().expect("event buffer poisoned"))
    }
}

impl EventSink for EvaluationLifecycleSink {
    fn emit(&self, payload: EventPayload) {
        if payload.evaluation_subject().is_some() {
            self.events
                .lock()
                .expect("event buffer poisoned")
                .push(payload);
        }
    }
}

#[derive(Debug, Clone)]
pub struct TeamCoordinator {
    repository: Repository,
    config: ForgeConfig,
    store: Store,
    registry: AgentRegistry,
}

impl TeamCoordinator {
    pub fn new(repository: Repository, config: ForgeConfig, store: Store) -> Self {
        Self {
            repository,
            config,
            store,
            registry: AgentRegistry::builtin(),
        }
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    pub async fn execute(&self, request: TeamRequest) -> TeamResult<TeamReport> {
        request.task.validate()?;
        if request.task.repository != self.config.repository.name {
            return Err(forge_runner::RunnerError::WrongRepository {
                task_repository: request.task.repository.clone(),
                configured: self.config.repository.name.clone(),
            }
            .into());
        }
        if request.plan.root_objective != request.task.objective {
            return Err(TeamError::RootObjectiveMismatch);
        }
        let plan = request.plan.clone().validate()?;
        self.preflight_plan(&request.task, &plan).await?;
        let root_revision = TaskRevision::snapshot(request.task.clone())?;
        let persisted_revision = self.store.upsert_task(&request.task).await?;
        if persisted_revision != *root_revision.revision_id() {
            return Err(TeamError::PlanSerialization(
                "root task revision changed while starting the team".into(),
            ));
        }
        let runner = self.runner();
        let base = runner.resolve_base(request.base_rev.as_deref())?;
        let team_id = self.store.next_team_execution_id().await?;
        let mut team = TeamExecution::new(
            team_id,
            request.task.task_id.clone(),
            root_revision.revision_id().clone(),
            base.as_str(),
            plan,
            request.plan_provenance.clone(),
        );
        let mut events = Vec::new();
        emit(
            &team,
            &mut events,
            TeamEventPayload::TeamStarted {
                task_id: request.task.task_id.clone(),
                base_commit: base.as_str().into(),
            },
        );
        emit(
            &team,
            &mut events,
            TeamEventPayload::TeamPlanResolved {
                plan_fingerprint: team.plan.fingerprint.clone(),
                node_count: team.nodes.len() as u64,
            },
        );
        team.status = TeamStatus::Running;
        self.persist(&team, &events).await?;

        for node_id in team.plan.topological_order.clone() {
            if self.config.team.stop_on_required_node_failure && required_node_failed(&team) {
                block_node(
                    &mut team,
                    &node_id,
                    "team policy stopped after a required node failure",
                    &mut events,
                )?;
                continue;
            }
            let definition = team
                .plan
                .node(&node_id)
                .cloned()
                .ok_or_else(|| TeamError::MissingNode(node_id.clone()))?;
            let failed_dependency = dependency_executions(&team, &definition)?
                .iter()
                .find(|dependency| dependency.status != TeamNodeStatus::Succeeded)
                .map(|dependency| dependency.node_id.clone());
            if let Some(failed_node_id) = failed_dependency {
                block_node(
                    &mut team,
                    &node_id,
                    &format!("dependency `{failed_node_id}` did not succeed"),
                    &mut events,
                )?;
                self.persist(&team, &events).await?;
                continue;
            }

            let input_artifacts = direct_input_artifacts(&team, &definition);
            let delivered_kinds = input_artifacts
                .iter()
                .map(|artifact| artifact.kind)
                .collect::<BTreeSet<_>>();
            if let Some(missing) = definition
                .inputs
                .iter()
                .find(|kind| !delivered_kinds.contains(kind))
            {
                fail_node(
                    &mut team,
                    &node_id,
                    TeamFailureKind::Integration,
                    format!("declared handoff artifact {missing:?} was not published"),
                    &mut events,
                )?;
                self.persist(&team, &events).await?;
                continue;
            }
            let input_commit = match resolve_input_commit(&team, &definition) {
                Ok(commit) => commit,
                Err(error @ TeamError::IntegrationConflict { .. }) => {
                    fail_node(
                        &mut team,
                        &node_id,
                        TeamFailureKind::Integration,
                        error.to_string(),
                        &mut events,
                    )?;
                    self.persist(&team, &events).await?;
                    continue;
                }
                Err(error) => return Err(error),
            };
            {
                let node = team
                    .node_mut(&node_id)
                    .ok_or_else(|| TeamError::MissingNode(node_id.clone()))?;
                node.status = TeamNodeStatus::Ready;
                node.input_commit = Some(input_commit.clone());
                node.input_artifact_ids = input_artifacts
                    .iter()
                    .map(|artifact| artifact.artifact_id.clone())
                    .collect();
            }
            emit(
                &team,
                &mut events,
                TeamEventPayload::NodeReady {
                    node_id: node_id.clone(),
                },
            );
            if !definition.depends_on.is_empty() {
                for dependency in &definition.depends_on {
                    emit(
                        &team,
                        &mut events,
                        TeamEventPayload::HandoffCompleted {
                            from: dependency.clone(),
                            to: node_id.clone(),
                            artifact_count: input_artifacts
                                .iter()
                                .filter(|artifact| &artifact.producer_node_id == dependency)
                                .count() as u64,
                        },
                    );
                }
            }
            {
                let node = team
                    .node_mut(&node_id)
                    .ok_or_else(|| TeamError::MissingNode(node_id.clone()))?;
                node.status = TeamNodeStatus::Running;
                node.started_at = Some(Utc::now());
            }
            emit(
                &team,
                &mut events,
                TeamEventPayload::NodeStarted {
                    node_id: node_id.clone(),
                    input_commit: input_commit.clone(),
                },
            );
            self.persist(&team, &events).await?;

            let result = if definition.execution.requires_agent() {
                self.execute_agent_node(
                    &request,
                    &root_revision,
                    &definition,
                    &input_commit,
                    &input_artifacts,
                    &mut team,
                    &mut events,
                )
                .await
            } else {
                self.execute_deterministic_node(&definition, &input_commit, &mut team, &mut events)
                    .await
            };
            if let Err(error) = result {
                match error {
                    TeamError::AssignmentBlocked {
                        reason,
                        decision_id,
                        ..
                    } => {
                        assignment_block_node(
                            &mut team,
                            &node_id,
                            &reason,
                            decision_id,
                            &mut events,
                        )?;
                    }
                    error => {
                        fail_node(
                            &mut team,
                            &node_id,
                            TeamFailureKind::Infrastructure,
                            error.to_string(),
                            &mut events,
                        )?;
                    }
                }
            }
            self.persist(&team, &events).await?;
        }

        self.finish_team(&request.task, &mut team, &mut events)
            .await?;
        self.persist(&team, &events).await?;
        Ok(TeamReport {
            team,
            events_recorded: events.len(),
            execution_strategy: "sequential_topological",
        })
    }

    fn runner(&self) -> Runner {
        Runner::new(
            self.repository.clone(),
            self.config.clone(),
            self.store.clone(),
        )
    }

    async fn preflight_plan(
        &self,
        task: &EngineeringTask,
        plan: &forge_core::ValidatedTeamPlan,
    ) -> TeamResult<()> {
        let runner = self.runner();
        for node in &plan.plan.nodes {
            let Some(assignment) = &node.assignment else {
                continue;
            };
            let forge_core::TeamAssignmentStrategy::Explicit { agent } = assignment else {
                self.auto_candidates(task, None, &node.required_capabilities)
                    .map_err(|_| TeamError::AssignmentUnavailable(node.node_id.clone()))?;
                continue;
            };
            let descriptor = self
                .registry
                .get(agent.as_str())
                .ok_or_else(|| TeamError::AssignmentUnavailable(node.node_id.clone()))?;
            if descriptor.adapter_status != AdapterStatus::Implemented
                || node
                    .required_capabilities
                    .iter()
                    .any(|capability| !descriptor.capabilities.contains(capability))
            {
                return Err(TeamError::AssignmentUnavailable(node.node_id.clone()));
            }
            let run_request = RunRequest::new(task.clone(), agent.as_str());
            let config = runner.agent_config(&run_request)?;
            let adapter = self.registry.adapter(agent.as_str(), &config)?;
            if !self
                .registry
                .availability(&adapter.descriptor())
                .is_runnable()
            {
                return Err(TeamError::AssignmentUnavailable(node.node_id.clone()));
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn execute_agent_node(
        &self,
        request: &TeamRequest,
        root_revision: &TaskRevision,
        definition: &TeamPlanNode,
        input_commit: &str,
        input_artifacts: &[TeamArtifact],
        team: &mut TeamExecution,
        events: &mut Vec<TeamEvent>,
    ) -> TeamResult<()> {
        let node_task = derive_node_task(
            &request.task,
            root_revision,
            team,
            definition,
            input_commit,
            input_artifacts,
        )?;
        let root_task_id = team.root_task_id.clone();
        let root_task_revision_id = team.root_task_revision_id.clone();
        let team_execution_id = team.team_execution_id.clone();
        {
            let node = team
                .node_mut(&definition.node_id)
                .ok_or_else(|| TeamError::MissingNode(definition.node_id.clone()))?;
            node.task = Some(node_task.clone());
            node.lineage = Some(NodeTaskLineage {
                root_task_id,
                root_task_revision_id,
                team_execution_id,
                node_id: definition.node_id.clone(),
                node_task_id: node_task.task_id.clone(),
                input_commit: input_commit.into(),
                input_artifact_ids: input_artifacts
                    .iter()
                    .map(|artifact| artifact.artifact_id.clone())
                    .collect(),
            });
        }
        let (assignment, selected_agent) = self
            .resolve_assignment(definition, &node_task, request.timeout)
            .await?;
        {
            let node = team
                .node_mut(&definition.node_id)
                .ok_or_else(|| TeamError::MissingNode(definition.node_id.clone()))?;
            node.assignment = Some(assignment.clone());
            node.routing_decision_id = assignment.routing_decision_id.clone();
        }

        let adapter = self
            .registry
            .adapter(selected_agent.as_str(), &assignment.agent)?;
        let mut run_request = RunRequest::new(node_task, selected_agent.as_str());
        run_request.base_rev = Some(input_commit.into());
        run_request.timeout = request.timeout;
        run_request.keep_workspace = request.keep_workspace;
        run_request.execution_provenance = self
            .config
            .execution_provenance_for(selected_agent.as_str());
        run_request.selection_source = assignment.selection_source.clone();
        let report = match self.runner().execute(run_request, adapter.as_ref()).await {
            Ok(report) => report,
            Err(error) => {
                fail_node(
                    team,
                    &definition.node_id,
                    TeamFailureKind::Infrastructure,
                    error.to_string(),
                    events,
                )?;
                return Ok(());
            }
        };
        if let Some(decision_id) = &assignment.routing_decision_id {
            self.store
                .link_routing_decision_run(decision_id, &report.run.run_id)
                .await?;
        }
        {
            let node = team
                .node_mut(&definition.node_id)
                .ok_or_else(|| TeamError::MissingNode(definition.node_id.clone()))?;
            node.run_ids.push(report.run.run_id.clone());
        }
        self.interpret_agent_result(definition, input_commit, report, team, events)
            .await
    }

    async fn resolve_assignment(
        &self,
        definition: &TeamPlanNode,
        task: &EngineeringTask,
        timeout: Option<Duration>,
    ) -> TeamResult<(ResolvedTeamAssignment, AgentId)> {
        match definition.assignment.as_ref() {
            Some(forge_core::TeamAssignmentStrategy::Explicit { agent }) => {
                let mut request = RunRequest::new(task.clone(), agent.as_str());
                request.timeout = timeout;
                let config = self.runner().agent_config(&request)?;
                Ok((
                    ResolvedTeamAssignment {
                        agent: config,
                        selection_source: SelectionSource::Manual,
                        routing_decision_id: None,
                    },
                    agent.clone(),
                ))
            }
            Some(forge_core::TeamAssignmentStrategy::Auto) => {
                let candidates =
                    match self.auto_candidates(task, timeout, &definition.required_capabilities) {
                        Ok(candidates) => candidates,
                        Err(TeamError::Router(error)) => {
                            return Err(TeamError::AssignmentBlocked {
                                node: definition.node_id.clone(),
                                reason: error.to_string(),
                                decision_id: None,
                            });
                        }
                        Err(error) => return Err(error),
                    };
                let revision = TaskRevision::snapshot(task.clone())?;
                self.store.upsert_task(task).await?;
                let routing = RoutingRequest::new(
                    revision,
                    candidates,
                    RoutingEvidencePolicy::default(),
                    self.config.routing.minimum_evidence(),
                    self.config.routing.exploration_policy,
                    Utc::now(),
                );
                let record = RoutingContract::new(self.store.clone())
                    .route(&routing, &self.config.routing)
                    .await?;
                match &record.decision {
                    RoutingDecision::Selected { agent, .. } => Ok((
                        ResolvedTeamAssignment {
                            agent: agent.config.clone(),
                            selection_source: SelectionSource::Automatic {
                                decision_id: record.decision_id.clone(),
                                router_version: ROUTER_VERSION.into(),
                                evidence_fingerprint: record.evidence_fingerprint.clone(),
                            },
                            routing_decision_id: Some(record.decision_id.clone()),
                        },
                        agent.agent_id.clone(),
                    )),
                    RoutingDecision::InsufficientEvidence { .. } => {
                        Err(TeamError::AssignmentBlocked {
                            node: definition.node_id.clone(),
                            reason: "insufficient routing evidence".into(),
                            decision_id: Some(record.decision_id.clone()),
                        })
                    }
                    RoutingDecision::CompeteRecommended { .. } => {
                        Err(TeamError::AssignmentBlocked {
                            node: definition.node_id.clone(),
                            reason: "routing recommended an explicit competition".into(),
                            decision_id: Some(record.decision_id.clone()),
                        })
                    }
                }
            }
            None => Err(TeamError::AssignmentUnavailable(definition.node_id.clone())),
        }
    }

    fn auto_candidates(
        &self,
        task: &EngineeringTask,
        timeout: Option<Duration>,
        capabilities: &[Capability],
    ) -> TeamResult<forge_core::CandidateAgentSet> {
        let runner = self.runner();
        let mut requested = Vec::new();
        for descriptor in self
            .registry
            .descriptors()
            .iter()
            .filter(|descriptor| descriptor.adapter_status == AdapterStatus::Implemented)
        {
            let mut run_request = RunRequest::new(task.clone(), descriptor.agent_id.as_str());
            run_request.timeout = timeout;
            let config = runner.agent_config(&run_request)?;
            let adapter = self
                .registry
                .adapter(descriptor.agent_id.as_str(), &config)?;
            if self
                .registry
                .availability(&adapter.descriptor())
                .is_runnable()
            {
                requested.push(CandidateRequest {
                    config,
                    availability: CandidateAvailability::Available,
                });
            }
        }
        let requirements = CandidateRequirements {
            capabilities: capabilities.iter().cloned().collect::<BTreeSet<_>>(),
        };
        Ok(resolve_candidates(
            self.registry.descriptors(),
            requested,
            &requirements,
        )?)
    }

    async fn interpret_agent_result(
        &self,
        definition: &TeamPlanNode,
        input_commit: &str,
        report: RunReport,
        team: &mut TeamExecution,
        events: &mut Vec<TeamEvent>,
    ) -> TeamResult<()> {
        match definition.execution {
            TeamExecutionType::Analysis => {
                if report
                    .run
                    .patch
                    .as_ref()
                    .is_some_and(|patch| !patch.is_empty())
                {
                    fail_node(
                        team,
                        &definition.node_id,
                        TeamFailureKind::Engineering,
                        "analysis node modified the repository instead of returning evidence"
                            .into(),
                        events,
                    )?;
                    return Ok(());
                }
                let report_text = report
                    .run
                    .execution
                    .as_ref()
                    .and_then(|execution| execution.self_report.clone());
                let Some(report_text) = report_text else {
                    fail_node(
                        team,
                        &definition.node_id,
                        TeamFailureKind::AgentProcess,
                        "analysis node produced no structured or textual findings".into(),
                        events,
                    )?;
                    return Ok(());
                };
                let structured = serde_json::from_str(&report_text).ok();
                if definition
                    .outputs
                    .contains(&TeamArtifactKind::StructuredFindings)
                    && structured.is_none()
                {
                    fail_node(
                        team,
                        &definition.node_id,
                        TeamFailureKind::Engineering,
                        "analysis node promised structured_findings but returned only prose".into(),
                        events,
                    )?;
                    return Ok(());
                }
                let outputs = if definition.outputs.is_empty() {
                    if structured.is_some() {
                        vec![TeamArtifactKind::StructuredFindings]
                    } else {
                        vec![TeamArtifactKind::Analysis]
                    }
                } else {
                    definition.outputs.clone()
                };
                for output in outputs {
                    let content = match output {
                        TeamArtifactKind::StructuredFindings => TeamArtifactContent::InlineJson {
                            value: structured.clone().expect("validated above"),
                        },
                        TeamArtifactKind::Analysis => TeamArtifactContent::Text {
                            value: report_text.clone(),
                        },
                        _ => unreachable!("analysis output kinds validated with the plan"),
                    };
                    publish_artifact(
                        &self.store,
                        team,
                        &definition.node_id,
                        output,
                        content,
                        events,
                    )
                    .await?;
                }
                succeed_node(team, &definition.node_id, Some(input_commit.into()), events)?;
            }
            TeamExecutionType::Implementation => {
                let patch = report.run.patch.as_ref();
                let commit = patch.and_then(|patch| patch.head_commit.clone());
                if report.outcome() != RunOutcome::Passed || commit.is_none() {
                    let kind = if report.outcome() == RunOutcome::Errored {
                        TeamFailureKind::Infrastructure
                    } else if report.run.execution.as_ref().is_some_and(|execution| {
                        matches!(
                            execution.status,
                            AgentExecutionStatus::StartFailed | AgentExecutionStatus::Cancelled
                        )
                    }) {
                        TeamFailureKind::AgentProcess
                    } else if report.outcome() == RunOutcome::NoChange {
                        TeamFailureKind::Engineering
                    } else {
                        TeamFailureKind::Evaluation
                    };
                    fail_node(
                        team,
                        &definition.node_id,
                        kind,
                        format!("implementation run concluded {}", report.outcome()),
                        events,
                    )?;
                    return Ok(());
                }
                let commit = commit.expect("checked above");
                publish_artifact(
                    &self.store,
                    team,
                    &definition.node_id,
                    TeamArtifactKind::CandidateCommit,
                    TeamArtifactContent::Commit {
                        commit: commit.clone(),
                    },
                    events,
                )
                .await?;
                publish_artifact(
                    &self.store,
                    team,
                    &definition.node_id,
                    TeamArtifactKind::CandidatePatch,
                    TeamArtifactContent::InlineJson {
                        value: serde_json::to_value(patch.expect("commit requires patch"))?,
                    },
                    events,
                )
                .await?;
                if let Some(evaluation) = &report.evaluation {
                    publish_artifact(
                        &self.store,
                        team,
                        &definition.node_id,
                        TeamArtifactKind::Evaluation,
                        TeamArtifactContent::InlineJson {
                            value: serde_json::to_value(evaluation)?,
                        },
                        events,
                    )
                    .await?;
                    if definition.outputs.contains(&TeamArtifactKind::Metrics) {
                        publish_artifact(
                            &self.store,
                            team,
                            &definition.node_id,
                            TeamArtifactKind::Metrics,
                            TeamArtifactContent::InlineJson {
                                value: serde_json::to_value(&evaluation.metrics)?,
                            },
                            events,
                        )
                        .await?;
                    }
                }
                succeed_node(team, &definition.node_id, Some(commit), events)?;
            }
            TeamExecutionType::Review => {
                if report
                    .run
                    .patch
                    .as_ref()
                    .is_some_and(|patch| !patch.is_empty())
                {
                    fail_node(
                        team,
                        &definition.node_id,
                        TeamFailureKind::Review,
                        "review node modified the candidate instead of reviewing it".into(),
                        events,
                    )?;
                    return Ok(());
                }
                let prose = report
                    .run
                    .execution
                    .as_ref()
                    .and_then(|execution| execution.self_report.clone());
                let review = prose
                    .as_deref()
                    .and_then(|value| serde_json::from_str::<ReviewResult>(value).ok())
                    .unwrap_or(ReviewResult {
                        decision: ReviewDecision::Inconclusive,
                        findings: Vec::new(),
                        prose,
                    });
                publish_artifact(
                    &self.store,
                    team,
                    &definition.node_id,
                    TeamArtifactKind::Review,
                    TeamArtifactContent::InlineJson {
                        value: serde_json::to_value(&review)?,
                    },
                    events,
                )
                .await?;
                team.node_mut(&definition.node_id)
                    .ok_or_else(|| TeamError::MissingNode(definition.node_id.clone()))?
                    .review = Some(review.clone());
                emit(
                    team,
                    events,
                    TeamEventPayload::ReviewCompleted {
                        node_id: definition.node_id.clone(),
                        decision: review.decision,
                    },
                );
                succeed_node(team, &definition.node_id, Some(input_commit.into()), events)?;
            }
            TeamExecutionType::Integration | TeamExecutionType::Verification => {
                unreachable!("deterministic nodes do not use an agent")
            }
        }
        Ok(())
    }

    async fn execute_deterministic_node(
        &self,
        definition: &TeamPlanNode,
        input_commit: &str,
        team: &mut TeamExecution,
        events: &mut Vec<TeamEvent>,
    ) -> TeamResult<()> {
        if definition.execution == TeamExecutionType::Integration {
            publish_artifact(
                &self.store,
                team,
                &definition.node_id,
                TeamArtifactKind::Integration,
                TeamArtifactContent::Commit {
                    commit: input_commit.into(),
                },
                events,
            )
            .await?;
        }
        succeed_node(team, &definition.node_id, Some(input_commit.into()), events)
    }

    async fn finish_team(
        &self,
        task: &EngineeringTask,
        team: &mut TeamExecution,
        events: &mut Vec<TeamEvent>,
    ) -> TeamResult<()> {
        let candidate = terminal_candidate_commit(team);
        match candidate {
            Ok(commit) => {
                emit(
                    team,
                    events,
                    TeamEventPayload::IntegrationStarted { candidate_count: 1 },
                );
                match self.final_evaluate(task, team, &commit, events).await {
                    Ok((candidate, evaluation)) => {
                        emit(
                            team,
                            events,
                            TeamEventPayload::IntegrationCompleted {
                                commit: commit.clone(),
                            },
                        );
                        team.final_candidate = Some(candidate);
                        team.final_evaluation = Some(evaluation);
                    }
                    Err(error) => {
                        team.status = TeamStatus::InfrastructureFailed;
                        team.outcome = Some(TeamOutcome::Errored);
                        team.failure_reason = Some(error.to_string());
                    }
                }
            }
            Err(error) => {
                team.failure_reason = Some(error.to_string());
            }
        }
        team.execution_provenance = aggregate_provenance(&self.store, team).await;
        team.resources = aggregate_resources(&self.store, team).await;
        let baseline = self
            .store
            .compatible_single_agent_baseline(
                &team.root_task_revision_id,
                &team.base_commit,
                team.execution_provenance,
            )
            .await?;
        team.outcome = Some(derive_team_outcome(team));
        if team
            .final_candidate
            .as_ref()
            .is_some_and(|candidate| candidate.patch.files_changed == 0)
            && team.failure_reason.is_none()
        {
            team.failure_reason = Some("final candidate contains no candidate patch".into());
        }
        team.status = match team.outcome {
            Some(TeamOutcome::Passed | TeamOutcome::Inconclusive) => TeamStatus::Completed,
            Some(TeamOutcome::Blocked) => TeamStatus::Blocked,
            Some(TeamOutcome::Failed) => TeamStatus::CompletedWithFailures,
            Some(TeamOutcome::Errored) | None => TeamStatus::InfrastructureFailed,
        };
        team.completed_at = Some(Utc::now());
        team.baseline_comparison = Some(compare_baseline(team, baseline));
        emit(
            team,
            events,
            TeamEventPayload::TeamCompleted {
                outcome: team.outcome.unwrap_or(TeamOutcome::Errored),
            },
        );
        Ok(())
    }

    async fn final_evaluate(
        &self,
        task: &EngineeringTask,
        team: &TeamExecution,
        candidate_commit: &str,
        events: &mut Vec<TeamEvent>,
    ) -> TeamResult<(FinalCandidate, TeamFinalEvaluation)> {
        emit(
            team,
            events,
            TeamEventPayload::FinalEvaluationStarted {
                commit: candidate_commit.into(),
            },
        );
        let layout = Layout::new(self.repository.root().to_path_buf());
        let manager =
            WorktreeManager::new(self.repository.clone(), layout.worktrees_root(&self.config))?;
        let name = format!("{}-final", team.team_execution_id);
        let branch = format!(
            "{}team-{}-final",
            self.config.workspaces.branch_prefix, team.team_execution_id
        );
        let worktree = manager.create(&name, candidate_commit, &branch)?;
        let subject = EvaluationSubject::TeamExecution(team.team_execution_id.clone());
        let sink = EvaluationLifecycleSink::default();
        let workspace = Workspace::for_evaluation(
            WorkspaceKind::Worktree,
            worktree.path().to_path_buf(),
            branch,
            team.base_commit.clone(),
        );
        let result = async {
            let delta = workspace_delta(&workspace.path, &team.base_commit)?;
            let mut policy = PatchPolicy::default();
            for metrics_file in task.evaluation.metrics_files() {
                policy = policy.with_excluded_path(metrics_file);
            }
            let candidate = policy.apply(&delta);
            stage_candidate_patch(&workspace.path, &team.base_commit, &candidate)?;
            let artifacts_dir = layout.teams_dir().join(team.team_execution_id.as_str());
            std::fs::create_dir_all(&artifacts_dir).map_err(|source| TeamError::ReadTask {
                path: artifacts_dir.clone(),
                source,
            })?;
            let diff_path = artifacts_dir.join("final.patch.diff");
            let diff = cached_patch(&workspace.path, &team.base_commit)?;
            std::fs::write(&diff_path, diff).map_err(|source| TeamError::ReadTask {
                path: diff_path.clone(),
                source,
            })?;
            let patch = PatchSummary {
                base_commit: team.base_commit.clone(),
                head_commit: Some(candidate_commit.into()),
                files_changed: candidate.files_changed(),
                insertions: candidate.insertions(),
                deletions: candidate.deletions(),
                binary_files: candidate.binary_files(),
                diff_path: Some(diff_path),
                excluded: candidate.excluded.clone(),
            };
            let integrity = task.protection.check(&delta)?;
            let plan = EvaluationPlan::resolve(task);
            let evaluation = if plan.is_empty() {
                None
            } else {
                let runner = ProcessRunner::new(EnvPolicy::conservative());
                let context = EvalContext::new(&workspace, task, &runner, &sink)
                    .with_patch(&patch)
                    .with_default_timeout(Some(Duration::from_secs(
                        self.config.defaults.timeout_secs,
                    )))
                    .with_artifacts_dir(&artifacts_dir);
                Some(EvaluationEngine::execute_subject(&plan, subject.clone(), &context).await)
            };
            let verdict = evaluation
                .as_ref()
                .map(|evaluation| evaluation.verdict)
                .unwrap_or(Verdict::Inconclusive);
            let contributing_nodes = team
                .plan
                .topological_order
                .iter()
                .filter(|id| {
                    team.plan.node(id).is_some_and(|node| {
                        matches!(
                            node.execution,
                            TeamExecutionType::Implementation | TeamExecutionType::Integration
                        )
                    }) && team
                        .node(id)
                        .is_some_and(|node| node.status == TeamNodeStatus::Succeeded)
                })
                .cloned()
                .collect::<Vec<_>>();
            let contributing_runs = contributing_nodes
                .iter()
                .filter_map(|id| team.node(id))
                .flat_map(|node| node.run_ids.iter().cloned())
                .collect::<Vec<_>>();
            let lineage = contributing_nodes
                .iter()
                .filter_map(|id| team.node(id).and_then(|node| node.output_commit.clone()))
                .collect::<Vec<_>>();
            Ok::<_, TeamError>((
                FinalCandidate {
                    base_commit: team.base_commit.clone(),
                    integrated_commit: candidate_commit.into(),
                    contributing_nodes,
                    contributing_runs,
                    patch,
                    lineage,
                },
                TeamFinalEvaluation {
                    integrity,
                    evaluation,
                    verdict,
                },
            ))
        }
        .await;
        for event in sink.drain() {
            emit(
                team,
                events,
                TeamEventPayload::EvaluationLifecycle { event },
            );
        }
        let _ = manager.remove(&worktree, true);
        if let Ok((_, evaluation)) = &result {
            emit(
                team,
                events,
                TeamEventPayload::FinalEvaluationCompleted {
                    verdict: evaluation.verdict,
                },
            );
        }
        result
    }

    async fn persist(&self, team: &TeamExecution, events: &[TeamEvent]) -> TeamResult<()> {
        self.store.save_team_execution(team).await?;
        self.store.append_team_events(events).await?;
        Ok(())
    }
}

fn derive_node_task(
    root: &EngineeringTask,
    root_revision: &TaskRevision,
    team: &TeamExecution,
    definition: &TeamPlanNode,
    input_commit: &str,
    input_artifacts: &[TeamArtifact],
) -> TeamResult<EngineeringTask> {
    let position = team
        .plan
        .topological_order
        .iter()
        .position(|id| id == &definition.node_id)
        .ok_or_else(|| TeamError::MissingNode(definition.node_id.clone()))?;
    let node_task_id = TaskId::new(format!(
        "TN-{:04}-{:04}",
        team.team_execution_id.sequence().unwrap_or(0),
        position + 1
    ))
    .map_err(|error| TeamError::PlanSerialization(error.to_string()))?;
    let mut task = root.clone();
    task.task_id = node_task_id;
    task.objective = format!(
        "Root objective: {}\n\nTeam node objective: {}",
        root.objective, definition.objective
    );
    task.constraints.push(format!(
        "This is node `{}` of team `{}`. Work only on this node objective.",
        definition.node_id, team.team_execution_id
    ));
    task.constraints.push(format!(
        "Start from the exact input commit `{input_commit}`."
    ));
    task.constraints.extend(definition.constraints.clone());
    if !definition.outputs.is_empty() {
        task.constraints.push(format!(
            "Expected typed outputs: {}.",
            definition
                .outputs
                .iter()
                .map(|output| format!("{output:?}"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    for artifact in input_artifacts {
        task.constraints.push(format!(
            "Dependency artifact {} ({:?}, sha256 {}): {}",
            artifact.artifact_id,
            artifact.kind,
            artifact.content_sha256,
            render_artifact(artifact)
        ));
    }
    if matches!(
        definition.execution,
        TeamExecutionType::Analysis | TeamExecutionType::Review
    ) {
        task.evaluation = EvaluationSpec::default();
        task.constraints.push(
            "Do not modify repository files. Return the requested evidence as your final response."
                .into(),
        );
    }
    task.constraints.push(format!(
        "Lineage root revision: {}.",
        root_revision.revision_id()
    ));
    Ok(task)
}

fn render_artifact(artifact: &TeamArtifact) -> String {
    const MAX_INLINE: usize = 8 * 1024;
    let rendered = match &artifact.content {
        TeamArtifactContent::InlineJson { value } => value.to_string(),
        TeamArtifactContent::Text { value } => value.clone(),
        TeamArtifactContent::FileReference { path, sha256 } => {
            format!("file `{path}` (sha256 {sha256})")
        }
        TeamArtifactContent::Commit { commit } => format!("commit `{commit}`"),
    };
    let mut characters = rendered.chars();
    let prefix = characters.by_ref().take(MAX_INLINE).collect::<String>();
    if characters.next().is_none() {
        prefix
    } else {
        format!("{prefix}… [truncated; use persisted artifact reference]")
    }
}

fn dependency_executions<'a>(
    team: &'a TeamExecution,
    definition: &TeamPlanNode,
) -> TeamResult<Vec<&'a forge_core::TeamNodeExecution>> {
    definition
        .depends_on
        .iter()
        .map(|id| {
            team.node(id)
                .ok_or_else(|| TeamError::MissingNode(id.clone()))
        })
        .collect()
}

fn direct_input_artifacts(team: &TeamExecution, definition: &TeamPlanNode) -> Vec<TeamArtifact> {
    if definition.inputs.is_empty() {
        return Vec::new();
    }
    let dependencies = definition.depends_on.iter().collect::<BTreeSet<_>>();
    team.artifacts
        .iter()
        .filter(|artifact| {
            dependencies.contains(&artifact.producer_node_id)
                && definition.inputs.contains(&artifact.kind)
        })
        .cloned()
        .collect()
}

fn resolve_input_commit(team: &TeamExecution, definition: &TeamPlanNode) -> TeamResult<String> {
    let mut commits = definition
        .depends_on
        .iter()
        .filter_map(|id| team.node(id).and_then(|node| node.output_commit.clone()))
        .collect::<BTreeSet<_>>();
    if commits.len() > 1 {
        commits.remove(&team.base_commit);
    }
    match commits.len() {
        0 => Ok(team.base_commit.clone()),
        1 => Ok(commits.into_iter().next().expect("one commit")),
        _ => Err(TeamError::IntegrationConflict {
            node: definition.node_id.clone(),
            commits: commits.into_iter().collect(),
        }),
    }
}

async fn publish_artifact(
    store: &Store,
    team: &mut TeamExecution,
    node_id: &TeamNodeId,
    kind: TeamArtifactKind,
    content: TeamArtifactContent,
    events: &mut Vec<TeamEvent>,
) -> TeamResult<()> {
    let artifact = TeamArtifact::new(
        store.next_team_artifact_id().await?,
        team.team_execution_id.clone(),
        node_id.clone(),
        kind,
        content,
        Utc::now(),
    )?;
    let artifact_id = artifact.artifact_id.clone();
    team.artifacts.push(artifact);
    team.node_mut(node_id)
        .ok_or_else(|| TeamError::MissingNode(node_id.clone()))?
        .output_artifact_ids
        .push(artifact_id.clone());
    emit(
        team,
        events,
        TeamEventPayload::ArtifactPublished {
            artifact_id,
            node_id: node_id.clone(),
        },
    );
    Ok(())
}

fn succeed_node(
    team: &mut TeamExecution,
    node_id: &TeamNodeId,
    output_commit: Option<String>,
    events: &mut Vec<TeamEvent>,
) -> TeamResult<()> {
    let run_id = {
        let node = team
            .node_mut(node_id)
            .ok_or_else(|| TeamError::MissingNode(node_id.clone()))?;
        node.status = TeamNodeStatus::Succeeded;
        node.output_commit = output_commit;
        node.finished_at = Some(Utc::now());
        node.run_ids.last().cloned()
    };
    emit(
        team,
        events,
        TeamEventPayload::NodeCompleted {
            node_id: node_id.clone(),
            run_id,
        },
    );
    Ok(())
}

fn fail_node(
    team: &mut TeamExecution,
    node_id: &TeamNodeId,
    kind: TeamFailureKind,
    reason: String,
    events: &mut Vec<TeamEvent>,
) -> TeamResult<()> {
    let node = team
        .node_mut(node_id)
        .ok_or_else(|| TeamError::MissingNode(node_id.clone()))?;
    node.status = TeamNodeStatus::Failed;
    node.failure_kind = Some(kind);
    node.failure_reason = Some(reason.clone());
    node.finished_at = Some(Utc::now());
    emit(
        team,
        events,
        TeamEventPayload::NodeFailed {
            node_id: node_id.clone(),
            reason,
        },
    );
    Ok(())
}

fn block_node(
    team: &mut TeamExecution,
    node_id: &TeamNodeId,
    reason: &str,
    events: &mut Vec<TeamEvent>,
) -> TeamResult<()> {
    let node = team
        .node_mut(node_id)
        .ok_or_else(|| TeamError::MissingNode(node_id.clone()))?;
    node.status = TeamNodeStatus::Blocked;
    node.failure_reason = Some(reason.into());
    node.finished_at = Some(Utc::now());
    emit(
        team,
        events,
        TeamEventPayload::NodeBlocked {
            node_id: node_id.clone(),
            reason: reason.into(),
        },
    );
    Ok(())
}

fn assignment_block_node(
    team: &mut TeamExecution,
    node_id: &TeamNodeId,
    reason: &str,
    decision_id: Option<forge_core::RoutingDecisionId>,
    events: &mut Vec<TeamEvent>,
) -> TeamResult<()> {
    let node = team
        .node_mut(node_id)
        .ok_or_else(|| TeamError::MissingNode(node_id.clone()))?;
    node.status = TeamNodeStatus::AssignmentBlocked;
    node.failure_kind = Some(TeamFailureKind::Assignment);
    node.failure_reason = Some(reason.into());
    node.routing_decision_id = decision_id;
    node.finished_at = Some(Utc::now());
    emit(
        team,
        events,
        TeamEventPayload::NodeBlocked {
            node_id: node_id.clone(),
            reason: reason.into(),
        },
    );
    Ok(())
}

fn required_node_failed(team: &TeamExecution) -> bool {
    team.nodes.iter().any(|node| {
        team.plan
            .node(&node.node_id)
            .is_some_and(|definition| definition.required)
            && matches!(
                node.status,
                TeamNodeStatus::Failed
                    | TeamNodeStatus::Blocked
                    | TeamNodeStatus::AssignmentBlocked
            )
    })
}

fn terminal_candidate_commit(team: &TeamExecution) -> TeamResult<String> {
    let commits = team
        .plan
        .terminal_nodes()
        .into_iter()
        .filter(|definition| node_has_candidate_lineage(team, &definition.node_id))
        .filter_map(|definition| {
            team.node(&definition.node_id).and_then(|node| {
                (node.status == TeamNodeStatus::Succeeded)
                    .then(|| node.output_commit.clone())
                    .flatten()
            })
        })
        .collect::<BTreeSet<_>>();
    match commits.len() {
        0 => Err(TeamError::NoFinalCandidate),
        1 => Ok(commits.into_iter().next().expect("one commit")),
        _ => Err(TeamError::MultipleFinalCandidates(
            commits.into_iter().collect(),
        )),
    }
}

fn node_has_candidate_lineage(team: &TeamExecution, node_id: &TeamNodeId) -> bool {
    let Some(definition) = team.plan.node(node_id) else {
        return false;
    };
    definition.execution == TeamExecutionType::Implementation
        || definition
            .depends_on
            .iter()
            .any(|dependency| node_has_candidate_lineage(team, dependency))
}

fn derive_team_outcome(team: &TeamExecution) -> TeamOutcome {
    if team.status == TeamStatus::InfrastructureFailed {
        return TeamOutcome::Errored;
    }
    if team.nodes.iter().any(|node| {
        team.plan
            .node(&node.node_id)
            .is_some_and(|definition| definition.required)
            && matches!(
                node.status,
                TeamNodeStatus::Blocked | TeamNodeStatus::AssignmentBlocked
            )
    }) {
        return TeamOutcome::Blocked;
    }
    if team.nodes.iter().any(|node| {
        team.plan
            .node(&node.node_id)
            .is_some_and(|definition| definition.required)
            && node.status == TeamNodeStatus::Failed
    }) {
        return TeamOutcome::Failed;
    }
    if team.nodes.iter().any(|node| {
        node.review
            .as_ref()
            .is_some_and(|review| review.decision == ReviewDecision::RequestChanges)
    }) {
        return TeamOutcome::Failed;
    }
    let Some(final_evaluation) = &team.final_evaluation else {
        return TeamOutcome::Failed;
    };
    if team
        .final_candidate
        .as_ref()
        .is_none_or(|candidate| candidate.patch.files_changed == 0)
    {
        return TeamOutcome::Failed;
    }
    if !final_evaluation.integrity.is_acceptable() {
        return TeamOutcome::Failed;
    }
    match final_evaluation.verdict {
        Verdict::Pass => TeamOutcome::Passed,
        Verdict::Fail => TeamOutcome::Failed,
        Verdict::Inconclusive => TeamOutcome::Inconclusive,
    }
}

async fn aggregate_provenance(store: &Store, team: &TeamExecution) -> ExecutionProvenance {
    let mut values = HashSet::new();
    for run_id in team.run_ids() {
        match store.load_run(&run_id).await {
            Ok(Some(run)) => {
                values.insert(run.execution_provenance);
            }
            _ => return ExecutionProvenance::Unknown,
        }
    }
    if values.len() == 1 {
        values.into_iter().next().unwrap_or_default()
    } else {
        ExecutionProvenance::Unknown
    }
}

async fn aggregate_resources(store: &Store, team: &TeamExecution) -> TeamResourceSummary {
    let run_ids = team.run_ids();
    let has_runs = !run_ids.is_empty();
    let mut summary = TeamResourceSummary {
        agent_run_count: run_ids.len() as u64,
        failed_attempt_count: team
            .nodes
            .iter()
            .filter(|node| node.status == TeamNodeStatus::Failed)
            .map(|node| node.run_ids.len() as u64)
            .sum(),
        warning_count: 0,
        total_run_duration_ms: has_runs.then_some(0),
        total_tokens: has_runs.then_some(0),
        known_cost_usd: has_runs.then_some(0.0),
    };
    for run_id in run_ids {
        let Ok(Some(run)) = store.load_run(&run_id).await else {
            summary.total_run_duration_ms = None;
            summary.total_tokens = None;
            summary.known_cost_usd = None;
            continue;
        };
        summary.warning_count = summary
            .warning_count
            .saturating_add(run.warnings.len() as u64);
        summary.total_run_duration_ms = summary
            .total_run_duration_ms
            .zip(
                run.total_duration()
                    .and_then(|duration| duration.num_milliseconds().try_into().ok()),
            )
            .map(|(total, value)| total.saturating_add(value));
        summary.total_tokens = summary
            .total_tokens
            .zip(run.usage().total_tokens())
            .map(|(total, value)| total.saturating_add(value));
        summary.known_cost_usd = summary
            .known_cost_usd
            .zip(run.usage().cost_usd)
            .map(|(total, value)| total + value);
    }
    summary
}

fn compare_baseline(
    team: &TeamExecution,
    baseline: Option<SingleAgentBaseline>,
) -> TeamBaselineComparison {
    let Some(baseline) = baseline else {
        return TeamBaselineComparison::unavailable();
    };
    let correctness = match (
        team.outcome == Some(TeamOutcome::Passed),
        baseline.outcome == RunOutcome::Passed,
    ) {
        (true, true) => TeamComparisonRelation::Tie,
        (true, false) => TeamComparisonRelation::TeamBetter,
        (false, true) => TeamComparisonRelation::BaselineBetter,
        (false, false) => TeamComparisonRelation::Incomparable,
    };
    let mut comparable_benchmarks = std::collections::BTreeMap::new();
    if let (Some(team_evaluation), Some(baseline_evaluation)) = (
        team.final_evaluation
            .as_ref()
            .and_then(|final_evaluation| final_evaluation.evaluation.as_ref()),
        baseline.evaluation.as_ref(),
    ) {
        for metric in &team_evaluation.metrics {
            let Some(peer) = baseline_evaluation.metrics.iter().find(|candidate| {
                candidate.name == metric.name
                    && candidate.unit == metric.unit
                    && candidate.direction == metric.direction
            }) else {
                continue;
            };
            comparable_benchmarks.insert(
                metric.name.clone(),
                compare_metric(metric.value, peer.value, metric.direction),
            );
        }
    }
    TeamBaselineComparison {
        baseline_run_id: Some(baseline.run_id.clone()),
        correctness,
        integrity: compare_lower(
            team.final_evaluation
                .as_ref()
                .map(|evaluation| integrity_rank(evaluation.integrity.status)),
            baseline
                .integrity
                .as_ref()
                .map(|integrity| integrity_rank(integrity.status)),
        ),
        runtime: compare_lower(
            team.resources
                .total_run_duration_ms
                .map(|value| value as f64),
            baseline.runtime_ms.map(|value| value as f64),
        ),
        tokens: compare_lower(
            team.resources.total_tokens.map(|value| value as f64),
            baseline.total_tokens.map(|value| value as f64),
        ),
        known_cost: compare_lower(team.resources.known_cost_usd, baseline.known_cost_usd),
        patch_size: compare_lower(
            team.final_candidate
                .as_ref()
                .map(|candidate| candidate.patch.lines_changed() as f64),
            baseline.patch_lines.map(|value| value as f64),
        ),
        warnings: compare_lower(
            Some(team.resources.warning_count as f64),
            Some(baseline.warning_count as f64),
        ),
        comparable_benchmarks,
        note: None,
    }
}

fn integrity_rank(status: forge_core::IntegrityStatus) -> f64 {
    match status {
        forge_core::IntegrityStatus::Clean => 0.0,
        forge_core::IntegrityStatus::Modified => 1.0,
        forge_core::IntegrityStatus::Missing => 2.0,
    }
}

fn compare_lower(team: Option<f64>, baseline: Option<f64>) -> TeamComparisonRelation {
    match (team, baseline) {
        (Some(left), Some(right)) if (left - right).abs() < f64::EPSILON => {
            TeamComparisonRelation::Tie
        }
        (Some(left), Some(right)) if left < right => TeamComparisonRelation::TeamBetter,
        (Some(_), Some(_)) => TeamComparisonRelation::BaselineBetter,
        _ => TeamComparisonRelation::Unavailable,
    }
}

fn compare_metric(team: f64, baseline: f64, direction: Direction) -> TeamComparisonRelation {
    if (team - baseline).abs() < f64::EPSILON {
        return TeamComparisonRelation::Tie;
    }
    match direction {
        Direction::HigherIsBetter => {
            if team > baseline {
                TeamComparisonRelation::TeamBetter
            } else {
                TeamComparisonRelation::BaselineBetter
            }
        }
        Direction::LowerIsBetter => {
            if team < baseline {
                TeamComparisonRelation::TeamBetter
            } else {
                TeamComparisonRelation::BaselineBetter
            }
        }
        Direction::Neutral => TeamComparisonRelation::Incomparable,
    }
}

fn emit(team: &TeamExecution, events: &mut Vec<TeamEvent>, payload: TeamEventPayload) {
    events.push(TeamEvent {
        team_execution_id: team.team_execution_id.clone(),
        seq: events.len() as u64 + 1,
        timestamp: Utc::now(),
        payload,
    });
}

#[cfg(test)]
mod tests {
    use forge_core::ids::{AgentId, TaskId, TeamExecutionId};
    use forge_core::task::TaskRevisionId;
    use forge_core::team::{TEAM_PLAN_VERSION, TeamAssignmentStrategy};

    use super::*;

    fn node(id: &str, execution: TeamExecutionType, dependencies: &[&str]) -> TeamPlanNode {
        TeamPlanNode {
            node_id: TeamNodeId::new(id).unwrap(),
            objective: format!("execute {id}"),
            execution,
            depends_on: dependencies
                .iter()
                .map(|dependency| TeamNodeId::new(*dependency).unwrap())
                .collect(),
            inputs: Vec::new(),
            outputs: Vec::new(),
            constraints: Vec::new(),
            required_capabilities: Vec::new(),
            assignment: execution
                .requires_agent()
                .then(|| TeamAssignmentStrategy::Explicit {
                    agent: AgentId::new("claude").unwrap(),
                }),
            required: true,
        }
    }

    fn team(nodes: Vec<TeamPlanNode>) -> TeamExecution {
        let plan = TeamPlan {
            plan_version: TEAM_PLAN_VERSION.into(),
            root_objective: "repair the fixture".into(),
            nodes,
        }
        .validate()
        .unwrap();
        TeamExecution::new(
            TeamExecutionId::sequential(1),
            TaskId::sequential(1),
            TaskRevisionId::from_stored("legacy:T-0001").unwrap(),
            "base",
            plan,
            PlanProvenance::explicit(),
        )
    }

    #[test]
    fn dependency_commit_is_inherited_through_analysis_and_review() {
        let mut team = team(vec![
            node("inspect", TeamExecutionType::Analysis, &[]),
            node("implement", TeamExecutionType::Implementation, &["inspect"]),
            node("review", TeamExecutionType::Review, &["implement"]),
        ]);
        team.node_mut(&TeamNodeId::new("inspect").unwrap())
            .unwrap()
            .output_commit = Some("base".into());
        assert_eq!(
            resolve_input_commit(
                &team,
                team.plan
                    .node(&TeamNodeId::new("implement").unwrap())
                    .unwrap()
            )
            .unwrap(),
            "base"
        );
        team.node_mut(&TeamNodeId::new("implement").unwrap())
            .unwrap()
            .output_commit = Some("candidate".into());
        assert_eq!(
            resolve_input_commit(
                &team,
                team.plan.node(&TeamNodeId::new("review").unwrap()).unwrap()
            )
            .unwrap(),
            "candidate"
        );
    }

    #[test]
    fn parallel_candidate_commits_require_explicit_integration() {
        let mut team = team(vec![
            node("left", TeamExecutionType::Implementation, &[]),
            node("right", TeamExecutionType::Implementation, &[]),
            node(
                "integrate",
                TeamExecutionType::Integration,
                &["left", "right"],
            ),
        ]);
        team.node_mut(&TeamNodeId::new("left").unwrap())
            .unwrap()
            .output_commit = Some("left-commit".into());
        team.node_mut(&TeamNodeId::new("right").unwrap())
            .unwrap()
            .output_commit = Some("right-commit".into());
        assert!(matches!(
            resolve_input_commit(
                &team,
                team.plan
                    .node(&TeamNodeId::new("integrate").unwrap())
                    .unwrap()
            ),
            Err(TeamError::IntegrationConflict { .. })
        ));
    }

    #[test]
    fn unchanged_analysis_and_one_candidate_form_an_unambiguous_handoff() {
        let mut team = team(vec![
            node("inspect", TeamExecutionType::Analysis, &[]),
            node("implement", TeamExecutionType::Implementation, &[]),
            node(
                "review",
                TeamExecutionType::Review,
                &["inspect", "implement"],
            ),
        ]);
        team.node_mut(&TeamNodeId::new("inspect").unwrap())
            .unwrap()
            .output_commit = Some("base".into());
        team.node_mut(&TeamNodeId::new("implement").unwrap())
            .unwrap()
            .output_commit = Some("candidate".into());
        assert_eq!(
            resolve_input_commit(
                &team,
                team.plan.node(&TeamNodeId::new("review").unwrap()).unwrap()
            )
            .unwrap(),
            "candidate"
        );
    }

    #[test]
    fn isolated_analysis_does_not_become_a_final_code_candidate() {
        let mut team = team(vec![
            node("inspect", TeamExecutionType::Analysis, &[]),
            node("implement", TeamExecutionType::Implementation, &[]),
        ]);
        let inspect = TeamNodeId::new("inspect").unwrap();
        team.node_mut(&inspect).unwrap().status = TeamNodeStatus::Succeeded;
        team.node_mut(&inspect).unwrap().output_commit = Some("base".into());
        let implement = TeamNodeId::new("implement").unwrap();
        team.node_mut(&implement).unwrap().status = TeamNodeStatus::Succeeded;
        team.node_mut(&implement).unwrap().output_commit = Some("candidate".into());
        assert_eq!(terminal_candidate_commit(&team).unwrap(), "candidate");
    }

    #[test]
    fn approving_review_cannot_override_a_failing_final_evaluation() {
        let mut team = team(vec![
            node("implement", TeamExecutionType::Implementation, &[]),
            node("review", TeamExecutionType::Review, &["implement"]),
        ]);
        for node in &mut team.nodes {
            node.status = TeamNodeStatus::Succeeded;
        }
        team.node_mut(&TeamNodeId::new("review").unwrap())
            .unwrap()
            .review = Some(ReviewResult {
            decision: ReviewDecision::Approve,
            findings: Vec::new(),
            prose: None,
        });
        team.final_evaluation = Some(TeamFinalEvaluation {
            integrity: Default::default(),
            evaluation: None,
            verdict: Verdict::Fail,
        });
        assert_eq!(derive_team_outcome(&team), TeamOutcome::Failed);
    }

    #[test]
    fn an_empty_integrated_candidate_never_passes_on_green_checks_alone() {
        let mut team = team(vec![node(
            "implement",
            TeamExecutionType::Implementation,
            &[],
        )]);
        team.nodes[0].status = TeamNodeStatus::Succeeded;
        team.final_candidate = Some(FinalCandidate {
            base_commit: "base".into(),
            integrated_commit: "candidate".into(),
            contributing_nodes: vec![TeamNodeId::new("implement").unwrap()],
            contributing_runs: Vec::new(),
            patch: PatchSummary {
                base_commit: "base".into(),
                head_commit: Some("candidate".into()),
                files_changed: 0,
                insertions: 0,
                deletions: 0,
                binary_files: 0,
                diff_path: None,
                excluded: Vec::new(),
            },
            lineage: vec!["candidate".into()],
        });
        team.final_evaluation = Some(TeamFinalEvaluation {
            integrity: Default::default(),
            evaluation: None,
            verdict: Verdict::Pass,
        });
        assert_eq!(derive_team_outcome(&team), TeamOutcome::Failed);
    }
}
