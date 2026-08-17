//! Trusted routing-evidence queries over the immutable experience ledger.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use forge_core::ids::{AgentId, ExperimentId, RunId};
use forge_core::routing::{
    AgentEvidenceCount, EvidenceExclusionCount, EvidenceExclusionReason, ExcludedRoutingEvidence,
    RoutingEvidence, RoutingEvidenceRecord, RoutingEvidenceSnapshot, RoutingEvidenceSummary,
    RoutingFeatures, RoutingReadiness, RoutingReadinessReason, RoutingRequest, RoutingTarget,
    UnresolvedRoutingTarget,
};
use forge_core::run::{AgentExecutionStatus, AgentRun, ExecutionProvenance, RunOutcome, RunStatus};
use forge_core::task::{EngineeringTask, TaskRevision};
use sqlx::{QueryBuilder, Row, Sqlite};
use tokio::sync::Barrier;

use crate::experience::{TaskExperience, similarity};
use crate::{Store, StoreError, StoreResult};

impl Store {
    /// Retrieves compact, policy-filtered observations for the exact immutable
    /// request. SQL and ledger decoding remain entirely inside `forge-store`.
    ///
    /// All mutable ledger inputs are read by one SQLite statement. SQLite
    /// therefore supplies one snapshot even while other connections complete
    /// runs or evaluations; routing never stitches together profiles, run
    /// records, and evaluator summaries observed at different instants.
    pub async fn routing_evidence(&self, request: &RoutingRequest) -> StoreResult<RoutingEvidence> {
        self.routing_evidence_in_snapshot(request, None).await
    }

    async fn routing_evidence_in_snapshot(
        &self,
        request: &RoutingRequest,
        test_barriers: Option<(std::sync::Arc<Barrier>, std::sync::Arc<Barrier>)>,
    ) -> StoreResult<RoutingEvidence> {
        let mut transaction = self.pool().begin().await?;
        // Establish the SQLite read snapshot before any routing inputs are
        // collected. The count is deliberately unused; all domain input still
        // comes from the single joined statement below.
        let _: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runs")
            .fetch_one(&mut *transaction)
            .await?;
        if let Some((ready, resume)) = test_barriers {
            ready.wait().await;
            resume.wait().await;
        }
        let target = TaskExperience {
            revision_id: request.task_revision().revision_id().clone(),
            task_id: request.task_revision().task().task_id.clone(),
            repository: request.task_revision().task().repository.clone(),
            objective: request.task_revision().task().objective.clone(),
            definition: request.task_revision().task().clone(),
            classification: request.task_revision().task().effective_classification(),
            components: request.task_revision().task().components.clone(),
            tags: request.task_revision().task().tags.clone(),
        };

        let candidate_ids = request
            .candidates()
            .agent_ids()
            .map(AgentId::as_str)
            .collect::<Vec<_>>();
        let mut query = QueryBuilder::<Sqlite>::new(
            "SELECT r.run_id, r.task_revision_id, r.agent_id, r.config_fingerprint, \
                    r.experiment_id, r.execution_provenance, r.created_at, r.finished_at, \
                    r.record_json, tr.definition_json, e.summary_json, \
                    e.finished_at AS evaluation_finished_at \
             FROM runs r \
             JOIN task_revisions tr ON tr.revision_id = r.task_revision_id \
             LEFT JOIN evaluations e ON e.run_id = r.run_id \
             WHERE r.created_at <= ",
        );
        query.push_bind(request.historical_cutoff().to_rfc3339());
        query.push(" AND r.agent_id IN (");
        let mut separated = query.separated(", ");
        for agent_id in &candidate_ids {
            separated.push_bind(agent_id);
        }
        separated.push_unseparated(") ORDER BY r.created_at, r.run_id");
        let rows = query.build().fetch_all(&mut *transaction).await?;
        transaction.commit().await?;
        let historical_runs_found = rows.len() as u64;

        let mut eligible = Vec::new();
        let mut excluded = Vec::new();
        let mut comparable_revisions = BTreeSet::new();
        for row in rows {
            let run_id = parse_run_id(row.try_get("run_id")?)?;
            let revision_id = forge_core::TaskRevisionId::from_stored(
                row.try_get::<String, _>("task_revision_id")?,
            )
            .map_err(|error| StoreError::Corrupt(error.to_string()))?;
            let definition: EngineeringTask =
                serde_json::from_str(&row.try_get::<String, _>("definition_json")?)?;
            let historical_revision = TaskRevision::from_stored(revision_id.clone(), definition)
                .map_err(|error| StoreError::Corrupt(error.to_string()))?;
            let profile = TaskExperience {
                revision_id: revision_id.clone(),
                task_id: historical_revision.task().task_id.clone(),
                repository: historical_revision.task().repository.clone(),
                objective: historical_revision.task().objective.clone(),
                definition: historical_revision.task().clone(),
                classification: historical_revision.task().effective_classification(),
                components: historical_revision.task().components.clone(),
                tags: historical_revision.task().tags.clone(),
            };
            let (similarity_score, similarity_reasons) = similarity(&target, &profile);
            if similarity_score >= request.evidence_policy().minimum_similarity_score {
                comparable_revisions.insert(revision_id.clone());
            }

            let run: AgentRun = serde_json::from_str(&row.try_get::<String, _>("record_json")?)?;
            if run.run_id != run_id
                || run.created_at.to_rfc3339() != row.try_get::<String, _>("created_at")?
            {
                return Err(StoreError::Corrupt(format!(
                    "run `{run_id}` differs between indexed and complete records"
                )));
            }
            let stored_provenance: ExecutionProvenance =
                parse_enum(&row.try_get::<String, _>("execution_provenance")?)?;
            if run.execution_provenance != stored_provenance {
                return Err(StoreError::Corrupt(format!(
                    "run `{run_id}` provenance differs between indexed and complete records"
                )));
            }
            let evaluation = row
                .try_get::<Option<String>, _>("summary_json")?
                .map(|raw| serde_json::from_str::<forge_core::EvaluationSummary>(&raw))
                .transpose()?;
            let evaluation_finished_at = row
                .try_get::<Option<String>, _>("evaluation_finished_at")?
                .map(|raw| parse_time(&raw))
                .transpose()?;
            let exclusion = exclusion_reason(
                &run,
                evaluation.as_ref(),
                evaluation_finished_at,
                similarity_score,
                request,
            );
            if let Some(reason) = exclusion {
                excluded.push(ExcludedRoutingEvidence { run_id, reason });
                continue;
            }

            let outcome = run.outcome.ok_or_else(|| {
                StoreError::Corrupt(format!(
                    "eligible routing run `{}` has no outcome",
                    run.run_id
                ))
            })?;
            let target = RoutingTarget::from_outcome(outcome).ok_or_else(|| {
                StoreError::Corrupt(format!(
                    "eligible routing run `{}` has infrastructure outcome",
                    run.run_id
                ))
            })?;
            let execution = run.execution.as_ref().ok_or_else(|| {
                StoreError::Corrupt(format!(
                    "eligible routing run `{}` has no execution",
                    run.run_id
                ))
            })?;
            let experiment_id = row
                .try_get::<Option<String>, _>("experiment_id")?
                .map(parse_experiment_id)
                .transpose()?;
            let config_fingerprint: String = row.try_get("config_fingerprint")?;
            if config_fingerprint != run.agent.fingerprint() {
                return Err(StoreError::Corrupt(format!(
                    "run `{}` agent configuration fingerprint is inconsistent",
                    run.run_id
                )));
            }
            eligible.push(RoutingEvidenceRecord {
                run_id: run.run_id.clone(),
                task_revision_id: revision_id,
                agent_id: run.agent.agent_id.clone(),
                agent_config: run.agent.clone(),
                config_fingerprint,
                features: RoutingFeatures::from_revision(&historical_revision),
                similarity_score,
                similarity_reasons,
                run_status: run.status,
                agent_status: execution.status,
                outcome,
                target,
                integrity: run.integrity.as_ref().map(|value| value.status),
                evaluator_summary: evaluation,
                agent_runtime_ms: Some(execution.duration_ms),
                provider_reported_usage: execution.usage.clone(),
                known_cost_usd: execution.usage.cost_usd,
                provenance: run.execution_provenance,
                selection_source: run.selection_source.clone(),
                experiment_id,
                created_at: run.created_at,
            });
        }

        eligible.sort_by(|left, right| {
            left.created_at
                .cmp(&right.created_at)
                .then_with(|| left.run_id.cmp(&right.run_id))
        });
        excluded.sort_by(|left, right| left.run_id.cmp(&right.run_id));
        let summary = evidence_summary(
            request,
            historical_runs_found,
            comparable_revisions.len() as u64,
            &eligible,
            &excluded,
        );
        let readiness = readiness(request, &summary);
        let snapshot = RoutingEvidenceSnapshot::build(request, &eligible, &excluded)?;
        Ok(RoutingEvidence {
            eligible,
            excluded,
            summary,
            readiness,
            snapshot,
        })
    }
}

