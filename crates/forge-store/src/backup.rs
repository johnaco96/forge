//! Consistent SQLite backup, verification, and staged restore.

use std::path::{Path, PathBuf};
use std::str::FromStr;

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use crate::{Store, StoreError, StoreResult};

pub const LATEST_MIGRATION_VERSION: i64 = 12;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreVerification {
    pub integrity_ok: bool,
    pub foreign_key_violations: u64,
    pub migration_version: i64,
    pub run_count: u64,
}

impl StoreVerification {
    pub fn is_usable(&self) -> bool {
        self.integrity_ok
            && self.foreign_key_violations == 0
            && self.migration_version <= LATEST_MIGRATION_VERSION
    }
}

impl Store {
    /// Creates a single-file, transactionally consistent snapshot using
    /// SQLite's `VACUUM INTO`. Committed WAL content is included; copying only
    /// the main database file is never used.
    pub async fn backup_to(&self, destination: impl AsRef<Path>) -> StoreResult<StoreVerification> {
        let destination = destination.as_ref();
        prepare_destination(destination)?;
        let staging = partial_path(destination, "backup");
        remove_if_exists(&staging)?;

        let result = async {
            sqlx::query("VACUUM INTO ?1")
                .bind(staging.to_string_lossy().as_ref())
                .execute(self.pool())
                .await?;
            let verification = Self::verify_file(&staging).await?;
            if !verification.is_usable()
                || verification.migration_version != LATEST_MIGRATION_VERSION
            {
                return Err(StoreError::Corrupt(format!(
                    "backup verification failed: {verification:?}"
                )));
            }
            std::fs::rename(&staging, destination).map_err(|source| StoreError::Io {
                context: format!(
                    "publishing verified backup {} as {}",
                    staging.display(),
                    destination.display()
                ),
                source,
            })?;
            Ok(verification)
        }
        .await;

        if result.is_err() {
            let _ = std::fs::remove_file(&staging);
        }
        result
    }

