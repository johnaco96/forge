//! Command-based evaluation.
//!
//! Every evaluator a repository can declare today — tests, build, lint,
//! benchmark, custom — reduces to "run this command in the workspace and look
//! at what happened". Richer evaluators (complexity, security scanning,
//! benchmark output parsing) implement [`Evaluator`](crate::Evaluator)
//! alongside this one rather than special-casing it.

use std::fs;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use forge_core::result::{
    CheckResult, Direction, EvaluatorExecutionStatus, EvaluatorKind, Metric, Verdict,
};
use forge_core::task::CommandSpec;
use forge_executor::ExecRequest;

use crate::error::EvalResult;
use crate::evaluator::{EvalContext, Evaluator};

/// Lines of failing output kept as the check's explanation.
const DETAIL_LINES: usize = 20;

/// Runs one command and turns its exit status into a verdict.
#[derive(Debug, Clone)]
pub struct CommandEvaluator {
    name: String,
    kind: EvaluatorKind,
    spec: CommandSpec,
}

impl CommandEvaluator {
    pub fn new(name: impl Into<String>, spec: CommandSpec) -> Self {
        let name = name.into();
        Self {
            kind: kind_for_id(&name),
            name,
            spec,
        }
    }

    pub fn with_kind(name: impl Into<String>, kind: EvaluatorKind, spec: CommandSpec) -> Self {
        Self {
            name: name.into(),
            kind,
            spec,
        }
    }

    pub fn spec(&self) -> &CommandSpec {
        &self.spec
    }
}

#[async_trait]
impl Evaluator for CommandEvaluator {
    fn id(&self) -> &str {
        &self.name
    }

    fn kind(&self) -> EvaluatorKind {
        self.kind
    }

    fn required(&self) -> bool {
        self.spec.required
    }

    fn command(&self) -> Option<&str> {
        Some(&self.spec.command)
    }

    fn required_tools(&self) -> &[forge_core::task::EvaluatorToolRequirement] {
        &self.spec.required_tools
    }

    async fn evaluate(&self, ctx: &EvalContext<'_>) -> EvalResult<CheckResult> {
        let cwd = ctx.working_dir(self.spec.working_dir.as_deref());
        for requirement in &self.spec.required_tools {
            ctx.runner
                .preflight_evaluator_tool(&cwd, &self.name, requirement, ctx.events)
                .await?;
        }
        // ExecRequest defaults to an empty invocation credential policy. That
        // default applies to built-in and repository-defined custom
        // evaluators alike; Forge currently has no separate trusted
        // credential-bearing evaluator configuration.
        let request = ExecRequest::shell(&self.spec.command, cwd)
            .with_label(format!("{}: {}", self.name, self.spec.command))
            .with_default_timeout(
                self.spec
                    .timeout_secs
                    .map(std::time::Duration::from_secs)
                    .or(ctx.default_timeout),
            );

        let outcome = ctx.runner.run(&request, ctx.events).await?;
        if outcome.cancelled {
            return Err(crate::EvalError::Cancelled {
                check: self.name.clone(),
            });
        }

        // A timeout is a failure of the change, not of the measurement: a suite
        // that cannot finish inside its budget has not passed. An evaluator
        // that could not be executed at all is handled above, as an error.
        let verdict = if outcome.success() {
            Verdict::Pass
        } else {
            Verdict::Fail
        };

        // The full output is the evidence; `detail` is only a summary of it.
        let (output_path, warnings) = match ctx.output_path_for(&self.name) {
            Some(path) => match write_output(&path, &outcome) {
                Ok(path) => (Some(path), Vec::new()),
                Err(error) => (
                    None,
                    vec![format!(
                        "could not write evaluator output artifact: {error}"
                    )],
                ),
            },
            None => (None, Vec::new()),
        };

        let detail = if verdict == Verdict::Pass {
            None
        } else if outcome.timed_out {
            Some(format!(
                "timed out after {:.1}s\n{}",
                outcome.duration.as_secs_f64(),
                outcome.tail(DETAIL_LINES)
            ))
        } else {
            Some(outcome.tail(DETAIL_LINES))
        };

        Ok(CheckResult {
            name: self.name.clone(),
            kind: self.kind(),
            required: self.required(),
            verdict,
            execution_status: EvaluatorExecutionStatus::Completed,
            command: Some(self.spec.command.clone()),
            exit_code: outcome.exit_code,
            duration_ms: outcome.duration_ms(),
            detail,
            output_path,
            metrics: vec![
                Metric::new(
                    format!("{}.duration_ms", self.name),
                    outcome.duration_ms() as f64,
                    self.name.clone(),
                    Direction::LowerIsBetter,
                )
                .with_unit("ms"),
            ],
            warnings,
            execution_error: None,
            infrastructure_failures: outcome.infrastructure_failures,
        })
    }
}

