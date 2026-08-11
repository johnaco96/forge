//! Running a repository's declared evaluators and assembling the verdict.

use chrono::Utc;
use forge_core::events::EventPayload;
use forge_core::ids::RunId;
use forge_core::result::{CheckResult, Dimension, Evaluation, Score, Verdict};
use forge_core::task::EngineeringTask;

use crate::BenchmarkEvaluator;
use crate::command::CommandEvaluator;
use crate::evaluator::{EvalContext, Evaluator};

/// The evaluators that will judge one run.
pub struct EvaluatorSet {
    evaluators: Vec<Box<dyn Evaluator>>,
}

impl EvaluatorSet {
    pub fn new(evaluators: Vec<Box<dyn Evaluator>>) -> Self {
        Self { evaluators }
    }

    /// Builds the set a task declares.
    ///
    /// Order matters: build before tests before lint before benchmark, so the
    /// cheapest and most fundamental signal arrives first.
    pub fn from_task(task: &EngineeringTask) -> Self {
        let mut evaluators: Vec<Box<dyn Evaluator>> = Vec::new();
        for (name, spec) in task.evaluation.checks() {
            if name == "benchmark" {
                if let Some(benchmark) = &task.evaluation.benchmark {
                    evaluators.push(Box::new(BenchmarkEvaluator::new(benchmark.clone())));
                }
            } else {
                evaluators.push(Box::new(CommandEvaluator::new(name, spec)));
            }
        }
        Self::new(evaluators)
    }

    pub fn is_empty(&self) -> bool {
        self.evaluators.is_empty()
    }

    pub fn len(&self) -> usize {
        self.evaluators.len()
    }

    pub fn names(&self) -> Vec<String> {
        self.evaluators
            .iter()
            .map(|e| e.name().to_string())
            .collect()
    }

    /// Runs every evaluator and assembles Forge's judgment.
    ///
    /// An evaluator that cannot run produces an `Inconclusive` check rather
    /// than aborting: the rest of the evidence is still worth having, and an
    /// evaluation missing a signal must not read as a clean pass.
    pub async fn run(&self, run_id: RunId, ctx: &EvalContext<'_>) -> Evaluation {
        let started_at = Utc::now();
        ctx.events.emit(EventPayload::EvaluationStarted {
            evaluators: self.names(),
        });

        let mut checks = Vec::with_capacity(self.evaluators.len());
        for evaluator in &self.evaluators {
            if evaluator.name() == "benchmark" {
                ctx.events.emit(EventPayload::BenchmarkStarted {
                    name: evaluator.name().to_string(),
                });
            }

            let check = match evaluator.evaluate(ctx).await {
                Ok(check) => check,
                Err(err) => {
                    tracing::warn!(check = evaluator.name(), %err, "evaluator could not run");
                    CheckResult {
                        name: evaluator.name().to_string(),
                        kind: evaluator.kind().to_string(),
                        verdict: Verdict::Inconclusive,
                        command: None,
                        exit_code: None,
                        duration_ms: 0,
                        detail: Some(err.to_string()),
                        output_path: None,
                        metrics: Vec::new(),
                    }
                }
            };

            emit_check_events(ctx, &check);
            checks.push(check);
        }

        let evaluation = Evaluation::from_checks(run_id, checks, started_at, Utc::now());
        let evaluation = derive_dimensions(evaluation);

        ctx.events.emit(EventPayload::EvaluationCompleted {
            verdict: evaluation.verdict,
        });
        if !evaluation.dimensions.is_empty() {
            ctx.events.emit(EventPayload::RunScored {
                dimensions: evaluation
                    .dimensions
                    .iter()
                    .map(|(dimension, score)| (*dimension, score.get()))
                    .collect(),
            });
        }

        evaluation
    }
}

fn emit_check_events(ctx: &EvalContext<'_>, check: &CheckResult) {
    match (check.name.as_str(), check.verdict) {
        ("tests", Verdict::Pass) => ctx.events.emit(EventPayload::TestPassed {
            suite: None,
            duration_ms: check.duration_ms,
        }),
        ("tests", Verdict::Fail) => ctx.events.emit(EventPayload::TestFailed {
            suite: None,
            duration_ms: check.duration_ms,
            detail: check.detail.clone(),
        }),
        ("benchmark", _) => ctx.events.emit(EventPayload::BenchmarkCompleted {
            name: check.name.clone(),
            value: check
                .metrics
                .iter()
                .find(|metric| !metric.name.ends_with(".duration_ms"))
                .map(|metric| metric.value),
            unit: check
                .metrics
                .iter()
                .find(|metric| !metric.name.ends_with(".duration_ms"))
                .and_then(|metric| metric.unit.clone()),
            duration_ms: check.duration_ms,
        }),
        _ => {}
    }
}

