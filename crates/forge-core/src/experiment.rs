//! Competitive experiments and their dimensional comparisons.
//!
//! An experiment is a group of ordinary [`AgentRun`](crate::AgentRun)
//! records that share one task and one resolved base commit. It stores links to
//! runs, never copies their evidence. Comparisons are pairwise and dimensional:
//! Forge deliberately has no built-in overall ranking policy.

use std::collections::BTreeSet;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::{ExperimentId, RunId, TaskId};
use crate::integrity::IntegrityStatus;
use crate::result::{Direction, Evaluation, Metric, Verdict};
use crate::run::{AgentRun, RunOutcome};

/// Where a competitive experiment is in its lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExperimentStatus {
    Running,
    Completed,
    Failed,
}

impl ExperimentStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }
}

/// A set of independent runs from one recorded repository state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Experiment {
    pub experiment_id: ExperimentId,
    pub task_id: TaskId,
    pub repository: String,
    pub base_commit: String,
    /// Requested participants, in execution order. This is experiment
    /// configuration, not duplicated run evidence.
    pub agents: Vec<String>,
    /// Links to the ordinary run records produced so far.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub run_ids: Vec<RunId>,
    pub status: ExperimentStatus,
    pub created_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comparison: Option<Comparison>,
}

impl Experiment {
    pub fn new(
        experiment_id: ExperimentId,
        task_id: TaskId,
        repository: impl Into<String>,
        base_commit: impl Into<String>,
        agents: Vec<String>,
    ) -> Self {
        Self {
            experiment_id,
            task_id,
            repository: repository.into(),
            base_commit: base_commit.into(),
            agents,
            run_ids: Vec::new(),
            status: ExperimentStatus::Running,
            created_at: Utc::now(),
            completed_at: None,
            failure_reason: None,
            comparison: None,
        }
    }

    pub fn record_run(&mut self, run_id: RunId) {
        if !self.run_ids.contains(&run_id) {
            self.run_ids.push(run_id);
        }
    }

    pub fn complete(&mut self, comparison: Comparison) {
        self.comparison = Some(comparison);
        self.status = ExperimentStatus::Completed;
        self.completed_at = Some(Utc::now());
    }

    pub fn fail(&mut self, reason: impl Into<String>) {
        self.failure_reason = Some(reason.into());
        self.status = ExperimentStatus::Failed;
        self.completed_at = Some(Utc::now());
    }
}

/// A comparison axis. Dynamic evaluator and benchmark names remain structured
/// without forcing them into a closed enum.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ComparisonKey {
    Outcome,
    Integrity,
    Check { name: String },
    BenchmarkMetric { name: String },
    EvaluatorMetric { evaluator_id: String, name: String },
    Runtime,
    ProviderReportedTokens,
    Cost,
    PatchLines,
    FilesChanged,
    Warnings,
}

impl ComparisonKey {
    pub fn label(&self) -> String {
        match self {
            Self::Outcome => "Correctness".to_string(),
            Self::Integrity => "Integrity".to_string(),
            Self::Check { name } => format!("Check: {name}"),
            Self::BenchmarkMetric { name } => format!("Benchmark: {name}"),
            Self::EvaluatorMetric { evaluator_id, name } => {
                format!("{evaluator_id}: {name}")
            }
            Self::Runtime => "Agent runtime".to_string(),
            Self::ProviderReportedTokens => "Tokens".to_string(),
            Self::Cost => "Cost".to_string(),
            Self::PatchLines => "Patch size".to_string(),
            Self::FilesChanged => "Files changed".to_string(),
            Self::Warnings => "Warnings".to_string(),
        }
    }
}

/// The relation of the left run to the right run on one dimension.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComparisonRelation {
    Better,
    Worse,
    Equal,
    NotComparable,
    Missing,
}

impl ComparisonRelation {
    pub fn describe(self) -> &'static str {
        match self {
            Self::Better => "better",
            Self::Worse => "worse",
            Self::Equal => "equal",
            Self::NotComparable => "not comparable",
            Self::Missing => "missing",
        }
    }
}

/// One left-to-right pair. Pairwise relations scale to more than two agents
/// without inventing a scalar leaderboard.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairwiseComparison {
    pub left_run_id: RunId,
    pub right_run_id: RunId,
    pub relation: ComparisonRelation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DimensionComparison {
    pub key: ComparisonKey,
    pub pairs: Vec<PairwiseComparison>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Structured relationships for an experiment. There is intentionally no
/// overall winner field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Comparison {
    pub experiment_id: ExperimentId,
    pub dimensions: Vec<DimensionComparison>,
}