fn exclusion_reason(
    run: &forge_core::AgentRun,
    evaluation: Option<&forge_core::EvaluationSummary>,
    evaluation_finished_at: Option<DateTime<Utc>>,
    similarity_score: f64,
    request: &RoutingRequest,
) -> Option<EvidenceExclusionReason> {
    let policy = request.evidence_policy();
    let expected = request
        .candidates()
        .as_slice()
        .iter()
        .find(|candidate| candidate.agent_id == run.agent.agent_id)
        .expect("routing SQL filters runs to requested candidate agents");
    let actual_fingerprint = run.agent.fingerprint();
    if actual_fingerprint != expected.config_fingerprint {
        return Some(EvidenceExclusionReason::CandidateConfigurationMismatch {
            agent_id: run.agent.agent_id.clone(),
            config_fingerprint: actual_fingerprint,
        });
    }
    if !policy
        .allowed_provenance
        .contains(&run.execution_provenance)
    {
        return Some(match run.execution_provenance {
            ExecutionProvenance::Synthetic => EvidenceExclusionReason::SyntheticProvenance,
            ExecutionProvenance::Unknown => EvidenceExclusionReason::UnknownProvenance,
            ExecutionProvenance::Imported => EvidenceExclusionReason::ImportedProvenance,
            provenance => EvidenceExclusionReason::ProvenanceNotAllowed { provenance },
        });
    }
    match run.finished_at {
        Some(completed_at) if completed_at > request.historical_cutoff() => {
            return Some(EvidenceExclusionReason::OutcomeAvailableAfterCutoff { completed_at });
        }
        None if run.status == RunStatus::Completed => {
            return Some(EvidenceExclusionReason::MissingCompletionTimestamp);
        }
        _ => {}
    }
    if matches!(run.status, RunStatus::Failed | RunStatus::Cancelled)
        || run.outcome == Some(RunOutcome::Errored)
    {
        return Some(EvidenceExclusionReason::InfrastructureFailure);
    }
    if policy.require_completed_run && run.status != RunStatus::Completed {
        return Some(EvidenceExclusionReason::IncompleteRun { status: run.status });
    }
    let Some(execution) = run.execution.as_ref() else {
        return policy
            .require_execution_record
            .then_some(EvidenceExclusionReason::MissingExecution);
    };
    if matches!(
        execution.status,
        AgentExecutionStatus::StartFailed | AgentExecutionStatus::Cancelled
    ) {
        return Some(EvidenceExclusionReason::InfrastructureFailure);
    }
    let Some(outcome) = run.outcome else {
        return Some(EvidenceExclusionReason::MissingOutcome);
    };
    if outcome == RunOutcome::Errored {
        return Some(EvidenceExclusionReason::InfrastructureFailure);
    }
    if let Some(completed_at) = evaluation_finished_at
        && completed_at > request.historical_cutoff()
    {
        return Some(EvidenceExclusionReason::EvaluationAvailableAfterCutoff { completed_at });
    }
    let integrity = run.integrity.as_ref().map(|value| value.status);
    if integrity.is_none() && policy.require_acceptable_integrity {
        return Some(EvidenceExclusionReason::MissingIntegrity);
    }
    if let Some(status) = integrity
        && status != forge_core::IntegrityStatus::Clean
        && (policy.require_acceptable_integrity || outcome == RunOutcome::Passed)
    {
        return Some(EvidenceExclusionReason::IntegrityViolation { status });
    }
    if similarity_score < policy.minimum_similarity_score {
        return Some(EvidenceExclusionReason::InsufficientSimilarity);
    }
    if outcome != RunOutcome::NoChange && evaluation.is_none() {
        return Some(EvidenceExclusionReason::MissingEvaluation);
    }
    if policy.exclude_evaluator_infrastructure_errors
        && evaluation.is_some_and(|summary| summary.execution_errors > 0)
    {
        return Some(EvidenceExclusionReason::EvaluatorInfrastructureFailure);
    }
    None
}

