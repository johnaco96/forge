use std::path::{Path, PathBuf};

use async_trait::async_trait;
use chrono::Utc;
use forge_core::config::WorldModelConfig;
use forge_core::ids::WorldModelSnapshotId;
use forge_core::world::{
    ExtractorIdentity, ExtractorRecord, ExtractorStatus, SnapshotRelation,
    WORLD_MODEL_SCHEMA_VERSION, WorldModelEvent, WorldModelEventPayload, WorldModelFacts,
    WorldModelSnapshot, WorldModelSnapshotSource, WorldModelSnapshotStatus,
};
use forge_git::{GitError, Repository};
use forge_store::Store;
use sha2::{Digest, Sha256};

use crate::{RustWorkspaceExtractor, TaskHistoryExtractor, WorldBuildError, WorldBuildResult};

pub struct ExtractionContext<'a> {
    pub snapshot_id: &'a WorldModelSnapshotId,
    pub repository: &'a Repository,
    pub repository_name: &'a str,
    pub commit: &'a str,
    pub store: &'a Store,
}

impl ExtractionContext<'_> {
    /// Resolves one existing repository-relative path without following a
    /// symlink or escaping the repository root.
    pub fn safe_path(&self, relative: &str) -> WorldBuildResult<PathBuf> {
        let relative = forge_core::RepositoryPath::new(relative)?;
        let root =
            self.repository
                .root()
                .canonicalize()
                .map_err(|source| WorldBuildError::Read {
                    path: self.repository.root().to_path_buf(),
                    source,
                })?;
        let mut current = root.clone();
        for segment in Path::new(relative.as_str()).components() {
            let std::path::Component::Normal(segment) = segment else {
                return Err(WorldBuildError::UnsafeRepositoryPath(PathBuf::from(
                    relative.as_str(),
                )));
            };
            current.push(segment);
            if current.exists() {
                let metadata = std::fs::symlink_metadata(&current).map_err(|source| {
                    WorldBuildError::Read {
                        path: current.clone(),
                        source,
                    }
                })?;
                if metadata.file_type().is_symlink() {
                    return Err(WorldBuildError::UnsafeRepositoryPath(current));
                }
            }
        }
        if current.exists() {
            let canonical = current
                .canonicalize()
                .map_err(|source| WorldBuildError::Read {
                    path: current.clone(),
                    source,
                })?;
            if !canonical.starts_with(&root) {
                return Err(WorldBuildError::UnsafeRepositoryPath(current));
            }
        }
        Ok(current)
    }
}

#[async_trait]
pub trait WorldModelExtractor: Send + Sync {
    fn identity(&self) -> ExtractorIdentity;
    async fn extract(&self, context: &ExtractionContext<'_>) -> WorldBuildResult<WorldModelFacts>;
}

struct ConfiguredExtractor {
    extractor: Box<dyn WorldModelExtractor>,
    required: bool,
}

pub struct WorldModelBuilder {
    extractors: Vec<ConfiguredExtractor>,
}

impl WorldModelBuilder {
    pub fn new() -> Self {
        Self {
            extractors: Vec::new(),
        }
    }

    pub fn from_config(config: &WorldModelConfig) -> WorldBuildResult<Self> {
        if !config.enabled {
            return Err(WorldBuildError::Disabled);
        }
        let mut builder = Self::new();
        if config.structure {
            builder.add_extractor(RustWorkspaceExtractor, true);
        }
        if config.task_metadata || config.history {
            builder.add_extractor(
                TaskHistoryExtractor {
                    include_task_files: config.task_metadata,
                    include_history: config.history,
                },
                false,
            );
        }
        Ok(builder)
    }

    pub fn add_extractor(&mut self, extractor: impl WorldModelExtractor + 'static, required: bool) {
        self.extractors.push(ConfiguredExtractor {
            extractor: Box::new(extractor),
            required,
        });
    }

