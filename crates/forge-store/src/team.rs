//! Persistence and historical queries for team orchestration.

use std::collections::{BTreeMap, BTreeSet};

use forge_core::events::{EvaluationSubject, EventPayload};
use forge_core::ids::{RunId, TeamArtifactId, TeamExecutionId};
use forge_core::task::TaskRevisionId;
use forge_core::team::{
    PlanSourceKind, SingleAgentBaseline, TeamArtifact, TeamArtifactKind, TeamEvent,
    TeamEventPayload, TeamExecution, TeamExecutionType, TeamFailureKind,
};
use sqlx::Row;

use crate::{Store, StoreError, StoreResult};

const TEAM_EXECUTION_COUNTER: &str = "team_execution";
const TEAM_ARTIFACT_COUNTER: &str = "team_artifact";

impl Store {
    pub async fn next_team_execution_id(&self) -> StoreResult<TeamExecutionId> {
        Ok(TeamExecutionId::sequential(
            self.next_counter(TEAM_EXECUTION_COUNTER).await?,
        ))
    }

    pub async fn next_team_artifact_id(&self) -> StoreResult<TeamArtifactId> {
        Ok(TeamArtifactId::sequential(
            self.next_counter(TEAM_ARTIFACT_COUNTER).await?,
        ))
    }