/// Borrowed evidence used to derive a comparison without copying run data.
#[derive(Clone, Copy)]
pub struct ComparisonInput<'a> {
    pub run: &'a AgentRun,
    pub evaluation: Option<&'a Evaluation>,
}

impl<'a> ComparisonInput<'a> {
    pub fn new(run: &'a AgentRun, evaluation: Option<&'a Evaluation>) -> Self {
        Self { run, evaluation }
    }
}

impl Comparison {
    pub fn from_runs(experiment_id: ExperimentId, runs: &[ComparisonInput<'_>]) -> Self {
        let mut dimensions = vec![
            dimension(ComparisonKey::Outcome, runs, None, |left, right| {
                pair(
                    left,
                    right,
                    compare_outcomes(left.run.outcome, right.run.outcome),
                    None,
                )
            }),
            dimension(ComparisonKey::Integrity, runs, None, |left, right| {
                pair(
                    left,
                    right,
                    compare_integrity(
                        left.run.integrity.as_ref().map(|value| value.status),
                        right.run.integrity.as_ref().map(|value| value.status),
                    ),
                    None,
                )
            }),
        ];

        let check_names: BTreeSet<_> = runs
            .iter()
            .filter_map(|input| input.evaluation)
            .flat_map(|evaluation| evaluation.checks.iter().map(|check| check.name.clone()))
            .collect();
        for name in check_names {
            dimensions.push(dimension(
                ComparisonKey::Check { name: name.clone() },
                runs,
                None,
                |left, right| {
                    let verdict = |input: ComparisonInput<'_>| {
                        input
                            .evaluation
                            .and_then(|evaluation| evaluation.check(&name))
                            .map(|check| check.verdict)
                    };
                    pair(
                        left,
                        right,
                        compare_verdicts(verdict(left), verdict(right)),
                        None,
                    )
                },
            ));
        }

        let metric_keys: BTreeSet<_> = runs
            .iter()
            .filter_map(|input| input.evaluation)
            .flat_map(|evaluation| evaluation.metrics.iter())
            .filter(|metric| !metric.name.ends_with(".duration_ms"))
            .map(|metric| (metric.source.clone(), metric.name.clone()))
            .collect();
        for (source, name) in metric_keys {
            let key = if source == "benchmark" {
                ComparisonKey::BenchmarkMetric { name: name.clone() }
            } else {
                ComparisonKey::EvaluatorMetric {
                    evaluator_id: source.clone(),
                    name: name.clone(),
                }
            };
            dimensions.push(dimension(key, runs, None, |left, right| {
                compare_metric_pair(left, right, &source, &name)
            }));
        }

        dimensions.extend([
            dimension(ComparisonKey::Runtime, runs, None, |left, right| {
                pair(
                    left,
                    right,
                    compare_lower(duration_ms(left.run), duration_ms(right.run)),
                    None,
                )
            }),
            dimension(
                ComparisonKey::ProviderReportedTokens,
                runs,
                Some("provider-reported; not normalized across providers".to_string()),
                |left, right| {
                    pair(
                        left,
                        right,
                        compare_lower(
                            left.run.usage().total_tokens(),
                            right.run.usage().total_tokens(),
                        ),
                        None,
                    )
                },
            ),
            dimension(
                ComparisonKey::Cost,
                runs,
                Some("unavailable cost is not zero".to_string()),
                |left, right| {
                    let left_cost = left.run.usage().cost_usd;
                    let right_cost = right.run.usage().cost_usd;
                    let relation = match (left_cost, right_cost) {
                        (Some(left), Some(right)) => compare_lower(Some(left), Some(right)),
                        (None, None) => ComparisonRelation::Missing,
                        _ => ComparisonRelation::NotComparable,
                    };
                    pair(left, right, relation, None)
                },
            ),
            dimension(ComparisonKey::PatchLines, runs, None, |left, right| {
                pair(
                    left,
                    right,
                    compare_lower(
                        left.run.patch.as_ref().map(|patch| patch.lines_changed()),
                        right.run.patch.as_ref().map(|patch| patch.lines_changed()),
                    ),
                    None,
                )
            }),
            dimension(ComparisonKey::FilesChanged, runs, None, |left, right| {
                pair(
                    left,
                    right,
                    compare_lower(
                        left.run.patch.as_ref().map(|patch| patch.files_changed),
                        right.run.patch.as_ref().map(|patch| patch.files_changed),
                    ),
                    None,
                )
            }),
            dimension(ComparisonKey::Warnings, runs, None, |left, right| {
                pair(
                    left,
                    right,
                    compare_lower(
                        left.run
                            .patch
                            .as_ref()
                            .map(|_| left.run.warnings.len() as u64),
                        right
                            .run
                            .patch
                            .as_ref()
                            .map(|_| right.run.warnings.len() as u64),
                    ),
                    None,
                )
            }),
        ]);

        Self {
            experiment_id,
            dimensions,
        }
    }