    pub async fn build(
        &self,
        snapshot_id: WorldModelSnapshotId,
        repository: &Repository,
        repository_name: &str,
        commit: &str,
        store: &Store,
    ) -> WorldBuildResult<WorldModelBuildReport> {
        let head = repository.resolve("HEAD")?;
        if head != commit {
            return Err(WorldBuildError::CheckoutCommitMismatch {
                requested: commit.into(),
                head,
            });
        }
        if !repository.is_clean()? {
            return Err(WorldBuildError::DirtyRepository);
        }
        let mut events = Vec::new();
        emit(
            &snapshot_id,
            &mut events,
            WorldModelEventPayload::WorldModelBuildStarted {
                repository: repository_name.into(),
                commit: commit.into(),
            },
        );
        let context = ExtractionContext {
            snapshot_id: &snapshot_id,
            repository,
            repository_name,
            commit,
            store,
        };
        let mut facts = WorldModelFacts::default();
        let mut records = Vec::new();
        for configured in &self.extractors {
            let identity = configured.extractor.identity();
            emit(
                &snapshot_id,
                &mut events,
                WorldModelEventPayload::ExtractorStarted {
                    extractor: identity.clone(),
                },
            );
            match configured.extractor.extract(&context).await {
                Ok(extracted) => {
                    let count = extracted.summary().total();
                    facts.extend(extracted);
                    records.push(ExtractorRecord {
                        identity: identity.clone(),
                        required: configured.required,
                        status: ExtractorStatus::Completed,
                        facts_produced: count,
                        configuration_fingerprint: extractor_fingerprint(
                            &identity,
                            configured.required,
                        ),
                        error: None,
                    });
                    emit(
                        &snapshot_id,
                        &mut events,
                        WorldModelEventPayload::ExtractorCompleted {
                            extractor: identity,
                            fact_count: count,
                        },
                    );
                }
                Err(error) => {
                    let message = error.to_string();
                    records.push(ExtractorRecord {
                        identity: identity.clone(),
                        required: configured.required,
                        status: ExtractorStatus::Failed,
                        facts_produced: 0,
                        configuration_fingerprint: extractor_fingerprint(
                            &identity,
                            configured.required,
                        ),
                        error: Some(message.clone()),
                    });
                    emit(
                        &snapshot_id,
                        &mut events,
                        WorldModelEventPayload::ExtractorFailed {
                            extractor: identity,
                            required: configured.required,
                            error: message,
                        },
                    );
                }
            }
        }
        facts.canonicalize();
        let status = if records
            .iter()
            .any(|record| record.required && record.status == ExtractorStatus::Failed)
        {
            WorldModelSnapshotStatus::Failed
        } else if records
            .iter()
            .any(|record| record.status == ExtractorStatus::Failed)
        {
            WorldModelSnapshotStatus::Partial
        } else {
            WorldModelSnapshotStatus::Complete
        };
        let snapshot = WorldModelSnapshot {
            snapshot_id: snapshot_id.clone(),
            repository: repository_name.into(),
            commit: commit.into(),
            created_at: Utc::now(),
            source: if self.extractors.len() > 1 {
                WorldModelSnapshotSource::Mixed
            } else {
                WorldModelSnapshotSource::Deterministic
            },
            schema_version: WORLD_MODEL_SCHEMA_VERSION.into(),
            status,
            extractors: records,
            facts,
        };
        if let Err(error) = snapshot.validate() {
            emit(
                &snapshot_id,
                &mut events,
                WorldModelEventPayload::WorldModelBuildFailed {
                    error: error.to_string(),
                },
            );
            return Err(error.into());
        }
        emit(
            &snapshot_id,
            &mut events,
            WorldModelEventPayload::WorldModelValidated {
                fact_count: snapshot.summary().total(),
            },
        );
        emit(
            &snapshot_id,
            &mut events,
            WorldModelEventPayload::WorldModelSnapshotCreated { status },
        );
        Ok(WorldModelBuildReport { snapshot, events })
    }
}

impl Default for WorldModelBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone)]
pub struct WorldModelBuildReport {
    pub snapshot: WorldModelSnapshot,
    pub events: Vec<WorldModelEvent>,
}

pub fn snapshot_relation(
    repository: &Repository,
    snapshot_commit: &str,
    target_commit: &str,
) -> SnapshotRelation {
    if snapshot_commit == target_commit {
        return SnapshotRelation::Exact;
    }
    if repository.resolve(snapshot_commit).is_err() || repository.resolve(target_commit).is_err() {
        return SnapshotRelation::UnknownRelation;
    }
    match repository.git([
        "merge-base",
        "--is-ancestor",
        snapshot_commit,
        target_commit,
    ]) {
        Ok(_) => SnapshotRelation::Ancestor,
        Err(GitError::CommandFailed { code: Some(1), .. }) => SnapshotRelation::Stale,
        Err(_) => SnapshotRelation::UnknownRelation,
    }
}

fn extractor_fingerprint(identity: &ExtractorIdentity, required: bool) -> String {
    let mut digest = Sha256::new();
    digest.update(identity.name.as_bytes());
    digest.update([0]);
    digest.update(identity.version.as_bytes());
    digest.update([u8::from(required)]);
    format!("{:x}", digest.finalize())
}