    /// Verifies integrity, foreign keys, migration compatibility, and basic
    /// readability without migrating or mutating the file.
    pub async fn verify_file(path: impl AsRef<Path>) -> StoreResult<StoreVerification> {
        let path = path.as_ref();
        if !path.is_file() {
            return Err(StoreError::NotFound(format!(
                "store file `{}`",
                path.display()
            )));
        }
        let options = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))?
            .read_only(true)
            .create_if_missing(false)
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await?;
        let integrity: String = sqlx::query_scalar("PRAGMA integrity_check")
            .fetch_one(&pool)
            .await?;
        let foreign_key_violations: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM pragma_foreign_key_check")
                .fetch_one(&pool)
                .await?;
        let migration_version: Option<i64> =
            sqlx::query_scalar("SELECT MAX(version) FROM _sqlx_migrations WHERE success = 1")
                .fetch_one(&pool)
                .await?;
        let run_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runs")
            .fetch_one(&pool)
            .await?;
        pool.close().await;
        Ok(StoreVerification {
            integrity_ok: integrity == "ok",
            foreign_key_violations: foreign_key_violations.max(0) as u64,
            migration_version: migration_version.unwrap_or(0),
            run_count: run_count.max(0) as u64,
        })
    }

    /// Restores through a separate staged database, migrates that stage
    /// forward, verifies it, and only then replaces `destination`. If any step
    /// fails, the existing destination is left in place or rolled back.
    pub async fn restore_from(
        source: impl AsRef<Path>,
        destination: impl AsRef<Path>,
        replace: bool,
    ) -> StoreResult<StoreVerification> {
        let source = source.as_ref();
        let destination = destination.as_ref();
        if source == destination {
            return Err(StoreError::Corrupt(
                "backup source and restore destination are the same path".into(),
            ));
        }
        let source_verification = Self::verify_file(source).await?;
        if !source_verification.is_usable() {
            return Err(StoreError::Corrupt(format!(
                "backup is not restorable: {source_verification:?}"
            )));
        }
        if destination.exists() && !replace {
            return Err(StoreError::Corrupt(format!(
                "restore destination `{}` exists; explicit replacement is required",
                destination.display()
            )));
        }
        prepare_parent(destination)?;
        let staging = partial_path(destination, "restore");
        let rollback = partial_path(destination, "rollback");
        remove_if_exists(&staging)?;
        remove_if_exists(&rollback)?;

        let result = async {
            vacuum_file_into(source, &staging).await?;

            // Migration happens only on the stage. An incompatible or corrupt
            // backup therefore cannot touch the existing operational store.
            let staged = Store::open(&staging).await?;
            staged.close().await;
            let verification = Self::verify_file(&staging).await?;
            if !verification.is_usable()
                || verification.migration_version != LATEST_MIGRATION_VERSION
            {
                return Err(StoreError::Corrupt(format!(
                    "restored stage verification failed: {verification:?}"
                )));
            }

            if destination.exists() {
                checkpoint_for_replace(destination).await?;
                // A zero-length WAL still carries database identity. It must
                // not be left beside the newly installed main file.
                remove_sqlite_sidecars(destination)?;
                std::fs::rename(destination, &rollback).map_err(|source| StoreError::Io {
                    context: format!("staging existing store `{}` for rollback", destination.display()),
                    source,
                })?;
            }
            if let Err(source) = std::fs::rename(&staging, destination) {
                if rollback.exists() {
                    let _ = std::fs::rename(&rollback, destination);
                }
                return Err(StoreError::Io {
                    context: format!("installing restored store `{}`", destination.display()),
                    source,
                });
            }

            match Self::verify_file(destination).await {
                Ok(installed) if installed == verification => {
                    remove_if_exists(&rollback)?;
                    Ok(installed)
                }
                verification_error => {
                    let failed = partial_path(destination, "failed-restore");
                    let _ = std::fs::rename(destination, &failed);
                    if rollback.exists() {
                        let _ = std::fs::rename(&rollback, destination);
                    }
                    let _ = std::fs::remove_file(failed);
                    match verification_error {
                        Ok(actual) => Err(StoreError::Corrupt(format!(
                            "installed restore changed during verification: expected {verification:?}, got {actual:?}"
                        ))),
                        Err(error) => Err(error),
                    }
                }
            }
        }
        .await;

        if result.is_err() {
            let _ = std::fs::remove_file(&staging);
            if rollback.exists() && !destination.exists() {
                let _ = std::fs::rename(&rollback, destination);
            }
        }
        result
    }
}

async fn vacuum_file_into(source: &Path, destination: &Path) -> StoreResult<()> {
    let options = SqliteConnectOptions::from_str(&format!("sqlite://{}", source.display()))?
        .read_only(true)
        .create_if_missing(false);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await?;
    sqlx::query("VACUUM INTO ?1")
        .bind(destination.to_string_lossy().as_ref())
        .execute(&pool)
        .await?;
    pool.close().await;
    Ok(())
}

async fn checkpoint_for_replace(path: &Path) -> StoreResult<()> {
    let options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(false)
        .busy_timeout(std::time::Duration::from_secs(2));
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await?;
    let row: (i64, i64, i64) = sqlx::query_as("PRAGMA wal_checkpoint(TRUNCATE)")
        .fetch_one(&pool)
        .await?;
    pool.close().await;
    if row.0 != 0 {
        return Err(StoreError::Corrupt(format!(
            "store `{}` is busy; stop other Forge processes before restore",
            path.display()
        )));
    }
    Ok(())
}

fn prepare_destination(path: &Path) -> StoreResult<()> {
    if path.exists() {
        return Err(StoreError::Corrupt(format!(
            "backup destination `{}` already exists",
            path.display()
        )));
    }
    prepare_parent(path)
}

