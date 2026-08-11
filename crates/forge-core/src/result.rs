//! Forge's independent judgment of a change.
//!
//! Two rules from the design shape these types:
//!
//! 1. An evaluation is produced by Forge, never by the agent that wrote the
//!    code. Nothing here can be populated from agent self-reporting.
//! 2. Raw measurements are never discarded and never collapsed into a single
//!    scalar at record time. [`Evaluation`] therefore has no overall score
//!    field: weighting is a question for the reader of the data, and weights
//!    will change as evidence accumulates.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::RunId;

/// The outcome of a check or of a whole evaluation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Pass,
    Fail,
    /// The check could not be executed, so it says nothing either way. Kept
    /// distinct from `Fail` so a broken benchmark script is not recorded as a
    /// regression.
    Inconclusive,
}

impl std::fmt::Display for Verdict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pass => f.write_str("PASS"),
            Self::Fail => f.write_str("FAIL"),
            Self::Inconclusive => f.write_str("INCONCLUSIVE"),
        }
    }
}

/// A normalized axis of engineering quality.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Dimension {
    Correctness,
    Performance,
    Memory,
    Maintainability,
    Security,
    ChangeSize,
    Complexity,
    BuildTime,
    RuntimeStability,
    CostEfficiency,
}

impl Dimension {
    pub const ALL: [Dimension; 10] = [
        Dimension::Correctness,
        Dimension::Performance,
        Dimension::Memory,
        Dimension::Maintainability,
        Dimension::Security,
        Dimension::ChangeSize,
        Dimension::Complexity,
        Dimension::BuildTime,
        Dimension::RuntimeStability,
        Dimension::CostEfficiency,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Correctness => "correctness",
            Self::Performance => "performance",
            Self::Memory => "memory",
            Self::Maintainability => "maintainability",
            Self::Security => "security",
            Self::ChangeSize => "change_size",
            Self::Complexity => "complexity",
            Self::BuildTime => "build_time",
            Self::RuntimeStability => "runtime_stability",
            Self::CostEfficiency => "cost_efficiency",
        }
    }
}

impl std::fmt::Display for Dimension {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("score must be a finite number between 0.0 and 1.0, got {0}")]
pub struct ScoreError(String);

/// A normalized score in `[0.0, 1.0]`.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Deserialize)]
#[serde(try_from = "f64")]
pub struct Score(f64);

impl Serialize for Score {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_f64(self.0)
    }
}

impl Score {
    pub fn new(value: f64) -> Result<Self, ScoreError> {
        if value.is_finite() && (0.0..=1.0).contains(&value) {
            Ok(Self(value))
        } else {
            Err(ScoreError(value.to_string()))
        }
    }

    /// Clamps into range. Use when the caller has already decided that
    /// out-of-range input is a saturation, not an error.
    pub fn clamped(value: f64) -> Self {
        if value.is_nan() {
            return Self(0.0);
        }
        Self(value.clamp(0.0, 1.0))
    }

    pub fn get(self) -> f64 {
        self.0
    }
}

impl TryFrom<f64> for Score {
    type Error = ScoreError;

    fn try_from(value: f64) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl std::fmt::Display for Score {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:.2}", self.0)
    }
}

/// Which way is better for a raw measurement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    #[serde(rename = "maximize", alias = "higher_is_better")]
    HigherIsBetter,
    #[serde(rename = "minimize", alias = "lower_is_better")]
    LowerIsBetter,
    /// Informational; comparing two values says nothing about quality.
    #[serde(rename = "neutral")]
    Neutral,
}

impl Direction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::HigherIsBetter => "maximize",
            Self::LowerIsBetter => "minimize",
            Self::Neutral => "neutral",
        }
    }
}

impl std::fmt::Display for Direction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Why a benchmark metric name was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "invalid metric name `{0}`: names must be non-empty, printable, and at most 128 characters"
)]
pub struct MetricNameError(String);

/// A validated metric key from a structured benchmark result.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct MetricName(String);

impl MetricName {
    pub fn new(name: impl Into<String>) -> Result<Self, MetricNameError> {
        let name = name.into();
        if name.trim().is_empty()
            || name.chars().count() > 128
            || name.chars().any(char::is_control)
        {
            return Err(MetricNameError(name));
        }
        Ok(Self(name))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for MetricName {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let name = String::deserialize(deserializer)?;
        Self::new(name).map_err(serde::de::Error::custom)
    }
}

/// One value in `.forge-metrics.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetricValue {
    pub value: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    pub direction: Direction,
}

