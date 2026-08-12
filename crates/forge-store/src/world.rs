//! Persistence and typed queries for immutable repository world models.

use std::collections::BTreeSet;

use crate::{Store, StoreError, StoreResult};
use forge_core::ids::WorldModelSnapshotId;
use forge_core::task::EngineeringTask;
use forge_core::world::{
    SnapshotRelation, WorldEntityKind, WorldFactRecord, WorldModelContext, WorldModelDiff,
    WorldModelEvent, WorldModelEventPayload, WorldModelSnapshot, WorldModelSnapshotStatus,
    WorldQueryKind,
};

const WORLD_MODEL_COUNTER: &str = "world_model_snapshot";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorldModelQuery {
    pub snapshot_id: WorldModelSnapshotId,
    pub kind: WorldQueryKind,
    pub term: Option<String>,
    pub limit: u32,
}

impl WorldModelQuery {
    pub fn new(snapshot_id: WorldModelSnapshotId, kind: WorldQueryKind) -> Self {
        Self {
            snapshot_id,
            kind,
            term: None,
            limit: 50,
        }
    }
}

impl Store {
    pub async fn next_world_model_snapshot_id(&self) -> StoreResult<WorldModelSnapshotId> {
        Ok(WorldModelSnapshotId::sequential(
            self.next_counter(WORLD_MODEL_COUNTER).await?,
        ))
    }

    pub async fn insert_world_model_snapshot(
        &self,
        snapshot: &WorldModelSnapshot,
    ) -> StoreResult<()> {
        snapshot
            .validate()
            .map_err(|error| StoreError::Corrupt(error.to_string()))?;
        if let Some(existing) = self
            .load_world_model_snapshot(&snapshot.snapshot_id)
            .await?
        {
            return if existing == *snapshot {
                Ok(())
            } else {
                Err(StoreError::WorldModelSnapshotConflict {
                    snapshot_id: snapshot.snapshot_id.to_string(),
                })
            };
        }

        let mut transaction = self.pool().begin().await?;
        let summary = snapshot.summary();
        sqlx::query(
            "INSERT INTO world_model_snapshots (
                snapshot_id, repository, commit_hash, schema_version, source,
                status, created_at, fact_count, record_json
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        )
        .bind(snapshot.snapshot_id.as_str())
        .bind(&snapshot.repository)
        .bind(&snapshot.commit)
        .bind(&snapshot.schema_version)
        .bind(serde_name(&snapshot.source)?)
        .bind(serde_name(&snapshot.status)?)
        .bind(snapshot.created_at.to_rfc3339())
        .bind(summary.total() as i64)
        .bind(serde_json::to_string(snapshot)?)
        .execute(&mut *transaction)
        .await?;

        for extractor in &snapshot.extractors {
            sqlx::query(
                "INSERT INTO world_model_extractors (
                    snapshot_id, extractor_name, extractor_version, required,
                    status, facts_produced, record_json
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )
            .bind(snapshot.snapshot_id.as_str())
            .bind(&extractor.identity.name)
            .bind(&extractor.identity.version)
            .bind(extractor.required)
            .bind(serde_name(&extractor.status)?)
            .bind(extractor.facts_produced as i64)
            .bind(serde_json::to_string(extractor)?)
            .execute(&mut *transaction)
            .await?;
        }

        for record in snapshot.facts.records() {
            sqlx::query(
                "INSERT INTO world_model_facts (
                    snapshot_id, fact_id, fact_kind, display_name, search_text, record_json
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )
            .bind(snapshot.snapshot_id.as_str())
            .bind(record.id().as_str())
            .bind(record.kind().as_str())
            .bind(record.display_name())
            .bind(record.search_text())
            .bind(serde_json::to_string(&record)?)
            .execute(&mut *transaction)
            .await?;

            if let WorldFactRecord::KnownFailureMode(failure) = &record {
                for run_id in &failure.related_runs {
                    sqlx::query(
                        "INSERT INTO world_model_fact_runs (snapshot_id, fact_id, run_id)
                         SELECT ?1, ?2, ?3 WHERE EXISTS (
                             SELECT 1 FROM runs WHERE run_id = ?3
                         )",
                    )
                    .bind(snapshot.snapshot_id.as_str())
                    .bind(record.id().as_str())
                    .bind(run_id.as_str())
                    .execute(&mut *transaction)
                    .await?;
                }
            }
            if let WorldFactRecord::Component(component) = &record {
                for task_id in &component.related_tasks {
                    sqlx::query(
                        "INSERT INTO world_model_fact_tasks (snapshot_id, fact_id, task_id)
                         SELECT ?1, ?2, ?3 WHERE EXISTS (
                             SELECT 1 FROM tasks WHERE task_id = ?3
                         )",
                    )
                    .bind(snapshot.snapshot_id.as_str())
                    .bind(record.id().as_str())
                    .bind(task_id.as_str())
                    .execute(&mut *transaction)
                    .await?;
                }
            }
        }

        if snapshot.status != WorldModelSnapshotStatus::Failed {
            sqlx::query(
                "INSERT INTO world_model_current (repository, snapshot_id)
                 VALUES (?1, ?2)
                 ON CONFLICT (repository) DO UPDATE SET snapshot_id = excluded.snapshot_id",
            )
            .bind(&snapshot.repository)
            .bind(snapshot.snapshot_id.as_str())
            .execute(&mut *transaction)
            .await?;
        }
        transaction.commit().await?;
        Ok(())
    }

