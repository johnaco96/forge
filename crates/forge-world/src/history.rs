use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use async_trait::async_trait;
use forge_core::ids::{TaskId, WorldModelFactId};
use forge_core::task::EngineeringTask;
use forge_core::world::{
    Component, EvidenceConfidence, ExtractorIdentity, FactMetadata, FailureModeStatus, Invariant,
    KnownFailureMode, RepositoryPath, SourceLocation, WorldEntityKind, WorldEntityRef,
    WorldModelFacts, WorldModelProvenance, WorldModelProvenanceSource,
};
use forge_store::FailureFilter;

use crate::{ExtractionContext, WorldBuildError, WorldBuildResult, WorldModelExtractor};

const EXTRACTOR_NAME: &str = "forge-task-history";
const EXTRACTOR_VERSION: &str = "1";

/// Extracts task-declared components and invariants plus compact links to the
/// immutable experience ledger. It never copies evaluator output or logs.
pub struct TaskHistoryExtractor {
    pub include_task_files: bool,
    pub include_history: bool,
}

#[derive(Debug)]
struct ComponentSeed {
    name: String,
    related_tasks: BTreeSet<TaskId>,
    provenance: Vec<WorldModelProvenance>,
    confidence: EvidenceConfidence,
}

#[async_trait]
impl WorldModelExtractor for TaskHistoryExtractor {
    fn identity(&self) -> ExtractorIdentity {
        ExtractorIdentity::new(EXTRACTOR_NAME, EXTRACTOR_VERSION)
    }

    async fn extract(&self, context: &ExtractionContext<'_>) -> WorldBuildResult<WorldModelFacts> {
        let mut components = BTreeMap::<String, ComponentSeed>::new();
        let mut invariants = Vec::new();
        if self.include_task_files {
            for task_file in task_files(context)? {
                let task = load_task(context, &task_file)?;
                task.validate().map_err(|error| WorldBuildError::Parse {
                    path: context.repository.root().join(task_file.as_str()),
                    message: error.to_string(),
                })?;
                if task.repository != context.repository_name {
                    continue;
                }
                let declared_components = if task.components.is_empty() {
                    vec!["repository".to_string()]
                } else {
                    task.components.clone()
                };
                for name in &declared_components {
                    add_component_from_task(
                        &mut components,
                        name,
                        &task.task_id,
                        &task_file,
                        context.commit,
                    );
                }
                let subject_name = &declared_components[0];
                let subject_id = component_id(subject_name);
                let related_evaluators = task
                    .evaluation
                    .checks()
                    .into_iter()
                    .map(|(name, _)| name)
                    .collect::<Vec<_>>();
                for (index, statement) in task.constraints.iter().enumerate() {
                    invariants.push(Invariant {
                        metadata: FactMetadata::new(
                            WorldModelFactId::stable(
                                WorldEntityKind::Invariant,
                                &format!("task:{}:constraint:{index}", task.task_id),
                            ),
                            context.snapshot_id.clone(),
                            EvidenceConfidence::Declared,
                            task_provenance(&task_file, context.commit),
                        ),
                        subject: WorldEntityRef::new(
                            WorldEntityKind::Component,
                            subject_id.clone(),
                        ),
                        statement: statement.clone(),
                        enforcement: Some("Forge task constraint".into()),
                        related_evaluators: related_evaluators.clone(),
                    });
                }
            }
        }

        let mut known_failure_modes = Vec::new();
        if self.include_history {
            let failures = context
                .store
                .failures(&FailureFilter {
                    repository: Some(context.repository_name.to_string()),
                    limit: 1_000,
                    ..Default::default()
                })
                .await?;
            for failure in failures {
                let failure_components = if failure.components.is_empty() {
                    vec!["repository".to_string()]
                } else {
                    failure.components.clone()
                };
                let mut component_ids = Vec::new();
                for name in &failure_components {
                    add_component_from_run(&mut components, name, &failure.run_id);
                    component_ids.push(component_id(name));
                }
                let mut related_commits = vec![failure.base_commit.clone()];
                if let Some(candidate_commit) = &failure.candidate_commit {
                    related_commits.push(candidate_commit.clone());
                }
                related_commits.sort();
                related_commits.dedup();
                let related_evaluators = failure
                    .failed_evaluators
                    .iter()
                    .map(|evaluator| evaluator.evaluator_id.clone())
                    .collect::<Vec<_>>();
                let symptoms = if related_evaluators.is_empty() {
                    vec![format!("Forge outcome {:?}", failure.outcome)]
                } else {
                    related_evaluators
                        .iter()
                        .map(|evaluator| format!("evaluator `{evaluator}` failed"))
                        .collect()
                };
                known_failure_modes.push(KnownFailureMode {
                    metadata: FactMetadata::new(
                        WorldModelFactId::stable(
                            WorldEntityKind::KnownFailureMode,
                            &format!("historical-run:{}", failure.run_id),
                        ),
                        context.snapshot_id.clone(),
                        EvidenceConfidence::Observed,
                        history_provenance(&failure.run_id),
                    ),
                    components: component_ids,
                    description: failure.failure_reason.unwrap_or_else(|| {
                        format!("Historical Forge run {} did not pass", failure.run_id)
                    }),
                    symptoms,
                    known_trigger: None,
                    related_runs: vec![failure.run_id],
                    related_evaluators,
                    related_commits,
                    status: FailureModeStatus::Open,
                });
            }
        }

        let mut facts = WorldModelFacts {
            components: components
                .into_values()
                .map(|seed| Component {
                    metadata: FactMetadata {
                        id: component_id(&seed.name),
                        snapshot_id: context.snapshot_id.clone(),
                        confidence: seed.confidence,
                        provenance: seed.provenance,
                        contradicts: Vec::new(),
                    },
                    name: seed.name.clone(),
                    description: format!("Repository component `{}`", seed.name),
                    paths: Vec::new(),
                    parent: None,
                    tags: vec!["task-classified".into()],
                    related_tasks: seed.related_tasks.into_iter().collect(),
                })
                .collect(),
            invariants,
            known_failure_modes,
            ..Default::default()
        };
        facts.canonicalize();
        Ok(facts)
    }
}