fn kind_for_id(id: &str) -> EvaluatorKind {
    match id {
        "tests" => EvaluatorKind::Test,
        "benchmark" => EvaluatorKind::Benchmark,
        "lint" => EvaluatorKind::Lint,
        "security" => EvaluatorKind::Security,
        "complexity" => EvaluatorKind::Complexity,
        "build" => EvaluatorKind::Build,
        _ => EvaluatorKind::Custom,
    }
}

/// Writes a check's captured output, returning where it landed.
///
/// A failure to write is not a failure of the check: losing the log is worth a
/// warning, not a lost measurement.
fn write_output(path: &Path, outcome: &forge_executor::ExecOutcome) -> std::io::Result<PathBuf> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut body = String::new();
    body.push_str(&format!("$ {}\n", outcome.label));
    body.push_str(&format!(
        "exit: {}    duration: {}ms    timed_out: {}\n",
        outcome
            .exit_code
            .map(|c| c.to_string())
            .unwrap_or_else(|| "killed".to_string()),
        outcome.duration_ms(),
        outcome.timed_out
    ));
    if !outcome.stdout.is_empty() {
        body.push_str("\n--- stdout ---\n");
        body.push_str(&outcome.stdout);
        if outcome.stdout_truncated {
            body.push_str("\n[output truncated]\n");
        }
    }
    if !outcome.stderr.is_empty() {
        body.push_str("\n--- stderr ---\n");
        body.push_str(&outcome.stderr);
        if outcome.stderr_truncated {
            body.push_str("\n[output truncated]\n");
        }
    }
    fs::write(path, body)?;
    Ok(path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestWorkspace;
    use forge_core::events::NullSink;
    use forge_executor::ProcessRunner;

    #[tokio::test]
    async fn a_passing_command_passes() {
        let ws = TestWorkspace::new();
        let runner = ProcessRunner::conservative();
        let ctx = EvalContext::new(&ws.workspace, &ws.task, &runner, &NullSink);

        let check = CommandEvaluator::new("tests", CommandSpec::new("exit 0"))
            .evaluate(&ctx)
            .await
            .unwrap();

        assert_eq!(check.verdict, Verdict::Pass);
        assert_eq!(check.exit_code, Some(0));
        assert!(check.detail.is_none());
        assert_eq!(check.metrics.len(), 1);
    }

    #[tokio::test]
    async fn a_failing_command_fails_and_keeps_the_evidence() {
        let ws = TestWorkspace::new();
        let runner = ProcessRunner::conservative();
        let ctx = EvalContext::new(&ws.workspace, &ws.task, &runner, &NullSink);

        let check = CommandEvaluator::new(
            "tests",
            CommandSpec::new("echo 'test storage::checkpoint FAILED' >&2; exit 101"),
        )
        .evaluate(&ctx)
        .await
        .unwrap();

        assert_eq!(check.verdict, Verdict::Fail);
        assert_eq!(check.exit_code, Some(101));
        assert!(check.detail.unwrap().contains("checkpoint FAILED"));
    }

    #[tokio::test]
    async fn a_declared_missing_tool_is_infrastructure_not_engineering_failure() {
        let ws = TestWorkspace::new();
        let runner = ProcessRunner::conservative();
        let ctx = EvalContext::new(&ws.workspace, &ws.task, &runner, &NullSink);
        let marker = ws.workspace.path.join("agent-must-not-have-produced-this");
        let mut spec = CommandSpec::new(format!("touch {}", marker.display()));
        spec.required_tools
            .push(forge_core::task::EvaluatorToolRequirement::new(
                "forge-evaluator-tool-that-does-not-exist",
            ));

        let error = CommandEvaluator::new("lint", spec)
            .evaluate(&ctx)
            .await
            .unwrap_err();

        assert!(matches!(
            error,
            crate::EvalError::Exec(forge_executor::ExecError::Infrastructure(
                forge_core::run::InfrastructureFailure {
                    kind: forge_core::run::InfrastructureFailureKind::EvaluatorToolUnavailable,
                    ..
                }
            ))
        ));
        assert!(
            !marker.exists(),
            "evaluator command ran after failed preflight"
        );
    }

    #[tokio::test]
    async fn a_present_declared_tool_allows_normal_engineering_evidence() {
        let ws = TestWorkspace::new();
        let runner = ProcessRunner::conservative();
        let ctx = EvalContext::new(&ws.workspace, &ws.task, &runner, &NullSink);
        let mut spec = CommandSpec::new("exit 7");
        spec.required_tools.push(
            forge_core::task::EvaluatorToolRequirement::new("git")
                .with_version_contains("git version"),
        );

        let check = CommandEvaluator::new("tests", spec)
            .evaluate(&ctx)
            .await
            .unwrap();
        assert_eq!(check.verdict, Verdict::Fail);
        assert_eq!(check.execution_status, EvaluatorExecutionStatus::Completed);
        assert_eq!(check.exit_code, Some(7));
        assert!(check.infrastructure_failures.is_empty());
    }

    #[tokio::test]
    async fn commands_run_inside_the_workspace() {
        let ws = TestWorkspace::new();
        std::fs::write(ws.workspace.path.join("marker"), "here").unwrap();
        let runner = ProcessRunner::conservative();
        let ctx = EvalContext::new(&ws.workspace, &ws.task, &runner, &NullSink);

        let check = CommandEvaluator::new("tests", CommandSpec::new("test -f marker"))
            .evaluate(&ctx)
            .await
            .unwrap();
        assert_eq!(check.verdict, Verdict::Pass);
    }

    #[tokio::test]
    async fn a_check_can_run_in_a_subdirectory() {
        let ws = TestWorkspace::new();
        std::fs::create_dir_all(ws.workspace.path.join("crates/inner")).unwrap();
        std::fs::write(ws.workspace.path.join("crates/inner/marker"), "here").unwrap();
        let runner = ProcessRunner::conservative();
        let ctx = EvalContext::new(&ws.workspace, &ws.task, &runner, &NullSink);

        let mut spec = CommandSpec::new("test -f marker");
        spec.working_dir = Some("crates/inner".to_string());

        let check = CommandEvaluator::new("tests", spec)
            .evaluate(&ctx)
            .await
            .unwrap();
        assert_eq!(check.verdict, Verdict::Pass);
    }

    #[tokio::test]
    async fn a_hanging_check_fails_with_its_timeout_recorded() {
        let ws = TestWorkspace::new();
        let runner = ProcessRunner::conservative();
        let ctx = EvalContext::new(&ws.workspace, &ws.task, &runner, &NullSink);

        let mut spec = CommandSpec::new("sleep 30");
        spec.timeout_secs = Some(1);

        let check = CommandEvaluator::new("tests", spec)
            .evaluate(&ctx)
            .await
            .unwrap();

        assert_eq!(check.verdict, Verdict::Fail);
        assert!(check.detail.unwrap().contains("timed out"));
    }

    #[tokio::test]
    async fn an_unrunnable_check_is_an_error_not_a_failure() {
        // Forge failing to measure must never be recorded as the change being
        // wrong.
        let mut ws = TestWorkspace::new();
        ws.workspace.path = ws.workspace.path.join("does-not-exist");
        let runner = ProcessRunner::conservative();
        let ctx = EvalContext::new(&ws.workspace, &ws.task, &runner, &NullSink);

        let err = CommandEvaluator::new("tests", CommandSpec::new("true"))
            .evaluate(&ctx)
            .await
            .unwrap_err();
        assert!(
            matches!(
                err,
                crate::EvalError::Exec(forge_executor::ExecError::MissingWorkingDirectory(_))
            ),
            "{err}"
        );
    }
}
