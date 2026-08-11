//! Resolving and running a repository's declared evaluation plan.

use std::time::Instant;

use chrono::Utc;
use forge_core::events::{EvaluationSubject, EventPayload};
use forge_core::ids::RunId;
use forge_core::result::{CheckResult, Dimension, Evaluation, EvaluatorKind, Score, Verdict};
use forge_core::task::EngineeringTask;

use crate::evaluator::{EvaluationContext, Evaluator};
use crate::{
    BenchmarkEvaluator, BuildEvaluator, ComplexityEvaluator, CustomEvaluator, LintEvaluator,
    SecurityEvaluator, TestEvaluator,
};

/// An immutable, trusted set of evaluators resolved from the task before the
/// coding agent starts. Candidate changes can neither add checks nor alter
/// their command, timeout, requiredness, working directory, or output path.
pub struct EvaluationPlan {
    evaluators: Vec<Box<dyn Evaluator>>,
}

impl EvaluationPlan {
    pub fn new(evaluators: Vec<Box<dyn Evaluator>>) -> Self {
        Self { evaluators }
    }

    pub fn resolve(task: &EngineeringTask) -> Self {
        let config = &task.evaluation;
        let mut evaluators: Vec<Box<dyn Evaluator>> = Vec::new();

        if let Some(spec) = &config.build {
            evaluators.push(Box::new(BuildEvaluator::new(spec.clone())));
        }
        if let Some(spec) = &config.tests {
            evaluators.push(Box::new(TestEvaluator::new(spec.clone())));
        }
        if let Some(spec) = &config.lint {
            evaluators.push(Box::new(LintEvaluator::new(spec.clone())));
        }
        if let Some(spec) = &config.security {
            evaluators.push(Box::new(SecurityEvaluator::new(spec.clone())));
        }
        if let Some(spec) = &config.complexity {
            evaluators.push(Box::new(ComplexityEvaluator::new(spec.clone())));
        }
        if let Some(spec) = &config.benchmark {
            evaluators.push(Box::new(BenchmarkEvaluator::new(spec.clone())));
        }
        for custom in &config.custom {
            evaluators.push(Box::new(CustomEvaluator::new(
                custom.name.clone(),
                custom.spec.clone(),
                custom.metrics_file.clone(),
            )));
        }
        Self::new(evaluators)
    }

    /// Compatibility spelling retained for Phase 0-1 callers.
    pub fn from_task(task: &EngineeringTask) -> Self {
        Self::resolve(task)
    }

    pub fn is_empty(&self) -> bool {
        self.evaluators.is_empty()
    }

    pub fn len(&self) -> usize {
        self.evaluators.len()
    }

    pub fn ids(&self) -> Vec<String> {
        self.evaluators
            .iter()
            .map(|evaluator| evaluator.id().to_string())
            .collect()
    }

    /// Compatibility spelling retained for Phase 0-1 callers.
    pub fn names(&self) -> Vec<String> {
        self.ids()
    }

    pub fn engine(self) -> EvaluationEngine {
        EvaluationEngine { plan: self }
    }

    /// Compatibility entry point; all execution still flows through the one
    /// evaluation engine.
    pub async fn run(&self, run_id: RunId, ctx: &EvaluationContext<'_>) -> Evaluation {
        EvaluationEngine::run_plan(self, run_id.into(), ctx).await
    }
}

/// The single orchestration path for evaluator lifecycle events, error
/// isolation, result collection, summary verdicts, and dimensions.
pub struct EvaluationEngine {
    plan: EvaluationPlan,
}

impl EvaluationEngine {
    pub fn new(plan: EvaluationPlan) -> Self {
        Self { plan }
    }

    pub fn plan(&self) -> &EvaluationPlan {
        &self.plan
    }

    pub async fn run(&self, run_id: RunId, ctx: &EvaluationContext<'_>) -> Evaluation {
        Self::run_plan(&self.plan, run_id.into(), ctx).await
    }

    pub async fn execute(
        plan: &EvaluationPlan,
        run_id: RunId,
        ctx: &EvaluationContext<'_>,
    ) -> Evaluation {
        Self::run_plan(plan, run_id.into(), ctx).await
    }

