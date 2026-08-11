//! A workspace-shaped fixture that needs no Git repository.

use forge_core::ids::{RunId, TaskId};
use forge_core::integrity::ProtectionPolicy;
use forge_core::task::{CommandSpec, EngineeringTask, EvaluationSpec, NamedCommand, TaskMetadata};
use forge_core::workspace::{Workspace, WorkspaceKind};
use tempfile::TempDir;

pub struct TestWorkspace {
    _temp: TempDir,
    pub workspace: Workspace,
    pub task: EngineeringTask,
}

impl TestWorkspace {
    pub fn new() -> Self {
        Self::with_task(task_with(&[]))
    }

    pub fn with_task(task: EngineeringTask) -> Self {
        let temp = tempfile::tempdir().expect("temp dir");
        let workspace = Workspace::new(
            RunId::sequential(1),
            WorkspaceKind::Directory,
            temp.path().to_path_buf(),
            "forge/R-0001",
            "0000000000000000000000000000000000000000",
        );
        Self {
            _temp: temp,
            workspace,
            task,
        }
    }
}

impl Default for TestWorkspace {
    fn default() -> Self {
        Self::new()
    }
}

/// Builds a task whose evaluation section contains the given checks.
pub fn task_with(checks: &[(&str, &str)]) -> EngineeringTask {
    let mut evaluation = EvaluationSpec::default();
    for (name, command) in checks {
        let spec = CommandSpec::new(*command);
        match *name {
            "tests" => evaluation.tests = Some(spec),
            "benchmark" => evaluation.benchmark = Some(spec.into()),
            "lint" => evaluation.lint = Some(spec),
            "security" => evaluation.security = Some(spec),
            "complexity" => evaluation.complexity = Some(spec.into()),
            "build" => evaluation.build = Some(spec),
            other => evaluation.custom.push(NamedCommand {
                name: other.to_string(),
                spec,
                metrics_file: None,
            }),
        }
    }

    EngineeringTask {
        task_id: TaskId::sequential(1),
        repository: "test-repo".to_string(),
        objective: "Improve something measurable in the storage layer".to_string(),
        constraints: vec!["All existing tests must pass".to_string()],
        evaluation,
        protection: ProtectionPolicy::default(),
        metadata: TaskMetadata::default(),
        classification: Default::default(),
        components: Vec::new(),
        tags: Vec::new(),
    }
}
