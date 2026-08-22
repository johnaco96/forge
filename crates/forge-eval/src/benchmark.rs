//! Structured benchmark evaluation.
//!
//! The command's exit status remains evidence, but benchmark values come only
//! from the declared JSON file. Forge never scrapes arbitrary terminal text.

use std::fs;
use std::path::{Component, Path, PathBuf};

use async_trait::async_trait;
use forge_core::result::{BenchmarkMetrics, CheckResult, EvaluatorKind, Verdict};
use forge_core::task::BenchmarkSpec;

use crate::command::CommandEvaluator;
use crate::error::{EvalError, EvalResult};
use crate::evaluator::{EvalContext, Evaluator};

#[derive(Debug, Clone)]
pub struct BenchmarkEvaluator {
    id: String,
    kind: EvaluatorKind,
    spec: BenchmarkSpec,
}

impl BenchmarkEvaluator {
    pub fn new(spec: BenchmarkSpec) -> Self {
        Self {
            id: "benchmark".to_string(),
            kind: EvaluatorKind::Benchmark,
            spec,
        }
    }

    /// Builds another structured, command-backed evaluator using the exact
    /// benchmark metrics contract and trust checks.
    pub fn structured(id: impl Into<String>, kind: EvaluatorKind, spec: BenchmarkSpec) -> Self {
        Self {
            id: id.into(),
            kind,
            spec,
        }
    }

    fn metrics_path(&self, workspace: &Path) -> EvalResult<Option<PathBuf>> {
        let Some(relative) = &self.spec.metrics_file else {
            return Ok(None);
        };
        let normalized = relative.replace('\\', "/");
        let path = Path::new(&normalized);
        if relative.is_empty()
            || path.is_absolute()
            || normalized
                .as_bytes()
                .get(1)
                .is_some_and(|byte| *byte == b':')
            || path
                .components()
                .any(|component| !matches!(component, Component::Normal(_)))
        {
            return Err(EvalError::NotMeasurable {
                check: self.id.clone(),
                reason: format!("metrics file `{relative}` is not a safe repository-relative path"),
            });
        }
        ensure_no_symlinked_parents(workspace, path, &self.id)?;
        Ok(Some(workspace.join(path)))
    }
}

fn ensure_no_symlinked_parents(
    workspace: &Path,
    relative: &Path,
    evaluator_id: &str,
) -> EvalResult<()> {
    let mut current = workspace.to_path_buf();
    let components: Vec<_> = relative.components().collect();
    for component in components.iter().take(components.len().saturating_sub(1)) {
        let Component::Normal(segment) = component else {
            unreachable!("validated by metrics_path")
        };
        current.push(segment);
        if fs::symlink_metadata(&current).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
            return Err(EvalError::NotMeasurable {
                check: evaluator_id.to_string(),
                reason: format!(
                    "metrics file path traverses symlinked directory `{}`",
                    current.display()
                ),
            });
        }
    }
    Ok(())
}

#[async_trait]
impl Evaluator for BenchmarkEvaluator {
    fn id(&self) -> &str {
        &self.id
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
        let metrics_path = self.metrics_path(&ctx.workspace.path)?;
        if let Some(path) = &metrics_path
            && path.exists()
        {
            fs::remove_file(path).map_err(|source| EvalError::NotMeasurable {
                check: self.id.clone(),
                reason: format!(
                    "could not clear stale metrics file `{}`: {source}",
                    path.display()
                ),
            })?;
        }

        let mut check = CommandEvaluator::with_kind(&self.id, self.kind, self.spec.command_spec())
            .evaluate(ctx)
            .await?;

        // A failed benchmark command is a valid negative measurement. Only a
        // successful command promises a metrics file.
        if check.verdict != Verdict::Pass {
            return Ok(check);
        }

        let Some(_) = metrics_path else {
            return Ok(check);
        };
        // The trusted command may have replaced a parent path. Revalidate the
        // resolved shape before reading anything.
        let path = self
            .metrics_path(&ctx.workspace.path)?
            .expect("metrics file was configured");
        if fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
            check.verdict = Verdict::Inconclusive;
            check.detail = Some(format!(
                "structured metrics file `{}` is a symlink",
                path.display()
            ));
            return Ok(check);
        }
        let parsed = fs::read_to_string(&path)
            .map_err(|source| format!("could not read `{}`: {source}", path.display()))
            .and_then(|raw| {
                serde_json::from_str::<BenchmarkMetrics>(&raw)
                    .map_err(|source| format!("invalid structured metrics: {source}"))
            })
            .and_then(|metrics| metrics.into_metrics(self.id.clone()));