    pub fn dimension(&self, key: &ComparisonKey) -> Option<&DimensionComparison> {
        self.dimensions
            .iter()
            .find(|dimension| &dimension.key == key)
    }
}

fn dimension(
    key: ComparisonKey,
    runs: &[ComparisonInput<'_>],
    note: Option<String>,
    compare: impl Fn(ComparisonInput<'_>, ComparisonInput<'_>) -> PairwiseComparison,
) -> DimensionComparison {
    let mut pairs = Vec::new();
    for left in 0..runs.len() {
        for right in (left + 1)..runs.len() {
            pairs.push(compare(runs[left], runs[right]));
        }
    }
    DimensionComparison { key, pairs, note }
}

fn pair(
    left: ComparisonInput<'_>,
    right: ComparisonInput<'_>,
    relation: ComparisonRelation,
    note: Option<String>,
) -> PairwiseComparison {
    PairwiseComparison {
        left_run_id: left.run.run_id.clone(),
        right_run_id: right.run.run_id.clone(),
        relation,
        note,
    }
}

fn compare_outcomes(left: Option<RunOutcome>, right: Option<RunOutcome>) -> ComparisonRelation {
    match (left, right) {
        (None, _) | (_, None) => ComparisonRelation::Missing,
        (Some(left), Some(right)) if left == right => ComparisonRelation::Equal,
        (Some(RunOutcome::Passed), Some(_)) => ComparisonRelation::Better,
        (Some(_), Some(RunOutcome::Passed)) => ComparisonRelation::Worse,
        // Unknown, absent, and measured-negative outcomes do not have a
        // trustworthy ordering relative to one another.
        _ => ComparisonRelation::NotComparable,
    }
}

fn compare_integrity(
    left: Option<IntegrityStatus>,
    right: Option<IntegrityStatus>,
) -> ComparisonRelation {
    compare_ranked(left.map(integrity_rank), right.map(integrity_rank))
}

fn integrity_rank(status: IntegrityStatus) -> u8 {
    match status {
        IntegrityStatus::Clean => 2,
        IntegrityStatus::Modified => 1,
        IntegrityStatus::Missing => 0,
    }
}

fn compare_verdicts(left: Option<Verdict>, right: Option<Verdict>) -> ComparisonRelation {
    match (left, right) {
        (None, _) | (_, None) => ComparisonRelation::Missing,
        (Some(left), Some(right)) if left == right => ComparisonRelation::Equal,
        (Some(Verdict::Pass), Some(_)) => ComparisonRelation::Better,
        (Some(_), Some(Verdict::Pass)) => ComparisonRelation::Worse,
        _ => ComparisonRelation::NotComparable,
    }
}

fn compare_metric_pair(
    left: ComparisonInput<'_>,
    right: ComparisonInput<'_>,
    source: &str,
    name: &str,
) -> PairwiseComparison {
    let (Some(left_metric), Some(right_metric)) = (
        evaluator_metric(left, source, name),
        evaluator_metric(right, source, name),
    ) else {
        return pair(left, right, ComparisonRelation::Missing, None);
    };
    if left_metric.unit != right_metric.unit {
        return pair(
            left,
            right,
            ComparisonRelation::NotComparable,
            Some("incompatible units; Forge does not convert units yet".to_string()),
        );
    }
    if left_metric.direction != right_metric.direction {
        return pair(
            left,
            right,
            ComparisonRelation::NotComparable,
            Some("metric directions differ".to_string()),
        );
    }
    let relation = compare_metrics(left_metric, right_metric);
    pair(left, right, relation, None)
}

fn evaluator_metric<'a>(
    input: ComparisonInput<'a>,
    source: &str,
    name: &str,
) -> Option<&'a Metric> {
    input.evaluation.and_then(|evaluation| {
        evaluation
            .metrics
            .iter()
            .find(|metric| metric.source == source && metric.name == name)
    })
}