fn emit(
    snapshot_id: &WorldModelSnapshotId,
    events: &mut Vec<WorldModelEvent>,
    payload: WorldModelEventPayload,
) {
    events.push(WorldModelEvent {
        snapshot_id: snapshot_id.clone(),
        seq: events.len() as u64 + 1,
        timestamp: Utc::now(),
        payload,
    });
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::process::Command;

    use async_trait::async_trait;
    use forge_core::ids::WorldModelSnapshotId;
    use forge_core::world::{ExtractorIdentity, WorldModelFacts, WorldModelSnapshotStatus};

    use super::*;

    struct EmptyExtractor(&'static str);

    #[async_trait]
    impl WorldModelExtractor for EmptyExtractor {
        fn identity(&self) -> ExtractorIdentity {
            ExtractorIdentity::new(self.0, "1")
        }

        async fn extract(
            &self,
            _context: &ExtractionContext<'_>,
        ) -> WorldBuildResult<WorldModelFacts> {
            Ok(WorldModelFacts::default())
        }
    }

    struct FailingExtractor(&'static str);

    #[async_trait]
    impl WorldModelExtractor for FailingExtractor {
        fn identity(&self) -> ExtractorIdentity {
            ExtractorIdentity::new(self.0, "1")
        }

        async fn extract(
            &self,
            _context: &ExtractionContext<'_>,
        ) -> WorldBuildResult<WorldModelFacts> {
            Err(WorldBuildError::Extractor {
                extractor: self.0.into(),
                message: "controlled failure".into(),
            })
        }
    }

    fn repository() -> (tempfile::TempDir, Repository, String) {
        let temp = tempfile::tempdir().unwrap();
        git(temp.path(), &["init", "-q"]);
        git(temp.path(), &["config", "user.name", "Forge Test"]);
        git(
            temp.path(),
            &["config", "user.email", "forge@example.invalid"],
        );
        std::fs::write(temp.path().join("README.md"), "fixture\n").unwrap();
        git(temp.path(), &["add", "README.md"]);
        git(temp.path(), &["commit", "-qm", "initial"]);
        let repository = Repository::discover(temp.path()).unwrap();
        let commit = repository.resolve("HEAD").unwrap();
        (temp, repository, commit)
    }

    fn git(directory: &Path, args: &[&str]) {
        assert!(
            Command::new("git")
                .args(args)
                .current_dir(directory)
                .status()
                .unwrap()
                .success()
        );
    }

    #[tokio::test]
    async fn optional_failure_is_partial_and_required_failure_is_failed() {
        let (_temp, repository, commit) = repository();
        let store = Store::open_in_memory().await.unwrap();

        let mut optional = WorldModelBuilder::new();
        optional.add_extractor(EmptyExtractor("complete"), true);
        optional.add_extractor(FailingExtractor("optional"), false);
        let report = optional
            .build(
                WorldModelSnapshotId::sequential(1),
                &repository,
                "fixture",
                &commit,
                &store,
            )
            .await
            .unwrap();
        assert_eq!(report.snapshot.status, WorldModelSnapshotStatus::Partial);
        assert!(report.events.iter().any(|event| matches!(
            event.payload,
            WorldModelEventPayload::ExtractorFailed {
                required: false,
                ..
            }
        )));

        let mut required = WorldModelBuilder::new();
        required.add_extractor(FailingExtractor("required"), true);
        let report = required
            .build(
                WorldModelSnapshotId::sequential(2),
                &repository,
                "fixture",
                &commit,
                &store,
            )
            .await
            .unwrap();
        assert_eq!(report.snapshot.status, WorldModelSnapshotStatus::Failed);
        assert!(report.snapshot.validate().is_ok());
    }

    #[tokio::test]
    async fn extraction_refuses_dirty_or_mismatched_repository_state() {
        let (temp, repository, commit) = repository();
        let store = Store::open_in_memory().await.unwrap();
        let builder = WorldModelBuilder::new();
        let mismatch = builder
            .build(
                WorldModelSnapshotId::sequential(1),
                &repository,
                "fixture",
                &"f".repeat(40),
                &store,
            )
            .await;
        assert!(matches!(
            mismatch,
            Err(WorldBuildError::CheckoutCommitMismatch { .. })
        ));

        std::fs::write(temp.path().join("README.md"), "dirty\n").unwrap();
        let dirty = builder
            .build(
                WorldModelSnapshotId::sequential(2),
                &repository,
                "fixture",
                &commit,
                &store,
            )
            .await;
        assert!(matches!(dirty, Err(WorldBuildError::DirtyRepository)));
    }
}