    pub async fn save_team_execution(&self, team: &TeamExecution) -> StoreResult<()> {
        let validated_plan = team
            .plan
            .plan
            .clone()
            .validate()
            .map_err(|error| StoreError::Corrupt(error.to_string()))?;
        if validated_plan != team.plan {
            return Err(StoreError::Corrupt(format!(
                "team `{}` contains a non-canonical validated plan",
                team.team_execution_id
            )));
        }
        validate_team_references(team)?;
        let existing: Option<(String, String, String, String)> = sqlx::query_as(
            "SELECT root_task_revision_id, base_commit, plan_fingerprint, record_json
             FROM team_executions WHERE team_execution_id = ?1",
        )
        .bind(team.team_execution_id.as_str())
        .fetch_optional(self.pool())
        .await?;
        if let Some((revision, base, fingerprint, record_json)) = existing {
            let existing_team: TeamExecution = serde_json::from_str(&record_json)?;
            if revision != team.root_task_revision_id.as_str()
                || base != team.base_commit
                || fingerprint != team.plan.fingerprint
                || existing_team.root_task_id != team.root_task_id
                || existing_team.plan_provenance != team.plan_provenance
                || existing_team.world_model_context != team.world_model_context
                || existing_team.created_at != team.created_at
            {
                return Err(StoreError::TeamPlanConflict {
                    team_execution_id: team.team_execution_id.to_string(),
                    existing: format!(
                        "{}:{}:{}:{:?}",
                        existing_team.root_task_revision_id,
                        existing_team.base_commit,
                        existing_team.plan.fingerprint,
                        existing_team.plan_provenance.source
                    ),
                    attempted: format!(
                        "{}:{}:{}:{:?}",
                        team.root_task_revision_id,
                        team.base_commit,
                        team.plan.fingerprint,
                        team.plan_provenance.source
                    ),
                });
            }
            if existing_team.completed_at.is_some() && &existing_team != team {
                return Err(StoreError::TeamExecutionFinalized {
                    team_execution_id: team.team_execution_id.to_string(),
                });
            }
        }

        let mut tx = self.pool().begin().await?;
        sqlx::query(
            "INSERT INTO team_executions (
                team_execution_id, root_task_id, root_task_revision_id, base_commit,
                plan_version, plan_fingerprint, plan_source, execution_provenance,
                status, outcome, final_commit, baseline_run_id, created_at, completed_at,
                record_json, world_model_snapshot_id
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
             ON CONFLICT (team_execution_id) DO UPDATE SET
                execution_provenance = excluded.execution_provenance,
                status = excluded.status,
                outcome = excluded.outcome,
                final_commit = excluded.final_commit,
                baseline_run_id = excluded.baseline_run_id,
                completed_at = excluded.completed_at,
                record_json = excluded.record_json",
        )
        .bind(team.team_execution_id.as_str())
        .bind(team.root_task_id.as_str())
        .bind(team.root_task_revision_id.as_str())
        .bind(&team.base_commit)
        .bind(&team.plan.plan.plan_version)
        .bind(&team.plan.fingerprint)
        .bind(plan_source(&team.plan_provenance.source))
        .bind(team.execution_provenance.as_str())
        .bind(team.status.as_str())
        .bind(team.outcome.map(|outcome| outcome.as_str()))
        .bind(
            team.final_candidate
                .as_ref()
                .map(|candidate| candidate.integrated_commit.as_str()),
        )
        .bind(
            team.baseline_comparison
                .as_ref()
                .and_then(|comparison| comparison.baseline_run_id.as_ref())
                .map(RunId::as_str),
        )
        .bind(team.created_at.to_rfc3339())
        .bind(team.completed_at.map(|timestamp| timestamp.to_rfc3339()))
        .bind(serde_json::to_string(team)?)
        .bind(
            team.world_model_context
                .as_ref()
                .map(|context| context.snapshot_id.as_str()),
        )
        .execute(&mut *tx)
        .await?;

        for node in &team.nodes {
            let definition = team.plan.node(&node.node_id).ok_or_else(|| {
                StoreError::Corrupt(format!("missing plan node `{}`", node.node_id))
            })?;
            sqlx::query(
                "INSERT INTO team_nodes (
                    team_execution_id, node_id, execution_type, required, status, node_task_id,
                    assigned_agent_id, config_fingerprint, selection_source,
                    routing_decision_id, input_commit, output_commit, failure_kind, record_json
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
                 ON CONFLICT (team_execution_id, node_id) DO UPDATE SET
                    status = excluded.status,
                    node_task_id = excluded.node_task_id,
                    assigned_agent_id = excluded.assigned_agent_id,
                    config_fingerprint = excluded.config_fingerprint,
                    selection_source = excluded.selection_source,
                    routing_decision_id = excluded.routing_decision_id,
                    input_commit = excluded.input_commit,
                    output_commit = excluded.output_commit,
                    failure_kind = excluded.failure_kind,
                    record_json = excluded.record_json",
            )
            .bind(team.team_execution_id.as_str())
            .bind(node.node_id.as_str())
            .bind(execution_type(definition.execution))
            .bind(i64::from(definition.required))
            .bind(node.status.as_str())
            .bind(node.task.as_ref().map(|task| task.task_id.as_str()))
            .bind(
                node.assignment
                    .as_ref()
                    .map(|assignment| assignment.agent.agent_id.as_str()),
            )
            .bind(
                node.assignment
                    .as_ref()
                    .map(|assignment| assignment.agent.fingerprint()),
            )
            .bind(
                node.assignment
                    .as_ref()
                    .map(|assignment| assignment.selection_source.as_str()),
            )
            .bind(
                node.routing_decision_id
                    .as_ref()
                    .map(|decision| decision.as_str()),
            )
            .bind(node.input_commit.as_deref())
            .bind(node.output_commit.as_deref())
            .bind(node.failure_kind.map(failure_kind))
            .bind(serde_json::to_string(node)?)
            .execute(&mut *tx)
            .await?;

            for (attempt, run_id) in node.run_ids.iter().enumerate() {
                let attempt_number = attempt as u64 + 1;
                let existing_run: Option<String> = sqlx::query_scalar(
                    "SELECT run_id FROM team_node_runs
                     WHERE team_execution_id = ?1 AND node_id = ?2 AND attempt = ?3",
                )
                .bind(team.team_execution_id.as_str())
                .bind(node.node_id.as_str())
                .bind(attempt_number as i64)
                .fetch_optional(&mut *tx)
                .await?;
                if let Some(existing) = existing_run {
                    if existing != run_id.as_str() {
                        return Err(StoreError::TeamRunLinkConflict {
                            team_execution_id: team.team_execution_id.to_string(),
                            node_id: node.node_id.to_string(),
                            attempt: attempt_number,
                            existing,
                            attempted: run_id.to_string(),
                        });
                    }
                    continue;
                }
                sqlx::query(
                    "INSERT INTO team_node_runs (team_execution_id, node_id, attempt, run_id)
                     VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT (team_execution_id, node_id, attempt) DO NOTHING",
                )
                .bind(team.team_execution_id.as_str())
                .bind(node.node_id.as_str())
                .bind(attempt_number as i64)
                .bind(run_id.as_str())
                .execute(&mut *tx)
                .await?;
            }
        }

        for edge in &team.plan.edges {
            sqlx::query(
                "INSERT INTO team_edges (team_execution_id, from_node_id, to_node_id)
                 VALUES (?1, ?2, ?3) ON CONFLICT DO NOTHING",
            )
            .bind(team.team_execution_id.as_str())
            .bind(edge.from.as_str())
            .bind(edge.to.as_str())
            .execute(&mut *tx)
            .await?;
        }

        for artifact in &team.artifacts {
            let existing_record: Option<String> =
                sqlx::query_scalar("SELECT record_json FROM team_artifacts WHERE artifact_id = ?1")
                    .bind(artifact.artifact_id.as_str())
                    .fetch_optional(&mut *tx)
                    .await?;
            if let Some(existing_record) = existing_record {
                let existing: TeamArtifact = serde_json::from_str(&existing_record)?;
                if existing != *artifact {
                    return Err(StoreError::TeamArtifactConflict {
                        artifact_id: artifact.artifact_id.to_string(),
                        existing: existing.content_sha256,
                        attempted: artifact.content_sha256.clone(),
                    });
                }
                continue;
            }
            sqlx::query(
                "INSERT INTO team_artifacts (
                    artifact_id, team_execution_id, producer_node_id, artifact_kind,
                    content_sha256, created_at, record_json
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )
            .bind(artifact.artifact_id.as_str())
            .bind(team.team_execution_id.as_str())
            .bind(artifact.producer_node_id.as_str())
            .bind(artifact_kind(artifact.kind))
            .bind(&artifact.content_sha256)
            .bind(artifact.created_at.to_rfc3339())
            .bind(serde_json::to_string(artifact)?)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn load_team_execution(
        &self,
        id: &TeamExecutionId,
    ) -> StoreResult<Option<TeamExecution>> {
        let json: Option<String> = sqlx::query_scalar(
            "SELECT record_json FROM team_executions WHERE team_execution_id = ?1",
        )
        .bind(id.as_str())
        .fetch_optional(self.pool())
        .await?;
        json.map(|value| serde_json::from_str(&value).map_err(Into::into))
            .transpose()
    }

    pub async fn teams_for_task(
        &self,
        revision_id: &TaskRevisionId,
    ) -> StoreResult<Vec<TeamExecution>> {
        let rows: Vec<String> = sqlx::query_scalar(
            "SELECT record_json FROM team_executions
             WHERE root_task_revision_id = ?1 ORDER BY created_at DESC, team_execution_id DESC",
        )
        .bind(revision_id.as_str())
        .fetch_all(self.pool())
        .await?;
        rows.into_iter()
            .map(|value| serde_json::from_str(&value).map_err(Into::into))
            .collect()
    }

    pub async fn append_team_events(&self, events: &[TeamEvent]) -> StoreResult<()> {
        for event in events {
            if let TeamEventPayload::EvaluationLifecycle { event: lifecycle } = &event.payload
                && lifecycle.evaluation_subject()
                    != Some(&EvaluationSubject::TeamExecution(
                        event.team_execution_id.clone(),
                    ))
            {
                let subject = lifecycle
                    .evaluation_subject()
                    .map(evaluation_subject_name)
                    .unwrap_or_else(|| "missing".into());
                return Err(StoreError::TeamEvaluationEventSubjectConflict {
                    team_execution_id: event.team_execution_id.to_string(),
                    subject,
                });
            }
            sqlx::query(
                "INSERT INTO team_events (
                    team_execution_id, seq, timestamp, event_type, data_json
                 ) VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT (team_execution_id, seq) DO NOTHING",
            )
            .bind(event.team_execution_id.as_str())
            .bind(event.seq as i64)
            .bind(event.timestamp.to_rfc3339())
            .bind(team_event_type(&event.payload))
            .bind(serde_json::to_string(event)?)
            .execute(self.pool())
            .await?;
        }
        Ok(())
    }

    pub async fn team_events_for(&self, id: &TeamExecutionId) -> StoreResult<Vec<TeamEvent>> {
        let rows: Vec<String> = sqlx::query_scalar(
            "SELECT data_json FROM team_events WHERE team_execution_id = ?1 ORDER BY seq",
        )
        .bind(id.as_str())
        .fetch_all(self.pool())
        .await?;
        rows.into_iter()
            .map(|value| serde_json::from_str(&value).map_err(Into::into))
            .collect()
    }

    /// The independent evaluator lifecycle for a team's integrated candidate.
    pub async fn evaluation_events_for_team(
        &self,
        id: &TeamExecutionId,
    ) -> StoreResult<Vec<EventPayload>> {
        Ok(self
            .team_events_for(id)
            .await?
            .into_iter()
            .filter_map(|event| match event.payload {
                TeamEventPayload::EvaluationLifecycle { event } => Some(event),
                _ => None,
            })
            .collect())
    }

    /// Latest ordinary run with identical task revision, base commit,
    /// evaluation semantics, and execution provenance. Team node runs are
    /// explicitly excluded.
    pub async fn compatible_single_agent_baseline(
        &self,
        revision_id: &TaskRevisionId,
        base_commit: &str,
        execution_provenance: forge_core::ExecutionProvenance,
    ) -> StoreResult<Option<SingleAgentBaseline>> {
        let row = sqlx::query(
            "SELECT r.run_id FROM runs r
             WHERE r.task_revision_id = ?1 AND r.base_commit = ?2
               AND r.execution_provenance = ?3
               AND r.status = 'completed' AND r.outcome IS NOT NULL
               AND NOT EXISTS (SELECT 1 FROM team_node_runs tnr WHERE tnr.run_id = r.run_id)
             ORDER BY r.created_at DESC, r.run_id DESC LIMIT 1",
        )
        .bind(revision_id.as_str())
        .bind(base_commit)
        .bind(execution_provenance.as_str())
        .fetch_optional(self.pool())
        .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let run_id = RunId::new(row.try_get::<String, _>("run_id")?)
            .map_err(|error| StoreError::Corrupt(error.to_string()))?;
        let run = self
            .load_run(&run_id)
            .await?
            .ok_or_else(|| StoreError::NotFound(format!("run `{run_id}`")))?;
        let outcome = run
            .outcome
            .ok_or_else(|| StoreError::Corrupt(format!("baseline `{run_id}` has no outcome")))?;
        Ok(Some(SingleAgentBaseline {
            run_id: run_id.clone(),
            execution_provenance: run.execution_provenance,
            outcome,
            integrity: run.integrity.clone(),
            runtime_ms: run
                .total_duration()
                .and_then(|duration| duration.num_milliseconds().try_into().ok()),
            total_tokens: run.usage().total_tokens(),
            known_cost_usd: run.usage().cost_usd,
            patch_lines: run.patch.as_ref().map(|patch| patch.lines_changed()),
            warning_count: run.warnings.len() as u64,
            evaluation: self.load_evaluation(&run_id).await?,
        }))
    }
}

fn validate_team_references(team: &TeamExecution) -> StoreResult<()> {
    let expected_nodes = team
        .plan
        .plan
        .nodes
        .iter()
        .map(|node| node.node_id.clone())
        .collect::<BTreeSet<_>>();
    let actual_nodes = team
        .nodes
        .iter()
        .map(|node| node.node_id.clone())
        .collect::<BTreeSet<_>>();
    if expected_nodes != actual_nodes || actual_nodes.len() != team.nodes.len() {
        return Err(StoreError::Corrupt(format!(
            "team `{}` node executions do not exactly match its immutable plan",
            team.team_execution_id
        )));
    }

    let mut artifacts = BTreeMap::new();
    for artifact in &team.artifacts {
        if artifact.team_execution_id != team.team_execution_id
            || !actual_nodes.contains(&artifact.producer_node_id)
        {
            return Err(StoreError::Corrupt(format!(
                "team artifact `{}` has invalid team or producer lineage",
                artifact.artifact_id
            )));
        }
        let reconstructed = TeamArtifact::new(
            artifact.artifact_id.clone(),
            artifact.team_execution_id.clone(),
            artifact.producer_node_id.clone(),
            artifact.kind,
            artifact.content.clone(),
            artifact.created_at,
        )?;
        if reconstructed.content_sha256 != artifact.content_sha256 {
            return Err(StoreError::Corrupt(format!(
                "team artifact `{}` content hash does not match its content",
                artifact.artifact_id
            )));
        }
        if artifacts
            .insert(artifact.artifact_id.clone(), artifact)
            .is_some()
        {
            return Err(StoreError::Corrupt(format!(
                "team `{}` repeats artifact `{}`",
                team.team_execution_id, artifact.artifact_id
            )));
        }
    }

    let mut runs = BTreeSet::new();
    for node in &team.nodes {
        for run_id in &node.run_ids {
            if !runs.insert(run_id) {
                return Err(StoreError::Corrupt(format!(
                    "team `{}` links run `{run_id}` more than once",
                    team.team_execution_id
                )));
            }
        }
        for artifact_id in &node.output_artifact_ids {
            let artifact = artifacts.get(artifact_id).ok_or_else(|| {
                StoreError::Corrupt(format!(
                    "team node `{}` references missing output artifact `{artifact_id}`",
                    node.node_id
                ))
            })?;
            if artifact.producer_node_id != node.node_id {
                return Err(StoreError::Corrupt(format!(
                    "team node `{}` claims artifact `{artifact_id}` produced by `{}`",
                    node.node_id, artifact.producer_node_id
                )));
            }
        }
        for artifact_id in &node.input_artifact_ids {
            let artifact = artifacts.get(artifact_id).ok_or_else(|| {
                StoreError::Corrupt(format!(
                    "team node `{}` references missing input artifact `{artifact_id}`",
                    node.node_id
                ))
            })?;
            let definition = team.plan.node(&node.node_id).expect("validated above");
            if !definition.depends_on.contains(&artifact.producer_node_id) {
                return Err(StoreError::Corrupt(format!(
                    "team node `{}` received artifact `{artifact_id}` from non-dependency `{}`",
                    node.node_id, artifact.producer_node_id
                )));
            }
        }
        if let Some(lineage) = &node.lineage
            && (lineage.root_task_id != team.root_task_id
                || lineage.root_task_revision_id != team.root_task_revision_id
                || lineage.team_execution_id != team.team_execution_id
                || lineage.node_id != node.node_id
                || node.task.as_ref().map(|task| &task.task_id) != Some(&lineage.node_task_id)
                || node.input_commit.as_deref() != Some(lineage.input_commit.as_str())
                || node.input_artifact_ids != lineage.input_artifact_ids)
        {
            return Err(StoreError::Corrupt(format!(
                "team node `{}` has inconsistent task lineage",
                node.node_id
            )));
        }
    }
    for artifact in &team.artifacts {
        if !team
            .nodes
            .iter()
            .find(|node| node.node_id == artifact.producer_node_id)
            .is_some_and(|node| node.output_artifact_ids.contains(&artifact.artifact_id))
        {
            return Err(StoreError::Corrupt(format!(
                "team artifact `{}` is not linked from its producer node",
                artifact.artifact_id
            )));
        }
    }
    Ok(())
}

fn plan_source(source: &PlanSourceKind) -> &'static str {
    match source {
        PlanSourceKind::Explicit => "explicit",
        PlanSourceKind::Generated => "generated",
        PlanSourceKind::Imported => "imported",
    }
}

