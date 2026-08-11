//! The trust boundary.
//!
//! ```text
//! agent ──▶ change
//! ─────────── TRUST BOUNDARY ───────────
//! evaluator ──▶ verdict
//! ```
//!
//! Everything on this side of the line is executed by Forge, against the code
//! in the workspace, with no input from the agent that wrote it. Evaluators are
//! deterministic by construction: they run commands and read exit codes. An
//! LLM reviewer may be added later as an additional signal, never as the
//! primary source of truth.

use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use forge_core::events::EventSink;
use forge_core::ids::{RunId, TaskId};
use forge_core::result::{CheckResult, EvaluatorKind};
use forge_core::run::PatchSummary;
use forge_core::task::{EngineeringTask, EvaluationSpec};
use forge_core::workspace::Workspace;
use forge_executor::ProcessRunner;

use crate::error::EvalResult;

/// Everything an evaluator is given.
///
/// Notably absent: the agent's own report of what it did.
pub struct EvaluationContext<'a> {
    pub run_id: &'a RunId,
    pub task_id: &'a TaskId,
    pub repository: &'a str,
    pub base_commit: &'a str,
    /// The workspace containing the change under evaluation.
    pub workspace: &'a Workspace,
    /// Immutable task configuration captured before agent execution.
    pub evaluation_config: &'a EvaluationSpec,
    /// Trusted patch evidence captured by Forge, when evaluation follows a run.
    pub patch: Option<&'a PatchSummary>,
    pub runner: &'a ProcessRunner,
    pub events: &'a dyn EventSink,
    /// Applied to checks that do not declare their own timeout.
    pub default_timeout: Option<Duration>,
    /// Where full check output is written. Without it, only the summarized
    /// `detail` survives.
    pub artifacts_dir: Option<PathBuf>,
}

impl<'a> EvaluationContext<'a> {
    pub fn new(
        workspace: &'a Workspace,
        task: &'a EngineeringTask,
        runner: &'a ProcessRunner,
        events: &'a dyn EventSink,
    ) -> Self {
        Self {
            run_id: &workspace.run_id,
            task_id: &task.task_id,
            repository: &task.repository,
            base_commit: &workspace.base_commit,
            workspace,
            evaluation_config: &task.evaluation,
            patch: None,
            runner,
            events,
            default_timeout: None,
            artifacts_dir: None,
        }
    }

    pub fn with_patch(mut self, patch: &'a PatchSummary) -> Self {
        self.patch = Some(patch);
        self
    }

    pub fn with_default_timeout(mut self, timeout: Option<Duration>) -> Self {
        self.default_timeout = timeout;
        self
    }

    /// Sets the directory full check output is written to.
    pub fn with_artifacts_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.artifacts_dir = Some(dir.into());
        self
    }

    /// Resolves a check's working directory inside the workspace.
    pub fn working_dir(&self, relative: Option<&str>) -> PathBuf {
        match relative {
            Some(rel) => self.workspace.path.join(rel),
            None => self.workspace.path.clone(),
        }
    }

    /// Path for a check's captured output.
    ///
    /// Check names come from user-authored task files, so the name is reduced
    /// to a safe single path segment before it is joined to anything. A check
    /// named `../../etc/passwd` writes inside the run's artifact directory and
    /// nowhere else.
    pub fn output_path_for(&self, check: &str) -> Option<PathBuf> {
        let dir = self.artifacts_dir.as_ref()?.join("checks");
        Some(dir.join(format!("{}.log", sanitize_segment(check))))
    }
}

/// Reduces an arbitrary string to a safe filename segment.
///
/// Everything outside `[A-Za-z0-9_-]` becomes an underscore, so no separator,
/// parent reference, or leading dot can survive into a path.
pub fn sanitize_segment(raw: &str) -> String {
    let mut out: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    // Collapse runs of underscores so `../..` does not become `______`.
    while out.contains("__") {
        out = out.replace("__", "_");
    }
    let trimmed = out.trim_matches('_');
    if trimmed.is_empty() {
        "check".to_string()
    } else {
        trimmed.chars().take(64).collect()
    }
}

/// One independent measurement of a change.
#[async_trait]
pub trait Evaluator: Send + Sync {
    /// Unique within an evaluation, e.g. `tests`.
    fn id(&self) -> &str;

    /// Provider-agnostic evaluator category.
    fn kind(&self) -> EvaluatorKind;

    /// Whether this evaluator participates in the overall verdict.
    fn required(&self) -> bool;

    /// Trusted command displayed in lifecycle events, if command-backed.
    fn command(&self) -> Option<&str> {
        None
    }

    /// Measures the change.
    ///
    /// Returning `Err` means Forge could not perform the measurement at all.
    /// A failing check is a [`CheckResult`] with a `Fail` verdict, not an
    /// error — that distinction is what keeps "the code is broken" separate
    /// from "Forge is broken".
    async fn evaluate(&self, ctx: &EvaluationContext<'_>) -> EvalResult<CheckResult>;
}

/// Backwards-compatible short name for callers from earlier phases.
pub type EvalContext<'a> = EvaluationContext<'a>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestWorkspace;
    use forge_core::events::NullSink;

    #[test]
    fn check_names_are_reduced_to_safe_filename_segments() {
        assert_eq!(sanitize_segment("tests"), "tests");
        assert_eq!(
            sanitize_segment("integration-tests_2"),
            "integration-tests_2"
        );
        assert_eq!(sanitize_segment("../../etc/passwd"), "etc_passwd");
        assert_eq!(sanitize_segment("a/b"), "a_b");
        assert_eq!(sanitize_segment("///"), "check");
        assert_eq!(sanitize_segment(""), "check");
        assert!(sanitize_segment(&"x".repeat(200)).len() <= 64);
    }

    #[test]
    fn a_hostile_check_name_cannot_escape_the_artifact_directory() {
        let ws = TestWorkspace::new();
        let runner = ProcessRunner::conservative();
        let artifacts = PathBuf::from("/repo/.forge/runs/R-0001");
        let ctx = EvalContext::new(&ws.workspace, &ws.task, &runner, &NullSink)
            .with_artifacts_dir(&artifacts);

        let path = ctx.output_path_for("../../../../etc/cron.d/evil").unwrap();
        assert!(
            path.starts_with(artifacts.join("checks")),
            "escaped to {}",
            path.display()
        );
        assert_eq!(
            path.file_name().unwrap().to_string_lossy(),
            "etc_cron_d_evil.log"
        );
    }

    #[test]
    fn without_an_artifacts_directory_no_output_is_written() {
        let ws = TestWorkspace::new();
        let runner = ProcessRunner::conservative();
        let ctx = EvalContext::new(&ws.workspace, &ws.task, &runner, &NullSink);
        assert!(ctx.output_path_for("tests").is_none());
    }
}