    pub async fn load_world_model_snapshot(
        &self,
        snapshot_id: &WorldModelSnapshotId,
    ) -> StoreResult<Option<WorldModelSnapshot>> {
        let record: Option<String> = sqlx::query_scalar(
            "SELECT record_json FROM world_model_snapshots WHERE snapshot_id = ?1",
        )
        .bind(snapshot_id.as_str())
        .fetch_optional(self.pool())
        .await?;
        record
            .map(|record| serde_json::from_str(&record).map_err(Into::into))
            .transpose()
    }

    pub async fn current_world_model(
        &self,
        repository: &str,
    ) -> StoreResult<Option<WorldModelSnapshot>> {
        let snapshot_id: Option<String> =
            sqlx::query_scalar("SELECT snapshot_id FROM world_model_current WHERE repository = ?1")
                .bind(repository)
                .fetch_optional(self.pool())
                .await?;
        let Some(snapshot_id) = snapshot_id else {
            return Ok(None);
        };
        let snapshot_id = WorldModelSnapshotId::new(snapshot_id)
            .map_err(|error| StoreError::Corrupt(error.to_string()))?;
        self.load_world_model_snapshot(&snapshot_id).await
    }

    pub async fn world_model_for_commit(
        &self,
        repository: &str,
        commit: &str,
    ) -> StoreResult<Option<WorldModelSnapshot>> {
        let record: Option<String> = sqlx::query_scalar(
            "SELECT record_json FROM world_model_snapshots
             WHERE repository = ?1 AND commit_hash = ?2 AND status <> 'failed'
             ORDER BY created_at DESC, snapshot_id DESC LIMIT 1",
        )
        .bind(repository)
        .bind(commit)
        .fetch_optional(self.pool())
        .await?;
        record
            .map(|record| serde_json::from_str(&record).map_err(Into::into))
            .transpose()
    }

