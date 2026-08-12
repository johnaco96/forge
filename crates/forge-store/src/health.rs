//! Persistence and evidence queries for longitudinal repository health.
//!
//! Two responsibilities, kept apart:
//!
//! - **Evidence collection** ([`Store::health_run_evidence`]) reads the
//!   engineering record the ledger already holds, in the typed shape the health
//!   builder needs. It copies nothing and interprets nothing.
//! - **Snapshot persistence** stores immutable `H-*` records and the mutable
//!   pointer to the latest successful one.
//!
//! Diffs and trends are not stored. They are deterministic functions of the
//! snapshots and a recorded algorithm version, so persisting them would create
//! a derivable answer that could drift away from its evidence.

use chrono::{DateTime, Utc};
use forge_core::health::{
    HealthEvent, HealthSnapshotStatus, RepositoryHealthSnapshot, RunPatchState,
};
use forge_core::ids::{HealthSnapshotId, RunId};
use forge_core::result::Evaluation;
use forge_core::run::{RunOutcome, RunStatus};
use sqlx::Row;

use crate::{Store, StoreError, StoreResult};

const HEALTH_COUNTER: &str = "repository_health_snapshot";

/// The patch facts that decide which repository state a run's evidence
/// describes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatchFacts {
    /// The commit the candidate was recorded as, when it was committed.
    pub head_commit: Option<String>,
    /// Whether the run changed nothing.
    pub is_empty: bool,
}

impl PatchFacts {
    /// Borrowed view for [`MeasuredRepositoryState::for_run`].
    ///
    /// [`MeasuredRepositoryState::for_run`]: forge_core::health::MeasuredRepositoryState::for_run
    pub fn as_state(&self) -> RunPatchState<'_> {
        RunPatchState {
            head_commit: self.head_commit.as_deref(),
            is_empty: self.is_empty,
        }
    }
}

/// One run's engineering evidence, in the shape health construction needs.
///
/// Deliberately carries `base_commit` *and* the patch facts rather than a
/// single "commit" field: deciding which of them a measurement belongs to is
/// the health builder's job, and flattening them here would hide the choice.
#[derive(Debug, Clone, PartialEq)]
pub struct HealthRunEvidence {
    pub run_id: RunId,
    pub base_commit: String,
    pub patch: Option<PatchFacts>,
    pub status: RunStatus,
    pub outcome: Option<RunOutcome>,
    pub agent_id: String,
    pub config_fingerprint: String,
    pub created_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    /// Forge's own evaluation, with every check and raw metric.
    pub evaluation: Option<Evaluation>,
}

impl HealthRunEvidence {
    /// Whether Forge produced an independent evaluation for this run.
    pub fn has_evaluation(&self) -> bool {
        self.evaluation.is_some()
    }
}

impl Store {
    pub async fn next_health_snapshot_id(&self) -> StoreResult<HealthSnapshotId> {
        Ok(HealthSnapshotId::sequential(
            self.next_counter(HEALTH_COUNTER).await?,
        ))
    }