        match parsed {
            Ok(metrics) if !metrics.is_empty() => check.metrics.extend(metrics),
            Ok(_) => {
                check.verdict = Verdict::Inconclusive;
                check.detail = Some("structured benchmark metrics contained no values".to_string());
            }
            Err(reason) => {
                check.verdict = Verdict::Inconclusive;
                check.detail = Some(reason);
            }
        }
        Ok(check)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestWorkspace;
    use forge_core::events::NullSink;
    use forge_executor::ProcessRunner;

    #[tokio::test]
    async fn parses_typed_metrics_without_scraping_stdout() {
        let ws = TestWorkspace::new();
        let runner = ProcessRunner::conservative();
        let ctx = EvalContext::new(&ws.workspace, &ws.task, &runner, &NullSink);
        let command = r#"printf '%s' '{"metrics":{"throughput":{"value":4720.3,"unit":"MB/s","direction":"maximize"}}}' > .forge-metrics.json; echo 'ignore throughput: 999999'"#;
        let evaluator = BenchmarkEvaluator::new(
            BenchmarkSpec::new(command).with_metrics_file(".forge-metrics.json"),
        );

        let check = evaluator.evaluate(&ctx).await.unwrap();
        assert_eq!(check.verdict, Verdict::Pass);
        assert_eq!(check.metrics.len(), 2); // duration plus throughput
        let throughput = check
            .metrics
            .iter()
            .find(|metric| metric.name == "throughput")
            .unwrap();
        assert_eq!(throughput.value, 4720.3);
    }

    #[tokio::test]
    async fn complexity_and_custom_metrics_keep_typed_identity_and_source() {
        let ws = TestWorkspace::new();
        let runner = ProcessRunner::conservative();
        let ctx = EvalContext::new(&ws.workspace, &ws.task, &runner, &NullSink);
        for (id, kind, path) in [
            ("complexity", EvaluatorKind::Complexity, "complexity.json"),
            ("source_stats", EvaluatorKind::Custom, "source-stats.json"),
        ] {
            let command = format!(
                "printf '%s' '{{\"metrics\":{{\"score\":{{\"value\":3,\"unit\":\"points\",\"direction\":\"minimize\"}}}}}}' > {path}"
            );
            let spec = BenchmarkSpec::new(command).with_metrics_file(path);
            let check = BenchmarkEvaluator::structured(id, kind, spec)
                .evaluate(&ctx)
                .await
                .unwrap();
            assert_eq!(check.kind, kind);
            assert_eq!(
                check
                    .metrics
                    .iter()
                    .find(|metric| metric.name == "score")
                    .unwrap()
                    .source,
                id
            );
        }
    }

    #[tokio::test]
    async fn missing_or_malformed_metrics_make_a_green_command_inconclusive() {
        let ws = TestWorkspace::new();
        let runner = ProcessRunner::conservative();
        let ctx = EvalContext::new(&ws.workspace, &ws.task, &runner, &NullSink);

        for command in ["true", "printf 'not json' > .forge-metrics.json"] {
            let evaluator = BenchmarkEvaluator::new(
                BenchmarkSpec::new(command).with_metrics_file(".forge-metrics.json"),
            );
            let check = evaluator.evaluate(&ctx).await.unwrap();
            assert_eq!(check.verdict, Verdict::Inconclusive, "{command}");
            assert!(check.detail.is_some());
        }
    }

    #[tokio::test]
    async fn stale_agent_written_metrics_are_removed_before_the_command() {
        let ws = TestWorkspace::new();
        fs::write(
            ws.workspace.path.join(".forge-metrics.json"),
            r#"{"metrics":{"forged":{"value":1,"direction":"maximize"}}}"#,
        )
        .unwrap();
        let runner = ProcessRunner::conservative();
        let ctx = EvalContext::new(&ws.workspace, &ws.task, &runner, &NullSink);
        let evaluator = BenchmarkEvaluator::new(
            BenchmarkSpec::new("true").with_metrics_file(".forge-metrics.json"),
        );

        let check = evaluator.evaluate(&ctx).await.unwrap();
        assert_eq!(check.verdict, Verdict::Inconclusive);
        assert!(!check.metrics.iter().any(|metric| metric.name == "forged"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_symlinked_metrics_directory_cannot_escape_the_workspace() {
        use std::os::unix::fs::symlink;

        let ws = TestWorkspace::new();
        let outside = tempfile::tempdir().unwrap();
        symlink(outside.path(), ws.workspace.path.join("metrics")).unwrap();
        let runner = ProcessRunner::conservative();
        let ctx = EvalContext::new(&ws.workspace, &ws.task, &runner, &NullSink);
        let evaluator = BenchmarkEvaluator::new(
            BenchmarkSpec::new("true").with_metrics_file("metrics/results.json"),
        );

        let err = evaluator.evaluate(&ctx).await.unwrap_err();
        assert!(matches!(err, EvalError::NotMeasurable { .. }));
        assert!(!outside.path().join("results.json").exists());
    }
}