    pub async fn query_world_model(
        &self,
        query: &WorldModelQuery,
    ) -> StoreResult<Vec<WorldFactRecord>> {
        let records: Vec<String> = if let Some(term) = &query.term {
            sqlx::query_scalar(
                "SELECT record_json FROM world_model_facts
                 WHERE snapshot_id = ?1 AND (?2 = 'all' OR fact_kind = ?2)
                   AND instr(search_text, ?3) > 0
                 ORDER BY fact_kind, display_name, fact_id LIMIT ?4",
            )
            .bind(query.snapshot_id.as_str())
            .bind(query_kind_name(query.kind))
            .bind(term.to_ascii_lowercase())
            .bind(normalize_limit(query.limit) as i64)
            .fetch_all(self.pool())
            .await?
        } else {
            sqlx::query_scalar(
                "SELECT record_json FROM world_model_facts
                 WHERE snapshot_id = ?1 AND (?2 = 'all' OR fact_kind = ?2)
                 ORDER BY fact_kind, display_name, fact_id LIMIT ?3",
            )
            .bind(query.snapshot_id.as_str())
            .bind(query_kind_name(query.kind))
            .bind(normalize_limit(query.limit) as i64)
            .fetch_all(self.pool())
            .await?
        };
        records
            .into_iter()
            .map(|record| serde_json::from_str(&record).map_err(Into::into))
            .collect()
    }

    pub async fn append_world_model_events(&self, events: &[WorldModelEvent]) -> StoreResult<()> {
        for event in events {
            sqlx::query(
                "INSERT INTO world_model_events (
                    snapshot_id, seq, timestamp, event_type, data_json
                 ) VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT (snapshot_id, seq) DO NOTHING",
            )
            .bind(event.snapshot_id.as_str())
            .bind(event.seq as i64)
            .bind(event.timestamp.to_rfc3339())
            .bind(world_event_type(&event.payload))
            .bind(serde_json::to_string(event)?)
            .execute(self.pool())
            .await?;
        }
        Ok(())
    }

    pub async fn world_model_events(
        &self,
        snapshot_id: &WorldModelSnapshotId,
    ) -> StoreResult<Vec<WorldModelEvent>> {
        let records: Vec<String> = sqlx::query_scalar(
            "SELECT data_json FROM world_model_events
             WHERE snapshot_id = ?1 ORDER BY seq",
        )
        .bind(snapshot_id.as_str())
        .fetch_all(self.pool())
        .await?;
        records
            .into_iter()
            .map(|record| serde_json::from_str(&record).map_err(Into::into))
            .collect()
    }

    pub async fn world_model_diff(
        &self,
        from: &WorldModelSnapshotId,
        to: &WorldModelSnapshotId,
    ) -> StoreResult<WorldModelDiff> {
        let from = self
            .load_world_model_snapshot(from)
            .await?
            .ok_or_else(|| StoreError::NotFound(format!("world model `{from}`")))?;
        let to = self
            .load_world_model_snapshot(to)
            .await?
            .ok_or_else(|| StoreError::NotFound(format!("world model `{to}`")))?;
        Ok(from.diff(&to))
    }

    /// Exact-only context selection for ordinary runs, routing, and teams.
    pub async fn world_context_for_task(
        &self,
        task: &EngineeringTask,
        commit: &str,
        limit: usize,
    ) -> StoreResult<Option<WorldModelContext>> {
        let Some(snapshot) = self
            .world_model_for_commit(&task.repository, commit)
            .await?
        else {
            return Ok(None);
        };
        let terms = task
            .components
            .iter()
            .map(|component| component.to_ascii_lowercase())
            .chain(task.objective.split_whitespace().filter_map(|word| {
                let normalized = word
                    .trim_matches(|character: char| !character.is_alphanumeric())
                    .to_ascii_lowercase();
                (normalized.len() >= 5).then_some(normalized)
            }))
            .collect::<BTreeSet<_>>();
        let mut records = snapshot.facts.records();
        records.sort_by(|left, right| left.id().cmp(right.id()));
        let selected = records
            .into_iter()
            .filter(|record| {
                if let WorldFactRecord::Component(component) = record
                    && component.related_tasks.contains(&task.task_id)
                {
                    return true;
                }
                let text = record.search_text();
                terms.iter().any(|term| text.contains(term))
            })
            .take(limit)
            .map(|record| forge_core::world::WorldContextFact {
                id: record.id().clone(),
                kind: record.kind(),
                summary: record.display_name(),
            })
            .collect::<Vec<_>>();
        Ok(Some(WorldModelContext {
            snapshot_id: snapshot.snapshot_id,
            commit: snapshot.commit,
            relation: SnapshotRelation::Exact,
            facts: selected,
        }))
    }

    pub async fn world_model_count(&self) -> StoreResult<u64> {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM world_model_snapshots")
            .fetch_one(self.pool())
            .await?;
        Ok(count as u64)
    }
}