    /// Every run's evidence, most recent first.
    ///
    /// Evaluations are loaded per run rather than joined: they are canonical
    /// JSON documents, and reconstructing typed checks from a join would mean
    /// parsing the same document anyway.
    pub async fn health_run_evidence(&self, limit: u32) -> StoreResult<Vec<HealthRunEvidence>> {
        let rows = sqlx::query(
            "SELECT r.run_id, r.base_commit, r.status, r.outcome, r.agent_id,
                    r.config_fingerprint, r.created_at, r.finished_at,
                    p.head_commit AS head_commit,
                    p.files_changed AS files_changed,
                    p.insertions AS insertions,
                    p.deletions AS deletions,
                    p.run_id AS patch_run_id
             FROM runs r
             LEFT JOIN patches p ON p.run_id = r.run_id
             ORDER BY r.created_at DESC, r.run_id DESC
             LIMIT ?1",
        )
        .bind(limit)
        .fetch_all(self.pool())
        .await?;

        let mut evidence = Vec::with_capacity(rows.len());
        for row in rows {
            let run_id = RunId::new(row.try_get::<String, _>("run_id")?)
                .map_err(|error| StoreError::Corrupt(error.to_string()))?;

            let patch = match row.try_get::<Option<String>, _>("patch_run_id")? {
                Some(_) => {
                    let files: i64 = row.try_get("files_changed")?;
                    let insertions: i64 = row.try_get("insertions")?;
                    let deletions: i64 = row.try_get("deletions")?;
                    Some(PatchFacts {
                        head_commit: row.try_get("head_commit")?,
                        is_empty: files == 0 && insertions == 0 && deletions == 0,
                    })
                }
                None => None,
            };

            let status: RunStatus = parse_enum(&row.try_get::<String, _>("status")?)?;
            let outcome = row
                .try_get::<Option<String>, _>("outcome")?
                .map(|raw| parse_enum::<RunOutcome>(&raw))
                .transpose()?;

            evidence.push(HealthRunEvidence {
                evaluation: self.load_evaluation(&run_id).await?,
                run_id,
                base_commit: row.try_get("base_commit")?,
                patch,
                status,
                outcome,
                agent_id: row.try_get("agent_id")?,
                config_fingerprint: row.try_get("config_fingerprint")?,
                created_at: parse_time(&row.try_get::<String, _>("created_at")?)?,
                finished_at: row
                    .try_get::<Option<String>, _>("finished_at")?
                    .map(|raw| parse_time(&raw))
                    .transpose()?,
            });
        }
        Ok(evidence)
    }

