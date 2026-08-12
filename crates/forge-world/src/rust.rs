use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use forge_core::ids::WorldModelFactId;
use forge_core::world::{
    Component, Dependency, DependencyKind, EvidenceConfidence, ExtractorIdentity, FactMetadata,
    Interface, InterfaceKind, InterfaceVisibility, Module, RepositoryPath, SourceLocation,
    WorldEntityKind, WorldEntityRef, WorldModelFacts, WorldModelProvenance,
    WorldModelProvenanceSource,
};

use crate::{ExtractionContext, WorldBuildError, WorldBuildResult, WorldModelExtractor};

pub struct RustWorkspaceExtractor;

const EXTRACTOR_NAME: &str = "rust-workspace-structure";
const EXTRACTOR_VERSION: &str = "1";

#[derive(Debug)]
struct CrateRecord {
    name: String,
    description: String,
    root: RepositoryPath,
    manifest: RepositoryPath,
    dependencies: Vec<String>,
    has_library: bool,
}

#[async_trait]
impl WorldModelExtractor for RustWorkspaceExtractor {
    fn identity(&self) -> ExtractorIdentity {
        ExtractorIdentity::new(EXTRACTOR_NAME, EXTRACTOR_VERSION)
    }

    async fn extract(&self, context: &ExtractionContext<'_>) -> WorldBuildResult<WorldModelFacts> {
        let manifest_path = context.repository.root().join("Cargo.toml");
        if !manifest_path.exists() {
            return Ok(WorldModelFacts::default());
        }
        context.safe_path("Cargo.toml")?;
        let root = read_manifest(&manifest_path)?;
        let members = workspace_member_paths(context, &root)?;
        let mut crates = Vec::new();
        for member in members {
            let manifest_relative = if member.as_os_str().is_empty() {
                PathBuf::from("Cargo.toml")
            } else {
                member.join("Cargo.toml")
            };
            let manifest_relative_string = path_string(&manifest_relative);
            let manifest_path = context.safe_path(&manifest_relative_string)?;
            let manifest = read_manifest(&manifest_path)?;
            let Some(package) = manifest.get("package").and_then(toml::Value::as_table) else {
                continue;
            };
            let name = package
                .get("name")
                .and_then(toml::Value::as_str)
                .ok_or_else(|| WorldBuildError::Parse {
                    path: manifest_path.clone(),
                    message: "package.name is missing".into(),
                })?
                .to_string();
            let description = package
                .get("description")
                .and_then(toml::Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| format!("Rust crate {name}"));
            let root_relative = if member.as_os_str().is_empty() {
                RepositoryPath::new("src")?
            } else {
                RepositoryPath::new(path_string(&member))?
            };
            let library_relative = if member.as_os_str().is_empty() {
                "src/lib.rs".to_string()
            } else {
                format!("{}/src/lib.rs", path_string(&member))
            };
            let has_library = context.safe_path(&library_relative)?.is_file();
            let dependencies = manifest
                .get("dependencies")
                .and_then(toml::Value::as_table)
                .map(|dependencies| {
                    dependencies
                        .iter()
                        .map(|(name, value)| {
                            value
                                .as_table()
                                .and_then(|table| table.get("package"))
                                .and_then(toml::Value::as_str)
                                .unwrap_or(name)
                                .to_string()
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            crates.push(CrateRecord {
                name,
                description,
                root: root_relative,
                manifest: RepositoryPath::new(manifest_relative_string)?,
                dependencies,
                has_library,
            });
        }
        crates.sort_by(|left, right| left.name.cmp(&right.name));
        let by_name = crates
            .iter()
            .map(|krate| {
                (
                    krate.name.clone(),
                    WorldModelFactId::stable(
                        WorldEntityKind::Component,
                        &format!("rust-crate:{}", krate.name),
                    ),
                )
            })
            .collect::<BTreeMap<_, _>>();

        let mut facts = WorldModelFacts::default();
        for krate in &crates {
            let component_id = by_name[&krate.name].clone();
            facts.components.push(Component {
                metadata: metadata(
                    context,
                    WorldEntityKind::Component,
                    &format!("rust-crate:{}", krate.name),
                    &krate.manifest,
                ),
                name: krate.name.clone(),
                description: krate.description.clone(),
                paths: vec![krate.root.clone()],
                parent: None,
                tags: vec!["rust".into(), "crate".into()],
                related_tasks: Vec::new(),
            });
            let module_id = WorldModelFactId::stable(
                WorldEntityKind::Module,
                &format!("rust-module:{}", krate.root),
            );
            facts.modules.push(Module {
                metadata: metadata(
                    context,
                    WorldEntityKind::Module,
                    &format!("rust-module:{}", krate.root),
                    &krate.manifest,
                ),
                name: krate.name.clone(),
                path: krate.root.clone(),
                language: Some("rust".into()),
                component: Some(component_id.clone()),
            });
            if krate.has_library {
                let library_path = if krate.root.as_str() == "src" {
                    RepositoryPath::new("src/lib.rs")?
                } else {
                    RepositoryPath::new(format!("{}/src/lib.rs", krate.root))?
                };
                facts.interfaces.push(Interface {
                    metadata: metadata(
                        context,
                        WorldEntityKind::Interface,
                        &format!("rust-library-api:{}", krate.name),
                        &library_path,
                    ),
                    name: format!("{} public crate API", krate.name),
                    interface_kind: InterfaceKind::LibraryApi,
                    owner: WorldEntityRef::new(WorldEntityKind::Module, module_id),
                    location: SourceLocation::new(library_path, context.commit),
                    visibility: InterfaceVisibility::Public,
                    signature: Some(format!("crate:{}", krate.name)),
                });
            }
        }
        let mut seen = BTreeSet::new();
        for krate in &crates {
            let source_component = by_name[&krate.name].clone();
            for dependency_name in &krate.dependencies {
                let Some(target_component) = by_name.get(dependency_name) else {
                    continue;
                };
                let key = format!("rust-dependency:{}->{dependency_name}", krate.name);
                if !seen.insert(key.clone()) {
                    continue;
                }
                facts.dependencies.push(Dependency {
                    metadata: metadata(context, WorldEntityKind::Dependency, &key, &krate.manifest),
                    source: WorldEntityRef::new(
                        WorldEntityKind::Component,
                        source_component.clone(),
                    ),
                    target: WorldEntityRef::new(
                        WorldEntityKind::Component,
                        target_component.clone(),
                    ),
                    dependency_kind: DependencyKind::DependsOn,
                    evidence: Some(format!("Cargo dependency `{dependency_name}`")),
                });
            }
        }
        facts.canonicalize();
        Ok(facts)
    }
}

fn workspace_member_paths(
    context: &ExtractionContext<'_>,
    manifest: &toml::Value,
) -> WorldBuildResult<Vec<PathBuf>> {
    let mut members = Vec::new();
    if manifest.get("package").is_some() {
        members.push(PathBuf::new());
    }
    let declared = manifest
        .get("workspace")
        .and_then(|workspace| workspace.get("members"))
        .and_then(toml::Value::as_array)
        .cloned()
        .unwrap_or_default();
    for entry in declared {
        let pattern = entry.as_str().ok_or_else(|| WorldBuildError::Parse {
            path: context.repository.root().join("Cargo.toml"),
            message: "workspace member must be a string".into(),
        })?;
        if let Some(parent) = pattern.strip_suffix("/*") {
            let directory = context.safe_path(parent)?;
            let mut children = std::fs::read_dir(&directory)
                .map_err(|source| WorldBuildError::Read {
                    path: directory.clone(),
                    source,
                })?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|source| WorldBuildError::Read {
                    path: directory.clone(),
                    source,
                })?;
            children.sort_by_key(std::fs::DirEntry::file_name);
            for child in children {
                if child
                    .file_type()
                    .map_err(|source| WorldBuildError::Read {
                        path: child.path(),
                        source,
                    })?
                    .is_dir()
                    && child.path().join("Cargo.toml").is_file()
                {
                    members.push(PathBuf::from(parent).join(child.file_name()));
                }
            }
        } else {
            context.safe_path(&format!("{pattern}/Cargo.toml"))?;
            members.push(PathBuf::from(pattern));
        }
    }
    members.sort();
    members.dedup();
    Ok(members)
}

fn read_manifest(path: &Path) -> WorldBuildResult<toml::Value> {
    let body = std::fs::read_to_string(path).map_err(|source| WorldBuildError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    toml::from_str(&body).map_err(|error| WorldBuildError::Parse {
        path: path.to_path_buf(),
        message: error.to_string(),
    })
}

fn metadata(
    context: &ExtractionContext<'_>,
    kind: WorldEntityKind,
    key: &str,
    source_path: &RepositoryPath,
) -> FactMetadata {
    FactMetadata::new(
        WorldModelFactId::stable(kind, key),
        context.snapshot_id.clone(),
        EvidenceConfidence::Observed,
        WorldModelProvenance {
            extractor: ExtractorIdentity::new(EXTRACTOR_NAME, EXTRACTOR_VERSION),
            source: WorldModelProvenanceSource::SourceCode {
                location: SourceLocation::new(source_path.clone(), context.commit),
            },
        },
    )
}

fn path_string(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use forge_core::ids::WorldModelSnapshotId;
    use forge_core::world::SnapshotRelation;
    use forge_git::Repository;
    use forge_store::Store;

    use super::*;
    use crate::{ExtractionContext, WorldModelBuilder, snapshot_relation};

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

    fn write_crate(root: &Path, name: &str, dependency: Option<&str>) {
        let crate_root = root.join("crates").join(name);
        std::fs::create_dir_all(crate_root.join("src")).unwrap();
        let dependency = dependency
            .map(|dependency| {
                format!("\n[dependencies]\n{dependency} = {{ path = \"../{dependency}\" }}\n")
            })
            .unwrap_or_default();
        std::fs::write(
            crate_root.join("Cargo.toml"),
            format!(
                "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2024\"\n{dependency}"
            ),
        )
        .unwrap();
        std::fs::write(crate_root.join("src/lib.rs"), "pub fn marker() {}\n").unwrap();
    }

    fn fixture() -> (tempfile::TempDir, Repository, String) {
        let temp = tempfile::tempdir().unwrap();
        git(temp.path(), &["init", "-q"]);
        git(temp.path(), &["config", "user.name", "Forge Test"]);
        git(
            temp.path(),
            &["config", "user.email", "forge@example.invalid"],
        );
        std::fs::write(
            temp.path().join("Cargo.toml"),
            "[workspace]\nresolver = \"3\"\nmembers = [\"crates/*\"]\n",
        )
        .unwrap();
        write_crate(temp.path(), "core", None);
        write_crate(temp.path(), "api", Some("core"));
        git(temp.path(), &["add", "."]);
        git(temp.path(), &["commit", "-qm", "initial workspace"]);
        let repository = Repository::discover(temp.path()).unwrap();
        let commit = repository.resolve("HEAD").unwrap();
        (temp, repository, commit)
    }

    #[tokio::test]
    async fn rust_workspace_extraction_is_deterministic_and_typed() {
        let (_temp, repository, commit) = fixture();
        let store = Store::open_in_memory().await.unwrap();
        let snapshot_id = WorldModelSnapshotId::sequential(1);
        let context = ExtractionContext {
            snapshot_id: &snapshot_id,
            repository: &repository,
            repository_name: "fixture",
            commit: &commit,
            store: &store,
        };
        let extractor = RustWorkspaceExtractor;
        let first = extractor.extract(&context).await.unwrap();
        let second = extractor.extract(&context).await.unwrap();
        assert_eq!(first, second);
        assert_eq!(first.components.len(), 2);
        assert_eq!(first.modules.len(), 2);
        assert_eq!(first.interfaces.len(), 2);
        assert_eq!(first.dependencies.len(), 1);
        assert_eq!(
            first.dependencies[0].dependency_kind,
            DependencyKind::DependsOn
        );
        assert!(
            first
                .components
                .iter()
                .all(|component| component
                    .metadata
                    .provenance
                    .iter()
                    .all(|provenance| matches!(
                        provenance.source,
                        WorldModelProvenanceSource::SourceCode { .. }
                    )))
        );
    }

    #[tokio::test]
    async fn rebuild_on_newer_commit_preserves_history_and_reports_diff_and_relation() {
        let (temp, repository, first_commit) = fixture();
        let store = Store::open_in_memory().await.unwrap();
        let mut builder = WorldModelBuilder::new();
        builder.add_extractor(RustWorkspaceExtractor, true);
        let first = builder
            .build(
                WorldModelSnapshotId::sequential(1),
                &repository,
                "fixture",
                &first_commit,
                &store,
            )
            .await
            .unwrap()
            .snapshot;

        write_crate(temp.path(), "worker", Some("core"));
        git(temp.path(), &["add", "."]);
        git(temp.path(), &["commit", "-qm", "add worker"]);
        let second_commit = repository.resolve("HEAD").unwrap();
        let second = builder
            .build(
                WorldModelSnapshotId::sequential(2),
                &repository,
                "fixture",
                &second_commit,
                &store,
            )
            .await
            .unwrap()
            .snapshot;

        assert_eq!(
            snapshot_relation(&repository, &first_commit, &second_commit),
            SnapshotRelation::Ancestor
        );
        assert_eq!(
            snapshot_relation(&repository, &second_commit, &first_commit),
            SnapshotRelation::Stale
        );
        let diff = first.diff(&second);
        assert_eq!(diff.added.len(), 4);
        assert!(diff.removed.is_empty());
        assert!(diff.changed.is_empty());
        assert_eq!(first.commit, first_commit);
        assert_eq!(second.commit, second_commit);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn safe_path_rejects_repository_symlinks() {
        use std::os::unix::fs::symlink;

        let (temp, repository, commit) = fixture();
        let store = Store::open_in_memory().await.unwrap();
        symlink("Cargo.toml", temp.path().join("manifest-link")).unwrap();
        let snapshot_id = WorldModelSnapshotId::sequential(1);
        let context = ExtractionContext {
            snapshot_id: &snapshot_id,
            repository: &repository,
            repository_name: "fixture",
            commit: &commit,
            store: &store,
        };
        assert!(matches!(
            context.safe_path("manifest-link"),
            Err(WorldBuildError::UnsafeRepositoryPath(_))
        ));
    }
}