/// Maps checks onto normalized dimensions.
///
/// Only relationships that are unambiguous today are populated. Performance,
/// memory, and maintainability need baselines and normalization rules that
/// should be derived from real runs rather than invented here — the raw metrics
/// are preserved so those dimensions can be computed retroactively.
fn derive_dimensions(mut evaluation: Evaluation) -> Evaluation {
    if let Some(tests) = evaluation.check("tests") {
        match tests.verdict {
            Verdict::Pass => {
                evaluation
                    .dimensions
                    .insert(Dimension::Correctness, Score::clamped(1.0));
            }
            Verdict::Fail => {
                evaluation
                    .dimensions
                    .insert(Dimension::Correctness, Score::clamped(0.0));
            }
            Verdict::Inconclusive => {}
        }
    }
    evaluation
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestWorkspace, task_with};
    use forge_core::events::{NullSink, RecordingSink};
    use forge_executor::ProcessRunner;

    #[tokio::test]
    async fn a_task_without_evaluation_yields_an_inconclusive_verdict() {
        // Nothing was measured, so nothing is verified.
        let ws = TestWorkspace::new();
        let runner = ProcessRunner::conservative();
        let ctx = EvalContext::new(&ws.workspace, &ws.task, &runner, &NullSink);

        let set = EvaluatorSet::from_task(&ws.task);
        assert!(set.is_empty());

        let evaluation = set.run(RunId::sequential(1), &ctx).await;
        assert_eq!(evaluation.verdict, Verdict::Inconclusive);
        assert!(evaluation.dimensions.is_empty());
    }

    #[tokio::test]
    async fn every_declared_check_runs_and_is_recorded() {
        let task = task_with(&[("tests", "exit 0"), ("lint", "exit 0")]);
        let ws = TestWorkspace::with_task(task);
        let runner = ProcessRunner::conservative();
        let sink = RecordingSink::new(RunId::sequential(1));
        let ctx = EvalContext::new(&ws.workspace, &ws.task, &runner, &sink);

        let evaluation = EvaluatorSet::from_task(&ws.task)
            .run(RunId::sequential(1), &ctx)
            .await;

        assert_eq!(evaluation.verdict, Verdict::Pass);
        assert_eq!(evaluation.checks.len(), 2);
        assert_eq!(evaluation.metrics.len(), 2);
        assert_eq!(
            evaluation
                .dimensions
                .get(&Dimension::Correctness)
                .map(|s| s.get()),
            Some(1.0)
        );

        let types: Vec<&str> = sink.events().iter().map(|e| e.event_type()).collect();
        assert!(types.contains(&"EvaluationStarted"));
        assert!(types.contains(&"TestPassed"));
        assert!(types.contains(&"EvaluationCompleted"));
        assert!(types.contains(&"RunScored"));
    }

    #[tokio::test]
    async fn one_failing_check_fails_the_run_but_the_others_still_run() {
        let task = task_with(&[("tests", "exit 1"), ("lint", "exit 0")]);
        let ws = TestWorkspace::with_task(task);
        let runner = ProcessRunner::conservative();
        let sink = RecordingSink::new(RunId::sequential(1));
        let ctx = EvalContext::new(&ws.workspace, &ws.task, &runner, &sink);

        let evaluation = EvaluatorSet::from_task(&ws.task)
            .run(RunId::sequential(1), &ctx)
            .await;

        assert_eq!(evaluation.verdict, Verdict::Fail);
        assert_eq!(evaluation.check("lint").unwrap().verdict, Verdict::Pass);
        assert_eq!(
            evaluation
                .dimensions
                .get(&Dimension::Correctness)
                .map(|s| s.get()),
            Some(0.0)
        );
        assert!(sink.events().iter().any(|e| e.event_type() == "TestFailed"));
    }

    #[tokio::test]
    async fn checks_run_in_the_documented_order() {
        let task = task_with(&[
            ("benchmark", "exit 0"),
            ("lint", "exit 0"),
            ("tests", "exit 0"),
        ]);
        let set = EvaluatorSet::from_task(&task);
        assert_eq!(set.names(), vec!["tests", "lint", "benchmark"]);
    }

    #[tokio::test]
    async fn an_evaluator_that_cannot_run_does_not_produce_a_pass() {
        let task = task_with(&[("tests", "exit 0")]);
        let mut ws = TestWorkspace::with_task(task);
        // Point the workspace at a directory that does not exist, so the check
        // cannot be executed at all.
        ws.workspace.path = ws.workspace.path.join("gone");
        let runner = ProcessRunner::conservative();
        let ctx = EvalContext::new(&ws.workspace, &ws.task, &runner, &NullSink);

        let evaluation = EvaluatorSet::from_task(&ws.task)
            .run(RunId::sequential(1), &ctx)
            .await;

        assert_eq!(evaluation.verdict, Verdict::Inconclusive);
        assert_eq!(
            evaluation.check("tests").unwrap().verdict,
            Verdict::Inconclusive
        );
        // No correctness claim can be made from a check that never ran.
        assert!(evaluation.dimensions.is_empty());
    }
}