fn serde_name<T: serde::Serialize>(value: &T) -> StoreResult<String> {
    match serde_json::to_value(value)? {
        serde_json::Value::String(name) => Ok(name),
        other => Err(StoreError::Corrupt(format!(
            "expected string-valued enum, got {other}"
        ))),
    }
}

fn normalize_limit(limit: u32) -> u32 {
    limit.clamp(1, 1_000)
}

fn query_kind_name(kind: WorldQueryKind) -> &'static str {
    match kind {
        WorldQueryKind::All => "all",
        WorldQueryKind::Component => WorldEntityKind::Component.as_str(),
        WorldQueryKind::Module => WorldEntityKind::Module.as_str(),
        WorldQueryKind::Interface => WorldEntityKind::Interface.as_str(),
        WorldQueryKind::Contract => WorldEntityKind::Contract.as_str(),
        WorldQueryKind::Invariant => WorldEntityKind::Invariant.as_str(),
        WorldQueryKind::Dependency => WorldEntityKind::Dependency.as_str(),
        WorldQueryKind::Ownership => WorldEntityKind::Ownership.as_str(),
        WorldQueryKind::PerformanceConstraint => WorldEntityKind::PerformanceConstraint.as_str(),
        WorldQueryKind::HistoricalDecision => WorldEntityKind::HistoricalDecision.as_str(),
        WorldQueryKind::KnownFailureMode => WorldEntityKind::KnownFailureMode.as_str(),
    }
}