/// The version-zero structured benchmark output contract.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BenchmarkMetrics {
    pub metrics: BTreeMap<MetricName, MetricValue>,
}

impl BenchmarkMetrics {
    /// Converts file values into the raw metrics already used by evaluations
    /// and the SQLite ledger.
    pub fn into_metrics(self, source: impl Into<String>) -> Result<Vec<Metric>, String> {
        let source = source.into();
        self.metrics
            .into_iter()
            .map(|(name, metric)| {
                if !metric.value.is_finite() {
                    return Err(format!(
                        "metric `{}` must contain a finite number",
                        name.as_str()
                    ));
                }
                if metric
                    .unit
                    .as_ref()
                    .is_some_and(|unit| unit.trim().is_empty())
                {
                    return Err(format!("metric `{}` has an empty unit", name.as_str()));
                }
                let mut value = Metric::new(name.0, metric.value, source.clone(), metric.direction);
                value.unit = metric.unit;
                Ok(value)
            })
            .collect()
    }
}

/// One raw measurement, in its original units.
///
/// Metrics are the durable record. Dimensions are derived from them and may be
/// recomputed later; metrics are not.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Metric {
    pub name: String,
    pub value: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    /// What produced this number, e.g. `tests`, `benchmark`, `git`.
    pub source: String,
    pub direction: Direction,
}

impl Metric {
    pub fn new(
        name: impl Into<String>,
        value: f64,
        source: impl Into<String>,
        direction: Direction,
    ) -> Self {
        Self {
            name: name.into(),
            value,
            unit: None,
            source: source.into(),
            direction,
        }
    }

    pub fn with_unit(mut self, unit: impl Into<String>) -> Self {
        self.unit = Some(unit.into());
        self
    }
}

/// The result of running one evaluator.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CheckResult {
    /// Unique within an evaluation, e.g. `tests` or a custom check's name.
    pub name: String,
    /// The evaluator kind that produced it, e.g. `command`.
    pub kind: String,
    pub verdict: Verdict,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    pub duration_ms: u64,
    /// Short human-readable explanation, e.g. the tail of a failure log.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Full captured output, written to the run's artifact directory. The
    /// `detail` above is a summary; this is the evidence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_path: Option<std::path::PathBuf>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub metrics: Vec<Metric>,
}

/// Forge's judgment of one run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Evaluation {
    pub run_id: RunId,
    pub verdict: Verdict,
    pub checks: Vec<CheckResult>,
    /// Normalized axes, populated only where there is evidence for them.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub dimensions: BTreeMap<Dimension, Score>,
    /// Every raw measurement gathered, including those already inside `checks`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub metrics: Vec<Metric>,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
}

impl Evaluation {
    /// Aggregates checks into an evaluation.
    ///
    /// The verdict is deliberately pessimistic: any failing check fails the
    /// evaluation, and an inconclusive check prevents a clean pass. A run whose
    /// evidence is incomplete must not read as verified.
    pub fn from_checks(
        run_id: RunId,
        checks: Vec<CheckResult>,
        started_at: DateTime<Utc>,
        finished_at: DateTime<Utc>,
    ) -> Self {
        let verdict = if checks.iter().any(|c| c.verdict == Verdict::Fail) {
            Verdict::Fail
        } else if checks.is_empty() || checks.iter().any(|c| c.verdict == Verdict::Inconclusive) {
            Verdict::Inconclusive
        } else {
            Verdict::Pass
        };

        let metrics = checks.iter().flat_map(|c| c.metrics.clone()).collect();

        Self {
            run_id,
            verdict,
            checks,
            dimensions: BTreeMap::new(),
            metrics,
            started_at,
            finished_at,
        }
    }

    pub fn with_dimension(mut self, dimension: Dimension, score: Score) -> Self {
        self.dimensions.insert(dimension, score);
        self
    }

    pub fn check(&self, name: &str) -> Option<&CheckResult> {
        self.checks.iter().find(|c| c.name == name)
    }