    /// Records an immutable health snapshot.
    ///
    /// Re-inserting an identical snapshot succeeds; re-inserting a different
    /// one under the same id is refused, because a recorded measurement of the
    /// past must never change.
    pub async fn insert_health_snapshot(
        &self,
        snapshot: &RepositoryHealthSnapshot,
    ) -> StoreResult<()> {
        snapshot
            .validate()
            .map_err(|error| StoreError::Corrupt(error.to_string()))?;

        if let Some(existing) = self
            .load_health_snapshot(&snapshot.health_snapshot_id)
            .await?
        {
            return if existing == *snapshot {
                Ok(())
            } else {
                Err(StoreError::Corrupt(format!(
                    "health snapshot {} already exists with different content; \
                     historical health records are immutable",
                    snapshot.health_snapshot_id
                )))
            };
        }

        let measurement_count: usize = snapshot
            .dimensions
            .iter()
            .map(|dimension| dimension.measurements.len())
            .sum();

        let mut transaction = self.pool().begin().await?;

        sqlx::query(
            "INSERT INTO repository_health_snapshots (
                 health_snapshot_id, repository, commit_hash, world_model_snapshot_id,
                 schema_version, builder_version, status, created_at,
                 dimensions_available, measurement_count, runs_considered, record_json
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        )
        .bind(snapshot.health_snapshot_id.as_str())
        .bind(&snapshot.repository)
        .bind(&snapshot.commit)
        .bind(snapshot.world_model_snapshot_id.as_str())
        .bind(&snapshot.schema_version)
        .bind(&snapshot.provenance.builder_version)
        .bind(snapshot.status.as_str())
        .bind(snapshot.created_at.to_rfc3339())
        .bind(snapshot.available_dimensions() as i64)
        .bind(measurement_count as i64)
        .bind(snapshot.provenance.runs_considered as i64)
        .bind(serde_json::to_string(snapshot)?)
        .execute(&mut *transaction)
        .await?;

        for dimension in &snapshot.dimensions {
            sqlx::query(
                "INSERT INTO repository_health_dimensions (
                     health_snapshot_id, dimension, status, measurement_count
                 ) VALUES (?1, ?2, ?3, ?4)",
            )
            .bind(snapshot.health_snapshot_id.as_str())
            .bind(dimension.kind.as_str())
            .bind(dimension.status.as_str())
            .bind(dimension.measurements.len() as i64)
            .execute(&mut *transaction)
            .await?;

            for measurement in &dimension.measurements {
                let key = measurement.identity.comparability_key();
                sqlx::query(
                    "INSERT INTO repository_health_measurements (
                         health_snapshot_id, comparability_key, dimension, metric, unit,
                         direction, source, fingerprint, component, value, scope,
                         observations, measured_commit
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                )
                .bind(snapshot.health_snapshot_id.as_str())
                .bind(&key)
                .bind(dimension.kind.as_str())
                .bind(&measurement.identity.metric)
                .bind(measurement.identity.unit.as_deref())
                .bind(measurement.identity.direction.as_str())
                .bind(&measurement.identity.source)
                .bind(measurement.identity.fingerprint.as_deref())
                .bind(measurement.identity.component.as_deref())
                .bind(measurement.value)
                .bind(if measurement.scope.is_window() {
                    "window"
                } else {
                    "point_in_time"
                })
                .bind(measurement.scope.observations().map(|n| n as i64))
                .bind(measurement.scope.commit())
                .execute(&mut *transaction)
                .await?;

                for evidence in &measurement.evidence {
                    let (source, reference) = evidence_reference(evidence);
                    sqlx::query(
                        "INSERT INTO repository_health_evidence (
                             health_snapshot_id, comparability_key, evidence_source, reference
                         ) VALUES (?1, ?2, ?3, ?4)",
                    )
                    .bind(snapshot.health_snapshot_id.as_str())
                    .bind(&key)
                    .bind(source)
                    .bind(reference)
                    .execute(&mut *transaction)
                    .await?;
                }
            }
        }

        transaction.commit().await?;
        Ok(())
    }

    pub async fn load_health_snapshot(
        &self,
        id: &HealthSnapshotId,
    ) -> StoreResult<Option<RepositoryHealthSnapshot>> {
        let record: Option<String> = sqlx::query_scalar(
            "SELECT record_json FROM repository_health_snapshots WHERE health_snapshot_id = ?1",
        )
        .bind(id.as_str())
        .fetch_optional(self.pool())
        .await?;
        record
            .map(|json| Ok(serde_json::from_str(&json)?))
            .transpose()
    }

    /// Points the repository at a successful snapshot.
    ///
    /// A failed build must not replace a working pointer: the current health of
    /// a repository is the last thing Forge actually measured, not the last
    /// thing it tried to measure.
    pub async fn set_current_health_snapshot(
        &self,
        repository: &str,
        snapshot: &RepositoryHealthSnapshot,
    ) -> StoreResult<bool> {
        if snapshot.status == HealthSnapshotStatus::Failed {
            return Ok(false);
        }
        sqlx::query(
            "INSERT INTO repository_health_current (repository, health_snapshot_id)
             VALUES (?1, ?2)
             ON CONFLICT (repository) DO UPDATE SET health_snapshot_id = excluded.health_snapshot_id",
        )
        .bind(repository)
        .bind(snapshot.health_snapshot_id.as_str())
        .execute(self.pool())
        .await?;
        Ok(true)
    }

    pub async fn current_health_snapshot(
        &self,
        repository: &str,
    ) -> StoreResult<Option<RepositoryHealthSnapshot>> {
        let id: Option<String> = sqlx::query_scalar(
            "SELECT health_snapshot_id FROM repository_health_current WHERE repository = ?1",
        )
        .bind(repository)
        .fetch_optional(self.pool())
        .await?;
        match id {
            Some(raw) => {
                let id = HealthSnapshotId::new(raw)
                    .map_err(|error| StoreError::Corrupt(error.to_string()))?;
                self.load_health_snapshot(&id).await
            }
            None => Ok(None),
        }
    }

    /// Every health snapshot for a repository, oldest first.
    ///
    /// Chronological order by construction; ancestry filtering is the caller's
    /// job, because only the caller has a repository to ask about ancestry.
    pub async fn health_snapshots(
        &self,
        repository: &str,
        limit: u32,
    ) -> StoreResult<Vec<RepositoryHealthSnapshot>> {
        let rows: Vec<String> = sqlx::query_scalar(
            "SELECT record_json FROM repository_health_snapshots
             WHERE repository = ?1
             ORDER BY created_at ASC, health_snapshot_id ASC
             LIMIT ?2",
        )
        .bind(repository)
        .bind(limit)
        .fetch_all(self.pool())
        .await?;
        rows.into_iter()
            .map(|json| Ok(serde_json::from_str(&json)?))
            .collect()
    }

    /// The most recent snapshot recorded for an exact commit, if any.
    pub async fn health_snapshot_for_commit(
        &self,
        repository: &str,
        commit: &str,
    ) -> StoreResult<Option<RepositoryHealthSnapshot>> {
        let record: Option<String> = sqlx::query_scalar(
            "SELECT record_json FROM repository_health_snapshots
             WHERE repository = ?1 AND commit_hash = ?2
             ORDER BY created_at DESC
             LIMIT 1",
        )
        .bind(repository)
        .bind(commit)
        .fetch_optional(self.pool())
        .await?;
        record
            .map(|json| Ok(serde_json::from_str(&json)?))
            .transpose()
    }

    pub async fn health_snapshot_count(&self, repository: &str) -> StoreResult<u64> {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM repository_health_snapshots WHERE repository = ?1",
        )
        .bind(repository)
        .fetch_one(self.pool())
        .await?;
        Ok(count as u64)
    }

    /// Appends health lifecycle events. Re-flushing is a no-op.
    pub async fn append_health_events(&self, events: &[HealthEvent]) -> StoreResult<usize> {
        if events.is_empty() {
            return Ok(0);
        }
        let mut transaction = self.pool().begin().await?;
        let mut written = 0;
        for event in events {
            let result = sqlx::query(
                "INSERT INTO repository_health_events (
                     health_snapshot_id, seq, timestamp, event_type, data_json
                 ) VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT (health_snapshot_id, seq) DO NOTHING",
            )
            .bind(event.health_snapshot_id.as_str())
            .bind(event.seq as i64)
            .bind(event.timestamp.to_rfc3339())
            .bind(event.payload.event_type())
            .bind(serde_json::to_string(&event.payload)?)
            .execute(&mut *transaction)
            .await?;
            written += result.rows_affected() as usize;
        }
        transaction.commit().await?;
        Ok(written)
    }

    pub async fn health_events(&self, id: &HealthSnapshotId) -> StoreResult<Vec<HealthEvent>> {
        let rows = sqlx::query(
            "SELECT health_snapshot_id, seq, timestamp, data_json
             FROM repository_health_events
             WHERE health_snapshot_id = ?1
             ORDER BY seq",
        )
        .bind(id.as_str())
        .fetch_all(self.pool())
        .await?;

        rows.into_iter()
            .map(|row| {
                Ok(HealthEvent {
                    health_snapshot_id: HealthSnapshotId::new(
                        row.try_get::<String, _>("health_snapshot_id")?,
                    )
                    .map_err(|error| StoreError::Corrupt(error.to_string()))?,
                    seq: row.try_get::<i64, _>("seq")? as u64,
                    timestamp: parse_time(&row.try_get::<String, _>("timestamp")?)?,
                    payload: serde_json::from_str(&row.try_get::<String, _>("data_json")?)?,
                })
            })
            .collect()
    }
}

fn evidence_reference(evidence: &forge_core::health::HealthEvidence) -> (&'static str, String) {
    use forge_core::health::HealthEvidence::*;
    match evidence {
        WorldModelFact {
            snapshot_id,
            fact_id,
        } => ("world_model_fact", format!("{snapshot_id}/{fact_id}")),
        Run { run_id } => ("run", run_id.to_string()),
        Metric { run_id, metric } => ("metric", format!("{run_id}/{metric}")),
        TeamExecution { subject } => (
            "team_execution",
            subject
                .team_execution_id()
                .map(|id| id.to_string())
                .unwrap_or_default(),
        ),
        GitHistory { commit } => ("git_history", commit.clone()),
        ConfiguredConstraint { reference } => ("configured_constraint", reference.clone()),
    }
}

fn parse_enum<T: serde::de::DeserializeOwned>(raw: &str) -> StoreResult<T> {
    Ok(serde_json::from_str(
        &serde_json::Value::String(raw.to_string()).to_string(),
    )?)
}

fn parse_time(raw: &str) -> StoreResult<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw)
        .map(|time| time.with_timezone(&Utc))
        .map_err(|error| StoreError::Corrupt(format!("invalid timestamp `{raw}`: {error}")))
}