fn world_event_type(payload: &WorldModelEventPayload) -> &'static str {
    match payload {
        WorldModelEventPayload::WorldModelBuildStarted { .. } => "world_model_build_started",
        WorldModelEventPayload::ExtractorStarted { .. } => "extractor_started",
        WorldModelEventPayload::ExtractorCompleted { .. } => "extractor_completed",
        WorldModelEventPayload::ExtractorFailed { .. } => "extractor_failed",
        WorldModelEventPayload::WorldModelValidated { .. } => "world_model_validated",
        WorldModelEventPayload::WorldModelSnapshotCreated { .. } => "world_model_snapshot_created",
        WorldModelEventPayload::WorldModelBuildFailed { .. } => "world_model_build_failed",
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use forge_core::agent::AgentConfig;
    use forge_core::ids::{AgentId, RunId, TaskId, WorldModelFactId};
    use forge_core::integrity::ProtectionPolicy;
    use forge_core::result::{Direction, MetricName, MetricValue};
    use forge_core::run::AgentRun;
    use forge_core::task::{EngineeringTask, EvaluationSpec, TaskMetadata};
    use forge_core::world::{
        Component, ConstraintComparison, Contract, ContractStrength, DecisionStatus, Dependency,
        DependencyKind, EvidenceConfidence, ExtractorIdentity, ExtractorRecord, ExtractorStatus,
        FactMetadata, FailureModeStatus, HistoricalDecision, Interface, InterfaceKind,
        InterfaceVisibility, Invariant, KnownFailureMode, Module, OwnershipRecord,
        PerformanceConstraint, RepositoryPath, SourceLocation, WORLD_MODEL_SCHEMA_VERSION,
        WorldEntityKind, WorldEntityRef, WorldModelEvent, WorldModelEventPayload, WorldModelFacts,
        WorldModelProvenance, WorldModelProvenanceSource, WorldModelSnapshot,
        WorldModelSnapshotSource, WorldModelSnapshotStatus,
    };

    use super::*;

    const COMMIT_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const COMMIT_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn task() -> EngineeringTask {
        EngineeringTask {
            task_id: TaskId::sequential(1042),
            repository: "fixture".into(),
            objective: "Repair storage durability".into(),
            constraints: vec!["Writes remain atomic".into()],
            evaluation: EvaluationSpec::default(),
            protection: ProtectionPolicy::default(),
            metadata: TaskMetadata::default(),
            classification: Default::default(),
            components: vec!["storage".into()],
            tags: Vec::new(),
        }
    }

    fn metadata(
        snapshot_id: &WorldModelSnapshotId,
        kind: WorldEntityKind,
        key: &str,
        commit: &str,
        confidence: EvidenceConfidence,
    ) -> FactMetadata {
        FactMetadata::new(
            WorldModelFactId::stable(kind, key),
            snapshot_id.clone(),
            confidence,
            WorldModelProvenance {
                extractor: ExtractorIdentity::new("fixture", "1"),
                source: WorldModelProvenanceSource::SourceCode {
                    location: SourceLocation::new(
                        RepositoryPath::new("src/lib.rs").unwrap(),
                        commit,
                    ),
                },
            },
        )
    }

    fn snapshot(sequence: u64, commit: &str) -> WorldModelSnapshot {
        let snapshot_id = WorldModelSnapshotId::sequential(sequence);
        let component_id =
            WorldModelFactId::stable(WorldEntityKind::Component, "component:storage");
        let module_id = WorldModelFactId::stable(WorldEntityKind::Module, "module:storage");
        let facts = WorldModelFacts {
            components: vec![Component {
                metadata: metadata(
                    &snapshot_id,
                    WorldEntityKind::Component,
                    "component:storage",
                    commit,
                    EvidenceConfidence::Declared,
                ),
                name: "storage".into(),
                description: "Durable storage engine".into(),
                paths: vec![RepositoryPath::new("src").unwrap()],
                parent: None,
                tags: vec!["stateful".into()],
                related_tasks: vec![TaskId::sequential(1042)],
            }],
            modules: vec![Module {
                metadata: metadata(
                    &snapshot_id,
                    WorldEntityKind::Module,
                    "module:storage",
                    commit,
                    EvidenceConfidence::Observed,
                ),
                name: "storage".into(),
                path: RepositoryPath::new("src/lib.rs").unwrap(),
                language: Some("rust".into()),
                component: Some(component_id.clone()),
            }],
            interfaces: vec![Interface {
                metadata: metadata(
                    &snapshot_id,
                    WorldEntityKind::Interface,
                    "interface:storage",
                    commit,
                    EvidenceConfidence::Observed,
                ),
                name: "Storage API".into(),
                interface_kind: InterfaceKind::LibraryApi,
                owner: WorldEntityRef::new(WorldEntityKind::Module, module_id.clone()),
                location: SourceLocation::new(RepositoryPath::new("src/lib.rs").unwrap(), commit),
                visibility: InterfaceVisibility::Public,
                signature: Some("pub trait Storage".into()),
            }],
            contracts: vec![Contract {
                metadata: metadata(
                    &snapshot_id,
                    WorldEntityKind::Contract,
                    "contract:atomic",
                    commit,
                    EvidenceConfidence::Declared,
                ),
                subject: WorldEntityRef::new(WorldEntityKind::Component, component_id.clone()),
                statement: "Commit is atomic".into(),
                strength: ContractStrength::Explicit,
                source_location: Some(SourceLocation::new(
                    RepositoryPath::new("src/lib.rs").unwrap(),
                    commit,
                )),
            }],
            invariants: vec![Invariant {
                metadata: metadata(
                    &snapshot_id,
                    WorldEntityKind::Invariant,
                    "invariant:monotonic",
                    commit,
                    EvidenceConfidence::Declared,
                ),
                subject: WorldEntityRef::new(WorldEntityKind::Component, component_id.clone()),
                statement: "Sequence numbers are monotonic".into(),
                enforcement: Some("tests".into()),
                related_evaluators: vec!["tests".into()],
            }],
            dependencies: vec![Dependency {
                metadata: metadata(
                    &snapshot_id,
                    WorldEntityKind::Dependency,
                    "dependency:storage-module",
                    commit,
                    EvidenceConfidence::Observed,
                ),
                source: WorldEntityRef::new(WorldEntityKind::Component, component_id.clone()),
                target: WorldEntityRef::new(WorldEntityKind::Module, module_id),
                dependency_kind: DependencyKind::DependsOn,
                evidence: Some("manifest".into()),
            }],
            ownership: vec![OwnershipRecord {
                metadata: metadata(
                    &snapshot_id,
                    WorldEntityKind::Ownership,
                    "ownership:storage",
                    commit,
                    EvidenceConfidence::Declared,
                ),
                subject: WorldEntityRef::new(WorldEntityKind::Component, component_id.clone()),
                owner: "storage-team".into(),
            }],
            performance_constraints: vec![PerformanceConstraint {
                metadata: metadata(
                    &snapshot_id,
                    WorldEntityKind::PerformanceConstraint,
                    "performance:latency",
                    commit,
                    EvidenceConfidence::Declared,
                ),
                subject: WorldEntityRef::new(WorldEntityKind::Component, component_id.clone()),
                metric: MetricName::new("p99_latency_ms").unwrap(),
                comparison: ConstraintComparison::LessThan,
                threshold: MetricValue {
                    value: 10.0,
                    unit: Some("ms".into()),
                    direction: Direction::LowerIsBetter,
                },
                unit: Some("ms".into()),
                statement: "p99 latency is below 10ms".into(),
            }],
            historical_decisions: vec![HistoricalDecision {
                metadata: metadata(
                    &snapshot_id,
                    WorldEntityKind::HistoricalDecision,
                    "decision:sqlite",
                    commit,
                    EvidenceConfidence::Declared,
                ),
                title: "Use SQLite".into(),
                statement: "SQLite stores local metadata".into(),
                rationale: None,
                affected: vec![WorldEntityRef::new(
                    WorldEntityKind::Component,
                    component_id.clone(),
                )],
                effective_commit: Some(commit.into()),
                status: DecisionStatus::Accepted,
            }],
            known_failure_modes: vec![KnownFailureMode {
                metadata: metadata(
                    &snapshot_id,
                    WorldEntityKind::KnownFailureMode,
                    "failure:partial-write",
                    commit,
                    EvidenceConfidence::Observed,
                ),
                components: vec![component_id],
                description: "Partial write after interruption".into(),
                symptoms: vec!["checksum mismatch".into()],
                known_trigger: Some("process interruption".into()),
                related_runs: vec![RunId::sequential(1)],
                related_evaluators: vec!["tests".into()],
                related_commits: vec![commit.into()],
                status: FailureModeStatus::Open,
            }],
        };
        WorldModelSnapshot {
            snapshot_id,
            repository: "fixture".into(),
            commit: commit.into(),
            created_at: Utc::now(),
            source: WorldModelSnapshotSource::Mixed,
            schema_version: WORLD_MODEL_SCHEMA_VERSION.into(),
            status: WorldModelSnapshotStatus::Complete,
            extractors: vec![ExtractorRecord {
                identity: ExtractorIdentity::new("fixture", "1"),
                required: true,
                status: ExtractorStatus::Completed,
                facts_produced: 10,
                configuration_fingerprint: "fixture-v1".into(),
                error: None,
            }],
            facts,
        }
    }

    #[tokio::test]
    async fn every_fact_type_round_trips_queries_and_links_existing_evidence() {
        let store = Store::open_in_memory().await.unwrap();
        let task = task();
        store.upsert_task(&task).await.unwrap();
        let run = AgentRun::new(
            RunId::sequential(1),
            task.task_id.clone(),
            AgentConfig::new(AgentId::new("stub").unwrap(), "stub"),
            COMMIT_A,
        );
        store.save_run(&run, None).await.unwrap();
        let snapshot = snapshot(1, COMMIT_A);
        snapshot.validate().unwrap();
        store.insert_world_model_snapshot(&snapshot).await.unwrap();

        assert_eq!(
            store
                .load_world_model_snapshot(&snapshot.snapshot_id)
                .await
                .unwrap(),
            Some(snapshot.clone())
        );
        assert_eq!(snapshot.summary().total(), 10);
        for kind in [
            WorldQueryKind::Component,
            WorldQueryKind::Module,
            WorldQueryKind::Interface,
            WorldQueryKind::Contract,
            WorldQueryKind::Invariant,
            WorldQueryKind::Dependency,
            WorldQueryKind::Ownership,
            WorldQueryKind::PerformanceConstraint,
            WorldQueryKind::HistoricalDecision,
            WorldQueryKind::KnownFailureMode,
        ] {
            assert_eq!(
                store
                    .query_world_model(&WorldModelQuery::new(snapshot.snapshot_id.clone(), kind,))
                    .await
                    .unwrap()
                    .len(),
                1
            );
        }
        let linked_runs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM world_model_fact_runs")
            .fetch_one(store.pool())
            .await
            .unwrap();
        let linked_tasks: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM world_model_fact_tasks")
            .fetch_one(store.pool())
            .await
            .unwrap();
        assert_eq!((linked_runs, linked_tasks), (1, 1));
        let context = store
            .world_context_for_task(&task, COMMIT_A, 5)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(context.snapshot_id, snapshot.snapshot_id);
        assert_eq!(context.relation, SnapshotRelation::Exact);
        assert!(!context.facts.is_empty());
        assert!(
            store
                .world_context_for_task(&task, COMMIT_B, 5)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn immutable_snapshots_move_only_the_current_pointer_and_diff_deterministically() {
        let store = Store::open_in_memory().await.unwrap();
        let first = snapshot(1, COMMIT_A);
        store.insert_world_model_snapshot(&first).await.unwrap();
        let mut conflicting = first.clone();
        conflicting.facts.components[0].description = "rewritten".into();
        assert!(matches!(
            store.insert_world_model_snapshot(&conflicting).await,
            Err(StoreError::WorldModelSnapshotConflict { .. })
        ));

        let mut second = snapshot(2, COMMIT_B);
        second.facts.historical_decisions[0].effective_commit = Some(COMMIT_A.into());
        second.facts.known_failure_modes[0].related_commits = vec![COMMIT_A.into()];
        second.facts.components[0].description = "new architecture".into();
        store.insert_world_model_snapshot(&second).await.unwrap();
        assert_eq!(
            store
                .current_world_model("fixture")
                .await
                .unwrap()
                .unwrap()
                .snapshot_id,
            second.snapshot_id
        );
        assert_eq!(
            store
                .world_model_for_commit("fixture", COMMIT_A)
                .await
                .unwrap()
                .unwrap(),
            first
        );
        let diff = store
            .world_model_diff(&WorldModelSnapshotId::sequential(1), &second.snapshot_id)
            .await
            .unwrap();
        assert_eq!(diff.changed.len(), 1);
        assert!(diff.added.is_empty());
        assert!(diff.removed.is_empty());
    }

    #[tokio::test]
    async fn typed_world_events_round_trip_in_sequence() {
        let store = Store::open_in_memory().await.unwrap();
        let snapshot = snapshot(1, COMMIT_A);
        store.insert_world_model_snapshot(&snapshot).await.unwrap();
        let events = vec![
            WorldModelEvent {
                snapshot_id: snapshot.snapshot_id.clone(),
                seq: 1,
                timestamp: Utc::now(),
                payload: WorldModelEventPayload::WorldModelBuildStarted {
                    repository: "fixture".into(),
                    commit: COMMIT_A.into(),
                },
            },
            WorldModelEvent {
                snapshot_id: snapshot.snapshot_id.clone(),
                seq: 2,
                timestamp: Utc::now(),
                payload: WorldModelEventPayload::WorldModelSnapshotCreated {
                    status: WorldModelSnapshotStatus::Complete,
                },
            },
        ];
        store.append_world_model_events(&events).await.unwrap();
        assert_eq!(
            store
                .world_model_events(&snapshot.snapshot_id)
                .await
                .unwrap(),
            events
        );
    }
}
