//! SQLite-backed experience ledger.
//!
//! SQLite because Forge starts on one machine and the data is small; the API
//! here is deliberately narrow so PostgreSQL can replace it later without the
//! rest of the system noticing.

use std::path::Path;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use forge_core::agent::{AgentConfig, AgentDescriptor};
use forge_core::events::Event;
use forge_core::experiment::{Experiment, ExperimentEvent};
use forge_core::ids::{ExperimentId, RunId, TaskId};
use forge_core::result::{Evaluation, Verdict};
use forge_core::run::{AgentExecutionStatus, AgentRun, PatchSummary, RunOutcome, RunStatus};
use forge_core::task::EngineeringTask;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions, SqliteRow};
use sqlx::{Row, SqlitePool};

use crate::error::{StoreError, StoreResult};

/// Counter name used to allocate run ids.
const RUN_COUNTER: &str = "run";
/// Counter name used to allocate experiment ids.
const EXPERIMENT_COUNTER: &str = "experiment";

/// A run as it appears in listings.
#[derive(Debug, Clone, PartialEq)]
pub struct RunSummary {
    pub run_id: RunId,
    pub task_id: TaskId,
    pub agent_id: String,
    /// Where the run reached in the pipeline.
    pub status: RunStatus,
    /// How the agent process ended.
    pub agent_status: Option<AgentExecutionStatus>,
    /// What Forge concluded about the change.
    pub outcome: Option<RunOutcome>,
    /// `None` when the run never reached evaluation.
    pub verdict: Option<Verdict>,
    pub created_at: DateTime<Utc>,
    pub duration_ms: Option<u64>,
    pub files_changed: Option<u64>,
    pub lines_changed: Option<u64>,
    pub cost_usd: Option<f64>,
}

/// The experience ledger.
#[derive(Debug, Clone)]
pub struct Store {
    pool: SqlitePool,
}