    pub async fn execute_subject(
        plan: &EvaluationPlan,
        subject: EvaluationSubject,
        ctx: &EvaluationContext<'_>,
    ) -> Evaluation {
        Self::run_plan(plan, subject, ctx).await
    }

    async fn run_plan(
        plan: &EvaluationPlan,
        subject: EvaluationSubject,
        ctx: &EvaluationContext<'_>,
    ) -> Evaluation {
        let started_at = Utc::now();
        ctx.events.emit(EventPayload::EvaluationStarted {
            subject: subject.clone(),
            evaluators: plan.ids(),
        });

        let mut checks = Vec::with_capacity(plan.evaluators.len());
        for evaluator in &plan.evaluators {
            ctx.events.emit(EventPayload::EvaluatorStarted {
                subject: subject.clone(),
                evaluator_id: evaluator.id().to_string(),
                kind: evaluator.kind(),
                required: evaluator.required(),
                command: evaluator.command().map(str::to_string),
            });
            if evaluator.kind() == EvaluatorKind::Benchmark {
                ctx.events.emit(EventPayload::BenchmarkStarted {
                    name: evaluator.id().to_string(),
                });
            }

            let timer = Instant::now();
            let check = match evaluator.evaluate(ctx).await {
                Ok(check) => {
                    ctx.events.emit(EventPayload::EvaluatorCompleted {
                        subject: subject.clone(),
                        evaluator_id: check.name.clone(),
                        kind: check.kind,
                        verdict: check.verdict,
                        execution_status: check.execution_status,
                        duration_ms: check.duration_ms,
                        metric_count: check.metrics.len(),
                    });
                    check
                }
                Err(error) => {
                    let error = error.to_string();
                    tracing::warn!(check = evaluator.id(), %error, "evaluator could not run");
                    ctx.events.emit(EventPayload::EvaluatorFailed {
                        subject: subject.clone(),
                        evaluator_id: evaluator.id().to_string(),
                        kind: evaluator.kind(),
                        required: evaluator.required(),
                        error: error.clone(),
                    });
                    let mut check = CheckResult::execution_error(
                        evaluator.id(),
                        evaluator.kind(),
                        evaluator.required(),
                        error,
                    );
                    check.command = evaluator.command().map(str::to_string);
                    check.duration_ms = timer.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
                    check
                }
            };

            emit_legacy_check_events(ctx, &check);
            checks.push(check);
        }

        let evaluation = derive_dimensions(Evaluation::from_subject_checks(
            subject.clone(),
            checks,
            started_at,
            Utc::now(),
        ));
        ctx.events.emit(EventPayload::EvaluationCompleted {
            subject: subject.clone(),
            verdict: evaluation.verdict,
        });
        if matches!(subject, EvaluationSubject::Run(_)) && !evaluation.dimensions.is_empty() {
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

fn emit_legacy_check_events(ctx: &EvaluationContext<'_>, check: &CheckResult) {
    match (check.kind, check.verdict) {
        (EvaluatorKind::Test, Verdict::Pass) => ctx.events.emit(EventPayload::TestPassed {
            suite: None,
            duration_ms: check.duration_ms,
        }),
        (EvaluatorKind::Test, Verdict::Fail) => ctx.events.emit(EventPayload::TestFailed {
            suite: None,
            duration_ms: check.duration_ms,
            detail: check.detail.clone(),
        }),
        (EvaluatorKind::Benchmark, _) => ctx.events.emit(EventPayload::BenchmarkCompleted {
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

/// Derives only dimensions with an unambiguous, non-weighted interpretation.
/// Structured raw metrics remain canonical for all other dimensions.
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

/// Phase 0-1 public name retained as an alias.
pub type EvaluatorSet = EvaluationPlan;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestWorkspace, task_with};
    use std::sync::Mutex;

    use forge_core::events::{EvaluationSubject, EventPayload, EventSink, NullSink, RecordingSink};
    use forge_core::ids::TeamExecutionId;
    use forge_core::result::EvaluatorExecutionStatus;
    use forge_executor::ProcessRunner;

    #[derive(Default)]
    struct PayloadSink(Mutex<Vec<EventPayload>>);

    impl EventSink for PayloadSink {
        fn emit(&self, payload: EventPayload) {
            self.0.lock().unwrap().push(payload);
        }
    }

    #[tokio::test]
    async fn a_task_without_evaluation_yields_an_inconclusive_verdict() {
        let ws = TestWorkspace::new();
        let runner = ProcessRunner::conservative();
        let ctx = EvaluationContext::new(&ws.workspace, &ws.task, &runner, &NullSink);
        let plan = EvaluationPlan::resolve(&ws.task);
        assert!(plan.is_empty());
        let evaluation = EvaluationEngine::new(plan)
            .run(RunId::sequential(1), &ctx)
            .await;
        assert_eq!(evaluation.verdict, Verdict::Inconclusive);
    }

    #[tokio::test]
    async fn every_declared_check_runs_with_typed_lifecycle_events() {
        let task = task_with(&[("tests", "exit 0"), ("lint", "exit 0")]);
        let ws = TestWorkspace::with_task(task);
        let runner = ProcessRunner::conservative();
        let sink = RecordingSink::new(RunId::sequential(1));
        let ctx = EvaluationContext::new(&ws.workspace, &ws.task, &runner, &sink);
        let evaluation = EvaluationPlan::resolve(&ws.task)
            .run(RunId::sequential(1), &ctx)
            .await;

        assert_eq!(evaluation.verdict, Verdict::Pass);
        assert_eq!(
            evaluation.subject,
            EvaluationSubject::Run(RunId::sequential(1))
        );
        assert_eq!(evaluation.checks.len(), 2);
        assert_eq!(evaluation.check("tests").unwrap().kind, EvaluatorKind::Test);
        let types: Vec<&str> = sink
            .events()
            .iter()
            .map(|event| event.event_type())
            .collect();
        assert_eq!(
            types
                .iter()
                .filter(|kind| **kind == "EvaluatorStarted")
                .count(),
            2
        );
        assert_eq!(
            types
                .iter()
                .filter(|kind| **kind == "EvaluatorCompleted")
                .count(),
            2
        );
        assert!(sink.events().iter().all(|event| {
            event
                .payload
                .evaluation_subject()
                .is_none_or(|subject| subject == &evaluation.subject)
        }));
    }

    #[tokio::test]
    async fn lifecycle_events_preserve_a_team_subject_throughout_evaluation() {
        let task = task_with(&[("tests", "exit 0"), ("lint", "exit 0")]);
        let ws = TestWorkspace::with_task(task);
        let runner = ProcessRunner::conservative();
        let sink = PayloadSink::default();
        let ctx = EvaluationContext::new(&ws.workspace, &ws.task, &runner, &sink);
        let subject = EvaluationSubject::TeamExecution(TeamExecutionId::sequential(4));
        let plan = EvaluationPlan::resolve(&ws.task);
        let evaluation = EvaluationEngine::execute_subject(&plan, subject.clone(), &ctx).await;

        assert_eq!(evaluation.verdict, Verdict::Pass);
        assert_eq!(evaluation.subject, subject);
        let events = sink.0.lock().unwrap();
        let lifecycle = events
            .iter()
            .filter_map(|event| event.evaluation_subject().cloned())
            .collect::<Vec<_>>();
        assert_eq!(lifecycle.len(), 6);
        assert!(lifecycle.iter().all(|candidate| candidate == &subject));
    }

    #[tokio::test]
    async fn failure_does_not_suppress_later_evaluators() {
        let task = task_with(&[("tests", "exit 1"), ("lint", "exit 0")]);
        let ws = TestWorkspace::with_task(task);
        let runner = ProcessRunner::conservative();
        let ctx = EvaluationContext::new(&ws.workspace, &ws.task, &runner, &NullSink);
        let evaluation = EvaluationPlan::resolve(&ws.task)
            .run(RunId::sequential(1), &ctx)
            .await;
        assert_eq!(evaluation.verdict, Verdict::Fail);
        assert_eq!(evaluation.check("lint").unwrap().verdict, Verdict::Pass);
    }

    #[tokio::test]
    async fn all_six_phase_two_categories_flow_through_one_engine() {
        let mut task = task_with(&[
            ("tests", "true"),
            ("benchmark", "true"),
            ("lint", "true"),
            ("security", "true"),
            ("complexity", "true"),
            ("api_contract", "true"),
        ]);
        task.evaluation.build = None;
        let ws = TestWorkspace::with_task(task);
        let runner = ProcessRunner::conservative();
        let ctx = EvaluationContext::new(&ws.workspace, &ws.task, &runner, &NullSink);
        let evaluation = EvaluationPlan::resolve(&ws.task)
            .run(RunId::sequential(1), &ctx)
            .await;
        assert_eq!(evaluation.verdict, Verdict::Pass);
        assert_eq!(
            evaluation
                .checks
                .iter()
                .map(|check| check.kind)
                .collect::<Vec<_>>(),
            vec![
                EvaluatorKind::Test,
                EvaluatorKind::Lint,
                EvaluatorKind::Security,
                EvaluatorKind::Complexity,
                EvaluatorKind::Benchmark,
                EvaluatorKind::Custom,
            ]
        );
    }

    #[tokio::test]
    async fn optional_failure_is_preserved_without_blocking_a_pass() {
        let mut task = task_with(&[("tests", "true"), ("security", "false")]);
        task.evaluation.security.as_mut().unwrap().required = false;
        let ws = TestWorkspace::with_task(task);
        let runner = ProcessRunner::conservative();
        let ctx = EvaluationContext::new(&ws.workspace, &ws.task, &runner, &NullSink);
        let evaluation = EvaluationPlan::resolve(&ws.task)
            .run(RunId::sequential(1), &ctx)
            .await;
        assert_eq!(evaluation.verdict, Verdict::Pass);
        assert_eq!(evaluation.check("security").unwrap().verdict, Verdict::Fail);
        assert!(!evaluation.check("security").unwrap().required);
    }

    #[tokio::test]
    async fn one_execution_error_is_required_or_optional_without_hiding_other_results() {
        for (required, expected) in [(true, Verdict::Inconclusive), (false, Verdict::Pass)] {
            let mut task = task_with(&[("tests", "true"), ("security", "true")]);
            let security = task.evaluation.security.as_mut().unwrap();
            security.working_dir = Some("missing-but-safe".into());
            security.required = required;
            let ws = TestWorkspace::with_task(task);
            let runner = ProcessRunner::conservative();
            let ctx = EvaluationContext::new(&ws.workspace, &ws.task, &runner, &NullSink);
            let evaluation = EvaluationPlan::resolve(&ws.task)
                .run(RunId::sequential(1), &ctx)
                .await;
            assert_eq!(evaluation.verdict, expected);
            assert_eq!(evaluation.check("tests").unwrap().verdict, Verdict::Pass);
            assert_eq!(
                evaluation.check("security").unwrap().execution_status,
                EvaluatorExecutionStatus::Error
            );
            assert_eq!(
                evaluation.check("security").unwrap().verdict,
                Verdict::Inconclusive
            );
        }
    }

    #[tokio::test]
    async fn execution_error_is_not_an_evaluation_failure() {
        let task = task_with(&[("tests", "exit 0"), ("lint", "exit 0")]);
        let mut ws = TestWorkspace::with_task(task);
        ws.workspace.path = ws.workspace.path.join("gone");
        let runner = ProcessRunner::conservative();
        let sink = RecordingSink::new(RunId::sequential(1));
        let ctx = EvaluationContext::new(&ws.workspace, &ws.task, &runner, &sink);
        let evaluation = EvaluationPlan::resolve(&ws.task)
            .run(RunId::sequential(1), &ctx)
            .await;
        assert_eq!(evaluation.verdict, Verdict::Inconclusive);
        assert_eq!(
            evaluation.check("tests").unwrap().execution_status,
            EvaluatorExecutionStatus::Error
        );
        assert_eq!(evaluation.checks.len(), 2);
        assert_eq!(
            sink.events()
                .iter()
                .filter(|event| event.event_type() == "EvaluatorFailed")
                .count(),
            2
        );
    }

    #[test]
    fn plan_order_and_kinds_are_deterministic() {
        let task = task_with(&[
            ("benchmark", "exit 0"),
            ("lint", "exit 0"),
            ("tests", "exit 0"),
            ("security", "exit 0"),
        ]);
        let plan = EvaluationPlan::resolve(&task);
        assert_eq!(plan.ids(), vec!["tests", "lint", "security", "benchmark"]);
    }
}