fn compare_metrics(left: &Metric, right: &Metric) -> ComparisonRelation {
    if left.value == right.value {
        return ComparisonRelation::Equal;
    }
    match left.direction {
        Direction::HigherIsBetter if left.value > right.value => ComparisonRelation::Better,
        Direction::HigherIsBetter => ComparisonRelation::Worse,
        Direction::LowerIsBetter if left.value < right.value => ComparisonRelation::Better,
        Direction::LowerIsBetter => ComparisonRelation::Worse,
        Direction::Neutral => ComparisonRelation::NotComparable,
    }
}

fn duration_ms(run: &AgentRun) -> Option<u64> {
    run.execution
        .as_ref()
        .map(|execution| execution.duration_ms)
}

fn compare_lower<T: PartialOrd>(left: Option<T>, right: Option<T>) -> ComparisonRelation {
    match (left, right) {
        (None, _) | (_, None) => ComparisonRelation::Missing,
        (Some(left), Some(right)) if left == right => ComparisonRelation::Equal,
        (Some(left), Some(right)) if left < right => ComparisonRelation::Better,
        (Some(_), Some(_)) => ComparisonRelation::Worse,
    }
}

fn compare_ranked(left: Option<u8>, right: Option<u8>) -> ComparisonRelation {
    match (left, right) {
        (None, _) | (_, None) => ComparisonRelation::Missing,
        (Some(left), Some(right)) if left == right => ComparisonRelation::Equal,
        (Some(left), Some(right)) if left > right => ComparisonRelation::Better,
        (Some(_), Some(_)) => ComparisonRelation::Worse,
    }
}

/// One experiment-level lifecycle event. Run trajectories remain in the
/// existing run event stream.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExperimentEvent {
    pub experiment_id: ExperimentId,
    pub seq: u64,
    pub timestamp: DateTime<Utc>,
    #[serde(flatten)]
    pub payload: ExperimentEventPayload,
}

impl ExperimentEvent {
    pub fn event_type(&self) -> &'static str {
        self.payload.event_type()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event_type", content = "data")]
pub enum ExperimentEventPayload {
    ExperimentStarted {
        task_id: TaskId,
        repository: String,
        base_commit: String,
        agents: Vec<String>,
    },
    ParticipantRunStarted {
        run_id: RunId,
        agent_id: String,
    },
    ParticipantRunCompleted {
        run_id: RunId,
        agent_id: String,
        outcome: RunOutcome,
    },
    ExperimentCompleted {
        run_count: usize,
    },
    ExperimentFailed {
        reason: String,
    },
}

impl ExperimentEventPayload {
    pub fn event_type(&self) -> &'static str {
        match self {
            Self::ExperimentStarted { .. } => "ExperimentStarted",
            Self::ParticipantRunStarted { .. } => "ParticipantRunStarted",
            Self::ParticipantRunCompleted { .. } => "ParticipantRunCompleted",
            Self::ExperimentCompleted { .. } => "ExperimentCompleted",
            Self::ExperimentFailed { .. } => "ExperimentFailed",
        }
    }
}

/// In-memory experiment event recorder, mirroring the run-level recording
/// sink while keeping the two event identities distinct.
#[derive(Debug)]
pub struct ExperimentRecordingSink {
    experiment_id: ExperimentId,
    seq: AtomicU64,
    events: Mutex<Vec<ExperimentEvent>>,
}

impl ExperimentRecordingSink {
    pub fn new(experiment_id: ExperimentId) -> Self {
        Self {
            experiment_id,
            seq: AtomicU64::new(0),
            events: Mutex::new(Vec::new()),
        }
    }

    pub fn emit(&self, payload: ExperimentEventPayload) {
        self.events
            .lock()
            .expect("experiment event buffer poisoned")
            .push(ExperimentEvent {
                experiment_id: self.experiment_id.clone(),
                seq: self.seq.fetch_add(1, Ordering::SeqCst),
                timestamp: Utc::now(),
                payload,
            });
    }

    pub fn events(&self) -> Vec<ExperimentEvent> {
        self.events
            .lock()
            .expect("experiment event buffer poisoned")
            .clone()
    }