impl Store {
    /// Opens (creating if needed) the ledger at `path` and applies migrations.
    pub async fn open(path: impl AsRef<Path>) -> StoreResult<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(|source| StoreError::Io {
                context: format!("creating {}", parent.display()),
                source,
            })?;
        }

        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            // Referential integrity is off by default in SQLite; the schema
            // relies on it to keep orphaned events out of the ledger.
            .foreign_keys(true)
            .busy_timeout(std::time::Duration::from_secs(10))
            // Concurrent agent runs write events while readers query history.
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal);

        Self::connect(options).await
    }

    /// An ephemeral ledger, for tests.
    pub async fn open_in_memory() -> StoreResult<Self> {
        let options = SqliteConnectOptions::from_str("sqlite::memory:")
            .expect("valid in-memory url")
            .foreign_keys(true);
        Self::connect(options).await
    }

    async fn connect(options: SqliteConnectOptions) -> StoreResult<Self> {
        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(options)
            .await?;
        sqlx::migrate!("./migrations").run(&pool).await?;
        Ok(Self { pool })
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    pub async fn close(&self) {
        self.pool.close().await;
    }

    /// Allocates the next run id. Ids are never reused, even after deletion.
    pub async fn next_run_id(&self) -> StoreResult<RunId> {
        Ok(RunId::sequential(self.next_counter(RUN_COUNTER).await?))
    }

    pub async fn next_experiment_id(&self) -> StoreResult<ExperimentId> {
        Ok(ExperimentId::sequential(
            self.next_counter(EXPERIMENT_COUNTER).await?,
        ))
    }

    async fn next_counter(&self, name: &str) -> StoreResult<u64> {
        let value: i64 = sqlx::query_scalar(
            "INSERT INTO counters (name, value) VALUES (?1, 1)
             ON CONFLICT (name) DO UPDATE SET value = value + 1
             RETURNING value",
        )
        .bind(name)
        .fetch_one(&self.pool)
        .await?;
        Ok(value as u64)
    }

    pub async fn record_repository(&self, name: &str, root: &Path) -> StoreResult<()> {
        sqlx::query(
            "INSERT INTO repositories (name, root_path, created_at) VALUES (?1, ?2, ?3)
             ON CONFLICT (name) DO UPDATE SET root_path = excluded.root_path",
        )
        .bind(name)
        .bind(root.to_string_lossy().as_ref())
        .bind(now())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Records the agent catalogue, so historical runs stay interpretable even
    /// after an agent is renamed or removed from the registry.
    pub async fn record_agents(&self, descriptors: &[AgentDescriptor]) -> StoreResult<()> {
        for descriptor in descriptors {
            sqlx::query(
                "INSERT INTO agents (agent_id, display_name, harness, executable, adapter_status, capabilities_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT (agent_id) DO UPDATE SET
                     display_name = excluded.display_name,
                     harness = excluded.harness,
                     executable = excluded.executable,
                     adapter_status = excluded.adapter_status,
                     capabilities_json = excluded.capabilities_json",
            )
            .bind(descriptor.agent_id.as_str())
            .bind(&descriptor.display_name)
            .bind(&descriptor.harness)
            .bind(descriptor.executable.as_deref())
            .bind(descriptor.adapter_status.to_string())
            .bind(serde_json::to_string(&descriptor.capabilities)?)
            .execute(&self.pool)
            .await?;
        }
        Ok(())
    }

    pub async fn upsert_task(&self, task: &EngineeringTask) -> StoreResult<()> {
        sqlx::query(
            "INSERT INTO tasks (task_id, repository, objective, task_type, language, subsystem, definition_json, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT (task_id) DO UPDATE SET
                 repository = excluded.repository,
                 objective = excluded.objective,
                 task_type = excluded.task_type,
                 language = excluded.language,
                 subsystem = excluded.subsystem,
                 definition_json = excluded.definition_json",
        )
        .bind(task.task_id.as_str())
        .bind(&task.repository)
        .bind(&task.objective)
        .bind(task.metadata.task_type.as_deref())
        .bind(task.metadata.language.as_deref())
        .bind(task.metadata.subsystem.as_deref())
        .bind(serde_json::to_string(task)?)
        .bind(now())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn upsert_agent_config(&self, config: &AgentConfig) -> StoreResult<String> {
        let fingerprint = config.fingerprint();
        sqlx::query(
            "INSERT INTO agent_configs (fingerprint, agent_id, harness, model, tools_json, settings_json, first_seen_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT (fingerprint) DO NOTHING",
        )
        .bind(&fingerprint)
        .bind(config.agent_id.as_str())
        .bind(&config.harness)
        .bind(config.model.as_deref())
        .bind(serde_json::to_string(&config.tools)?)
        .bind(serde_json::to_string(&config.settings)?)
        .bind(now())
        .execute(&self.pool)
        .await?;
        Ok(fingerprint)
    }

    /// Writes a run record, inserting or replacing it wholesale.
    ///
    /// Runs are written repeatedly as they progress, so this is idempotent by
    /// design; an interrupted process leaves the last recorded state behind
    /// rather than a missing row.
    pub async fn save_run(
        &self,
        run: &AgentRun,
        experiment_id: Option<&ExperimentId>,
    ) -> StoreResult<()> {
        let fingerprint = self.upsert_agent_config(&run.agent).await?;
        sqlx::query(
            "INSERT INTO runs (
                 run_id, task_id, agent_id, config_fingerprint, experiment_id, base_commit, status,
                 created_at, started_at, finished_at, exit_code, failure_reason, workspace_path,
                 input_tokens, output_tokens, cost_usd, record_json, agent_status, outcome, branch
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)
             ON CONFLICT (run_id) DO UPDATE SET
                 status = excluded.status,
                 started_at = excluded.started_at,
                 finished_at = excluded.finished_at,
                 exit_code = excluded.exit_code,
                 failure_reason = excluded.failure_reason,
                 workspace_path = excluded.workspace_path,
                 input_tokens = excluded.input_tokens,
                 output_tokens = excluded.output_tokens,
                 cost_usd = excluded.cost_usd,
                 record_json = excluded.record_json,
                 agent_status = excluded.agent_status,
                 outcome = excluded.outcome,
                 branch = excluded.branch,
                 experiment_id = COALESCE(runs.experiment_id, excluded.experiment_id)",
        )
        .bind(run.run_id.as_str())
        .bind(run.task_id.as_str())
        .bind(run.agent.agent_id.as_str())
        .bind(&fingerprint)
        .bind(experiment_id.map(|id| id.as_str()))
        .bind(&run.base_commit)
        .bind(run.status.as_str())
        .bind(run.created_at.to_rfc3339())
        .bind(run.started_at.map(|t| t.to_rfc3339()))
        .bind(run.finished_at.map(|t| t.to_rfc3339()))
        .bind(run.exit_code())
        .bind(run.failure_reason.as_deref())
        .bind(
            run.workspace_path
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned()),
        )
        .bind(run.usage().input_tokens.map(|v| v as i64))
        .bind(run.usage().output_tokens.map(|v| v as i64))
        .bind(run.usage().cost_usd)
        .bind(serde_json::to_string(run)?)
        .bind(run.execution.as_ref().map(|e| e.status.as_str()))
        .bind(run.outcome.map(|o| o.as_str()))
        .bind(run.branch.as_deref())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Writes an experiment record without copying any participant run data.
    /// The run links live in `runs.experiment_id` and are reconciled on load.
    pub async fn save_experiment(&self, experiment: &Experiment) -> StoreResult<()> {
        sqlx::query(
            "INSERT INTO experiments (
                 experiment_id, task_id, repository, base_commit, agents_json, status,
                 created_at, completed_at, failure_reason, comparison_json, record_json
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT (experiment_id) DO UPDATE SET
                 status = excluded.status,
                 completed_at = excluded.completed_at,
                 failure_reason = excluded.failure_reason,
                 comparison_json = excluded.comparison_json,
                 record_json = excluded.record_json",
        )
        .bind(experiment.experiment_id.as_str())
        .bind(experiment.task_id.as_str())
        .bind(&experiment.repository)
        .bind(&experiment.base_commit)
        .bind(serde_json::to_string(&experiment.agents)?)
        .bind(experiment.status.as_str())
        .bind(experiment.created_at.to_rfc3339())
        .bind(experiment.completed_at.map(|time| time.to_rfc3339()))
        .bind(experiment.failure_reason.as_deref())
        .bind(
            experiment
                .comparison
                .as_ref()
                .map(serde_json::to_string)
                .transpose()?,
        )
        .bind(serde_json::to_string(experiment)?)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn load_experiment(
        &self,
        experiment_id: &ExperimentId,
    ) -> StoreResult<Option<Experiment>> {
        let record: Option<String> =
            sqlx::query_scalar("SELECT record_json FROM experiments WHERE experiment_id = ?1")
                .bind(experiment_id.as_str())
                .fetch_optional(&self.pool)
                .await?;
        let Some(record) = record else {
            return Ok(None);
        };
        let mut experiment: Experiment = serde_json::from_str(&record)?;
        let linked_run_ids: Vec<String> = sqlx::query_scalar(
            "SELECT run_id FROM runs WHERE experiment_id = ?1 ORDER BY created_at, run_id",
        )
        .bind(experiment_id.as_str())
        .fetch_all(&self.pool)
        .await?;
        experiment.run_ids = linked_run_ids
            .into_iter()
            .map(|raw| RunId::new(raw).map_err(|error| StoreError::Corrupt(error.to_string())))
            .collect::<StoreResult<Vec<_>>>()?;
        Ok(Some(experiment))
    }

    /// Appends experiment lifecycle events idempotently.
    pub async fn append_experiment_events(&self, events: &[ExperimentEvent]) -> StoreResult<usize> {
        if events.is_empty() {
            return Ok(0);
        }
        let mut tx = self.pool.begin().await?;
        let mut written = 0;
        for event in events {
            let result = sqlx::query(
                "INSERT INTO experiment_events (
                     experiment_id, seq, timestamp, event_type, data_json
                 ) VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT (experiment_id, seq) DO NOTHING",
            )
            .bind(event.experiment_id.as_str())
            .bind(event.seq as i64)
            .bind(event.timestamp.to_rfc3339())
            .bind(event.event_type())
            .bind(serde_json::to_string(&event.payload)?)
            .execute(&mut *tx)
            .await?;
            written += result.rows_affected() as usize;
        }
        tx.commit().await?;
        Ok(written)
    }

    pub async fn experiment_events_for(
        &self,
        experiment_id: &ExperimentId,
    ) -> StoreResult<Vec<ExperimentEvent>> {
        let rows = sqlx::query(
            "SELECT experiment_id, seq, timestamp, data_json FROM experiment_events
             WHERE experiment_id = ?1 ORDER BY seq",
        )
        .bind(experiment_id.as_str())
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter().map(decode_experiment_event).collect()
    }

    pub async fn load_run(&self, run_id: &RunId) -> StoreResult<Option<AgentRun>> {
        let record: Option<String> =
            sqlx::query_scalar("SELECT record_json FROM runs WHERE run_id = ?1")
                .bind(run_id.as_str())
                .fetch_optional(&self.pool)
                .await?;
        record
            .map(|json| Ok(serde_json::from_str(&json)?))
            .transpose()
    }

    /// Appends a run's events in one transaction.
    ///
    /// Re-appending events already stored is ignored rather than failing, so a
    /// retried flush cannot corrupt a trajectory.
    pub async fn append_events(&self, events: &[Event]) -> StoreResult<usize> {
        if events.is_empty() {
            return Ok(0);
        }
        let mut tx = self.pool.begin().await?;
        let mut written = 0;
        for event in events {
            let result = sqlx::query(
                "INSERT INTO events (run_id, seq, timestamp, event_type, data_json)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT (run_id, seq) DO NOTHING",
            )
            .bind(event.run_id.as_str())
            .bind(event.seq as i64)
            .bind(event.timestamp.to_rfc3339())
            .bind(event.event_type())
            .bind(serde_json::to_string(&event.payload)?)
            .execute(&mut *tx)
            .await?;
            written += result.rows_affected() as usize;
        }
        tx.commit().await?;
        Ok(written)
    }

    /// A run's trajectory, in order.
    pub async fn events_for(&self, run_id: &RunId) -> StoreResult<Vec<Event>> {
        let rows = sqlx::query(
            "SELECT run_id, seq, timestamp, data_json FROM events
             WHERE run_id = ?1 ORDER BY seq",
        )
        .bind(run_id.as_str())
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter().map(decode_event).collect()
    }

    pub async fn record_patch(&self, run_id: &RunId, patch: &PatchSummary) -> StoreResult<()> {
        sqlx::query(
            "INSERT INTO patches (run_id, base_commit, head_commit, files_changed, insertions, deletions, diff_path)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT (run_id) DO UPDATE SET
                 base_commit = excluded.base_commit,
                 head_commit = excluded.head_commit,
                 files_changed = excluded.files_changed,
                 insertions = excluded.insertions,
                 deletions = excluded.deletions,
                 diff_path = excluded.diff_path",
        )
        .bind(run_id.as_str())
        .bind(&patch.base_commit)
        .bind(patch.head_commit.as_deref())
        .bind(patch.files_changed as i64)
        .bind(patch.insertions as i64)
        .bind(patch.deletions as i64)
        .bind(patch.diff_path.as_ref().map(|p| p.to_string_lossy().into_owned()))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Stores an evaluation and every raw metric behind it.
    pub async fn record_evaluation(&self, evaluation: &Evaluation) -> StoreResult<()> {
        let mut tx = self.pool.begin().await?;

        sqlx::query(
            "INSERT INTO evaluations (run_id, verdict, started_at, finished_at, checks_json, dimensions_json, summary_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT (run_id) DO UPDATE SET
                 verdict = excluded.verdict,
                 started_at = excluded.started_at,
                 finished_at = excluded.finished_at,
                 checks_json = excluded.checks_json,
                 dimensions_json = excluded.dimensions_json,
                 summary_json = excluded.summary_json",
        )
        .bind(evaluation.run_id.as_str())
        .bind(enum_str(&evaluation.verdict)?)
        .bind(evaluation.started_at.to_rfc3339())
        .bind(evaluation.finished_at.to_rfc3339())
        .bind(serde_json::to_string(&evaluation.checks)?)
        .bind(serde_json::to_string(&evaluation.dimensions)?)
        .bind(serde_json::to_string(&evaluation.summary())?)
        .execute(&mut *tx)
        .await?;

        sqlx::query("DELETE FROM evaluator_results WHERE run_id = ?1")
            .bind(evaluation.run_id.as_str())
            .execute(&mut *tx)
            .await?;
        for check in &evaluation.checks {
            sqlx::query(
                "INSERT INTO evaluator_results (
                    run_id, evaluator_id, kind, required, verdict, execution_status,
                    duration_ms, command, exit_code, artifact_path, metric_count,
                    warning_count, execution_error, result_json
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            )
            .bind(evaluation.run_id.as_str())
            .bind(&check.name)
            .bind(check.kind.as_str())
            .bind(check.required)
            .bind(enum_str(&check.verdict)?)
            .bind(check.execution_status.as_str())
            .bind(check.duration_ms as i64)
            .bind(check.command.as_deref())
            .bind(check.exit_code)
            .bind(
                check
                    .output_path
                    .as_ref()
                    .map(|path| path.to_string_lossy().into_owned()),
            )
            .bind(check.metrics.len() as i64)
            .bind(check.warnings.len() as i64)
            .bind(check.execution_error.as_deref())
            .bind(serde_json::to_string(check)?)
            .execute(&mut *tx)
            .await?;
        }

        // Metrics are rewritten wholesale so a re-evaluation cannot leave
        // measurements from a previous attempt behind.
        sqlx::query("DELETE FROM metrics WHERE run_id = ?1")
            .bind(evaluation.run_id.as_str())
            .execute(&mut *tx)
            .await?;

        for metric in &evaluation.metrics {
            sqlx::query(
                "INSERT INTO metrics (run_id, name, value, unit, source, direction)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )
            .bind(evaluation.run_id.as_str())
            .bind(&metric.name)
            .bind(metric.value)
            .bind(metric.unit.as_deref())
            .bind(&metric.source)
            .bind(enum_str(&metric.direction)?)
            .execute(&mut *tx)
            .await?;
        }

        tx.commit().await?;
        Ok(())
    }

    pub async fn load_evaluation(&self, run_id: &RunId) -> StoreResult<Option<Evaluation>> {
        let row = sqlx::query(
            "SELECT e.run_id, e.verdict, e.started_at, e.finished_at, e.checks_json, e.dimensions_json
             FROM evaluations e WHERE e.run_id = ?1",
        )
        .bind(run_id.as_str())
        .fetch_optional(&self.pool)
        .await?;

        let Some(row) = row else {
            return Ok(None);
        };

        let metrics = sqlx::query(
            "SELECT name, value, unit, source, direction FROM metrics WHERE run_id = ?1 ORDER BY id",
        )
        .bind(run_id.as_str())
        .fetch_all(&self.pool)
        .await?;

        Ok(Some(Evaluation {
            run_id: run_id.clone(),
            verdict: parse_enum(&row.try_get::<String, _>("verdict")?)?,
            checks: parse_json(&row.try_get::<String, _>("checks_json")?)?,
            dimensions: parse_json(&row.try_get::<String, _>("dimensions_json")?)?,
            metrics: metrics
                .into_iter()
                .map(|m| {
                    Ok(forge_core::result::Metric {
                        name: m.try_get("name")?,
                        value: m.try_get("value")?,
                        unit: m.try_get("unit")?,
                        source: m.try_get("source")?,
                        direction: parse_enum(&m.try_get::<String, _>("direction")?)?,
                    })
                })
                .collect::<StoreResult<Vec<_>>>()?,
            started_at: parse_time(&row.try_get::<String, _>("started_at")?)?,
            finished_at: parse_time(&row.try_get::<String, _>("finished_at")?)?,
        }))
    }

    /// Most recent runs first.
    pub async fn list_runs(&self, limit: u32) -> StoreResult<Vec<RunSummary>> {
        let rows = sqlx::query(
            "SELECT r.run_id, r.task_id, r.agent_id, r.status, r.agent_status, r.outcome,
                    r.created_at, r.started_at, r.finished_at, r.cost_usd, e.verdict AS verdict,
                    p.files_changed AS files_changed, p.insertions AS insertions,
                    p.deletions AS deletions
             FROM runs r
             LEFT JOIN evaluations e ON e.run_id = r.run_id
             LEFT JOIN patches p ON p.run_id = r.run_id
             ORDER BY r.created_at DESC, r.run_id DESC
             LIMIT ?1",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter().map(decode_run_summary).collect()
    }

    /// Number of runs recorded, used for status output.
    pub async fn run_count(&self) -> StoreResult<u64> {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runs")
            .fetch_one(&self.pool)
            .await?;
        Ok(count as u64)
    }

    pub async fn experiment_count(&self) -> StoreResult<u64> {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM experiments")
            .fetch_one(&self.pool)
            .await?;
        Ok(count as u64)
    }
}

fn decode_event(row: SqliteRow) -> StoreResult<Event> {
    Ok(Event {
        run_id: RunId::new(row.try_get::<String, _>("run_id")?)
            .map_err(|e| StoreError::Corrupt(e.to_string()))?,
        seq: row.try_get::<i64, _>("seq")? as u64,
        timestamp: parse_time(&row.try_get::<String, _>("timestamp")?)?,
        payload: parse_json(&row.try_get::<String, _>("data_json")?)?,
    })
}

fn decode_experiment_event(row: SqliteRow) -> StoreResult<ExperimentEvent> {
    Ok(ExperimentEvent {
        experiment_id: ExperimentId::new(row.try_get::<String, _>("experiment_id")?)
            .map_err(|error| StoreError::Corrupt(error.to_string()))?,
        seq: row.try_get::<i64, _>("seq")? as u64,
        timestamp: parse_time(&row.try_get::<String, _>("timestamp")?)?,
        payload: parse_json(&row.try_get::<String, _>("data_json")?)?,
    })
}

fn decode_run_summary(row: SqliteRow) -> StoreResult<RunSummary> {
    let started_at: Option<String> = row.try_get("started_at")?;
    let finished_at: Option<String> = row.try_get("finished_at")?;
    let duration_ms = match (started_at, finished_at) {
        (Some(start), Some(end)) => {
            let start = parse_time(&start)?;
            let end = parse_time(&end)?;
            (end - start).num_milliseconds().try_into().ok()
        }
        _ => None,
    };

    let insertions: Option<i64> = row.try_get("insertions")?;
    let deletions: Option<i64> = row.try_get("deletions")?;
    let verdict: Option<String> = row.try_get("verdict")?;
    let agent_status: Option<String> = row.try_get("agent_status")?;
    let outcome: Option<String> = row.try_get("outcome")?;

    Ok(RunSummary {
        run_id: RunId::new(row.try_get::<String, _>("run_id")?)
            .map_err(|e| StoreError::Corrupt(e.to_string()))?,
        task_id: TaskId::new(row.try_get::<String, _>("task_id")?)
            .map_err(|e| StoreError::Corrupt(e.to_string()))?,
        agent_id: row.try_get("agent_id")?,
        status: parse_enum(&row.try_get::<String, _>("status")?)?,
        agent_status: agent_status
            .map(|s| parse_enum::<AgentExecutionStatus>(&s))
            .transpose()?,
        outcome: outcome.map(|o| parse_enum::<RunOutcome>(&o)).transpose()?,
        verdict: verdict.map(|v| parse_enum::<Verdict>(&v)).transpose()?,
        created_at: parse_time(&row.try_get::<String, _>("created_at")?)?,
        duration_ms,
        files_changed: row
            .try_get::<Option<i64>, _>("files_changed")?
            .map(|v| v as u64),
        lines_changed: match (insertions, deletions) {
            (Some(i), Some(d)) => Some((i + d) as u64),
            _ => None,
        },
        cost_usd: row.try_get("cost_usd")?,
    })
}

fn parse_json<T: serde::de::DeserializeOwned>(raw: &str) -> StoreResult<T> {
    Ok(serde_json::from_str(raw)?)
}

/// Stores an enum using its serde name.
///
/// Deliberately not `Debug` formatting: the column values are part of the
/// schema, and must match what the domain model reads back.
fn enum_str<T: serde::Serialize>(value: &T) -> StoreResult<String> {
    match serde_json::to_value(value)? {
        serde_json::Value::String(name) => Ok(name),
        other => Err(StoreError::Corrupt(format!(
            "expected a string-valued enum, got {other}"
        ))),
    }
}

/// Reads back a value written by [`enum_str`].
fn parse_enum<T: serde::de::DeserializeOwned>(raw: &str) -> StoreResult<T> {
    parse_json(&serde_json::Value::String(raw.to_string()).to_string())
}

fn parse_time(raw: &str) -> StoreResult<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw)
        .map(|t| t.with_timezone(&Utc))
        .map_err(|err| StoreError::Corrupt(format!("invalid timestamp `{raw}`: {err}")))
}

fn now() -> String {
    Utc::now().to_rfc3339()
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_core::agent::AgentConfig;
    use forge_core::events::{EventPayload, EventSink, RecordingSink};
    use forge_core::experiment::{
        Comparison, Experiment, ExperimentEventPayload, ExperimentRecordingSink, ExperimentStatus,
    };
    use forge_core::ids::AgentId;
    use forge_core::integrity::ProtectionPolicy;
    use forge_core::result::{
        CheckResult, Direction, EvaluatorExecutionStatus, EvaluatorKind, Metric, Verdict,
    };
    use forge_core::task::{CommandSpec, EvaluationSpec, TaskMetadata};

    async fn store() -> Store {
        Store::open_in_memory().await.unwrap()
    }

    fn task() -> EngineeringTask {
        EngineeringTask {
            task_id: TaskId::sequential(1042),
            repository: "distributed-runtime".into(),
            objective: "Improve checkpoint write throughput".into(),
            constraints: vec!["All existing tests must pass".into()],
            evaluation: EvaluationSpec {
                tests: Some(CommandSpec::new("cargo test --workspace")),
                ..Default::default()
            },
            protection: ProtectionPolicy::default(),
            metadata: TaskMetadata {
                task_type: Some("performance".into()),
                language: Some("rust".into()),
                ..Default::default()
            },
        }
    }

    fn run(run_id: RunId) -> AgentRun {
        AgentRun::new(
            run_id,
            TaskId::sequential(1042),
            AgentConfig::new(AgentId::new("claude").unwrap(), "claude-code").with_model("opus"),
            "a73cf21",
        )
    }

    #[tokio::test]
    async fn migrations_create_a_usable_ledger() {
        let store = store().await;
        assert_eq!(store.run_count().await.unwrap(), 0);
        assert!(store.list_runs(10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn run_ids_are_sequential_and_never_reused() {
        let store = store().await;
        assert_eq!(store.next_run_id().await.unwrap().as_str(), "R-0001");
        assert_eq!(store.next_run_id().await.unwrap().as_str(), "R-0002");
        assert_eq!(store.next_experiment_id().await.unwrap().as_str(), "E-0001");

        // Deleting a run must not hand its id out again.
        let run = run(RunId::sequential(3));
        store.upsert_task(&task()).await.unwrap();
        store.save_run(&run, None).await.unwrap();
        sqlx::query("DELETE FROM runs")
            .execute(store.pool())
            .await
            .unwrap();
        assert_eq!(store.next_run_id().await.unwrap().as_str(), "R-0003");
    }

    #[tokio::test]
    async fn an_experiment_persists_links_comparison_and_lifecycle_events() {
        let store = store().await;
        let task = task();
        store.upsert_task(&task).await.unwrap();
        let experiment_id = store.next_experiment_id().await.unwrap();
        let mut experiment = Experiment::new(
            experiment_id.clone(),
            task.task_id.clone(),
            &task.repository,
            "a73cf21",
            vec!["claude".into(), "codex".into()],
        );
        store.save_experiment(&experiment).await.unwrap();

        let run = run(RunId::sequential(1));
        store.save_run(&run, Some(&experiment_id)).await.unwrap();
        experiment.record_run(run.run_id.clone());
        experiment.complete(Comparison {
            experiment_id: experiment_id.clone(),
            dimensions: Vec::new(),
        });
        store.save_experiment(&experiment).await.unwrap();

        let sink = ExperimentRecordingSink::new(experiment_id.clone());
        sink.emit(ExperimentEventPayload::ExperimentStarted {
            task_id: task.task_id,
            repository: task.repository,
            base_commit: "a73cf21".into(),
            agents: vec!["claude".into(), "codex".into()],
        });
        sink.emit(ExperimentEventPayload::ExperimentCompleted { run_count: 1 });
        assert_eq!(
            store
                .append_experiment_events(&sink.events())
                .await
                .unwrap(),
            2
        );

        let loaded = store
            .load_experiment(&experiment_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(loaded.status, ExperimentStatus::Completed);
        assert_eq!(loaded.run_ids, vec![run.run_id]);
        assert!(loaded.comparison.is_some());
        assert_eq!(
            store
                .experiment_events_for(&experiment_id)
                .await
                .unwrap()
                .iter()
                .map(ExperimentEvent::event_type)
                .collect::<Vec<_>>(),
            vec!["ExperimentStarted", "ExperimentCompleted"]
        );
        assert_eq!(store.experiment_count().await.unwrap(), 1);
    }

    #[tokio::test]
    async fn a_run_round_trips_with_its_full_record() {
        let store = store().await;
        store.upsert_task(&task()).await.unwrap();

        let mut run = run(RunId::sequential(1));
        store.save_run(&run, None).await.unwrap();

        run.transition_to(RunStatus::Preparing).unwrap();
        run.transition_to(RunStatus::Running).unwrap();
        run.execution = Some(forge_core::run::AgentExecution {
            status: AgentExecutionStatus::NonZeroExit,
            exit_code: Some(1),
            timed_out: false,
            started_at: Utc::now(),
            finished_at: Utc::now(),
            duration_ms: 702_000,
            stdout_path: Some(std::path::PathBuf::from("stdout.log")),
            stderr_path: None,
            usage: forge_core::run::Usage {
                input_tokens: Some(94_201),
                output_tokens: Some(2_100),
                cost_usd: Some(1.23),
            },
            self_report: Some("I fixed everything".to_string()),
            harness_metadata: Default::default(),
        });
        run.patch = Some(PatchSummary {
            base_commit: "a73cf21".into(),
            head_commit: None,
            files_changed: 2,
            insertions: 10,
            deletions: 1,
            binary_files: 0,
            diff_path: None,
            excluded: Vec::new(),
        });
        run.evaluation_verdict = Some(Verdict::Pass);
        run.integrity = Some(forge_core::EvaluationIntegrity {
            allowed: vec!["tests/new_case.rs".into()],
            ..Default::default()
        });
        run.warnings = run.integrity.as_ref().unwrap().warnings();
        run.security = Some(forge_core::SecurityPosture::current(
            forge_core::AgentSecurity::new(Some("bypassPermissions".into()), true),
        ));
        run.finalize_outcome();
        run.transition_to(RunStatus::Completed).unwrap();
        store.save_run(&run, None).await.unwrap();

        let loaded = store.load_run(&run.run_id).await.unwrap().unwrap();
        assert_eq!(loaded, run);
        assert_eq!(loaded.integrity.as_ref().unwrap().allowed.len(), 1);
        assert_eq!(loaded.warnings.len(), 1);
        assert!(loaded.security.as_ref().unwrap().is_unconfined());
        assert_eq!(store.run_count().await.unwrap(), 1);
    }

    #[tokio::test]
    async fn the_agent_configuration_is_recorded_once_per_fingerprint() {
        let store = store().await;
        store.upsert_task(&task()).await.unwrap();

        store
            .save_run(&run(RunId::sequential(1)), None)
            .await
            .unwrap();
        store
            .save_run(&run(RunId::sequential(2)), None)
            .await
            .unwrap();

        let configs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_configs")
            .fetch_one(store.pool())
            .await
            .unwrap();
        assert_eq!(configs, 1);
    }

    #[tokio::test]
    async fn a_trajectory_survives_a_round_trip_in_order() {
        let store = store().await;
        store.upsert_task(&task()).await.unwrap();
        let run = run(RunId::sequential(1));
        store.save_run(&run, None).await.unwrap();

        let sink = RecordingSink::new(run.run_id.clone());
        sink.emit(EventPayload::RunStarted {
            task_id: run.task_id.clone(),
            agent_id: "claude".into(),
            base_commit: run.base_commit.clone(),
        });
        sink.emit(EventPayload::CommandExecuted {
            command: "cargo test -p storage".into(),
            exit_code: 1,
            duration_ms: 4821,
        });
        sink.emit(EventPayload::AgentFinished {
            status: AgentExecutionStatus::Completed,
            exit_code: Some(0),
            timed_out: false,
            duration_ms: 702_000,
            stdout_path: None,
            stderr_path: None,
        });

        let emitted = sink.drain();
        assert_eq!(store.append_events(&emitted).await.unwrap(), 3);

        let stored = store.events_for(&run.run_id).await.unwrap();
        assert_eq!(stored, emitted);
        assert_eq!(stored[1].event_type(), "CommandExecuted");
    }

    #[tokio::test]
    async fn re_flushing_events_does_not_duplicate_them() {
        let store = store().await;
        store.upsert_task(&task()).await.unwrap();
        let run = run(RunId::sequential(1));
        store.save_run(&run, None).await.unwrap();

        let sink = RecordingSink::new(run.run_id.clone());
        sink.emit(EventPayload::RunFailed {
            reason: "timeout".into(),
        });
        let events = sink.events();

        assert_eq!(store.append_events(&events).await.unwrap(), 1);
        assert_eq!(store.append_events(&events).await.unwrap(), 0);
        assert_eq!(store.events_for(&run.run_id).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn events_cannot_be_orphaned_from_their_run() {
        let store = store().await;
        let sink = RecordingSink::new(RunId::sequential(99));
        sink.emit(EventPayload::RunFailed {
            reason: "no such run".into(),
        });

        let err = store.append_events(&sink.events()).await.unwrap_err();
        assert!(matches!(err, StoreError::Database(_)), "{err}");
    }

    #[tokio::test]
    async fn an_evaluation_keeps_every_raw_metric() {
        let store = store().await;
        store.upsert_task(&task()).await.unwrap();
        let run = run(RunId::sequential(1));
        store.save_run(&run, None).await.unwrap();

        let now = Utc::now();
        let evaluation = Evaluation::from_checks(
            run.run_id.clone(),
            vec![CheckResult {
                name: "tests".into(),
                kind: EvaluatorKind::Test,
                required: true,
                verdict: Verdict::Pass,
                execution_status: EvaluatorExecutionStatus::Completed,
                command: Some("cargo test --workspace".into()),
                exit_code: Some(0),
                duration_ms: 702_000,
                detail: None,
                output_path: None,
                metrics: vec![
                    Metric::new(
                        "tests.duration_ms",
                        702_000.0,
                        "tests",
                        Direction::LowerIsBetter,
                    )
                    .with_unit("ms"),
                    Metric::new(
                        "benchmark.throughput",
                        4.72,
                        "benchmark",
                        Direction::HigherIsBetter,
                    )
                    .with_unit("GB/s"),
                ],
                warnings: Vec::new(),
                execution_error: None,
            }],
            now,
            now,
        );

        store.record_evaluation(&evaluation).await.unwrap();
        let loaded = store.load_evaluation(&run.run_id).await.unwrap().unwrap();

        assert_eq!(loaded.verdict, Verdict::Pass);
        assert_eq!(loaded.metrics.len(), 2);
        assert_eq!(
            loaded
                .metric("benchmark.throughput")
                .unwrap()
                .unit
                .as_deref(),
            Some("GB/s")
        );
        assert_eq!(loaded.checks, evaluation.checks);

        let row = sqlx::query(
            "SELECT kind, required, verdict, execution_status, metric_count
             FROM evaluator_results WHERE run_id = ?1 AND evaluator_id = 'tests'",
        )
        .bind(run.run_id.as_str())
        .fetch_one(store.pool())
        .await
        .unwrap();
        assert_eq!(row.get::<String, _>("kind"), "test");
        assert!(row.get::<bool, _>("required"));
        assert_eq!(row.get::<String, _>("verdict"), "pass");
        assert_eq!(row.get::<String, _>("execution_status"), "completed");
        assert_eq!(row.get::<i64, _>("metric_count"), 2);

        let summary: String =
            sqlx::query_scalar("SELECT summary_json FROM evaluations WHERE run_id = ?1")
                .bind(run.run_id.as_str())
                .fetch_one(store.pool())
                .await
                .unwrap();
        assert_eq!(
            serde_json::from_str::<forge_core::EvaluationSummary>(&summary)
                .unwrap()
                .evaluator_count,
            1
        );
    }

    #[tokio::test]
    async fn re_evaluating_replaces_metrics_rather_than_accumulating_them() {
        let store = store().await;
        store.upsert_task(&task()).await.unwrap();
        let run = run(RunId::sequential(1));
        store.save_run(&run, None).await.unwrap();

        let now = Utc::now();
        let check = |value: f64| CheckResult {
            name: "tests".into(),
            kind: EvaluatorKind::Test,
            required: true,
            verdict: Verdict::Pass,
            execution_status: EvaluatorExecutionStatus::Completed,
            command: None,
            exit_code: Some(0),
            duration_ms: 1,
            detail: None,
            output_path: None,
            metrics: vec![Metric::new(
                "tests.duration_ms",
                value,
                "tests",
                Direction::LowerIsBetter,
            )],
            warnings: Vec::new(),
            execution_error: None,
        };

        store
            .record_evaluation(&Evaluation::from_checks(
                run.run_id.clone(),
                vec![check(100.0)],
                now,
                now,
            ))
            .await
            .unwrap();
        store
            .record_evaluation(&Evaluation::from_checks(
                run.run_id.clone(),
                vec![check(200.0)],
                now,
                now,
            ))
            .await
            .unwrap();

        let loaded = store.load_evaluation(&run.run_id).await.unwrap().unwrap();
        assert_eq!(loaded.metrics.len(), 1);
        assert_eq!(loaded.metrics[0].value, 200.0);
    }

    #[tokio::test]
    async fn listings_join_the_run_its_patch_and_its_verdict() {
        let store = store().await;
        store.upsert_task(&task()).await.unwrap();

        let mut run = run(RunId::sequential(1));
        run.transition_to(RunStatus::Preparing).unwrap();
        run.transition_to(RunStatus::Running).unwrap();
        run.transition_to(RunStatus::Completed).unwrap();
        store.save_run(&run, None).await.unwrap();

        store
            .record_patch(
                &run.run_id,
                &PatchSummary {
                    base_commit: "a73cf21".into(),
                    head_commit: None,
                    files_changed: 3,
                    insertions: 120,
                    deletions: 63,
                    binary_files: 0,
                    diff_path: None,
                    excluded: Vec::new(),
                },
            )
            .await
            .unwrap();

        let now = Utc::now();
        store
            .record_evaluation(&Evaluation::from_checks(
                run.run_id.clone(),
                vec![CheckResult {
                    name: "tests".into(),
                    kind: EvaluatorKind::Test,
                    required: true,
                    verdict: Verdict::Pass,
                    execution_status: EvaluatorExecutionStatus::Completed,
                    command: None,
                    exit_code: Some(0),
                    duration_ms: 5,
                    detail: None,
                    output_path: None,
                    metrics: vec![],
                    warnings: Vec::new(),
                    execution_error: None,
                }],
                now,
                now,
            ))
            .await
            .unwrap();

        let summaries = store.list_runs(10).await.unwrap();
        assert_eq!(summaries.len(), 1);
        let summary = &summaries[0];
        assert_eq!(summary.run_id.as_str(), "R-0001");
        assert_eq!(summary.status, RunStatus::Completed);
        assert_eq!(summary.verdict, Some(Verdict::Pass));
        assert_eq!(summary.files_changed, Some(3));
        assert_eq!(summary.lines_changed, Some(183));
        assert!(summary.duration_ms.is_some());
    }

    #[tokio::test]
    async fn a_run_without_an_evaluation_lists_without_a_verdict() {
        let store = store().await;
        store.upsert_task(&task()).await.unwrap();
        let mut run = run(RunId::sequential(1));
        run.transition_to(RunStatus::Preparing).unwrap();
        run.fail("agent crashed").unwrap();
        store.save_run(&run, None).await.unwrap();

        let summaries = store.list_runs(10).await.unwrap();
        assert_eq!(summaries[0].status, RunStatus::Failed);
        assert_eq!(summaries[0].verdict, None);
        assert_eq!(summaries[0].files_changed, None);
    }

    #[tokio::test]
    async fn a_ledger_persists_across_reopening() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/forge.db");

        let store = Store::open(&path).await.unwrap();
        store.upsert_task(&task()).await.unwrap();
        store
            .save_run(&run(RunId::sequential(1)), None)
            .await
            .unwrap();
        store.close().await;

        let reopened = Store::open(&path).await.unwrap();
        assert_eq!(reopened.run_count().await.unwrap(), 1);
        // The counter survives too, so ids stay unique across processes.
        assert_eq!(reopened.next_run_id().await.unwrap().as_str(), "R-0001");
    }
}
