//! Typed historical queries over Forge's existing SQLite evidence.
//!
//! This is deliberately a query layer, not a second persistence model. The
//! normalized run/evaluator/metric tables and complete JSON records remain the
//! canonical evidence.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use forge_core::agent::AgentConfig;
use forge_core::experiment::ExperimentStatus;
use forge_core::ids::{ExperimentId, RunId, TaskId};
use forge_core::integrity::{EvaluationIntegrity, IntegrityStatus};
use forge_core::patch::PatchWarning;
use forge_core::result::{Evaluation, EvaluatorExecutionStatus, EvaluatorKind, Verdict};
use forge_core::run::{AgentExecutionStatus, AgentRun, PatchSummary, RunOutcome, RunStatus};
use forge_core::task::{EngineeringTask, TaskClassification};
use serde::{Deserialize, Serialize};
use sqlx::{QueryBuilder, Row, Sqlite};

use crate::sqlite::TaskRevisionId;
use crate::{Store, StoreError, StoreResult};

pub const EXPORT_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Default)]
pub struct HistoryFilter {
    pub agent_id: Option<String>,
    pub outcome: Option<RunOutcome>,
    pub task_id: Option<TaskId>,
    pub repository: Option<String>,
    pub experiment_id: Option<ExperimentId>,
    pub created_from: Option<DateTime<Utc>>,
    pub created_through: Option<DateTime<Utc>>,
    pub category: Option<String>,
    pub language: Option<String>,
    pub domain: Option<String>,
    pub difficulty: Option<String>,
    pub component: Option<String>,
    pub tag: Option<String>,
    pub limit: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskExperience {
    pub revision_id: TaskRevisionId,
    pub task_id: TaskId,
    pub repository: String,
    pub objective: String,
    /// Complete immutable task semantics for reproduction and export.
    pub definition: EngineeringTask,
    pub classification: TaskClassification,
    pub components: Vec<String>,
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunHistoryEntry {
    pub run_id: RunId,
    pub task_revision_id: TaskRevisionId,
    pub task_id: TaskId,
    pub agent_id: String,
    pub repository: String,
    pub status: RunStatus,
    pub agent_status: Option<AgentExecutionStatus>,
    pub outcome: Option<RunOutcome>,
    pub experiment_id: Option<ExperimentId>,
    pub classification: TaskClassification,
    pub components: Vec<String>,
    pub tags: Vec<String>,
    pub duration_ms: Option<u64>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CohortStatistics {
    pub value: String,
    pub total_runs: u64,
    pub passed: u64,
    /// PASS / total runs in this cohort.
    pub pass_rate: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentStatistics {
    pub agent_id: String,
    pub total_runs: u64,
    pub passed: u64,
    pub failed: u64,
    pub inconclusive: u64,
    pub no_change: u64,
    pub errored: u64,
    pub unresolved: u64,
    /// PASS / all recorded runs, including unresolved runs.
    pub pass_rate: f64,
    /// Agent-process duration, not queueing or evaluation time.
    pub median_runtime_ms: Option<u64>,
    pub runtime_samples: u64,
    pub median_provider_reported_tokens: Option<u64>,
    pub token_samples: u64,
    pub known_cost_total_usd: Option<f64>,
    pub median_known_cost_usd: Option<f64>,
    pub cost_samples: u64,
    pub median_patch_lines: Option<u64>,
    pub patch_samples: u64,
    pub integrity_violations: u64,
    pub by_category: Vec<CohortStatistics>,
    pub by_component: Vec<CohortStatistics>,
    pub unclassified_runs: u64,
}

#[derive(Debug, Clone, Default)]
pub struct FailureFilter {
    pub agent_id: Option<String>,
    pub repository: Option<String>,
    pub category: Option<String>,
    pub component: Option<String>,
    pub limit: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FailedEvaluatorSummary {
    pub evaluator_id: String,
    pub kind: EvaluatorKind,
    pub verdict: Verdict,
    pub execution_status: EvaluatorExecutionStatus,
    pub execution_error: Option<String>,
    pub artifact_path: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FailureSummary {
    pub run_id: RunId,
    pub task_revision_id: TaskRevisionId,
    pub task_id: TaskId,
    pub agent_id: String,
    pub repository: String,
    pub outcome: RunOutcome,
    pub failure_reason: Option<String>,
    pub category: Option<String>,
    pub components: Vec<String>,
    pub failed_evaluators: Vec<FailedEvaluatorSummary>,
    pub integrity: Option<EvaluationIntegrity>,
    pub warnings: Vec<PatchWarning>,
    pub duration_ms: Option<u64>,
    pub base_commit: String,
    pub candidate_commit: Option<String>,
    pub artifact_paths: Vec<PathBuf>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentTaskOutcomes {
    pub agent_id: String,
    pub total_runs: u64,
    pub passed: u64,
    pub failed: u64,
    pub inconclusive: u64,
    pub no_change: u64,
    pub errored: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskSimilarity {
    pub task: TaskExperience,
    pub score: f64,
    pub matched: Vec<String>,
    pub historical_outcomes: Vec<AgentTaskOutcomes>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExperimentRunHistory {
    pub run_id: RunId,
    pub agent_id: String,
    pub outcome: Option<RunOutcome>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExperimentHistoryEntry {
    pub experiment_id: ExperimentId,
    pub task_id: TaskId,
    pub repository: String,
    pub base_commit: String,
    pub participants: Vec<String>,
    pub runs: Vec<ExperimentRunHistory>,
    pub status: ExperimentStatus,
    pub created_at: DateTime<Utc>,
    pub duration_ms: Option<u64>,
}

/// Stable, normalized offline-analysis record. Artifact content is never
/// embedded; `artifact_paths` contains references only.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExportRecord {
    pub schema_version: u32,
    pub run_id: RunId,
    pub task_revision_id: TaskRevisionId,
    pub experiment_id: Option<ExperimentId>,
    pub task: TaskExperience,
    pub base_commit: String,
    pub agent: AgentConfig,
    pub status: RunStatus,
    pub agent_status: Option<AgentExecutionStatus>,
    pub outcome: Option<RunOutcome>,
    pub integrity: Option<EvaluationIntegrity>,
    pub evaluation: Option<Evaluation>,
    pub agent_runtime_ms: Option<u64>,
    pub provider_reported_input_tokens: Option<u64>,
    pub provider_reported_output_tokens: Option<u64>,
    pub provider_reported_total_tokens: Option<u64>,
    pub known_cost_usd: Option<f64>,
    pub patch: Option<PatchSummary>,
    pub warnings: Vec<PatchWarning>,
    pub artifact_paths: Vec<PathBuf>,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub failure_reason: Option<String>,
}

impl Store {
    pub async fn history(&self, filter: &HistoryFilter) -> StoreResult<Vec<RunHistoryEntry>> {
        let profiles = self.task_revision_profiles().await?;
        let mut query = QueryBuilder::<Sqlite>::new(
            "SELECT r.run_id, r.task_id, r.task_revision_id, r.agent_id, r.status, \
                    r.agent_status, r.outcome, \
                    r.experiment_id, r.created_at, r.started_at, r.finished_at \
             FROM runs r JOIN task_revisions t ON t.revision_id = r.task_revision_id \
             WHERE 1 = 1",
        );
        if let Some(agent_id) = &filter.agent_id {
            query.push(" AND r.agent_id = ").push_bind(agent_id);
        }
        if let Some(outcome) = filter.outcome {
            query.push(" AND r.outcome = ").push_bind(outcome.as_str());
        }
        if let Some(task_id) = &filter.task_id {
            query.push(" AND r.task_id = ").push_bind(task_id.as_str());
        }
        if let Some(repository) = &filter.repository {
            query.push(" AND t.repository = ").push_bind(repository);
        }
        if let Some(experiment_id) = &filter.experiment_id {
            query
                .push(" AND r.experiment_id = ")
                .push_bind(experiment_id.as_str());
        }
        if let Some(created_from) = filter.created_from {
            query
                .push(" AND r.created_at >= ")
                .push_bind(created_from.to_rfc3339());
        }
        if let Some(created_through) = filter.created_through {
            query
                .push(" AND r.created_at <= ")
                .push_bind(created_through.to_rfc3339());
        }
        if let Some(category) = &filter.category {
            query.push(" AND t.category = ").push_bind(category);
        }
        if let Some(language) = &filter.language {
            query.push(" AND t.language = ").push_bind(language);
        }
        if let Some(domain) = &filter.domain {
            query.push(" AND t.domain = ").push_bind(domain);
        }
        if let Some(difficulty) = &filter.difficulty {
            query.push(" AND t.difficulty = ").push_bind(difficulty);
        }
        if let Some(component) = &filter.component {
            query
                .push(" AND EXISTS (SELECT 1 FROM task_revision_components tc WHERE tc.revision_id = r.task_revision_id AND tc.component = ")
                .push_bind(component)
                .push(")");
        }
        if let Some(tag) = &filter.tag {
            query
                .push(" AND EXISTS (SELECT 1 FROM task_revision_tags tt WHERE tt.revision_id = r.task_revision_id AND tt.tag = ")
                .push_bind(tag)
                .push(")");
        }
        query.push(" ORDER BY r.created_at DESC, r.run_id DESC LIMIT ");
        query.push_bind(normalize_limit(filter.limit) as i64);

        let rows = query.build().fetch_all(self.pool()).await?;
        rows.into_iter()
            .map(|row| {
                let task_id = parse_task_id(row.try_get("task_id")?)?;
                let task_revision_id = parse_task_revision_id(row.try_get("task_revision_id")?)?;
                let profile = profiles.get(&task_revision_id).ok_or_else(|| {
                    StoreError::Corrupt(format!(
                        "run references missing task revision `{task_revision_id}`"
                    ))
                })?;
                Ok(RunHistoryEntry {
                    run_id: parse_run_id(row.try_get("run_id")?)?,
                    task_revision_id,
                    task_id,
                    agent_id: row.try_get("agent_id")?,
                    repository: profile.repository.clone(),
                    status: parse_enum(&row.try_get::<String, _>("status")?)?,
                    agent_status: optional_enum(row.try_get("agent_status")?)?,
                    outcome: optional_enum(row.try_get("outcome")?)?,
                    experiment_id: optional_experiment_id(row.try_get("experiment_id")?)?,
                    classification: profile.classification.clone(),
                    components: profile.components.clone(),
                    tags: profile.tags.clone(),
                    duration_ms: duration_from_columns(
                        row.try_get("started_at")?,
                        row.try_get("finished_at")?,
                    )?,
                    created_at: parse_time(&row.try_get::<String, _>("created_at")?)?,
                })
            })
            .collect()
    }

    pub async fn agent_statistics(&self, agent_id: &str) -> StoreResult<AgentStatistics> {
        let profiles = self.task_revision_profiles().await?;
        let rows = sqlx::query(
            "SELECT task_revision_id, record_json FROM runs
             WHERE agent_id = ?1 ORDER BY created_at, run_id",
        )
        .bind(agent_id)
        .fetch_all(self.pool())
        .await?;

        let mut counts = OutcomeCounts::default();
        let mut runtimes = Vec::new();
        let mut tokens = Vec::new();
        let mut costs = Vec::new();
        let mut patch_lines = Vec::new();
        let mut integrity_violations = 0;
        let mut categories: BTreeMap<String, OutcomeCounts> = BTreeMap::new();
        let mut components: BTreeMap<String, OutcomeCounts> = BTreeMap::new();
        let mut unclassified_runs = 0;

        for row in rows {
            let run: AgentRun = serde_json::from_str(&row.try_get::<String, _>("record_json")?)?;
            counts.record(run.outcome);
            if let Some(execution) = &run.execution {
                runtimes.push(execution.duration_ms);
            }
            if let Some(total) = run.usage().total_tokens() {
                tokens.push(total);
            }
            if let Some(cost) = run.usage().cost_usd {
                costs.push(cost);
            }
            if let Some(patch) = &run.patch {
                patch_lines.push(patch.lines_changed());
            }
            if run
                .integrity
                .as_ref()
                .is_some_and(|integrity| integrity.status != IntegrityStatus::Clean)
            {
                integrity_violations += 1;
            }

            let task_revision_id = parse_task_revision_id(row.try_get("task_revision_id")?)?;
            if let Some(profile) = profiles.get(&task_revision_id) {
                if let Some(category) = &profile.classification.category {
                    categories
                        .entry(category.clone())
                        .or_default()
                        .record(run.outcome);
                } else {
                    unclassified_runs += 1;
                }
                for component in &profile.components {
                    components
                        .entry(component.clone())
                        .or_default()
                        .record(run.outcome);
                }
            }
        }

        let cost_samples = costs.len() as u64;
        let token_samples = tokens.len() as u64;
        let runtime_samples = runtimes.len() as u64;
        let patch_samples = patch_lines.len() as u64;
        Ok(AgentStatistics {
            agent_id: agent_id.to_string(),
            total_runs: counts.total,
            passed: counts.passed,
            failed: counts.failed,
            inconclusive: counts.inconclusive,
            no_change: counts.no_change,
            errored: counts.errored,
            unresolved: counts.unresolved,
            pass_rate: rate(counts.passed, counts.total),
            median_runtime_ms: median_u64(&mut runtimes),
            runtime_samples,
            median_provider_reported_tokens: median_u64(&mut tokens),
            token_samples,
            known_cost_total_usd: (!costs.is_empty()).then(|| costs.iter().sum()),
            median_known_cost_usd: median_f64(&mut costs),
            cost_samples,
            median_patch_lines: median_u64(&mut patch_lines),
            patch_samples,
            integrity_violations,
            by_category: cohort_rows(categories),
            by_component: cohort_rows(components),
            unclassified_runs,
        })
    }

    pub async fn failures(&self, filter: &FailureFilter) -> StoreResult<Vec<FailureSummary>> {
        let profiles = self.task_revision_profiles().await?;
        let mut query = QueryBuilder::<Sqlite>::new(
            "SELECT r.run_id, r.task_id, r.task_revision_id, r.record_json, r.created_at \
             FROM runs r JOIN task_revisions t ON t.revision_id = r.task_revision_id \
             WHERE r.outcome IN ('failed', 'inconclusive', 'no_change', 'errored')",
        );
        if let Some(agent_id) = &filter.agent_id {
            query.push(" AND r.agent_id = ").push_bind(agent_id);
        }
        if let Some(repository) = &filter.repository {
            query.push(" AND t.repository = ").push_bind(repository);
        }
        if let Some(category) = &filter.category {
            query.push(" AND t.category = ").push_bind(category);
        }
        if let Some(component) = &filter.component {
            query
                .push(" AND EXISTS (SELECT 1 FROM task_revision_components tc WHERE tc.revision_id = r.task_revision_id AND tc.component = ")
                .push_bind(component)
                .push(")");
        }
        query.push(" ORDER BY r.created_at DESC, r.run_id DESC LIMIT ");
        query.push_bind(normalize_limit(filter.limit) as i64);
        let rows = query.build().fetch_all(self.pool()).await?;

        let mut summaries = Vec::with_capacity(rows.len());
        for row in rows {
            let run: AgentRun = serde_json::from_str(&row.try_get::<String, _>("record_json")?)?;
            let task_id = parse_task_id(row.try_get("task_id")?)?;
            let task_revision_id = parse_task_revision_id(row.try_get("task_revision_id")?)?;
            let profile = profiles.get(&task_revision_id).ok_or_else(|| {
                StoreError::Corrupt(format!(
                    "run references missing task revision `{task_revision_id}`"
                ))
            })?;
            let evaluator_rows = sqlx::query(
                "SELECT evaluator_id, kind, verdict, execution_status, execution_error, artifact_path \
                 FROM evaluator_results WHERE run_id = ?1 AND verdict <> 'pass' \
                 ORDER BY evaluator_id",
            )
            .bind(run.run_id.as_str())
            .fetch_all(self.pool())
            .await?;
            let failed_evaluators = evaluator_rows
                .into_iter()
                .map(|evaluator| {
                    Ok(FailedEvaluatorSummary {
                        evaluator_id: evaluator.try_get("evaluator_id")?,
                        kind: parse_enum(&evaluator.try_get::<String, _>("kind")?)?,
                        verdict: parse_enum(&evaluator.try_get::<String, _>("verdict")?)?,
                        execution_status: parse_enum(
                            &evaluator.try_get::<String, _>("execution_status")?,
                        )?,
                        execution_error: evaluator.try_get("execution_error")?,
                        artifact_path: evaluator
                            .try_get::<Option<String>, _>("artifact_path")?
                            .map(PathBuf::from),
                    })
                })
                .collect::<StoreResult<Vec<_>>>()?;
            let mut artifact_paths = run_artifact_paths(&run, None);
            artifact_paths.extend(
                failed_evaluators
                    .iter()
                    .filter_map(|evaluator| evaluator.artifact_path.clone()),
            );
            deduplicate_paths(&mut artifact_paths);
            summaries.push(FailureSummary {
                run_id: run.run_id.clone(),
                task_revision_id,
                task_id,
                agent_id: run.agent.agent_id.to_string(),
                repository: profile.repository.clone(),
                outcome: run.outcome.ok_or_else(|| {
                    StoreError::Corrupt(format!("failure run `{}` has no outcome", run.run_id))
                })?,
                failure_reason: run.failure_reason.clone(),
                category: profile.classification.category.clone(),
                components: profile.components.clone(),
                failed_evaluators,
                integrity: run.integrity.clone(),
                warnings: run.warnings.clone(),
                duration_ms: total_run_duration(&run),
                base_commit: run.base_commit.clone(),
                candidate_commit: run
                    .patch
                    .as_ref()
                    .and_then(|patch| patch.head_commit.clone()),
                artifact_paths,
                created_at: parse_time(&row.try_get::<String, _>("created_at")?)?,
            });
        }
        Ok(summaries)
    }

    pub async fn similar_tasks(
        &self,
        task_id: &TaskId,
        limit: u32,
    ) -> StoreResult<Vec<TaskSimilarity>> {
        let profiles = self.task_revision_profiles().await?;
        let target_revision: Option<String> = sqlx::query_scalar(
            "SELECT COALESCE(
                (SELECT task_revision_id FROM runs
                 WHERE task_id = ?1 ORDER BY created_at DESC, run_id DESC LIMIT 1),
                (SELECT current_revision_id FROM tasks WHERE task_id = ?1)
             )",
        )
        .bind(task_id.as_str())
        .fetch_one(self.pool())
        .await?;
        let target_revision = target_revision
            .map(TaskRevisionId::from_stored)
            .transpose()?
            .ok_or_else(|| StoreError::NotFound(format!("task `{task_id}`")))?;
        let target = profiles.get(&target_revision).ok_or_else(|| {
            StoreError::Corrupt(format!("missing similarity revision for `{task_id}`"))
        })?;
        let historical_revision_rows: Vec<String> = sqlx::query_scalar(
            "SELECT DISTINCT task_revision_id FROM runs ORDER BY task_revision_id",
        )
        .fetch_all(self.pool())
        .await?;
        let historical_revisions = historical_revision_rows
            .into_iter()
            .map(TaskRevisionId::from_stored)
            .collect::<StoreResult<BTreeSet<_>>>()?;
        let outcomes = self.task_revision_agent_outcomes().await?;
        let mut matches = profiles
            .values()
            .filter(|candidate| {
                candidate.task_id != *task_id
                    && historical_revisions.contains(&candidate.revision_id)
            })
            .map(|candidate| {
                let (score, matched) = similarity(target, candidate);
                TaskSimilarity {
                    task: candidate.clone(),
                    score,
                    matched,
                    historical_outcomes: outcomes
                        .get(&candidate.revision_id)
                        .cloned()
                        .unwrap_or_default(),
                }
            })
            .filter(|candidate| candidate.score > 0.0)
            .collect::<Vec<_>>();
        matches.sort_by(|left, right| {
            right
                .score
                .total_cmp(&left.score)
                .then_with(|| left.task.task_id.cmp(&right.task.task_id))
                .then_with(|| left.task.revision_id.cmp(&right.task.revision_id))
        });
        matches.truncate(normalize_limit(limit) as usize);
        Ok(matches)
    }

    pub async fn experiment_history(&self, limit: u32) -> StoreResult<Vec<ExperimentHistoryEntry>> {
        let rows = sqlx::query(
            "SELECT experiment_id, task_id, repository, base_commit, agents_json, status, \
                    created_at, completed_at FROM experiments \
             ORDER BY created_at DESC, experiment_id DESC LIMIT ?1",
        )
        .bind(normalize_limit(limit) as i64)
        .fetch_all(self.pool())
        .await?;
        let mut entries = Vec::with_capacity(rows.len());
        for row in rows {
            let experiment_id = parse_experiment_id(row.try_get("experiment_id")?)?;
            let run_rows = sqlx::query(
                "SELECT run_id, agent_id, outcome FROM runs \
                 WHERE experiment_id = ?1 ORDER BY created_at, run_id",
            )
            .bind(experiment_id.as_str())
            .fetch_all(self.pool())
            .await?;
            let runs = run_rows
                .into_iter()
                .map(|run| {
                    Ok(ExperimentRunHistory {
                        run_id: parse_run_id(run.try_get("run_id")?)?,
                        agent_id: run.try_get("agent_id")?,
                        outcome: optional_enum(run.try_get("outcome")?)?,
                    })
                })
                .collect::<StoreResult<Vec<_>>>()?;
            let created_at = parse_time(&row.try_get::<String, _>("created_at")?)?;
            let completed_at = row
                .try_get::<Option<String>, _>("completed_at")?
                .map(|raw| parse_time(&raw))
                .transpose()?;
            entries.push(ExperimentHistoryEntry {
                experiment_id,
                task_id: parse_task_id(row.try_get("task_id")?)?,
                repository: row.try_get("repository")?,
                base_commit: row.try_get("base_commit")?,
                participants: serde_json::from_str(&row.try_get::<String, _>("agents_json")?)?,
                runs,
                status: parse_enum(&row.try_get::<String, _>("status")?)?,
                created_at,
                duration_ms: completed_at
                    .and_then(|end| (end - created_at).num_milliseconds().try_into().ok()),
            });
        }
        Ok(entries)
    }

    pub async fn export_records(&self) -> StoreResult<Vec<ExportRecord>> {
        let profiles = self.task_revision_profiles().await?;
        let rows = sqlx::query(
            "SELECT run_id, task_id, task_revision_id, experiment_id, record_json FROM runs \
             ORDER BY created_at, run_id",
        )
        .fetch_all(self.pool())
        .await?;
        let mut records = Vec::with_capacity(rows.len());
        for row in rows {
            let run: AgentRun = serde_json::from_str(&row.try_get::<String, _>("record_json")?)?;
            let task_revision_id = parse_task_revision_id(row.try_get("task_revision_id")?)?;
            let task = profiles.get(&task_revision_id).cloned().ok_or_else(|| {
                StoreError::Corrupt(format!(
                    "run references missing task revision `{task_revision_id}`"
                ))
            })?;
            let evaluation = self.load_evaluation(&run.run_id).await?;
            let artifact_paths = run_artifact_paths(&run, evaluation.as_ref());
            let usage = run.usage();
            records.push(ExportRecord {
                schema_version: EXPORT_SCHEMA_VERSION,
                run_id: run.run_id.clone(),
                task_revision_id,
                experiment_id: optional_experiment_id(row.try_get("experiment_id")?)?,
                task,
                base_commit: run.base_commit.clone(),
                agent: run.agent.clone(),
                status: run.status,
                agent_status: run.execution.as_ref().map(|execution| execution.status),
                outcome: run.outcome,
                integrity: run.integrity.clone(),
                evaluation,
                agent_runtime_ms: run
                    .execution
                    .as_ref()
                    .map(|execution| execution.duration_ms),
                provider_reported_input_tokens: usage.input_tokens,
                provider_reported_output_tokens: usage.output_tokens,
                provider_reported_total_tokens: usage.total_tokens(),
                known_cost_usd: usage.cost_usd,
                patch: run.patch.clone(),
                warnings: run.warnings.clone(),
                artifact_paths,
                created_at: run.created_at,
                started_at: run.started_at,
                finished_at: run.finished_at,
                failure_reason: run.failure_reason.clone(),
            });
        }
        Ok(records)
    }

    async fn task_revision_profiles(
        &self,
    ) -> StoreResult<BTreeMap<TaskRevisionId, TaskExperience>> {
        let rows = sqlx::query(
            "SELECT revision_id, task_id, repository, objective, category, language, domain, \
                    difficulty, definition_json \
             FROM task_revisions ORDER BY task_id, revision_id",
        )
        .fetch_all(self.pool())
        .await?;
        let component_rows = sqlx::query(
            "SELECT revision_id, component FROM task_revision_components \
             ORDER BY revision_id, component",
        )
        .fetch_all(self.pool())
        .await?;
        let tag_rows = sqlx::query(
            "SELECT revision_id, tag FROM task_revision_tags ORDER BY revision_id, tag",
        )
        .fetch_all(self.pool())
        .await?;
        let mut components: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for row in component_rows {
            components
                .entry(row.try_get("revision_id")?)
                .or_default()
                .push(row.try_get("component")?);
        }
        let mut tags: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for row in tag_rows {
            tags.entry(row.try_get("revision_id")?)
                .or_default()
                .push(row.try_get("tag")?);
        }
        rows.into_iter()
            .map(|row| {
                let raw_revision_id: String = row.try_get("revision_id")?;
                let revision_id = TaskRevisionId::from_stored(raw_revision_id.clone())?;
                Ok((
                    revision_id.clone(),
                    TaskExperience {
                        revision_id,
                        task_id: parse_task_id(row.try_get("task_id")?)?,
                        repository: row.try_get("repository")?,
                        objective: row.try_get("objective")?,
                        definition: serde_json::from_str(
                            &row.try_get::<String, _>("definition_json")?,
                        )?,
                        classification: TaskClassification {
                            category: row.try_get("category")?,
                            language: row.try_get("language")?,
                            domain: row.try_get("domain")?,
                            difficulty: row.try_get("difficulty")?,
                        },
                        components: components.remove(&raw_revision_id).unwrap_or_default(),
                        tags: tags.remove(&raw_revision_id).unwrap_or_default(),
                    },
                ))
            })
            .collect()
    }

    async fn task_revision_agent_outcomes(
        &self,
    ) -> StoreResult<BTreeMap<TaskRevisionId, Vec<AgentTaskOutcomes>>> {
        let rows = sqlx::query(
            "SELECT task_revision_id, agent_id, outcome FROM runs \
             ORDER BY task_revision_id, agent_id, created_at",
        )
        .fetch_all(self.pool())
        .await?;
        let mut grouped: BTreeMap<(TaskRevisionId, String), OutcomeCounts> = BTreeMap::new();
        for row in rows {
            let revision_id = parse_task_revision_id(row.try_get("task_revision_id")?)?;
            let agent_id: String = row.try_get("agent_id")?;
            let outcome: Option<RunOutcome> = optional_enum(row.try_get("outcome")?)?;
            grouped
                .entry((revision_id, agent_id))
                .or_default()
                .record(outcome);
        }
        let mut result: BTreeMap<TaskRevisionId, Vec<AgentTaskOutcomes>> = BTreeMap::new();
        for ((revision_id, agent_id), counts) in grouped {
            result
                .entry(revision_id)
                .or_default()
                .push(AgentTaskOutcomes {
                    agent_id,
                    total_runs: counts.total,
                    passed: counts.passed,
                    failed: counts.failed,
                    inconclusive: counts.inconclusive,
                    no_change: counts.no_change,
                    errored: counts.errored,
                });
        }
        Ok(result)
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct OutcomeCounts {
    total: u64,
    passed: u64,
    failed: u64,
    inconclusive: u64,
    no_change: u64,
    errored: u64,
    unresolved: u64,
}

impl OutcomeCounts {
    fn record(&mut self, outcome: Option<RunOutcome>) {
        self.total += 1;
        match outcome {
            Some(RunOutcome::Passed) => self.passed += 1,
            Some(RunOutcome::Failed) => self.failed += 1,
            Some(RunOutcome::Inconclusive) => self.inconclusive += 1,
            Some(RunOutcome::NoChange) => self.no_change += 1,
            Some(RunOutcome::Errored) => self.errored += 1,
            None => self.unresolved += 1,
        }
    }
}

fn cohort_rows(values: BTreeMap<String, OutcomeCounts>) -> Vec<CohortStatistics> {
    values
        .into_iter()
        .map(|(value, counts)| CohortStatistics {
            value,
            total_runs: counts.total,
            passed: counts.passed,
            pass_rate: rate(counts.passed, counts.total),
        })
        .collect()
}

fn rate(passed: u64, total: u64) -> f64 {
    if total == 0 {
        0.0
    } else {
        passed as f64 / total as f64
    }
}

fn median_u64(values: &mut [u64]) -> Option<u64> {
    if values.is_empty() {
        return None;
    }
    values.sort_unstable();
    let middle = values.len() / 2;
    if values.len().is_multiple_of(2) {
        Some(((values[middle - 1] as u128 + values[middle] as u128) / 2) as u64)
    } else {
        Some(values[middle])
    }
}

fn median_f64(values: &mut [f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(|left, right| left.total_cmp(right));
    let middle = values.len() / 2;
    if values.len().is_multiple_of(2) {
        Some((values[middle - 1] + values[middle]) / 2.0)
    } else {
        Some(values[middle])
    }
}

/// Transparent fixed-weight similarity. Missing fields contribute nothing.
fn similarity(left: &TaskExperience, right: &TaskExperience) -> (f64, Vec<String>) {
    let mut score = 0.0;
    let mut matched = Vec::new();
    if left.repository == right.repository {
        score += 0.20;
        matched.push(format!("repository: {}", left.repository));
    }
    for (name, left_value, right_value, weight) in [
        (
            "category",
            left.classification.category.as_deref(),
            right.classification.category.as_deref(),
            0.20,
        ),
        (
            "language",
            left.classification.language.as_deref(),
            right.classification.language.as_deref(),
            0.15,
        ),
        (
            "domain",
            left.classification.domain.as_deref(),
            right.classification.domain.as_deref(),
            0.15,
        ),
        (
            "difficulty",
            left.classification.difficulty.as_deref(),
            right.classification.difficulty.as_deref(),
            0.10,
        ),
    ] {
        if left_value.is_some() && left_value == right_value {
            score += weight;
            matched.push(format!("{name}: {}", left_value.unwrap_or_default()));
        }
    }
    let component_overlap = jaccard(&left.components, &right.components);
    if component_overlap > 0.0 {
        score += 0.10 * component_overlap;
        for component in intersection(&left.components, &right.components) {
            matched.push(format!("component: {component}"));
        }
    }
    let tag_overlap = jaccard(&left.tags, &right.tags);
    if tag_overlap > 0.0 {
        score += 0.05 * tag_overlap;
        for tag in intersection(&left.tags, &right.tags) {
            matched.push(format!("tag: {tag}"));
        }
    }
    let left_tokens = objective_tokens(&left.objective);
    let right_tokens = objective_tokens(&right.objective);
    let objective_overlap = jaccard_sets(&left_tokens, &right_tokens);
    if objective_overlap > 0.0 {
        score += 0.05 * objective_overlap;
        matched.push(format!("objective-token overlap: {objective_overlap:.2}"));
    }
    (score.min(1.0), matched)
}

fn objective_tokens(value: &str) -> BTreeSet<String> {
    value
        .split(|character: char| !character.is_alphanumeric())
        .filter(|token| token.chars().count() >= 3)
        .map(str::to_lowercase)
        .collect()
}

fn jaccard(left: &[String], right: &[String]) -> f64 {
    let left = left.iter().cloned().collect::<BTreeSet<_>>();
    let right = right.iter().cloned().collect::<BTreeSet<_>>();
    jaccard_sets(&left, &right)
}

fn jaccard_sets(left: &BTreeSet<String>, right: &BTreeSet<String>) -> f64 {
    let union = left.union(right).count();
    if union == 0 {
        0.0
    } else {
        left.intersection(right).count() as f64 / union as f64
    }
}

fn intersection(left: &[String], right: &[String]) -> Vec<String> {
    let right = right.iter().collect::<BTreeSet<_>>();
    left.iter()
        .filter(|value| right.contains(value))
        .cloned()
        .collect()
}

fn total_run_duration(run: &AgentRun) -> Option<u64> {
    match (run.started_at, run.finished_at) {
        (Some(start), Some(end)) => (end - start).num_milliseconds().try_into().ok(),
        _ => None,
    }
}

fn duration_from_columns(
    started_at: Option<String>,
    finished_at: Option<String>,
) -> StoreResult<Option<u64>> {
    match (started_at, finished_at) {
        (Some(start), Some(end)) => {
            let start = parse_time(&start)?;
            let end = parse_time(&end)?;
            Ok((end - start).num_milliseconds().try_into().ok())
        }
        _ => Ok(None),
    }
}

fn run_artifact_paths(run: &AgentRun, evaluation: Option<&Evaluation>) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    paths.extend(run.artifacts.directory.clone());
    paths.extend(run.artifacts.prompt_path.clone());
    paths.extend(run.artifacts.trajectory_path.clone());
    if let Some(execution) = &run.execution {
        paths.extend(execution.stdout_path.clone());
        paths.extend(execution.stderr_path.clone());
    }
    if let Some(patch) = &run.patch {
        paths.extend(patch.diff_path.clone());
    }
    if let Some(evaluation) = evaluation {
        paths.extend(
            evaluation
                .checks
                .iter()
                .filter_map(|check| check.output_path.clone()),
        );
    }
    deduplicate_paths(&mut paths);
    paths
}

fn deduplicate_paths(paths: &mut Vec<PathBuf>) {
    let mut seen = BTreeSet::new();
    paths.retain(|path| seen.insert(path.clone()));
}

fn normalize_limit(limit: u32) -> u32 {
    if limit == 0 { 20 } else { limit.min(10_000) }
}

fn parse_enum<T: serde::de::DeserializeOwned>(raw: &str) -> StoreResult<T> {
    Ok(serde_json::from_value(serde_json::Value::String(
        raw.to_string(),
    ))?)
}

fn optional_enum<T: serde::de::DeserializeOwned>(raw: Option<String>) -> StoreResult<Option<T>> {
    raw.map(|value| parse_enum(&value)).transpose()
}

fn parse_time(raw: &str) -> StoreResult<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw)
        .map(|time| time.with_timezone(&Utc))
        .map_err(|error| StoreError::Corrupt(format!("invalid timestamp `{raw}`: {error}")))
}

fn parse_run_id(raw: String) -> StoreResult<RunId> {
    RunId::new(raw).map_err(|error| StoreError::Corrupt(error.to_string()))
}

fn parse_task_id(raw: String) -> StoreResult<TaskId> {
    TaskId::new(raw).map_err(|error| StoreError::Corrupt(error.to_string()))
}

fn parse_task_revision_id(raw: String) -> StoreResult<TaskRevisionId> {
    TaskRevisionId::from_stored(raw)
}

fn parse_experiment_id(raw: String) -> StoreResult<ExperimentId> {
    ExperimentId::new(raw).map_err(|error| StoreError::Corrupt(error.to_string()))
}

fn optional_experiment_id(raw: Option<String>) -> StoreResult<Option<ExperimentId>> {
    raw.map(parse_experiment_id).transpose()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use chrono::TimeDelta;
    use forge_core::agent::AgentConfig;
    use forge_core::experiment::{Comparison, Experiment};
    use forge_core::ids::AgentId;
    use forge_core::integrity::{EvaluationIntegrity, IntegrityStatus, ProtectionPolicy};
    use forge_core::patch::{PatchWarning, WarningKind};
    use forge_core::result::{
        CheckResult, Direction, EvaluatorExecutionStatus, EvaluatorKind, Metric, Verdict,
    };
    use forge_core::run::{
        AgentExecution, AgentExecutionStatus, PatchSummary, RunArtifacts, RunStatus, Usage,
    };
    use forge_core::task::{EngineeringTask, EvaluationSpec, TaskMetadata};
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

    use super::*;

    struct Fixture {
        store: Store,
        start: DateTime<Utc>,
    }

    impl Fixture {
        async fn new() -> Self {
            let store = Store::open_in_memory().await.unwrap();
            let start = DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
                .unwrap()
                .with_timezone(&Utc);

            let tasks = [
                task(
                    1000,
                    "runtime",
                    "Fix authentication token parsing",
                    Some(("bugfix", "rust", "auth", "medium")),
                    &["api", "auth"],
                    &["regression", "tokens"],
                ),
                task(
                    1001,
                    "runtime",
                    "Repair authentication token validation",
                    Some(("bugfix", "rust", "auth", "medium")),
                    &["api", "auth"],
                    &["regression"],
                ),
                task(
                    1002,
                    "runtime",
                    "Improve storage checkpoint throughput",
                    Some(("performance", "rust", "storage", "hard")),
                    &["storage"],
                    &["benchmark"],
                ),
                // Phase 0-2 spelling: effective classification must continue
                // to work without the new fields.
                legacy_task(1003),
                task(
                    1004,
                    "dashboard",
                    "Prevent empty navigation state",
                    Some(("bugfix", "typescript", "frontend", "easy")),
                    &["ui"],
                    &[],
                ),
            ];
            for task in &tasks {
                store.upsert_task(task).await.unwrap();
            }

            let experiment_id = ExperimentId::sequential(1);
            let mut experiment = Experiment::new(
                experiment_id.clone(),
                TaskId::sequential(1001),
                "runtime",
                "base-commit",
                vec!["codex".into(), "claude".into()],
            );
            experiment.created_at = start;
            store.save_experiment(&experiment).await.unwrap();

            let r1 = completed_run(
                1,
                1000,
                "codex",
                RunOutcome::Passed,
                start + TimeDelta::seconds(1),
                100,
                Some((100, 20)),
                None,
                Some((2, 15, 5)),
                IntegrityStatus::Clean,
            );
            let mut r2 = completed_run(
                2,
                1001,
                "codex",
                RunOutcome::Failed,
                start + TimeDelta::seconds(2),
                300,
                None,
                Some(0.20),
                Some((3, 30, 10)),
                IntegrityStatus::Modified,
            );
            r2.warnings.push(PatchWarning::new(
                WarningKind::ProtectedPathModified,
                Some("tests/auth.rs".into()),
                "protected test changed",
            ));
            r2.artifacts.directory = Some(PathBuf::from(".forge/runs/R-0002"));
            let r3 = completed_run(
                3,
                1002,
                "codex",
                RunOutcome::Inconclusive,
                start + TimeDelta::seconds(3),
                200,
                Some((300, 100)),
                Some(0.10),
                Some((4, 50, 10)),
                IntegrityStatus::Missing,
            );
            let r4 = completed_run(
                4,
                1001,
                "claude",
                RunOutcome::Passed,
                start + TimeDelta::seconds(4),
                500,
                Some((800, 200)),
                Some(0.50),
                Some((1, 8, 2)),
                IntegrityStatus::Clean,
            );
            let r5 = completed_run(
                5,
                1003,
                "claude",
                RunOutcome::Errored,
                start + TimeDelta::seconds(5),
                50,
                None,
                None,
                None,
                IntegrityStatus::Clean,
            );
            let r6 = completed_run(
                6,
                1004,
                "codex",
                RunOutcome::NoChange,
                start + TimeDelta::seconds(6),
                400,
                None,
                None,
                None,
                IntegrityStatus::Clean,
            );

            for run in [&r1, &r3, &r5, &r6] {
                store.save_run(run, None).await.unwrap();
                if let Some(patch) = &run.patch {
                    store.record_patch(&run.run_id, patch).await.unwrap();
                }
            }
            for run in [&r2, &r4] {
                store.save_run(run, Some(&experiment_id)).await.unwrap();
                if let Some(patch) = &run.patch {
                    store.record_patch(&run.run_id, patch).await.unwrap();
                }
                experiment.record_run(run.run_id.clone());
            }

            store
                .record_evaluation(&evaluation(&r1.run_id, Verdict::Pass, "benchmark", true))
                .await
                .unwrap();
            store
                .record_evaluation(&evaluation(&r2.run_id, Verdict::Fail, "tests", false))
                .await
                .unwrap();
            store
                .record_evaluation(&evaluation(
                    &r3.run_id,
                    Verdict::Inconclusive,
                    "benchmark",
                    false,
                ))
                .await
                .unwrap();
            store
                .record_evaluation(&evaluation(&r4.run_id, Verdict::Pass, "tests", true))
                .await
                .unwrap();

            experiment.complete(Comparison {
                experiment_id,
                dimensions: Vec::new(),
            });
            experiment.completed_at = Some(start + TimeDelta::seconds(10));
            store.save_experiment(&experiment).await.unwrap();

            Self { store, start }
        }
    }

    fn task(
        id: u64,
        repository: &str,
        objective: &str,
        classification: Option<(&str, &str, &str, &str)>,
        components: &[&str],
        tags: &[&str],
    ) -> EngineeringTask {
        let classification =
            classification.map_or_else(TaskClassification::default, |value| TaskClassification {
                category: Some(value.0.into()),
                language: Some(value.1.into()),
                domain: Some(value.2.into()),
                difficulty: Some(value.3.into()),
            });
        EngineeringTask {
            task_id: TaskId::sequential(id),
            repository: repository.into(),
            objective: objective.into(),
            constraints: Vec::new(),
            evaluation: EvaluationSpec::default(),
            protection: ProtectionPolicy::default(),
            metadata: TaskMetadata::default(),
            classification,
            components: components.iter().map(|value| (*value).into()).collect(),
            tags: tags.iter().map(|value| (*value).into()).collect(),
        }
    }

    fn legacy_task(id: u64) -> EngineeringTask {
        let mut task = task(
            id,
            "runtime",
            "Clarify executor documentation",
            None,
            &[],
            &[],
        );
        task.metadata.task_type = Some("documentation".into());
        task.metadata.language = Some("rust".into());
        task.metadata.subsystem = Some("executor".into());
        task
    }

    #[allow(clippy::too_many_arguments)]
    fn completed_run(
        run_id: u64,
        task_id: u64,
        agent: &str,
        outcome: RunOutcome,
        created_at: DateTime<Utc>,
        duration_ms: u64,
        tokens: Option<(u64, u64)>,
        cost: Option<f64>,
        patch: Option<(u64, u64, u64)>,
        integrity: IntegrityStatus,
    ) -> AgentRun {
        let started_at = created_at + TimeDelta::milliseconds(10);
        let finished_at = started_at + TimeDelta::milliseconds(duration_ms as i64);
        let mut run = AgentRun::new(
            RunId::sequential(run_id),
            TaskId::sequential(task_id),
            AgentConfig::new(AgentId::new(agent).unwrap(), format!("{agent}-harness"))
                .with_model(format!("{agent}-model")),
            "base-commit",
        );
        run.status = RunStatus::Completed;
        run.created_at = created_at;
        run.started_at = Some(started_at);
        run.finished_at = Some(finished_at);
        run.execution = Some(AgentExecution {
            status: AgentExecutionStatus::Completed,
            exit_code: Some(0),
            timed_out: false,
            started_at,
            finished_at,
            duration_ms,
            stdout_path: Some(PathBuf::from(format!(
                ".forge/runs/R-{run_id:04}/stdout.log"
            ))),
            stderr_path: None,
            usage: Usage {
                input_tokens: tokens.map(|value| value.0),
                output_tokens: tokens.map(|value| value.1),
                cost_usd: cost,
            },
            self_report: None,
            harness_metadata: BTreeMap::new(),
        });
        run.patch = patch.map(|(files, insertions, deletions)| PatchSummary {
            base_commit: "base-commit".into(),
            head_commit: Some(format!("candidate-{run_id}")),
            files_changed: files,
            insertions,
            deletions,
            binary_files: 0,
            diff_path: Some(PathBuf::from(format!(
                ".forge/runs/R-{run_id:04}/patch.diff"
            ))),
            excluded: Vec::new(),
        });
        run.evaluation_verdict = Some(match outcome {
            RunOutcome::Passed => Verdict::Pass,
            RunOutcome::Failed => Verdict::Fail,
            _ => Verdict::Inconclusive,
        });
        run.integrity = Some(EvaluationIntegrity {
            status: integrity,
            modified: if integrity == IntegrityStatus::Modified {
                vec!["tests/auth.rs".into()]
            } else {
                Vec::new()
            },
            added: Vec::new(),
            deleted: if integrity == IntegrityStatus::Missing {
                vec!["benches/checkpoint.rs".into()]
            } else {
                Vec::new()
            },
            allowed: Vec::new(),
        });
        run.outcome = Some(outcome);
        run.artifacts = RunArtifacts {
            directory: None,
            prompt_path: Some(PathBuf::from(format!(
                ".forge/runs/R-{run_id:04}/prompt.txt"
            ))),
            trajectory_path: None,
        };
        run
    }

    fn evaluation(run_id: &RunId, verdict: Verdict, name: &str, success: bool) -> Evaluation {
        let at = DateTime::parse_from_rfc3339("2026-01-01T00:01:00Z")
            .unwrap()
            .with_timezone(&Utc);
        Evaluation::from_checks(
            run_id.clone(),
            vec![CheckResult {
                name: name.into(),
                kind: if name == "benchmark" {
                    EvaluatorKind::Benchmark
                } else {
                    EvaluatorKind::Test
                },
                required: true,
                verdict,
                execution_status: if success {
                    EvaluatorExecutionStatus::Completed
                } else if verdict == Verdict::Inconclusive {
                    EvaluatorExecutionStatus::Error
                } else {
                    EvaluatorExecutionStatus::Completed
                },
                command: Some(format!("./{name}.sh")),
                exit_code: success.then_some(0).or(Some(1)),
                duration_ms: 25,
                detail: None,
                output_path: Some(PathBuf::from(format!("artifacts/{name}.log"))),
                metrics: vec![
                    Metric::new(
                        format!("{name}.duration_ms"),
                        25.0,
                        name,
                        Direction::LowerIsBetter,
                    )
                    .with_unit("ms"),
                ],
                warnings: Vec::new(),
                execution_error: (!success).then(|| "controlled evaluator failure".into()),
            }],
            at,
            at + TimeDelta::milliseconds(25),
        )
    }

    #[tokio::test]
    async fn history_is_newest_first_limited_and_filterable_across_dimensions() {
        let fixture = Fixture::new().await;
        let newest = fixture
            .store
            .history(&HistoryFilter {
                limit: 2,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(
            newest
                .iter()
                .map(|entry| entry.run_id.as_str())
                .collect::<Vec<_>>(),
            vec!["R-0006", "R-0005"]
        );

        let filtered = fixture
            .store
            .history(&HistoryFilter {
                agent_id: Some("codex".into()),
                outcome: Some(RunOutcome::Failed),
                repository: Some("runtime".into()),
                experiment_id: Some(ExperimentId::sequential(1)),
                created_from: Some(fixture.start + TimeDelta::seconds(2)),
                created_through: Some(fixture.start + TimeDelta::seconds(2)),
                category: Some("bugfix".into()),
                language: Some("rust".into()),
                domain: Some("auth".into()),
                difficulty: Some("medium".into()),
                component: Some("auth".into()),
                tag: Some("regression".into()),
                limit: 20,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].run_id.as_str(), "R-0002");
    }

    #[tokio::test]
    async fn agent_statistics_compute_outcomes_medians_and_missing_samples_honestly() {
        let fixture = Fixture::new().await;
        let stats = fixture.store.agent_statistics("codex").await.unwrap();
        assert_eq!(stats.total_runs, 4);
        assert_eq!(
            (
                stats.passed,
                stats.failed,
                stats.inconclusive,
                stats.no_change
            ),
            (1, 1, 1, 1)
        );
        assert_eq!(stats.pass_rate, 0.25);
        assert_eq!(stats.median_runtime_ms, Some(250));
        assert_eq!(stats.runtime_samples, 4);
        assert_eq!(stats.median_provider_reported_tokens, Some(260));
        assert_eq!(stats.token_samples, 2);
        assert!((stats.known_cost_total_usd.unwrap() - 0.30).abs() < f64::EPSILON);
        assert!((stats.median_known_cost_usd.unwrap() - 0.15).abs() < f64::EPSILON);
        assert_eq!(stats.cost_samples, 2);
        assert_eq!(stats.median_patch_lines, Some(40));
        assert_eq!(stats.patch_samples, 3);
        assert_eq!(stats.integrity_violations, 2);
        assert_eq!(
            stats
                .by_category
                .iter()
                .find(|cohort| cohort.value == "bugfix")
                .map(|cohort| (cohort.total_runs, cohort.passed)),
            Some((3, 1))
        );
        assert_eq!(
            stats
                .by_component
                .iter()
                .find(|cohort| cohort.value == "auth")
                .map(|cohort| (cohort.total_runs, cohort.passed)),
            Some((2, 1))
        );
    }

    #[tokio::test]
    async fn runs_bind_immutable_task_revisions_across_every_historical_query() {
        let store = Store::open_in_memory().await.unwrap();
        let start = DateTime::parse_from_rfc3339("2026-02-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let mut changing = task(
            1042,
            "runtime",
            "Debug scheduler contention",
            Some(("debugging", "rust", "concurrency", "medium")),
            &["scheduler"],
            &["regression"],
        );
        let old_revision = store.upsert_task(&changing).await.unwrap();
        let old_run = completed_run(
            1,
            1042,
            "codex",
            RunOutcome::Failed,
            start,
            100,
            None,
            None,
            Some((1, 5, 1)),
            IntegrityStatus::Clean,
        );
        store
            .save_run_at_task_revision(&old_run, None, &old_revision)
            .await
            .unwrap();
        store
            .record_evaluation(&evaluation(&old_run.run_id, Verdict::Fail, "tests", false))
            .await
            .unwrap();

        let peer = task(
            2001,
            "runtime",
            "Diagnose scheduler contention",
            Some(("debugging", "rust", "concurrency", "medium")),
            &["scheduler"],
            &["regression"],
        );
        let peer_revision = store.upsert_task(&peer).await.unwrap();
        let peer_run = completed_run(
            3,
            2001,
            "claude",
            RunOutcome::Passed,
            start + TimeDelta::seconds(2),
            100,
            None,
            None,
            Some((1, 2, 1)),
            IntegrityStatus::Clean,
        );
        store
            .save_run_at_task_revision(&peer_run, None, &peer_revision)
            .await
            .unwrap();

        changing.objective = "Improve storage throughput".into();
        changing.classification = TaskClassification {
            category: Some("performance".into()),
            language: Some("rust".into()),
            domain: Some("storage".into()),
            difficulty: Some("hard".into()),
        };
        changing.components = vec!["storage".into()];
        changing.tags = vec!["benchmark".into()];
        let new_revision = store.upsert_task(&changing).await.unwrap();
        assert_ne!(old_revision, new_revision);
        assert_eq!(store.upsert_task(&changing).await.unwrap(), new_revision);
        let rebind_error = store
            .save_run_at_task_revision(&old_run, None, &new_revision)
            .await
            .unwrap_err();
        assert!(matches!(
            rebind_error,
            StoreError::TaskRevisionConflict { .. }
        ));
        // Ordinary lifecycle updates resolve the existing binding before the
        // mutable task's current revision, so they cannot drift either.
        store.save_run(&old_run, None).await.unwrap();

        // A definition-only edit is not historical execution evidence. Until
        // a new run exists, similarity for the logical task stays anchored to
        // the old run-bound debugging revision.
        let after_unexecuted_edit = store.similar_tasks(&changing.task_id, 10).await.unwrap();
        let matched_peer = after_unexecuted_edit
            .iter()
            .find(|candidate| candidate.task.task_id == peer.task_id)
            .unwrap();
        assert_eq!(
            matched_peer.task.classification.category.as_deref(),
            Some("debugging")
        );
        assert!(
            matched_peer
                .matched
                .iter()
                .any(|matched| matched == "domain: concurrency")
        );

        let new_run = completed_run(
            2,
            1042,
            "codex",
            RunOutcome::Passed,
            start + TimeDelta::seconds(1),
            200,
            None,
            None,
            Some((2, 8, 2)),
            IntegrityStatus::Clean,
        );
        store
            .save_run_at_task_revision(&new_run, None, &new_revision)
            .await
            .unwrap();
        store
            .record_evaluation(&evaluation(&new_run.run_id, Verdict::Pass, "tests", true))
            .await
            .unwrap();

        let target = task(
            2000,
            "runtime",
            "Investigate scheduler contention",
            Some(("debugging", "rust", "concurrency", "medium")),
            &["scheduler"],
            &["regression"],
        );
        store.upsert_task(&target).await.unwrap();

        let history = store
            .history(&HistoryFilter {
                task_id: Some(TaskId::sequential(1042)),
                limit: 10,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].task_revision_id, new_revision);
        assert_eq!(
            history[0].classification.category.as_deref(),
            Some("performance")
        );
        assert_eq!(history[0].classification.domain.as_deref(), Some("storage"));
        assert_eq!(history[0].components, vec!["storage"]);
        assert_eq!(history[1].task_revision_id, old_revision);
        assert_eq!(
            history[1].classification.category.as_deref(),
            Some("debugging")
        );
        assert_eq!(
            history[1].classification.domain.as_deref(),
            Some("concurrency")
        );
        assert_eq!(history[1].components, vec!["scheduler"]);

        let stats = store.agent_statistics("codex").await.unwrap();
        assert_eq!(
            stats
                .by_category
                .iter()
                .map(|cohort| (cohort.value.as_str(), cohort.total_runs, cohort.passed))
                .collect::<Vec<_>>(),
            vec![("debugging", 1, 0), ("performance", 1, 1)]
        );

        let failures = store
            .failures(&FailureFilter {
                category: Some("debugging".into()),
                component: Some("scheduler".into()),
                limit: 10,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].task_revision_id, old_revision);

        let similar = store.similar_tasks(&target.task_id, 10).await.unwrap();
        let closest_1042 = similar
            .iter()
            .find(|candidate| candidate.task.task_id == changing.task_id)
            .unwrap();
        assert_eq!(closest_1042.task.revision_id, old_revision);
        assert_eq!(
            closest_1042.task.classification.category.as_deref(),
            Some("debugging")
        );
        assert_eq!(closest_1042.historical_outcomes[0].failed, 1);
        assert_eq!(closest_1042.historical_outcomes[0].passed, 0);

        let exported = store.export_records().await.unwrap();
        assert_eq!(exported.len(), 3);
        assert_eq!(exported[0].task_revision_id, old_revision);
        assert_eq!(
            exported[0].task.classification.domain.as_deref(),
            Some("concurrency")
        );
        assert_eq!(
            exported[0].task.definition.objective,
            "Debug scheduler contention"
        );
        assert_eq!(exported[1].task_revision_id, new_revision);
        assert_eq!(
            exported[1].task.classification.domain.as_deref(),
            Some("storage")
        );
        assert_eq!(
            exported[1].task.definition.objective,
            "Improve storage throughput"
        );
    }

    #[tokio::test]
    async fn failure_investigation_keeps_evaluator_integrity_warning_and_artifact_evidence() {
        let fixture = Fixture::new().await;
        let failures = fixture
            .store
            .failures(&FailureFilter {
                agent_id: Some("codex".into()),
                category: Some("bugfix".into()),
                component: Some("auth".into()),
                limit: 10,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(failures.len(), 1);
        let failure = &failures[0];
        assert_eq!(failure.run_id.as_str(), "R-0002");
        assert_eq!(failure.warnings.len(), 1);
        assert_eq!(failure.warnings[0].path.as_deref(), Some("tests/auth.rs"));
        assert_eq!(
            failure.integrity.as_ref().unwrap().status,
            IntegrityStatus::Modified
        );
        assert_eq!(failure.candidate_commit.as_deref(), Some("candidate-2"));
        assert_eq!(failure.failed_evaluators.len(), 1);
        assert_eq!(failure.failed_evaluators[0].evaluator_id, "tests");
        assert!(
            failure
                .artifact_paths
                .iter()
                .any(|path| path.ends_with("tests.log"))
        );
    }

    #[tokio::test]
    async fn similarity_is_deterministic_explainable_and_includes_outcomes() {
        let fixture = Fixture::new().await;
        let first = fixture
            .store
            .similar_tasks(&TaskId::sequential(1000), 10)
            .await
            .unwrap();
        let second = fixture
            .store
            .similar_tasks(&TaskId::sequential(1000), 10)
            .await
            .unwrap();
        assert_eq!(first, second);
        assert_eq!(first[0].task.task_id.as_str(), "T-1001");
        assert!(first[0].score > 0.90);
        assert!(
            first[0]
                .matched
                .iter()
                .any(|value| value == "category: bugfix")
        );
        assert!(
            first[0]
                .matched
                .iter()
                .any(|value| value == "component: auth")
        );
        assert_eq!(first[0].historical_outcomes.len(), 2);

        let error = fixture
            .store
            .similar_tasks(&TaskId::sequential(9999), 10)
            .await
            .unwrap_err();
        assert!(matches!(error, StoreError::NotFound(_)));
    }

    #[tokio::test]
    async fn legacy_task_metadata_is_available_as_effective_classification() {
        let fixture = Fixture::new().await;
        let history = fixture
            .store
            .history(&HistoryFilter {
                task_id: Some(TaskId::sequential(1003)),
                limit: 10,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(
            history[0].classification.category.as_deref(),
            Some("documentation")
        );
        assert_eq!(history[0].classification.language.as_deref(), Some("rust"));
        assert_eq!(
            history[0].classification.domain.as_deref(),
            Some("executor")
        );
    }

    #[tokio::test]
    async fn phase_three_database_migrates_in_place_and_binds_old_runs_to_a_snapshot() {
        let temp = tempfile::tempdir().unwrap();
        let old_migrations = temp.path().join("old-migrations");
        std::fs::create_dir(&old_migrations).unwrap();
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        for name in [
            "0001_init.sql",
            "0002_run_outcome.sql",
            "0003_experiments.sql",
            "0004_evaluator_results.sql",
            "0005_experience_queries.sql",
        ] {
            std::fs::copy(
                manifest.join("migrations").join(name),
                old_migrations.join(name),
            )
            .unwrap();
        }

        let database = temp.path().join("forge.db");
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(&database)
                    .create_if_missing(true)
                    .foreign_keys(true),
            )
            .await
            .unwrap();
        sqlx::migrate::Migrator::new(old_migrations)
            .await
            .unwrap()
            .run(&pool)
            .await
            .unwrap();

        let old_task = legacy_task(2000);
        sqlx::query(
            "INSERT INTO tasks (
                task_id, repository, objective, task_type, language, subsystem,
                definition_json, created_at, category, domain, difficulty
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        )
        .bind(old_task.task_id.as_str())
        .bind(&old_task.repository)
        .bind(&old_task.objective)
        .bind(old_task.metadata.task_type.as_deref())
        .bind(old_task.metadata.language.as_deref())
        .bind(old_task.metadata.subsystem.as_deref())
        .bind(serde_json::to_string(&old_task).unwrap())
        .bind("2026-01-01T00:00:00+00:00")
        .bind("documentation")
        .bind("executor")
        .bind(Option::<String>::None)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO task_components (task_id, component) VALUES (?1, ?2)")
            .bind(old_task.task_id.as_str())
            .bind("docs")
            .execute(&pool)
            .await
            .unwrap();
        let old_run = completed_run(
            20,
            2000,
            "codex",
            RunOutcome::Passed,
            DateTime::parse_from_rfc3339("2026-01-01T00:00:01Z")
                .unwrap()
                .with_timezone(&Utc),
            100,
            None,
            None,
            Some((1, 1, 0)),
            IntegrityStatus::Clean,
        );
        sqlx::query(
            "INSERT INTO runs (
                run_id, task_id, agent_id, config_fingerprint, base_commit, status,
                created_at, record_json, agent_status, outcome
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        )
        .bind(old_run.run_id.as_str())
        .bind(old_run.task_id.as_str())
        .bind(old_run.agent.agent_id.as_str())
        .bind(old_run.agent.fingerprint())
        .bind(&old_run.base_commit)
        .bind(old_run.status.as_str())
        .bind(old_run.created_at.to_rfc3339())
        .bind(serde_json::to_string(&old_run).unwrap())
        .bind(AgentExecutionStatus::Completed.as_str())
        .bind(RunOutcome::Passed.as_str())
        .execute(&pool)
        .await
        .unwrap();
        pool.close().await;

        // Opening with current Forge applies 0006 to the existing file; no
        // rebuild or data rewrite is required.
        let migrated = Store::open(&database).await.unwrap();
        let history = migrated
            .history(&HistoryFilter {
                category: Some("documentation".into()),
                domain: Some("executor".into()),
                limit: 10,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].run_id.as_str(), "R-0020");
        assert_eq!(history[0].task_revision_id.as_str(), "legacy:T-2000");
        assert_eq!(history[0].classification.language.as_deref(), Some("rust"));
        assert_eq!(history[0].components, vec!["docs"]);
    }

    #[tokio::test]
    async fn experiment_history_links_participants_runs_and_duration() {
        let fixture = Fixture::new().await;
        let experiments = fixture.store.experiment_history(10).await.unwrap();
        assert_eq!(experiments.len(), 1);
        assert_eq!(experiments[0].participants, vec!["codex", "claude"]);
        assert_eq!(experiments[0].runs.len(), 2);
        assert_eq!(experiments[0].duration_ms, Some(10_000));
    }

    #[tokio::test]
    async fn jsonl_export_is_versioned_normalized_round_trippable_and_references_logs() {
        let fixture = Fixture::new().await;
        let records = fixture.store.export_records().await.unwrap();
        assert_eq!(records.len(), 6);
        assert!(records.iter().all(|record| record.schema_version == 1));
        let record = records
            .iter()
            .find(|record| record.run_id.as_str() == "R-0002")
            .unwrap();
        assert_eq!(record.known_cost_usd, Some(0.20));
        assert_eq!(record.provider_reported_total_tokens, None);
        assert_eq!(record.evaluation.as_ref().unwrap().metrics.len(), 1);
        assert!(
            record
                .artifact_paths
                .iter()
                .any(|path| path.ends_with("stdout.log"))
        );

        let line = serde_json::to_string(record).unwrap();
        assert!(!line.contains("SECRET LARGE LOG CONTENT"));
        assert_eq!(
            serde_json::from_str::<ExportRecord>(&line).unwrap(),
            *record
        );
    }
}