    pub fn len(&self) -> usize {
        self.events
            .lock()
            .expect("experiment event buffer poisoned")
            .len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::agent::AgentConfig;
    use crate::ids::AgentId;
    use crate::result::{CheckResult, EvaluatorExecutionStatus, EvaluatorKind, Metric};
    use crate::run::{AgentExecution, PatchSummary, Usage};

    use super::*;

    fn run(number: u64, agent: &str, outcome: RunOutcome) -> AgentRun {
        let mut run = AgentRun::new(
            RunId::sequential(number),
            TaskId::sequential(1),
            AgentConfig::new(AgentId::new(agent).unwrap(), format!("{agent}-cli")),
            "abc123",
        );
        let now = Utc::now();
        run.started_at = Some(now);
        run.finished_at = Some(now);
        run.outcome = Some(outcome);
        run.integrity = Some(Default::default());
        run.patch = Some(PatchSummary {
            base_commit: "abc123".into(),
            head_commit: None,
            files_changed: number,
            insertions: number * 10,
            deletions: number,
            binary_files: 0,
            diff_path: None,
            excluded: Vec::new(),
            excluded_counts: Default::default(),
        });
        run.execution = Some(AgentExecution {
            status: crate::run::AgentExecutionStatus::Completed,
            exit_code: Some(0),
            timed_out: false,
            started_at: now,
            finished_at: now,
            duration_ms: 1,
            stdout_path: None,
            stderr_path: None,
            usage: Usage {
                input_tokens: Some(number * 100),
                output_tokens: Some(number * 10),
                cost_usd: (agent == "claude").then_some(0.01),
            },
            self_report: None,
            harness_metadata: BTreeMap::new(),
            infrastructure_failures: Vec::new(),
        });
        run
    }

    fn evaluation(run: &AgentRun, metric: Option<Metric>) -> Evaluation {
        let now = Utc::now();
        let mut metrics = vec![Metric::new(
            "tests.duration_ms",
            1.0,
            "tests",
            Direction::LowerIsBetter,
        )];
        if let Some(metric) = metric {
            metrics.push(metric);
        }
        Evaluation::from_checks(
            run.run_id.clone(),
            vec![CheckResult {
                name: "tests".into(),
                kind: EvaluatorKind::Test,
                required: true,
                verdict: if run.outcome == Some(RunOutcome::Passed) {
                    Verdict::Pass
                } else {
                    Verdict::Fail
                },
                execution_status: EvaluatorExecutionStatus::Completed,
                command: None,
                exit_code: Some(0),
                duration_ms: 1,
                detail: None,
                output_path: None,
                metrics,
                warnings: Vec::new(),
                execution_error: None,
                infrastructure_failures: Vec::new(),
            }],
            now,
            now,
        )
    }

    fn relation(comparison: &Comparison, key: ComparisonKey) -> ComparisonRelation {
        comparison.dimension(&key).unwrap().pairs[0].relation
    }

    #[test]
    fn compares_outcomes_patch_usage_and_unavailable_cost_without_an_overall_winner() {
        let claude = run(1, "claude", RunOutcome::Passed);
        let codex = run(2, "codex", RunOutcome::Failed);
        let claude_eval = evaluation(&claude, None);
        let codex_eval = evaluation(&codex, None);
        let comparison = Comparison::from_runs(
            ExperimentId::sequential(1),
            &[
                ComparisonInput::new(&claude, Some(&claude_eval)),
                ComparisonInput::new(&codex, Some(&codex_eval)),
            ],
        );

        assert_eq!(
            relation(&comparison, ComparisonKey::Outcome),
            ComparisonRelation::Better
        );
        assert_eq!(
            relation(&comparison, ComparisonKey::Integrity),
            ComparisonRelation::Equal
        );
        assert_eq!(
            relation(&comparison, ComparisonKey::ProviderReportedTokens),
            ComparisonRelation::Better
        );
        assert_eq!(
            relation(&comparison, ComparisonKey::PatchLines),
            ComparisonRelation::Better
        );
        assert_eq!(
            relation(&comparison, ComparisonKey::Cost),
            ComparisonRelation::NotComparable
        );
    }

    #[test]
    fn matching_benchmark_metrics_follow_direction() {
        let claude = run(1, "claude", RunOutcome::Passed);
        let codex = run(2, "codex", RunOutcome::Passed);
        let metric = |value| {
            Metric::new("throughput", value, "benchmark", Direction::HigherIsBetter)
                .with_unit("MB/s")
        };
        let claude_eval = evaluation(&claude, Some(metric(4_720.0)));
        let codex_eval = evaluation(&codex, Some(metric(4_910.0)));
        let comparison = Comparison::from_runs(
            ExperimentId::sequential(1),
            &[
                ComparisonInput::new(&claude, Some(&claude_eval)),
                ComparisonInput::new(&codex, Some(&codex_eval)),
            ],
        );

        assert_eq!(
            relation(
                &comparison,
                ComparisonKey::BenchmarkMetric {
                    name: "throughput".into()
                }
            ),
            ComparisonRelation::Worse
        );
    }

    #[test]
    fn missing_metrics_and_incompatible_units_are_not_guessed() {
        let claude = run(1, "claude", RunOutcome::Passed);
        let codex = run(2, "codex", RunOutcome::Passed);
        let claude_eval = evaluation(
            &claude,
            Some(
                Metric::new("throughput", 4.7, "benchmark", Direction::HigherIsBetter)
                    .with_unit("GB/s"),
            ),
        );
        let no_metric = evaluation(&codex, None);
        let missing = Comparison::from_runs(
            ExperimentId::sequential(1),
            &[
                ComparisonInput::new(&claude, Some(&claude_eval)),
                ComparisonInput::new(&codex, Some(&no_metric)),
            ],
        );
        assert_eq!(
            relation(
                &missing,
                ComparisonKey::BenchmarkMetric {
                    name: "throughput".into()
                }
            ),
            ComparisonRelation::Missing
        );

        let codex_eval = evaluation(
            &codex,
            Some(
                Metric::new(
                    "throughput",
                    4_900.0,
                    "benchmark",
                    Direction::HigherIsBetter,
                )
                .with_unit("MB/s"),
            ),
        );
        let incompatible = Comparison::from_runs(
            ExperimentId::sequential(2),
            &[
                ComparisonInput::new(&claude, Some(&claude_eval)),
                ComparisonInput::new(&codex, Some(&codex_eval)),
            ],
        );
        assert_eq!(
            relation(
                &incompatible,
                ComparisonKey::BenchmarkMetric {
                    name: "throughput".into()
                }
            ),
            ComparisonRelation::NotComparable
        );

        let wrong_direction = evaluation(
            &codex,
            Some(
                Metric::new("throughput", 4.9, "benchmark", Direction::LowerIsBetter)
                    .with_unit("GB/s"),
            ),
        );
        let incompatible = Comparison::from_runs(
            ExperimentId::sequential(3),
            &[
                ComparisonInput::new(&claude, Some(&claude_eval)),
                ComparisonInput::new(&codex, Some(&wrong_direction)),
            ],
        );
        assert_eq!(
            relation(
                &incompatible,
                ComparisonKey::BenchmarkMetric {
                    name: "throughput".into()
                }
            ),
            ComparisonRelation::NotComparable
        );
    }

    #[test]
    fn non_benchmark_evaluator_metrics_remain_separate_dimensions() {
        let claude = run(1, "claude", RunOutcome::Passed);
        let codex = run(2, "codex", RunOutcome::Passed);
        let metric = |value| {
            Metric::new(
                "branch_points",
                value,
                "complexity",
                Direction::LowerIsBetter,
            )
            .with_unit("points")
        };
        let claude_eval = evaluation(&claude, Some(metric(3.0)));
        let codex_eval = evaluation(&codex, Some(metric(5.0)));
        let comparison = Comparison::from_runs(
            ExperimentId::sequential(1),
            &[
                ComparisonInput::new(&claude, Some(&claude_eval)),
                ComparisonInput::new(&codex, Some(&codex_eval)),
            ],
        );
        assert_eq!(
            relation(
                &comparison,
                ComparisonKey::EvaluatorMetric {
                    evaluator_id: "complexity".into(),
                    name: "branch_points".into(),
                }
            ),
            ComparisonRelation::Better
        );
    }

    #[test]
    fn experiment_event_sequences_are_monotonic() {
        let sink = ExperimentRecordingSink::new(ExperimentId::sequential(1));
        sink.emit(ExperimentEventPayload::ExperimentStarted {
            task_id: TaskId::sequential(1),
            repository: "repo".into(),
            base_commit: "abc123".into(),
            agents: vec!["claude".into(), "codex".into()],
        });
        sink.emit(ExperimentEventPayload::ExperimentCompleted { run_count: 2 });
        assert_eq!(
            sink.events()
                .iter()
                .map(|event| event.seq)
                .collect::<Vec<_>>(),
            vec![0, 1]
        );
    }
}