fn task_files(context: &ExtractionContext<'_>) -> WorldBuildResult<Vec<RepositoryPath>> {
    let directory = context.safe_path(".forge/tasks")?;
    if !directory.is_dir() {
        return Ok(Vec::new());
    }
    let mut files = Vec::new();
    visit_task_directory(context, &directory, &mut files)?;
    files.sort();
    Ok(files)
}

fn visit_task_directory(
    context: &ExtractionContext<'_>,
    directory: &Path,
    files: &mut Vec<RepositoryPath>,
) -> WorldBuildResult<()> {
    let mut entries = std::fs::read_dir(directory)
        .map_err(|source| WorldBuildError::Read {
            path: directory.to_path_buf(),
            source,
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|source| WorldBuildError::Read {
            path: directory.to_path_buf(),
            source,
        })?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let file_type = entry.file_type().map_err(|source| WorldBuildError::Read {
            path: entry.path(),
            source,
        })?;
        if file_type.is_symlink() {
            return Err(WorldBuildError::UnsafeRepositoryPath(entry.path()));
        }
        if file_type.is_dir() {
            visit_task_directory(context, &entry.path(), files)?;
            continue;
        }
        if !file_type.is_file() || !is_task_extension(&entry.path()) {
            continue;
        }
        let relative = entry
            .path()
            .strip_prefix(context.repository.root())
            .map_err(|_| WorldBuildError::UnsafeRepositoryPath(entry.path()))?
            .to_string_lossy()
            .replace('\\', "/");
        context.safe_path(&relative)?;
        files.push(RepositoryPath::new(relative)?);
    }
    Ok(())
}

fn is_task_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| matches!(extension, "yaml" | "yml" | "json"))
}

fn load_task(
    context: &ExtractionContext<'_>,
    relative: &RepositoryPath,
) -> WorldBuildResult<EngineeringTask> {
    let path = context.safe_path(relative.as_str())?;
    let body = std::fs::read_to_string(&path).map_err(|source| WorldBuildError::Read {
        path: path.clone(),
        source,
    })?;
    let mut value: serde_yaml_ng::Value =
        serde_yaml_ng::from_str(&body).map_err(|error| WorldBuildError::Parse {
            path: path.clone(),
            message: error.to_string(),
        })?;
    // Phase 5 team plans add a transport-level `team` key around the same task
    // definition. The world extractor consumes the shared EngineeringTask only.
    if let Some(mapping) = value.as_mapping_mut() {
        mapping.remove(serde_yaml_ng::Value::String("team".into()));
    }
    serde_yaml_ng::from_value(value).map_err(|error| WorldBuildError::Parse {
        path,
        message: error.to_string(),
    })
}

fn add_component_from_task(
    components: &mut BTreeMap<String, ComponentSeed>,
    name: &str,
    task_id: &TaskId,
    path: &RepositoryPath,
    commit: &str,
) {
    let entry = components
        .entry(name.to_string())
        .or_insert_with(|| ComponentSeed {
            name: name.to_string(),
            related_tasks: BTreeSet::new(),
            provenance: Vec::new(),
            confidence: EvidenceConfidence::Declared,
        });
    entry.related_tasks.insert(task_id.clone());
    let provenance = task_provenance(path, commit);
    if !entry.provenance.contains(&provenance) {
        entry.provenance.push(provenance);
    }
    entry.confidence = EvidenceConfidence::Declared;
}