    pub fn metric(&self, name: &str) -> Option<&Metric> {
        self.metrics.iter().find(|m| m.name == name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(name: &str, verdict: Verdict) -> CheckResult {
        CheckResult {
            name: name.to_string(),
            kind: "command".to_string(),
            verdict,
            command: Some("cargo test".to_string()),
            exit_code: Some(0),
            duration_ms: 10,
            detail: None,
            output_path: None,
            metrics: vec![Metric::new(
                format!("{name}.duration_ms"),
                10.0,
                name,
                Direction::LowerIsBetter,
            )],
        }
    }

    #[test]
    fn scores_reject_out_of_range_and_nan() {
        assert!(Score::new(0.0).is_ok());
        assert!(Score::new(1.0).is_ok());
        assert!(Score::new(1.0001).is_err());
        assert!(Score::new(-0.1).is_err());
        assert!(Score::new(f64::NAN).is_err());
        assert!(Score::new(f64::INFINITY).is_err());
    }

    #[test]
    fn score_deserialization_is_validated() {
        assert!(serde_json::from_str::<Score>("0.5").is_ok());
        assert!(serde_json::from_str::<Score>("1.5").is_err());
    }

    #[test]
    fn clamping_saturates_and_maps_nan_to_zero() {
        assert_eq!(Score::clamped(2.0).get(), 1.0);
        assert_eq!(Score::clamped(-2.0).get(), 0.0);
        assert_eq!(Score::clamped(f64::NAN).get(), 0.0);
    }

    #[test]
    fn any_failing_check_fails_the_evaluation() {
        let now = Utc::now();
        let eval = Evaluation::from_checks(
            RunId::sequential(1),
            vec![check("tests", Verdict::Pass), check("lint", Verdict::Fail)],
            now,
            now,
        );
        assert_eq!(eval.verdict, Verdict::Fail);
    }

    #[test]
    fn incomplete_evidence_never_reads_as_pass() {
        let now = Utc::now();
        let inconclusive = Evaluation::from_checks(
            RunId::sequential(1),
            vec![
                check("tests", Verdict::Pass),
                check("benchmark", Verdict::Inconclusive),
            ],
            now,
            now,
        );
        assert_eq!(inconclusive.verdict, Verdict::Inconclusive);

        let no_checks = Evaluation::from_checks(RunId::sequential(2), vec![], now, now);
        assert_eq!(no_checks.verdict, Verdict::Inconclusive);
    }

    #[test]
    fn raw_metrics_are_collected_from_every_check() {
        let now = Utc::now();
        let eval = Evaluation::from_checks(
            RunId::sequential(1),
            vec![check("tests", Verdict::Pass), check("lint", Verdict::Pass)],
            now,
            now,
        );
        assert_eq!(eval.verdict, Verdict::Pass);
        assert_eq!(eval.metrics.len(), 2);
        assert!(eval.metric("tests.duration_ms").is_some());
    }

    #[test]
    fn structured_benchmark_metrics_use_the_stable_contract() {
        let raw = r#"{
          "metrics": {
            "throughput": {"value": 4720.3, "unit": "MB/s", "direction": "maximize"},
            "p99_latency": {"value": 4.2, "unit": "ms", "direction": "minimize"}
          }
        }"#;

        let parsed: BenchmarkMetrics = serde_json::from_str(raw).unwrap();
        let metrics = parsed.into_metrics("benchmark").unwrap();
        assert_eq!(metrics.len(), 2);
        assert_eq!(metrics[0].name, "p99_latency");
        assert_eq!(metrics[0].direction, Direction::LowerIsBetter);
        assert_eq!(metrics[1].name, "throughput");
        assert_eq!(metrics[1].direction, Direction::HigherIsBetter);
    }

    #[test]
    fn malformed_metric_names_and_values_are_rejected() {
        assert!(MetricName::new("   ").is_err());
        assert!(MetricName::new("bad\nname").is_err());

        let metrics = BenchmarkMetrics {
            metrics: BTreeMap::from([(
                MetricName::new("throughput").unwrap(),
                MetricValue {
                    value: f64::INFINITY,
                    unit: Some("MB/s".into()),
                    direction: Direction::HigherIsBetter,
                },
            )]),
        };
        assert!(metrics.into_metrics("benchmark").is_err());
    }

    #[test]
    fn old_direction_spellings_still_deserialize() {
        assert_eq!(
            serde_json::from_str::<Direction>("\"higher_is_better\"").unwrap(),
            Direction::HigherIsBetter
        );
        assert_eq!(
            serde_json::to_string(&Direction::LowerIsBetter).unwrap(),
            "\"minimize\""
        );
    }
}