fn prepare_parent(path: &Path) -> StoreResult<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| StoreError::Io {
            context: format!("creating backup directory `{}`", parent.display()),
            source,
        })?;
    }
    Ok(())
}

fn partial_path(path: &Path, label: &str) -> PathBuf {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("forge.db");
    path.with_file_name(format!(".{name}.{label}-{}-{stamp}", std::process::id()))
}

fn remove_if_exists(path: &Path) -> StoreResult<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(StoreError::Io {
            context: format!("removing `{}`", path.display()),
            source,
        }),
    }
}

fn remove_sqlite_sidecars(path: &Path) -> StoreResult<()> {
    for suffix in ["-wal", "-shm"] {
        let sidecar = PathBuf::from(format!("{}{suffix}", path.display()));
        remove_if_exists(&sidecar)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_core::agent::AgentConfig;
    use forge_core::ids::{AgentId, RunId, TaskId};
    use forge_core::run::AgentRun;
    use forge_core::task::{EngineeringTask, EvaluationSpec, TaskMetadata, TaskRevision};

    fn task() -> EngineeringTask {
        EngineeringTask {
            task_id: TaskId::sequential(1),
            repository: "forge".into(),
            objective: "exercise backup recovery".into(),
            constraints: Vec::new(),
            evaluation: EvaluationSpec::default(),
            protection: Default::default(),
            metadata: TaskMetadata::default(),
            classification: Default::default(),
            components: Vec::new(),
            tags: Vec::new(),
        }
    }

    fn run(id: u64) -> AgentRun {
        AgentRun::new(
            RunId::sequential(id),
            TaskId::sequential(1),
            AgentConfig::new(AgentId::new("fixture").unwrap(), "fixture"),
            "a".repeat(40),
        )
    }

    #[tokio::test]
    async fn wal_active_backup_restores_equal_history_without_touching_source() {
        let temp = tempfile::tempdir().unwrap();
        let active_path = temp.path().join("active.db");
        let backup_path = temp.path().join("backup.db");
        let restored_path = temp.path().join("restored.db");
        let active = Store::open(&active_path).await.unwrap();
        active.upsert_task(&task()).await.unwrap();
        active.save_run(&run(1), None).await.unwrap();

        let backup = active.backup_to(&backup_path).await.unwrap();
        assert_eq!(backup.run_count, 1);
        active.save_run(&run(2), None).await.unwrap();
        assert_eq!(active.run_count().await.unwrap(), 2);

        let restored = Store::restore_from(&backup_path, &restored_path, false)
            .await
            .unwrap();
        assert_eq!(restored.run_count, 1);
        let restored_store = Store::open(&restored_path).await.unwrap();
        assert_eq!(restored_store.export_records().await.unwrap().len(), 1);
        assert_eq!(active.export_records().await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn corrupt_backup_and_failed_restore_leave_existing_store_untouched() {
        let temp = tempfile::tempdir().unwrap();
        let existing_path = temp.path().join("existing.db");
        let corrupt_path = temp.path().join("corrupt.db");
        let existing = Store::open(&existing_path).await.unwrap();
        existing.upsert_task(&task()).await.unwrap();
        existing.save_run(&run(1), None).await.unwrap();
        existing.close().await;
        std::fs::write(&corrupt_path, b"not sqlite").unwrap();

        assert!(
            Store::restore_from(&corrupt_path, &existing_path, true)
                .await
                .is_err()
        );
        let reopened = Store::open(&existing_path).await.unwrap();
        assert_eq!(reopened.run_count().await.unwrap(), 1);
    }

    #[tokio::test]
    async fn backup_publication_refuses_overwrite_and_leaves_no_partial_file() {
        let temp = tempfile::tempdir().unwrap();
        let active = Store::open(temp.path().join("active.db")).await.unwrap();
        let destination = temp.path().join("backup.db");
        std::fs::write(&destination, b"keep").unwrap();
        assert!(active.backup_to(&destination).await.is_err());
        assert_eq!(std::fs::read(&destination).unwrap(), b"keep");
        assert_eq!(
            std::fs::read_dir(temp.path())
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| entry.file_name().to_string_lossy().contains("partial"))
                .count(),
            0
        );
    }

    #[tokio::test]
    async fn restore_migrates_a_verified_older_schema_only_in_staging() {
        let temp = tempfile::tempdir().unwrap();
        let migrations = temp.path().join("old-migrations");
        std::fs::create_dir(&migrations).unwrap();
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        for name in [
            "0001_init.sql",
            "0002_run_outcome.sql",
            "0003_experiments.sql",
            "0004_evaluator_results.sql",
            "0005_experience_queries.sql",
            "0006_immutable_task_revisions.sql",
            "0007_execution_provenance.sql",
        ] {
            std::fs::copy(
                manifest.join("migrations").join(name),
                migrations.join(name),
            )
            .unwrap();
        }
        let old_path = temp.path().join("phase-three.db");
        let options = SqliteConnectOptions::new()
            .filename(&old_path)
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::migrate::Migrator::new(migrations)
            .await
            .unwrap()
            .run(&pool)
            .await
            .unwrap();
        let historical_task = task();
        let revision = TaskRevision::snapshot(historical_task.clone()).unwrap();
        let historical_run = run(1);
        let created_at = historical_run.created_at.to_rfc3339();
        sqlx::query(
            "INSERT INTO tasks (
                task_id, repository, objective, definition_json, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
        )
        .bind(historical_task.task_id.as_str())
        .bind(&historical_task.repository)
        .bind(&historical_task.objective)
        .bind(serde_json::to_string(&historical_task).unwrap())
        .bind(&created_at)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO task_revisions (
                revision_id, task_id, repository, objective, definition_json, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )
        .bind(revision.revision_id().as_str())
        .bind(historical_task.task_id.as_str())
        .bind(&historical_task.repository)
        .bind(&historical_task.objective)
        .bind(serde_json::to_string(&historical_task).unwrap())
        .bind(&created_at)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("UPDATE tasks SET current_revision_id = ?2 WHERE task_id = ?1")
            .bind(historical_task.task_id.as_str())
            .bind(revision.revision_id().as_str())
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO agent_configs (
                fingerprint, agent_id, harness, tools_json, settings_json, first_seen_at
             ) VALUES (?1, ?2, ?3, '[]', '{}', ?4)",
        )
        .bind(historical_run.agent.fingerprint())
        .bind(historical_run.agent.agent_id.as_str())
        .bind(&historical_run.agent.harness)
        .bind(&created_at)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO runs (
                run_id, task_id, agent_id, config_fingerprint, base_commit, status,
                created_at, record_json, task_revision_id, execution_provenance
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'unknown')",
        )
        .bind(historical_run.run_id.as_str())
        .bind(historical_run.task_id.as_str())
        .bind(historical_run.agent.agent_id.as_str())
        .bind(historical_run.agent.fingerprint())
        .bind(&historical_run.base_commit)
        .bind(historical_run.status.as_str())
        .bind(&created_at)
        .bind(serde_json::to_string(&historical_run).unwrap())
        .bind(revision.revision_id().as_str())
        .execute(&pool)
        .await
        .unwrap();
        pool.close().await;
        assert_eq!(
            Store::verify_file(&old_path)
                .await
                .unwrap()
                .migration_version,
            7
        );

        let restored_path = temp.path().join("restored.db");
        let restored = Store::restore_from(&old_path, &restored_path, false)
            .await
            .unwrap();
        assert_eq!(restored.migration_version, LATEST_MIGRATION_VERSION);
        assert!(restored.integrity_ok);
        assert_eq!(restored.run_count, 1);
        let reopened = Store::open(&restored_path).await.unwrap();
        let records = reopened.export_records().await.unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].task.objective, "exercise backup recovery");
    }
}