fn add_component_from_run(
    components: &mut BTreeMap<String, ComponentSeed>,
    name: &str,
    run_id: &forge_core::ids::RunId,
) {
    let provenance = history_provenance(run_id);
    let entry = components
        .entry(name.to_string())
        .or_insert_with(|| ComponentSeed {
            name: name.to_string(),
            related_tasks: BTreeSet::new(),
            provenance: Vec::new(),
            confidence: EvidenceConfidence::Observed,
        });
    if !entry.provenance.contains(&provenance) {
        entry.provenance.push(provenance);
    }
}

fn component_id(name: &str) -> WorldModelFactId {
    WorldModelFactId::stable(
        WorldEntityKind::Component,
        &format!("task-component:{}", name.to_ascii_lowercase()),
    )
}

fn task_provenance(path: &RepositoryPath, commit: &str) -> WorldModelProvenance {
    WorldModelProvenance {
        extractor: ExtractorIdentity::new(EXTRACTOR_NAME, EXTRACTOR_VERSION),
        source: WorldModelProvenanceSource::UserDeclared {
            location: SourceLocation::new(path.clone(), commit),
        },
    }
}

fn history_provenance(run_id: &forge_core::ids::RunId) -> WorldModelProvenance {
    WorldModelProvenance {
        extractor: ExtractorIdentity::new(EXTRACTOR_NAME, EXTRACTOR_VERSION),
        source: WorldModelProvenanceSource::HistoricalRun {
            run_id: run_id.clone(),
        },
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::process::Command;

    use forge_core::agent::AgentConfig;
    use forge_core::ids::{AgentId, RunId, WorldModelSnapshotId};
    use forge_core::run::{AgentRun, RunOutcome, RunStatus};
    use forge_git::Repository;
    use forge_store::Store;

    use super::*;

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
    async fn task_and_history_extraction_links_declared_and_observed_evidence() {
        let temp = tempfile::tempdir().unwrap();
        git(temp.path(), &["init", "-q"]);
        git(temp.path(), &["config", "user.name", "Forge Test"]);
        git(
            temp.path(),
            &["config", "user.email", "forge@example.invalid"],
        );
        std::fs::create_dir_all(temp.path().join(".forge/tasks")).unwrap();
        std::fs::write(
            temp.path().join(".forge/tasks/storage.yaml"),
            "task_id: T-1042\nrepository: fixture\nobjective: Repair storage durability\nconstraints:\n  - Writes remain atomic\ncomponents:\n  - storage\nevaluation: {}\nteam:\n  plan_version: ignored-by-extractor\n",
        )
        .unwrap();
        git(temp.path(), &["add", "."]);
        git(temp.path(), &["commit", "-qm", "task metadata"]);

        let repository = Repository::discover(temp.path()).unwrap();
        let commit = repository.resolve("HEAD").unwrap();
        let store = Store::open_in_memory().await.unwrap();
        let task = EngineeringTask::load(temp.path().join(".forge/tasks/storage.yaml"));
        assert!(
            task.is_err(),
            "raw task parser must still reject team transport data"
        );
        let task = load_task(
            &ExtractionContext {
                snapshot_id: &WorldModelSnapshotId::sequential(1),
                repository: &repository,
                repository_name: "fixture",
                commit: &commit,
                store: &store,
            },
            &RepositoryPath::new(".forge/tasks/storage.yaml").unwrap(),
        )
        .unwrap();
        store.upsert_task(&task).await.unwrap();
        let mut run = AgentRun::new(
            RunId::sequential(1),
            task.task_id.clone(),
            AgentConfig::new(AgentId::new("stub").unwrap(), "stub"),
            &commit,
        );
        run.status = RunStatus::Completed;
        run.outcome = Some(RunOutcome::Failed);
        run.failure_reason = Some("atomicity evaluator failed".into());
        store.save_run(&run, None).await.unwrap();

        let snapshot_id = WorldModelSnapshotId::sequential(1);
        let context = ExtractionContext {
            snapshot_id: &snapshot_id,
            repository: &repository,
            repository_name: "fixture",
            commit: &commit,
            store: &store,
        };
        let facts = TaskHistoryExtractor {
            include_task_files: true,
            include_history: true,
        }
        .extract(&context)
        .await
        .unwrap();
        assert_eq!(facts.components.len(), 1);
        assert_eq!(facts.invariants.len(), 1);
        assert_eq!(facts.known_failure_modes.len(), 1);
        assert_eq!(facts.components[0].related_tasks, vec![task.task_id]);
        assert_eq!(
            facts.components[0].metadata.confidence,
            EvidenceConfidence::Declared
        );
        assert!(
            facts.components[0]
                .metadata
                .provenance
                .iter()
                .any(|provenance| {
                    matches!(
                        provenance.source,
                        WorldModelProvenanceSource::HistoricalRun { .. }
                    )
                })
        );
        assert_eq!(
            facts.known_failure_modes[0].related_runs,
            vec![RunId::sequential(1)]
        );
        assert_eq!(facts.invariants[0].related_evaluators, Vec::<String>::new());
    }
}
