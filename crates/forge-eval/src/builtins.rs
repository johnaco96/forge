//! Typed evaluator implementations built on the shared command and structured
//! metrics mechanics.

use async_trait::async_trait;
use forge_core::result::{CheckResult, EvaluatorKind};
use forge_core::task::{BenchmarkSpec, CommandSpec};

use crate::benchmark::BenchmarkEvaluator;
use crate::command::CommandEvaluator;
use crate::error::EvalResult;
use crate::evaluator::{EvaluationContext, Evaluator};

macro_rules! command_evaluator {
    ($type:ident, $id:literal, $kind:expr) => {
        #[derive(Debug, Clone)]
        pub struct $type {
            inner: CommandEvaluator,
        }

        impl $type {
            pub fn new(spec: CommandSpec) -> Self {
                Self {
                    inner: CommandEvaluator::with_kind($id, $kind, spec),
                }
            }
        }

        #[async_trait]
        impl Evaluator for $type {
            fn id(&self) -> &str {
                self.inner.id()
            }

            fn kind(&self) -> EvaluatorKind {
                $kind
            }

            fn required(&self) -> bool {
                self.inner.required()
            }

            fn command(&self) -> Option<&str> {
                self.inner.command()
            }

            async fn evaluate(&self, ctx: &EvaluationContext<'_>) -> EvalResult<CheckResult> {
                self.inner.evaluate(ctx).await
            }
        }
    };
}

command_evaluator!(TestEvaluator, "tests", EvaluatorKind::Test);
command_evaluator!(LintEvaluator, "lint", EvaluatorKind::Lint);
command_evaluator!(SecurityEvaluator, "security", EvaluatorKind::Security);
command_evaluator!(BuildEvaluator, "build", EvaluatorKind::Build);

/// Command-backed complexity measurement using Forge's structured metrics
/// contract. The repository chooses the tool and the metrics it emits.
#[derive(Debug, Clone)]
pub struct ComplexityEvaluator {
    inner: BenchmarkEvaluator,
}

impl ComplexityEvaluator {
    pub fn new(spec: BenchmarkSpec) -> Self {
        Self {
            inner: BenchmarkEvaluator::structured("complexity", EvaluatorKind::Complexity, spec),
        }
    }
}

#[async_trait]
impl Evaluator for ComplexityEvaluator {
    fn id(&self) -> &str {
        self.inner.id()
    }

    fn kind(&self) -> EvaluatorKind {
        EvaluatorKind::Complexity
    }

    fn required(&self) -> bool {
        self.inner.required()
    }

    fn command(&self) -> Option<&str> {
        self.inner.command()
    }

    async fn evaluate(&self, ctx: &EvaluationContext<'_>) -> EvalResult<CheckResult> {
        self.inner.evaluate(ctx).await
    }
}

/// Repository-defined evaluator with a stable ID and optional structured
/// metrics. Both forms share the same result and lifecycle contract.
#[derive(Debug, Clone)]
pub struct CustomEvaluator {
    inner: CustomInner,
}

#[derive(Debug, Clone)]
enum CustomInner {
    Command(CommandEvaluator),
    Structured(BenchmarkEvaluator),
}

impl CustomEvaluator {
    pub fn new(id: impl Into<String>, spec: CommandSpec, metrics_file: Option<String>) -> Self {
        let id = id.into();
        let inner = if let Some(metrics_file) = metrics_file {
            let mut structured = BenchmarkSpec::from(spec);
            structured.metrics_file = Some(metrics_file);
            CustomInner::Structured(BenchmarkEvaluator::structured(
                id,
                EvaluatorKind::Custom,
                structured,
            ))
        } else {
            CustomInner::Command(CommandEvaluator::with_kind(id, EvaluatorKind::Custom, spec))
        };
        Self { inner }
    }

    fn evaluator(&self) -> &dyn Evaluator {
        match &self.inner {
            CustomInner::Command(inner) => inner,
            CustomInner::Structured(inner) => inner,
        }
    }
}

#[async_trait]
impl Evaluator for CustomEvaluator {
    fn id(&self) -> &str {
        self.evaluator().id()
    }

    fn kind(&self) -> EvaluatorKind {
        EvaluatorKind::Custom
    }

    fn required(&self) -> bool {
        self.evaluator().required()
    }

    fn command(&self) -> Option<&str> {
        self.evaluator().command()
    }

    async fn evaluate(&self, ctx: &EvaluationContext<'_>) -> EvalResult<CheckResult> {
        match &self.inner {
            CustomInner::Command(inner) => inner.evaluate(ctx).await,
            CustomInner::Structured(inner) => inner.evaluate(ctx).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestWorkspace;
    use forge_core::events::NullSink;
    use forge_core::result::Verdict;
    use forge_executor::ProcessRunner;

    async fn verdict(evaluator: &dyn Evaluator) -> Verdict {
        let workspace = TestWorkspace::new();
        let runner = ProcessRunner::conservative();
        let context =
            EvaluationContext::new(&workspace.workspace, &workspace.task, &runner, &NullSink);
        evaluator.evaluate(&context).await.unwrap().verdict
    }

    #[tokio::test]
    async fn typed_test_and_lint_evaluators_preserve_command_results() {
        assert_eq!(
            verdict(&TestEvaluator::new(CommandSpec::new("true"))).await,
            Verdict::Pass
        );
        assert_eq!(
            verdict(&TestEvaluator::new(CommandSpec::new("false"))).await,
            Verdict::Fail
        );
        assert_eq!(
            verdict(&LintEvaluator::new(CommandSpec::new("true"))).await,
            Verdict::Pass
        );
        assert_eq!(
            verdict(&LintEvaluator::new(CommandSpec::new("false"))).await,
            Verdict::Fail
        );
    }

    #[tokio::test]
    async fn security_and_custom_evaluators_preserve_independent_results() {
        assert_eq!(
            verdict(&SecurityEvaluator::new(CommandSpec::new("true"))).await,
            Verdict::Pass
        );
        assert_eq!(
            verdict(&SecurityEvaluator::new(CommandSpec::new("false"))).await,
            Verdict::Fail
        );
        assert_eq!(
            verdict(&CustomEvaluator::new(
                "api_contract",
                CommandSpec::new("true"),
                None,
            ))
            .await,
            Verdict::Pass
        );
        assert_eq!(
            verdict(&CustomEvaluator::new(
                "api_contract",
                CommandSpec::new("false"),
                None,
            ))
            .await,
            Verdict::Fail
        );
    }
}