fn execution_type(value: TeamExecutionType) -> &'static str {
    match value {
        TeamExecutionType::Analysis => "analysis",
        TeamExecutionType::Implementation => "implementation",
        TeamExecutionType::Review => "review",
        TeamExecutionType::Integration => "integration",
        TeamExecutionType::Verification => "verification",
    }
}

fn artifact_kind(value: TeamArtifactKind) -> &'static str {
    match value {
        TeamArtifactKind::Analysis => "analysis",
        TeamArtifactKind::StructuredFindings => "structured_findings",
        TeamArtifactKind::CandidatePatch => "candidate_patch",
        TeamArtifactKind::CandidateCommit => "candidate_commit",
        TeamArtifactKind::Evaluation => "evaluation",
        TeamArtifactKind::Review => "review",
        TeamArtifactKind::Metrics => "metrics",
        TeamArtifactKind::FileReference => "file_reference",
        TeamArtifactKind::Integration => "integration",
    }
}

fn failure_kind(value: TeamFailureKind) -> &'static str {
    match value {
        TeamFailureKind::Engineering => "engineering",
        TeamFailureKind::AgentProcess => "agent_process",
        TeamFailureKind::Infrastructure => "infrastructure",
        TeamFailureKind::Evaluation => "evaluation",
        TeamFailureKind::Assignment => "assignment",
        TeamFailureKind::Integration => "integration",
        TeamFailureKind::Review => "review",
    }
}