fn evidence_summary(
    request: &RoutingRequest,
    historical_runs_found: u64,
    similar_task_revisions: u64,
    eligible: &[RoutingEvidenceRecord],
    excluded: &[ExcludedRoutingEvidence],
) -> RoutingEvidenceSummary {
    let mut exclusion_counts = BTreeMap::new();
    for record in excluded {
        *exclusion_counts.entry(record.reason.clone()).or_insert(0) += 1;
    }
    let excluded = exclusion_counts
        .into_iter()
        .map(|(reason, count)| EvidenceExclusionCount { reason, count })
        .collect();

    let mut counts = request
        .candidates()
        .agent_ids()
        .cloned()
        .map(|agent_id| {
            (
                agent_id.clone(),
                AgentEvidenceCount {
                    agent_id,
                    eligible: 0,
                    resolved: 0,
                    positive: 0,
                    negative: 0,
                    inconclusive: 0,
                    no_change: 0,
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
    for record in eligible {
        let count = counts
            .get_mut(&record.agent_id)
            .expect("eligible records were filtered to requested candidates");
        count.eligible += 1;
        match record.target {
            RoutingTarget::Positive => {
                count.resolved += 1;
                count.positive += 1;
            }
            RoutingTarget::Negative => {
                count.resolved += 1;
                count.negative += 1;
            }
            RoutingTarget::Unresolved(UnresolvedRoutingTarget::Inconclusive) => {
                count.inconclusive += 1;
            }
            RoutingTarget::Unresolved(UnresolvedRoutingTarget::NoChange) => {
                count.no_change += 1;
            }
        }
    }
    RoutingEvidenceSummary {
        historical_runs_found,
        eligible_runs: eligible.len() as u64,
        resolved_runs: eligible
            .iter()
            .filter(|record| record.target.is_resolved())
            .count() as u64,
        similar_task_revisions,
        excluded,
        per_agent: counts.into_values().collect(),
    }
}

fn readiness(request: &RoutingRequest, summary: &RoutingEvidenceSummary) -> RoutingReadiness {
    let minimum = request.minimum_evidence();
    let mut reasons = Vec::new();
    if summary.eligible_runs == 0 {
        reasons.push(RoutingReadinessReason::NoEligibleLiveHistory);
    }
    if summary.similar_task_revisions == 0 {
        reasons.push(RoutingReadinessReason::NoComparableHistoricalTasks);
    }
    let agents_with_resolved = summary
        .per_agent
        .iter()
        .filter(|agent| agent.resolved > 0)
        .count();
    if summary.per_agent.len() > 1 && agents_with_resolved == 1 {
        reasons.push(RoutingReadinessReason::OnlyOneCandidateHasResolvedEvidence);
    }
    if summary.resolved_runs < minimum.total {
        reasons.push(RoutingReadinessReason::InsufficientTotalEvidence {
            available: summary.resolved_runs,
            required: minimum.total,
        });
    }
    for agent in &summary.per_agent {
        if agent.resolved < minimum.per_agent {
            reasons.push(RoutingReadinessReason::InsufficientAgentEvidence {
                agent_id: agent.agent_id.clone(),
                available: agent.resolved,
                required: minimum.per_agent,
            });
        }
    }
    if reasons.is_empty() {
        RoutingReadiness::Ready
    } else {
        RoutingReadiness::InsufficientEvidence {
            reasons,
            eligible_runs: summary.eligible_runs,
            resolved_runs: summary.resolved_runs,
            required_runs: minimum.total,
        }
    }
}

fn parse_run_id(raw: String) -> StoreResult<RunId> {
    RunId::new(raw).map_err(|error| StoreError::Corrupt(error.to_string()))
}

fn parse_experiment_id(raw: String) -> StoreResult<ExperimentId> {
    ExperimentId::new(raw).map_err(|error| StoreError::Corrupt(error.to_string()))
}

fn parse_enum<T: serde::de::DeserializeOwned>(raw: &str) -> StoreResult<T> {
    Ok(serde_json::from_value(serde_json::Value::String(
        raw.to_string(),
    ))?)
}

fn parse_time(raw: &str) -> StoreResult<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|error| StoreError::Corrupt(format!("invalid timestamp `{raw}`: {error}")))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use chrono::{DateTime, TimeDelta, Utc};
    use forge_core::agent::AgentConfig;
    use forge_core::ids::{AgentId, RunId, TaskId};
    use forge_core::integrity::{EvaluationIntegrity, IntegrityStatus, ProtectionPolicy};
    use forge_core::result::{
        CheckResult, Evaluation, EvaluatorExecutionStatus, EvaluatorKind, Verdict,
    };
    use forge_core::routing::{
        CandidateAgent, CandidateAgentSet, EvidenceExclusionReason, ExplorationPolicy,
        MinimumRoutingEvidence, RoutingEvidencePolicy, RoutingReadiness, RoutingReadinessReason,
        RoutingRequest, RoutingTarget, UnresolvedRoutingTarget,
    };
    use forge_core::run::{
        AgentExecution, AgentExecutionStatus, AgentRun, ExecutionProvenance, PatchSummary,
        RunOutcome, RunStatus, Usage,
    };
    use forge_core::task::{
        EngineeringTask, EvaluationSpec, TaskClassification, TaskMetadata, TaskRevision,
    };

    use super::*;
    use crate::{EXPORT_SCHEMA_VERSION, HistoryFilter};

    fn task(category: &str, domain: &str, objective: &str) -> EngineeringTask {
        EngineeringTask {
            task_id: TaskId::sequential(1042),
            repository: "forge".into(),
            objective: objective.into(),
            constraints: Vec::new(),
            evaluation: EvaluationSpec::default(),
            protection: ProtectionPolicy::default(),
            metadata: TaskMetadata::default(),
            classification: TaskClassification {
                category: Some(category.into()),
                language: Some("rust".into()),
                domain: Some(domain.into()),
                difficulty: Some("medium".into()),
            },
            components: vec!["scheduler".into()],
            tags: vec!["regression".into()],
        }
    }

    fn config(agent: &str) -> AgentConfig {
        AgentConfig::new(
            AgentId::new(agent).unwrap(),
            format!("{agent}-test-harness"),
        )
        .with_model(format!("{agent}-model"))
    }

    fn candidates() -> CandidateAgentSet {
        CandidateAgentSet::new(
            ["alpha", "beta"]
                .into_iter()
                .map(|agent| {
                    CandidateAgent::new(AgentId::new(agent).unwrap(), config(agent)).unwrap()
                })
                .collect(),
        )
        .unwrap()
    }

    fn request(
        revision: TaskRevision,
        cutoff: DateTime<Utc>,
        minimum: MinimumRoutingEvidence,
    ) -> RoutingRequest {
        RoutingRequest::new(
            revision,
            candidates(),
            RoutingEvidencePolicy::default(),
            minimum,
            ExplorationPolicy::CompeteWhenUncertain,
            cutoff,
        )
    }

    struct Observation {
        id: u64,
        agent: &'static str,
        provenance: ExecutionProvenance,
        run_status: RunStatus,
        agent_status: AgentExecutionStatus,
        outcome: Option<RunOutcome>,
        integrity: Option<IntegrityStatus>,
        evaluator_error: bool,
    }

    async fn persist(
        store: &Store,
        task: &EngineeringTask,
        observation: Observation,
        at: DateTime<Utc>,
    ) {
        let revision_id = store.upsert_task(task).await.unwrap();
        let started = at + TimeDelta::milliseconds(10);
        let finished = started + TimeDelta::milliseconds(25);
        let mut run = AgentRun::new(
            RunId::sequential(observation.id),
            task.task_id.clone(),
            config(observation.agent),
            "base-commit",
        );
        run.execution_provenance = observation.provenance;
        run.status = observation.run_status;
        run.created_at = at;
        run.started_at = Some(started);
        run.finished_at = observation.run_status.is_terminal().then_some(finished);
        run.execution = Some(AgentExecution {
            status: observation.agent_status,
            exit_code: (observation.agent_status == AgentExecutionStatus::Completed).then_some(0),
            timed_out: observation.agent_status == AgentExecutionStatus::TimedOut,
            started_at: started,
            finished_at: finished,
            duration_ms: 25,
            stdout_path: None,
            stderr_path: None,
            usage: Usage {
                input_tokens: Some(10),
                output_tokens: Some(5),
                cost_usd: Some(0.01),
            },
            self_report: None,
            harness_metadata: BTreeMap::new(),
            infrastructure_failures: Vec::new(),
        });
        run.patch = Some(PatchSummary {
            base_commit: "base-commit".into(),
            head_commit: Some(format!("candidate-{}", observation.id)),
            files_changed: if observation.outcome == Some(RunOutcome::NoChange) {
                0
            } else {
                1
            },
            insertions: if observation.outcome == Some(RunOutcome::NoChange) {
                0
            } else {
                2
            },
            deletions: 0,
            binary_files: 0,
            diff_path: None,
            excluded: Vec::new(),
            excluded_counts: Default::default(),
        });
        run.integrity = observation.integrity.map(|status| EvaluationIntegrity {
            status,
            modified: if status == IntegrityStatus::Modified {
                vec!["tests/routing.rs".into()]
            } else {
                Vec::new()
            },
            added: Vec::new(),
            deleted: Vec::new(),
            allowed: Vec::new(),
        });
        run.outcome = observation.outcome;
        run.evaluation_verdict = observation.outcome.map(|outcome| match outcome {
            RunOutcome::Passed => Verdict::Pass,
            RunOutcome::Failed => Verdict::Fail,
            _ => Verdict::Inconclusive,
        });
        store
            .save_run_at_task_revision(&run, None, &revision_id)
            .await
            .unwrap();

        if let Some(outcome) = observation.outcome
            && outcome != RunOutcome::NoChange
            && outcome != RunOutcome::Errored
        {
            let check = if observation.evaluator_error {
                CheckResult::execution_error(
                    "tests",
                    EvaluatorKind::Test,
                    true,
                    "could not start evaluator",
                )
            } else {
                let verdict = match outcome {
                    RunOutcome::Passed => Verdict::Pass,
                    RunOutcome::Failed => Verdict::Fail,
                    _ => Verdict::Inconclusive,
                };
                CheckResult {
                    name: "tests".into(),
                    kind: EvaluatorKind::Test,
                    required: true,
                    verdict,
                    execution_status: EvaluatorExecutionStatus::Completed,
                    command: Some("cargo test".into()),
                    exit_code: Some(if verdict == Verdict::Pass { 0 } else { 1 }),
                    duration_ms: 10,
                    detail: None,
                    output_path: None,
                    metrics: Vec::new(),
                    warnings: Vec::new(),
                    execution_error: None,
                    infrastructure_failures: Vec::new(),
                }
            };
            let evaluation =
                Evaluation::from_checks(run.run_id.clone(), vec![check], started, finished);
            store.record_evaluation(&evaluation).await.unwrap();
        }
    }

    #[tokio::test]
    async fn completed_run_committed_after_snapshot_start_cannot_leak_into_decision() {
        let fixture = tempfile::tempdir().unwrap();
        let store = Store::open(fixture.path().join("routing.db"))
            .await
            .unwrap();
        let task = task(
            "debugging",
            "concurrency",
            "Repair concurrent queue wakeup ordering",
        );
        let start = DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        persist(
            &store,
            &task,
            Observation {
                id: 1,
                agent: "alpha",
                provenance: ExecutionProvenance::Live,
                run_status: RunStatus::Completed,
                agent_status: AgentExecutionStatus::Completed,
                outcome: Some(RunOutcome::Passed),
                integrity: Some(IntegrityStatus::Clean),
                evaluator_error: false,
            },
            start,
        )
        .await;

        let route_request = request(
            TaskRevision::snapshot(task.clone()).unwrap(),
            start + TimeDelta::days(1),
            MinimumRoutingEvidence {
                total: 1,
                per_agent: 0,
            },
        );
        let ready = Arc::new(Barrier::new(2));
        let resume = Arc::new(Barrier::new(2));
        let reader = store.clone();
        let reader_ready = ready.clone();
        let reader_resume = resume.clone();
        let read = tokio::spawn(async move {
            reader
                .routing_evidence_in_snapshot(&route_request, Some((reader_ready, reader_resume)))
                .await
                .unwrap()
        });
        ready.wait().await;

        persist(
            &store,
            &task,
            Observation {
                id: 2,
                agent: "beta",
                provenance: ExecutionProvenance::Live,
                run_status: RunStatus::Completed,
                agent_status: AgentExecutionStatus::Completed,
                outcome: Some(RunOutcome::Passed),
                integrity: Some(IntegrityStatus::Clean),
                evaluator_error: false,
            },
            start + TimeDelta::seconds(1),
        )
        .await;
        resume.wait().await;

        let evidence = read.await.unwrap();
        assert_eq!(
            evidence
                .eligible
                .iter()
                .map(|record| record.run_id.as_str())
                .collect::<Vec<_>>(),
            vec!["R-0001"]
        );
        let after = store
            .routing_evidence(&request(
                TaskRevision::snapshot(task).unwrap(),
                start + TimeDelta::days(1),
                MinimumRoutingEvidence {
                    total: 1,
                    per_agent: 0,
                },
            ))
            .await
            .unwrap();
        assert_eq!(after.eligible.len(), 2);
    }

    #[tokio::test]
    async fn default_policy_preserves_targets_and_explains_every_trust_exclusion() {
        let store = Store::open_in_memory().await.unwrap();
        let task = task(
            "debugging",
            "concurrency",
            "Repair concurrent queue wakeup ordering",
        );
        let revision = TaskRevision::snapshot(task.clone()).unwrap();
        let start = DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let observations = [
            Observation {
                id: 1,
                agent: "alpha",
                provenance: ExecutionProvenance::Live,
                run_status: RunStatus::Completed,
                agent_status: AgentExecutionStatus::Completed,
                outcome: Some(RunOutcome::Passed),
                integrity: Some(IntegrityStatus::Clean),
                evaluator_error: false,
            },
            Observation {
                id: 2,
                agent: "beta",
                provenance: ExecutionProvenance::Live,
                run_status: RunStatus::Completed,
                agent_status: AgentExecutionStatus::Completed,
                outcome: Some(RunOutcome::Failed),
                integrity: Some(IntegrityStatus::Clean),
                evaluator_error: false,
            },
            Observation {
                id: 3,
                agent: "alpha",
                provenance: ExecutionProvenance::Live,
                run_status: RunStatus::Completed,
                agent_status: AgentExecutionStatus::NonZeroExit,
                outcome: Some(RunOutcome::Inconclusive),
                integrity: Some(IntegrityStatus::Clean),
                evaluator_error: false,
            },
            Observation {
                id: 4,
                agent: "beta",
                provenance: ExecutionProvenance::Live,
                run_status: RunStatus::Completed,
                agent_status: AgentExecutionStatus::Completed,
                outcome: Some(RunOutcome::NoChange),
                integrity: Some(IntegrityStatus::Clean),
                evaluator_error: false,
            },
            Observation {
                id: 5,
                agent: "alpha",
                provenance: ExecutionProvenance::Synthetic,
                run_status: RunStatus::Completed,
                agent_status: AgentExecutionStatus::Completed,
                outcome: Some(RunOutcome::Passed),
                integrity: Some(IntegrityStatus::Clean),
                evaluator_error: false,
            },
            Observation {
                id: 6,
                agent: "beta",
                provenance: ExecutionProvenance::Unknown,
                run_status: RunStatus::Completed,
                agent_status: AgentExecutionStatus::Completed,
                outcome: Some(RunOutcome::Passed),
                integrity: Some(IntegrityStatus::Clean),
                evaluator_error: false,
            },
            // Deliberately inconsistent historical record: policy must never
            // let modified integrity become positive evidence.
            Observation {
                id: 7,
                agent: "alpha",
                provenance: ExecutionProvenance::Live,
                run_status: RunStatus::Completed,
                agent_status: AgentExecutionStatus::Completed,
                outcome: Some(RunOutcome::Passed),
                integrity: Some(IntegrityStatus::Modified),
                evaluator_error: false,
            },
            Observation {
                id: 8,
                agent: "beta",
                provenance: ExecutionProvenance::Live,
                run_status: RunStatus::Failed,
                agent_status: AgentExecutionStatus::StartFailed,
                outcome: Some(RunOutcome::Errored),
                integrity: None,
                evaluator_error: false,
            },
            Observation {
                id: 9,
                agent: "alpha",
                provenance: ExecutionProvenance::Live,
                run_status: RunStatus::Completed,
                agent_status: AgentExecutionStatus::Completed,
                outcome: Some(RunOutcome::Inconclusive),
                integrity: Some(IntegrityStatus::Clean),
                evaluator_error: true,
            },
        ];
        for observation in observations {
            let id = observation.id;
            persist(
                &store,
                &task,
                observation,
                start + TimeDelta::seconds(id as i64),
            )
            .await;
        }

        let routing = request(
            revision,
            start + TimeDelta::minutes(1),
            MinimumRoutingEvidence {
                total: 2,
                per_agent: 1,
            },
        );
        let evidence = store.routing_evidence(&routing).await.unwrap();
        assert_eq!(evidence.summary.historical_runs_found, 9);
        assert_eq!(evidence.summary.eligible_runs, 4);
        assert_eq!(evidence.summary.resolved_runs, 2);
        assert_eq!(evidence.readiness, RoutingReadiness::Ready);
        assert_eq!(
            evidence
                .eligible
                .iter()
                .map(|record| record.target)
                .collect::<Vec<_>>(),
            vec![
                RoutingTarget::Positive,
                RoutingTarget::Negative,
                RoutingTarget::Unresolved(UnresolvedRoutingTarget::Inconclusive),
                RoutingTarget::Unresolved(UnresolvedRoutingTarget::NoChange),
            ]
        );
        let exclusions = evidence
            .summary
            .excluded
            .iter()
            .map(|excluded| (&excluded.reason, excluded.count))
            .collect::<BTreeMap<_, _>>();
        for reason in [
            EvidenceExclusionReason::SyntheticProvenance,
            EvidenceExclusionReason::UnknownProvenance,
            EvidenceExclusionReason::IntegrityViolation {
                status: IntegrityStatus::Modified,
            },
            EvidenceExclusionReason::InfrastructureFailure,
            EvidenceExclusionReason::EvaluatorInfrastructureFailure,
        ] {
            assert_eq!(exclusions.get(&reason), Some(&1), "missing {reason:?}");
        }

        // Ordinary Phase 3 surfaces retain every run, including stubs.
        assert_eq!(
            store
                .history(&HistoryFilter {
                    task_id: Some(task.task_id.clone()),
                    limit: 20,
                    ..Default::default()
                })
                .await
                .unwrap()
                .len(),
            9
        );
        let export = store.export_records().await.unwrap();
        assert_eq!(export.len(), 9);
        assert!(
            export
                .iter()
                .all(|record| record.schema_version == EXPORT_SCHEMA_VERSION)
        );
        assert_eq!(
            export
                .iter()
                .find(|record| record.run_id == RunId::sequential(5))
                .unwrap()
                .execution_provenance,
            ExecutionProvenance::Synthetic
        );

        // Identical ledger state and request produce identical evidence order
        // and fingerprint.
        let repeated = store.routing_evidence(&routing).await.unwrap();
        assert_eq!(evidence.eligible, repeated.eligible);
        assert_eq!(
            evidence.snapshot.evidence_fingerprint,
            repeated.snapshot.evidence_fingerprint
        );
    }

    #[tokio::test]
    async fn readiness_reports_total_and_per_agent_shortfalls_without_recommending() {
        let store = Store::open_in_memory().await.unwrap();
        let task = task("debugging", "concurrency", "Repair queue ordering");
        let revision = TaskRevision::snapshot(task.clone()).unwrap();
        let start = Utc::now() - TimeDelta::minutes(1);
        persist(
            &store,
            &task,
            Observation {
                id: 1,
                agent: "alpha",
                provenance: ExecutionProvenance::Live,
                run_status: RunStatus::Completed,
                agent_status: AgentExecutionStatus::Completed,
                outcome: Some(RunOutcome::Passed),
                integrity: Some(IntegrityStatus::Clean),
                evaluator_error: false,
            },
            start,
        )
        .await;
        let evidence = store
            .routing_evidence(&request(
                revision,
                Utc::now(),
                MinimumRoutingEvidence {
                    total: 10,
                    per_agent: 3,
                },
            ))
            .await
            .unwrap();
        let RoutingReadiness::InsufficientEvidence { reasons, .. } = evidence.readiness else {
            panic!("one observation must not force a recommendation");
        };
        assert!(reasons.contains(&RoutingReadinessReason::OnlyOneCandidateHasResolvedEvidence));
        assert!(reasons.iter().any(|reason| matches!(
            reason,
            RoutingReadinessReason::InsufficientTotalEvidence {
                available: 1,
                required: 10
            }
        )));
        assert!(reasons.iter().any(|reason| matches!(
            reason,
            RoutingReadinessReason::InsufficientAgentEvidence {
                agent_id,
                available: 0,
                required: 3
            } if agent_id.as_str() == "beta"
        )));
    }

    #[tokio::test]
    async fn synthetic_and_unknown_history_cannot_make_routing_ready() {
        let store = Store::open_in_memory().await.unwrap();
        let task = task("debugging", "concurrency", "Repair queue ordering");
        let revision = TaskRevision::snapshot(task.clone()).unwrap();
        let start = Utc::now() - TimeDelta::minutes(1);
        for (id, agent, provenance) in [
            (1, "alpha", ExecutionProvenance::Synthetic),
            (2, "beta", ExecutionProvenance::Unknown),
        ] {
            persist(
                &store,
                &task,
                Observation {
                    id,
                    agent,
                    provenance,
                    run_status: RunStatus::Completed,
                    agent_status: AgentExecutionStatus::Completed,
                    outcome: Some(RunOutcome::Passed),
                    integrity: Some(IntegrityStatus::Clean),
                    evaluator_error: false,
                },
                start + TimeDelta::seconds(id as i64),
            )
            .await;
        }
        let evidence = store
            .routing_evidence(&request(
                revision,
                Utc::now(),
                MinimumRoutingEvidence {
                    total: 1,
                    per_agent: 1,
                },
            ))
            .await
            .unwrap();
        assert!(evidence.eligible.is_empty());
        assert!(matches!(
            &evidence.readiness,
            RoutingReadiness::InsufficientEvidence { reasons, .. }
                if reasons.contains(&RoutingReadinessReason::NoEligibleLiveHistory)
        ));
    }

    #[tokio::test]
    async fn routing_uses_run_bound_historical_revision_after_logical_task_edit() {
        let store = Store::open_in_memory().await.unwrap();
        let old = task("debugging", "concurrency", "Repair queue ordering");
        let old_revision = TaskRevision::snapshot(old.clone()).unwrap();
        let start = Utc::now() - TimeDelta::minutes(1);
        persist(
            &store,
            &old,
            Observation {
                id: 1,
                agent: "alpha",
                provenance: ExecutionProvenance::Live,
                run_status: RunStatus::Completed,
                agent_status: AgentExecutionStatus::Completed,
                outcome: Some(RunOutcome::Passed),
                integrity: Some(IntegrityStatus::Clean),
                evaluator_error: false,
            },
            start,
        )
        .await;
        let current = task("performance", "storage", "Improve checkpoint throughput");
        store.upsert_task(&current).await.unwrap();

        let evidence = store
            .routing_evidence(&request(
                old_revision,
                Utc::now(),
                MinimumRoutingEvidence {
                    total: 1,
                    per_agent: 1,
                },
            ))
            .await
            .unwrap();
        assert_eq!(evidence.eligible.len(), 1);
        let historical = &evidence.eligible[0].features;
        assert_eq!(
            historical.classification.category.as_deref(),
            Some("debugging")
        );
        assert_eq!(
            historical.classification.domain.as_deref(),
            Some("concurrency")
        );
        assert_ne!(
            historical.task_revision_id,
            store.upsert_task(&current).await.unwrap()
        );
    }

    #[tokio::test]
    async fn imported_history_obeys_policy_and_candidate_configuration_is_exact() {
        let store = Store::open_in_memory().await.unwrap();
        let task = task("debugging", "concurrency", "Repair queue ordering");
        let revision = TaskRevision::snapshot(task.clone()).unwrap();
        let start = Utc::now() - TimeDelta::minutes(1);
        persist(
            &store,
            &task,
            Observation {
                id: 1,
                agent: "alpha",
                provenance: ExecutionProvenance::Imported,
                run_status: RunStatus::Completed,
                agent_status: AgentExecutionStatus::Completed,
                outcome: Some(RunOutcome::Passed),
                integrity: Some(IntegrityStatus::Clean),
                evaluator_error: false,
            },
            start,
        )
        .await;

        let default_request = request(
            revision.clone(),
            Utc::now(),
            MinimumRoutingEvidence {
                total: 1,
                per_agent: 1,
            },
        );
        let excluded = store.routing_evidence(&default_request).await.unwrap();
        assert!(excluded.eligible.is_empty());
        assert_eq!(
            excluded.excluded[0].reason,
            EvidenceExclusionReason::ImportedProvenance
        );

        let mut imported_policy = RoutingEvidencePolicy::default();
        imported_policy
            .allowed_provenance
            .insert(ExecutionProvenance::Imported);
        let imported_request = RoutingRequest::new(
            revision.clone(),
            candidates(),
            imported_policy,
            MinimumRoutingEvidence {
                total: 1,
                per_agent: 1,
            },
            ExplorationPolicy::None,
            Utc::now(),
        );
        assert_eq!(
            store
                .routing_evidence(&imported_request)
                .await
                .unwrap()
                .eligible
                .len(),
            1
        );

        let mut changed_alpha = config("alpha");
        changed_alpha.model = Some("new-alpha-model".into());
        let changed_candidates = CandidateAgentSet::new(vec![
            CandidateAgent::new(AgentId::new("alpha").unwrap(), changed_alpha).unwrap(),
            CandidateAgent::new(AgentId::new("beta").unwrap(), config("beta")).unwrap(),
        ])
        .unwrap();
        let changed_request = RoutingRequest::new(
            revision,
            changed_candidates,
            imported_request.evidence_policy().clone(),
            MinimumRoutingEvidence {
                total: 1,
                per_agent: 1,
            },
            ExplorationPolicy::None,
            Utc::now(),
        );
        let changed = store.routing_evidence(&changed_request).await.unwrap();
        assert!(changed.eligible.is_empty());
        assert!(matches!(
            &changed.excluded[0].reason,
            EvidenceExclusionReason::CandidateConfigurationMismatch { agent_id, .. }
                if agent_id.as_str() == "alpha"
        ));
    }

    #[tokio::test]
    async fn evidence_added_after_cutoff_cannot_change_the_snapshot() {
        let store = Store::open_in_memory().await.unwrap();
        let task = task("debugging", "concurrency", "Repair queue ordering");
        let revision = TaskRevision::snapshot(task.clone()).unwrap();
        let start = DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        persist(
            &store,
            &task,
            Observation {
                id: 1,
                agent: "alpha",
                provenance: ExecutionProvenance::Live,
                run_status: RunStatus::Completed,
                agent_status: AgentExecutionStatus::Completed,
                outcome: Some(RunOutcome::Passed),
                integrity: Some(IntegrityStatus::Clean),
                evaluator_error: false,
            },
            start,
        )
        .await;
        let routing = request(
            revision,
            start + TimeDelta::minutes(1),
            MinimumRoutingEvidence {
                total: 1,
                per_agent: 1,
            },
        );
        let before = store.routing_evidence(&routing).await.unwrap();

        persist(
            &store,
            &task,
            Observation {
                id: 2,
                agent: "beta",
                provenance: ExecutionProvenance::Live,
                run_status: RunStatus::Completed,
                agent_status: AgentExecutionStatus::Completed,
                outcome: Some(RunOutcome::Failed),
                integrity: Some(IntegrityStatus::Clean),
                evaluator_error: false,
            },
            start + TimeDelta::minutes(2),
        )
        .await;
        let after = store.routing_evidence(&routing).await.unwrap();
        assert_eq!(before, after);
        assert_eq!(after.snapshot.eligible_run_ids, vec![RunId::sequential(1)]);
    }

    #[tokio::test]
    async fn run_started_before_cutoff_but_completed_after_is_excluded() {
        let store = Store::open_in_memory().await.unwrap();
        let task = task("debugging", "concurrency", "Repair queue ordering");
        let revision = TaskRevision::snapshot(task.clone()).unwrap();
        let start = DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        persist(
            &store,
            &task,
            Observation {
                id: 1,
                agent: "alpha",
                provenance: ExecutionProvenance::Live,
                run_status: RunStatus::Completed,
                agent_status: AgentExecutionStatus::Completed,
                outcome: Some(RunOutcome::Passed),
                integrity: Some(IntegrityStatus::Clean),
                evaluator_error: false,
            },
            start,
        )
        .await;

        let cutoff = start + TimeDelta::milliseconds(20);
        let evidence = store
            .routing_evidence(&request(
                revision,
                cutoff,
                MinimumRoutingEvidence {
                    total: 1,
                    per_agent: 1,
                },
            ))
            .await
            .unwrap();

        assert!(evidence.eligible.is_empty());
        assert!(matches!(
            evidence.excluded.as_slice(),
            [ExcludedRoutingEvidence {
                reason: EvidenceExclusionReason::OutcomeAvailableAfterCutoff { completed_at },
                ..
            }] if *completed_at == start + TimeDelta::milliseconds(35)
        ));
    }

    #[tokio::test]
    async fn outcome_and_evaluation_at_cutoff_are_eligible() {
        let store = Store::open_in_memory().await.unwrap();
        let task = task("debugging", "concurrency", "Repair queue ordering");
        let revision = TaskRevision::snapshot(task.clone()).unwrap();
        let start = DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        persist(
            &store,
            &task,
            Observation {
                id: 1,
                agent: "alpha",
                provenance: ExecutionProvenance::Live,
                run_status: RunStatus::Completed,
                agent_status: AgentExecutionStatus::Completed,
                outcome: Some(RunOutcome::Passed),
                integrity: Some(IntegrityStatus::Clean),
                evaluator_error: false,
            },
            start,
        )
        .await;

        let evidence = store
            .routing_evidence(&request(
                revision,
                start + TimeDelta::milliseconds(35),
                MinimumRoutingEvidence {
                    total: 1,
                    per_agent: 1,
                },
            ))
            .await
            .unwrap();

        assert_eq!(evidence.eligible.len(), 1);
        assert!(evidence.excluded.is_empty());
    }

    #[tokio::test]
    async fn evaluation_completed_after_cutoff_is_excluded() {
        let store = Store::open_in_memory().await.unwrap();
        let task = task("debugging", "concurrency", "Repair queue ordering");
        let revision = TaskRevision::snapshot(task.clone()).unwrap();
        let start = DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        persist(
            &store,
            &task,
            Observation {
                id: 1,
                agent: "alpha",
                provenance: ExecutionProvenance::Live,
                run_status: RunStatus::Completed,
                agent_status: AgentExecutionStatus::Completed,
                outcome: Some(RunOutcome::Passed),
                integrity: Some(IntegrityStatus::Clean),
                evaluator_error: false,
            },
            start,
        )
        .await;
        let run_id = RunId::sequential(1);
        let mut evaluation = store.load_evaluation(&run_id).await.unwrap().unwrap();
        evaluation.finished_at = start + TimeDelta::milliseconds(60);
        store.record_evaluation(&evaluation).await.unwrap();

        let cutoff = start + TimeDelta::milliseconds(40);
        let evidence = store
            .routing_evidence(&request(
                revision,
                cutoff,
                MinimumRoutingEvidence {
                    total: 1,
                    per_agent: 1,
                },
            ))
            .await
            .unwrap();

        assert!(evidence.eligible.is_empty());
        assert!(matches!(
            evidence.excluded.as_slice(),
            [ExcludedRoutingEvidence {
                reason: EvidenceExclusionReason::EvaluationAvailableAfterCutoff { completed_at },
                ..
            }] if *completed_at == start + TimeDelta::milliseconds(60)
        ));
    }
}