fn evaluation_subject_name(subject: &EvaluationSubject) -> String {
    match subject {
        EvaluationSubject::Run(run_id) => format!("run:{run_id}"),
        EvaluationSubject::TeamExecution(team_execution_id) => {
            format!("team_execution:{team_execution_id}")
        }
    }
}

fn team_event_type(payload: &TeamEventPayload) -> &'static str {
    match payload {
        TeamEventPayload::TeamStarted { .. } => "team_started",
        TeamEventPayload::TeamPlanResolved { .. } => "team_plan_resolved",
        TeamEventPayload::NodeReady { .. } => "node_ready",
        TeamEventPayload::NodeStarted { .. } => "node_started",
        TeamEventPayload::NodeCompleted { .. } => "node_completed",
        TeamEventPayload::NodeFailed { .. } => "node_failed",
        TeamEventPayload::NodeBlocked { .. } => "node_blocked",
        TeamEventPayload::ArtifactPublished { .. } => "artifact_published",
        TeamEventPayload::HandoffCompleted { .. } => "handoff_completed",
        TeamEventPayload::ReviewCompleted { .. } => "review_completed",
        TeamEventPayload::IntegrationStarted { .. } => "integration_started",
        TeamEventPayload::IntegrationCompleted { .. } => "integration_completed",
        TeamEventPayload::FinalEvaluationStarted { .. } => "final_evaluation_started",
        TeamEventPayload::FinalEvaluationCompleted { .. } => "final_evaluation_completed",
        TeamEventPayload::EvaluationLifecycle { event } => match event.event_type() {
            "EvaluationStarted" => "evaluation_started",
            "EvaluatorStarted" => "evaluator_started",
            "EvaluatorCompleted" => "evaluator_completed",
            "EvaluatorFailed" => "evaluator_failed",
            "EvaluationCompleted" => "evaluation_completed",
            _ => "evaluation_event",
        },
        TeamEventPayload::TeamCompleted { .. } => "team_completed",
        TeamEventPayload::TeamFailed { .. } => "team_failed",
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use forge_core::events::{EvaluationSubject, EventPayload};
    use forge_core::ids::{AgentId, TaskId, TeamNodeId};
    use forge_core::task::{EngineeringTask, TaskRevision};
    use forge_core::team::{
        PlanProvenance, PlanSourceKind, TeamArtifact, TeamArtifactContent, TeamAssignmentStrategy,
        TeamEventPayload, TeamExecution, TeamExecutionType, TeamPlan, TeamPlanNode,
    };

    use super::*;

    fn task() -> EngineeringTask {
        EngineeringTask {
            task_id: TaskId::sequential(1042),
            repository: "distributed-runtime".into(),
            objective: "Repair checkpoint contention".into(),
            constraints: vec!["Preserve recovery semantics".into()],
            evaluation: Default::default(),
            protection: Default::default(),
            metadata: Default::default(),
            classification: Default::default(),
            components: vec!["checkpoint".into()],
            tags: vec!["team".into()],
        }
    }

    fn plan() -> forge_core::ValidatedTeamPlan {
        TeamPlan::new(
            "Repair checkpoint contention",
            vec![TeamPlanNode {
                node_id: TeamNodeId::new("inspect").unwrap(),
                objective: "Inspect the contention".into(),
                execution: TeamExecutionType::Analysis,
                depends_on: Vec::new(),
                inputs: Vec::new(),
                outputs: vec![TeamArtifactKind::StructuredFindings],
                constraints: Vec::new(),
                required_capabilities: Vec::new(),
                assignment: Some(TeamAssignmentStrategy::Explicit {
                    agent: AgentId::new("claude").unwrap(),
                }),
                required: true,
            }],
        )
        .validate()
        .unwrap()
    }

    #[tokio::test]
    async fn team_round_trip_preserves_plan_artifacts_events_and_history() {
        let store = Store::open_in_memory().await.unwrap();
        let task = task();
        let revision_id = store.upsert_task(&task).await.unwrap();
        let id = store.next_team_execution_id().await.unwrap();
        let mut team = TeamExecution::new(
            id.clone(),
            task.task_id.clone(),
            revision_id.clone(),
            "abc123",
            plan(),
            PlanProvenance::explicit(),
        );
        let artifact = TeamArtifact::new(
            store.next_team_artifact_id().await.unwrap(),
            id.clone(),
            TeamNodeId::new("inspect").unwrap(),
            TeamArtifactKind::StructuredFindings,
            TeamArtifactContent::InlineJson {
                value: serde_json::json!({"summary": "lock held across I/O"}),
            },
            Utc::now(),
        )
        .unwrap();
        team.artifacts.push(artifact.clone());
        team.nodes[0]
            .output_artifact_ids
            .push(artifact.artifact_id.clone());
        store.save_team_execution(&team).await.unwrap();

        let events = vec![
            TeamEvent {
                team_execution_id: id.clone(),
                seq: 1,
                timestamp: Utc::now(),
                payload: TeamEventPayload::TeamStarted {
                    task_id: task.task_id,
                    base_commit: "abc123".into(),
                },
            },
            TeamEvent {
                team_execution_id: id.clone(),
                seq: 2,
                timestamp: Utc::now(),
                payload: TeamEventPayload::EvaluationLifecycle {
                    event: EventPayload::EvaluationStarted {
                        subject: EvaluationSubject::TeamExecution(id.clone()),
                        evaluators: vec!["tests".into()],
                    },
                },
            },
        ];
        store.append_team_events(&events).await.unwrap();

        assert_eq!(store.load_team_execution(&id).await.unwrap(), Some(team));
        assert_eq!(store.team_events_for(&id).await.unwrap(), events);
        let evaluation_events = store.evaluation_events_for_team(&id).await.unwrap();
        assert_eq!(evaluation_events.len(), 1);
        assert_eq!(
            evaluation_events[0].evaluation_subject(),
            Some(&EvaluationSubject::TeamExecution(id.clone()))
        );
        let history = store.teams_for_task(&revision_id).await.unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].team_execution_id, id);
    }

    #[tokio::test]
    async fn historical_plan_and_plan_provenance_are_immutable() {
        let store = Store::open_in_memory().await.unwrap();
        let task = task();
        let revision_id = store.upsert_task(&task).await.unwrap();
        let mut team = TeamExecution::new(
            store.next_team_execution_id().await.unwrap(),
            task.task_id,
            revision_id,
            "abc123",
            plan(),
            PlanProvenance::explicit(),
        );
        store.save_team_execution(&team).await.unwrap();

        team.plan_provenance.source = PlanSourceKind::Imported;
        assert!(matches!(
            store.save_team_execution(&team).await.unwrap_err(),
            StoreError::TeamPlanConflict { .. }
        ));
    }

    #[tokio::test]
    async fn completed_team_results_cannot_be_rewritten() {
        let store = Store::open_in_memory().await.unwrap();
        let task = task();
        let revision_id = store.upsert_task(&task).await.unwrap();
        let mut team = TeamExecution::new(
            store.next_team_execution_id().await.unwrap(),
            task.task_id,
            revision_id,
            "abc123",
            plan(),
            PlanProvenance::explicit(),
        );
        team.status = forge_core::TeamStatus::Completed;
        team.outcome = Some(forge_core::TeamOutcome::Inconclusive);
        team.completed_at = Some(Utc::now());
        store.save_team_execution(&team).await.unwrap();
        store.save_team_execution(&team).await.unwrap();

        team.failure_reason = Some("rewritten after completion".into());
        assert!(matches!(
            store.save_team_execution(&team).await.unwrap_err(),
            StoreError::TeamExecutionFinalized { .. }
        ));
    }

    #[test]
    fn task_revision_used_by_team_is_content_addressed() {
        let task = task();
        let revision = TaskRevision::snapshot(task).unwrap();
        assert!(revision.revision_id().as_str().starts_with("TR-"));
    }
}
