//! Commit-bound repository health, and how it changes over time.
//!
//! Phases 0–6 answer "did this patch pass?" for one candidate at a time. This
//! module answers a different question: *did the repository improve?* — which
//! only has meaning across commits, and only when the things being compared
//! were measured the same way.
//!
//! ```text
//! WorldModelSnapshot @ commit   ─┐
//! evaluations/metrics @ commit  ─┤──▶ RepositoryHealthSnapshot (immutable)
//! run + failure history         ─┘             │
//!                                              ▼
//!                            RepositoryHealthDiff / HealthTrend
//! ```
//!
//! Three rules shape everything here.
//!
//! **Evidence stays raw.** Dimensions hold measurements in their original units
//! with their own direction; there is no single health score, because
//! collapsing "performance improved, complexity worsened" into one number
//! destroys the only information a reader could act on.
//!
//! **Missing is not zero.** A repository with no benchmark has an *unavailable*
//! performance dimension, never a performance of 0. A security scan that never
//! ran is not a clean bill of health.
//!
//! **Comparisons require comparability.** Two numbers sharing a display name
//! are not a time series. `cargo test --lib` and `cargo test --workspace` both
//! produce "test duration" and measure different things, so every measurement
//! carries an identity that includes how it was produced.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::events::EvaluationSubject;
use crate::ids::{HealthSnapshotId, RunId, WorldModelFactId, WorldModelSnapshotId};
use crate::result::Direction;
use crate::world::{SnapshotRelation, WorldModelSnapshotStatus};

/// Schema of the persisted health record.
pub const HEALTH_SCHEMA_VERSION: &str = "health-v1";
/// Identity of the measurement-collection algorithm.
///
/// Changing how a dimension is measured must change this, so that snapshots
/// built under different rules are never silently compared.
pub const HEALTH_BUILDER_VERSION: &str = "health-builder-v1";
/// Identity of the diff/trend interpretation algorithm.
pub const TREND_ALGORITHM_VERSION: &str = "longitudinal-trend-v1";

/// Fewest points before a trend is anything but `InsufficientData`.
pub const MIN_TREND_POINTS: usize = 3;

// ---------------------------------------------------------------- observation

/// What a measurement is a measurement *of*.
///
/// Some numbers describe a repository at an instant — dependency count,
/// interface count. Others describe behaviour across a stretch of history —
/// failure rate, test reliability. A rate without its denominator is not a
/// measurement, so windows carry the observation count that produced them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "scope", rename_all = "snake_case")]
pub enum ObservationScope {
    /// True of the repository as it exists at exactly this commit.
    PointInTime { commit: String },
    /// Derived from history up to and including `end_commit`.
    Window {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        start: Option<DateTime<Utc>>,
        end: DateTime<Utc>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        start_commit: Option<String>,
        end_commit: String,
        /// The denominator. A failure rate of 0.5 over 2 runs and over 200 runs
        /// are different claims.
        observations: u64,
    },
}

impl ObservationScope {
    pub fn point(commit: impl Into<String>) -> Self {
        Self::PointInTime {
            commit: commit.into(),
        }
    }

    pub fn window(end_commit: impl Into<String>, end: DateTime<Utc>, observations: u64) -> Self {
        Self::Window {
            start: None,
            end,
            start_commit: None,
            end_commit: end_commit.into(),
            observations,
        }
    }

    pub fn commit(&self) -> &str {
        match self {
            Self::PointInTime { commit } => commit,
            Self::Window { end_commit, .. } => end_commit,
        }
    }

    /// The denominator, for window measurements.
    pub fn observations(&self) -> Option<u64> {
        match self {
            Self::PointInTime { .. } => None,
            Self::Window { observations, .. } => Some(*observations),
        }
    }

    pub fn is_window(&self) -> bool {
        matches!(self, Self::Window { .. })
    }

    /// How to describe the scope in a report.
    pub fn describe(&self) -> String {
        match self {
            Self::PointInTime { commit } => format!("at {}", short_commit(commit)),
            Self::Window {
                observations,
                end_commit,
                ..
            } => format!(
                "over {observations} observation{} through {}",
                if *observations == 1 { "" } else { "s" },
                short_commit(end_commit)
            ),
        }
    }
}

// --------------------------------------------------------------- comparability

/// What makes two measurements the same measurement.
///
/// This is the identity a time series is keyed by. It deliberately includes the
/// producing configuration: a build time from one command is not a data point
/// in the series of another, however similar the names look.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct MeasurementIdentity {
    /// Metric name as its producer emitted it.
    pub metric: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    pub direction: Direction,
    /// What produced it: an evaluator id, an extractor name, `ledger`.
    pub source: String,
    /// Configuration fingerprint of the producer, where one exists. Two
    /// measurements from differently-configured runs of the same evaluator are
    /// not comparable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
    /// Component the measurement is scoped to, when facts are component-level.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub component: Option<String>,
}

impl MeasurementIdentity {
    pub fn new(metric: impl Into<String>, direction: Direction, source: impl Into<String>) -> Self {
        Self {
            metric: metric.into(),
            unit: None,
            direction,
            source: source.into(),
            fingerprint: None,
            component: None,
        }
    }

    pub fn with_unit(mut self, unit: impl Into<String>) -> Self {
        self.unit = Some(unit.into());
        self
    }

    pub fn with_fingerprint(mut self, fingerprint: impl Into<String>) -> Self {
        self.fingerprint = Some(fingerprint.into());
        self
    }

    pub fn with_component(mut self, component: impl Into<String>) -> Self {
        self.component = Some(component.into());
        self
    }

    /// Stable key for grouping a time series.
    ///
    /// Every field that could change what a number means is included, so two
    /// values only share a key when they are genuinely the same measurement.
    pub fn comparability_key(&self) -> String {
        let mut digest = Sha256::new();
        for part in [
            self.metric.as_str(),
            self.unit.as_deref().unwrap_or(""),
            self.direction.as_str(),
            self.source.as_str(),
            self.fingerprint.as_deref().unwrap_or(""),
            self.component.as_deref().unwrap_or(""),
        ] {
            digest.update(part.as_bytes());
            digest.update([0x1f]);
        }
        format!("{:x}", digest.finalize())[..24].to_string()
    }

    /// Whether two measurements belong to the same series.
    pub fn is_comparable_with(&self, other: &Self) -> bool {
        self.comparability_key() == other.comparability_key()
    }

    /// Display label for reports.
    pub fn label(&self) -> String {
        let mut label = self.metric.clone();
        if let Some(component) = &self.component {
            label = format!("{component}/{label}");
        }
        label
    }
}

// -------------------------------------------------------------------- evidence

/// Where a measurement came from.
///
/// References existing records by their existing ids rather than copying them:
/// the ledger already holds the evidence, and duplicating it would create a
/// second version of the truth that could drift.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum HealthEvidence {
    WorldModelFact {
        snapshot_id: WorldModelSnapshotId,
        fact_id: WorldModelFactId,
    },
    /// A whole run contributed to the measurement.
    Run { run_id: RunId },
    /// One named metric from one run's evaluation.
    Metric { run_id: RunId, metric: String },
    /// A multi-agent execution contributed.
    TeamExecution { subject: EvaluationSubject },
    /// Derived from Git history alone.
    GitHistory { commit: String },
    /// A configured constraint or threshold.
    ConfiguredConstraint { reference: String },
}

// ------------------------------------------------------------------ dimensions

/// A longitudinal axis of repository health.
///
/// Provider-neutral by construction: nothing here knows about Rust, cargo, or
/// any particular evaluator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthDimensionKind {
    TestReliability,
    Complexity,
    DependencyCount,
    BuildTime,
    RuntimePerformance,
    Memory,
    Security,
    Duplication,
    ApiStability,
    FailureFrequency,
    RegressionFrequency,
}

impl HealthDimensionKind {
    /// Every dimension the roadmap names, in report order.
    pub const ALL: [HealthDimensionKind; 11] = [
        Self::TestReliability,
        Self::Complexity,
        Self::DependencyCount,
        Self::BuildTime,
        Self::RuntimePerformance,
        Self::Memory,
        Self::Security,
        Self::Duplication,
        Self::ApiStability,
        Self::FailureFrequency,
        Self::RegressionFrequency,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::TestReliability => "test_reliability",
            Self::Complexity => "complexity",
            Self::DependencyCount => "dependencies",
            Self::BuildTime => "build_time",
            Self::RuntimePerformance => "runtime_performance",
            Self::Memory => "memory",
            Self::Security => "security",
            Self::Duplication => "duplication",
            Self::ApiStability => "api_stability",
            Self::FailureFrequency => "failure_frequency",
            Self::RegressionFrequency => "regression_frequency",
        }
    }

    /// Human label for report columns.
    pub fn label(self) -> &'static str {
        match self {
            Self::TestReliability => "Test reliability",
            Self::Complexity => "Complexity",
            Self::DependencyCount => "Dependencies",
            Self::BuildTime => "Build time",
            Self::RuntimePerformance => "Performance",
            Self::Memory => "Memory",
            Self::Security => "Security",
            Self::Duplication => "Duplication",
            Self::ApiStability => "API stability",
            Self::FailureFrequency => "Failures",
            Self::RegressionFrequency => "Regressions",
        }
    }

    /// Whether this dimension describes an instant or a stretch of history.
    pub fn is_window_dimension(self) -> bool {
        matches!(
            self,
            Self::TestReliability | Self::FailureFrequency | Self::RegressionFrequency
        )
    }
}

impl std::fmt::Display for HealthDimensionKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Whether a dimension could be measured at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DimensionStatus {
    /// Measured, with evidence.
    Available,
    /// Measured, but the evidence is known to be incomplete.
    Partial,
    /// Nothing in this repository produces this measurement. Explicitly not
    /// the same as a measurement of zero.
    Unavailable,
}

impl DimensionStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::Partial => "partial",
            Self::Unavailable => "unavailable",
        }
    }
}

impl std::fmt::Display for DimensionStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One raw number, in its own units, with its own provenance.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HealthMeasurement {
    pub identity: MeasurementIdentity,
    pub value: f64,
    pub scope: ObservationScope,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<HealthEvidence>,
}

impl HealthMeasurement {
    pub fn new(identity: MeasurementIdentity, value: f64, scope: ObservationScope) -> Self {
        Self {
            identity,
            value,
            scope,
            evidence: Vec::new(),
        }
    }

    pub fn with_evidence(mut self, evidence: Vec<HealthEvidence>) -> Self {
        self.evidence = evidence;
        self
    }

    /// Formats the value with its unit, for reports.
    pub fn display_value(&self) -> String {
        let rendered = format_number(self.value);
        match &self.identity.unit {
            Some(unit) => format!("{rendered} {unit}"),
            None => rendered,
        }
    }
}

/// One dimension of repository health at one commit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HealthDimension {
    pub kind: HealthDimensionKind,
    pub status: DimensionStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub measurements: Vec<HealthMeasurement>,
    /// Why the dimension is partial or unavailable, in plain words.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

impl HealthDimension {
    /// A dimension nothing in this repository produces.
    pub fn unavailable(kind: HealthDimensionKind, reason: impl Into<String>) -> Self {
        Self {
            kind,
            status: DimensionStatus::Unavailable,
            measurements: Vec::new(),
            notes: vec![reason.into()],
        }
    }

    /// A dimension with measurements behind it.
    pub fn available(kind: HealthDimensionKind, measurements: Vec<HealthMeasurement>) -> Self {
        let status = if measurements.is_empty() {
            DimensionStatus::Unavailable
        } else {
            DimensionStatus::Available
        };
        Self {
            kind,
            status,
            measurements,
            notes: Vec::new(),
        }
    }

    /// A dimension measured from incomplete evidence.
    pub fn partial(
        kind: HealthDimensionKind,
        measurements: Vec<HealthMeasurement>,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            status: DimensionStatus::Partial,
            measurements,
            notes: vec![reason.into()],
        }
    }

    pub fn is_available(&self) -> bool {
        self.status != DimensionStatus::Unavailable
    }

    pub fn measurement(&self, metric: &str) -> Option<&HealthMeasurement> {
        self.measurements
            .iter()
            .find(|m| m.identity.metric == metric)
    }
}

// -------------------------------------------------------------------- snapshot

/// Whether a health snapshot rests on complete evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthSnapshotStatus {
    /// Every dimension the repository can produce was measured.
    Complete,
    /// Some dimension was unavailable or partial, or the world model was.
    Partial,
    /// Construction failed; the record exists to say so.
    Failed,
}

impl HealthSnapshotStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Partial => "partial",
            Self::Failed => "failed",
        }
    }
}

impl std::fmt::Display for HealthSnapshotStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What a health snapshot was built from, precisely enough to rebuild it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthProvenance {
    pub builder_version: String,
    /// The exact world model used. Never "the current one".
    pub world_model_snapshot_id: WorldModelSnapshotId,
    pub world_model_status: WorldModelSnapshotStatus,
    /// How far back window measurements looked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_start: Option<DateTime<Utc>>,
    /// Runs considered when building window dimensions.
    pub runs_considered: u64,
}

impl HealthProvenance {
    /// How complete the world model behind this snapshot was.
    pub fn world_model_status_label(&self) -> String {
        match self.world_model_status {
            WorldModelSnapshotStatus::Complete => "complete".to_string(),
            WorldModelSnapshotStatus::Partial => "partial".to_string(),
            WorldModelSnapshotStatus::Failed => "failed".to_string(),
        }
    }
}

/// Repository health at one exact commit. Immutable once recorded.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RepositoryHealthSnapshot {
    pub health_snapshot_id: HealthSnapshotId,
    pub repository: String,
    /// The commit this describes. Every measurement is a claim about *this*
    /// commit and no other.
    pub commit: String,
    pub world_model_snapshot_id: WorldModelSnapshotId,
    pub created_at: DateTime<Utc>,
    pub schema_version: String,
    pub status: HealthSnapshotStatus,
    pub dimensions: Vec<HealthDimension>,
    pub provenance: HealthProvenance,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HealthError {
    #[error("health repository must not be empty")]
    EmptyRepository,
    #[error("health snapshot commit `{0}` is not a full Git object id")]
    InvalidCommit(String),
    #[error("unsupported health schema `{0}`")]
    UnsupportedSchema(String),
    #[error("dimension `{0}` appears more than once")]
    DuplicateDimension(HealthDimensionKind),
    #[error("dimension `{kind}` is {status} but carries no measurements")]
    StatusContradictsMeasurements {
        kind: HealthDimensionKind,
        status: &'static str,
    },
    #[error(
        "measurement `{metric}` in dimension `{kind}` is scoped to commit `{found}`, \
         but the snapshot describes `{expected}`"
    )]
    MeasurementCommitMismatch {
        kind: HealthDimensionKind,
        metric: String,
        expected: String,
        found: String,
    },
    #[error("measurement `{metric}` has a non-finite value")]
    NonFiniteMeasurement { metric: String },
    #[error(
        "window measurement `{metric}` records no observations; a rate without \
         a denominator is not a measurement"
    )]
    WindowWithoutObservations { metric: String },
}

impl RepositoryHealthSnapshot {
    /// Rejects a snapshot that would misrepresent its own evidence.
    pub fn validate(&self) -> Result<(), HealthError> {
        if self.repository.trim().is_empty() {
            return Err(HealthError::EmptyRepository);
        }
        if self.commit.len() != 40 || !self.commit.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(HealthError::InvalidCommit(self.commit.clone()));
        }
        if self.schema_version != HEALTH_SCHEMA_VERSION {
            return Err(HealthError::UnsupportedSchema(self.schema_version.clone()));
        }

        let mut seen = BTreeSet::new();
        for dimension in &self.dimensions {
            if !seen.insert(dimension.kind) {
                return Err(HealthError::DuplicateDimension(dimension.kind));
            }
            if dimension.status != DimensionStatus::Unavailable && dimension.measurements.is_empty()
            {
                return Err(HealthError::StatusContradictsMeasurements {
                    kind: dimension.kind,
                    status: dimension.status.as_str(),
                });
            }
            for measurement in &dimension.measurements {
                if !measurement.value.is_finite() {
                    return Err(HealthError::NonFiniteMeasurement {
                        metric: measurement.identity.metric.clone(),
                    });
                }
                // The whole point of commit binding: evidence measured
                // elsewhere must not be presented as truth about this commit.
                if measurement.scope.commit() != self.commit {
                    return Err(HealthError::MeasurementCommitMismatch {
                        kind: dimension.kind,
                        metric: measurement.identity.metric.clone(),
                        expected: self.commit.clone(),
                        found: measurement.scope.commit().to_string(),
                    });
                }
                if measurement.scope.observations() == Some(0) {
                    return Err(HealthError::WindowWithoutObservations {
                        metric: measurement.identity.metric.clone(),
                    });
                }
            }
        }
        Ok(())
    }

    pub fn dimension(&self, kind: HealthDimensionKind) -> Option<&HealthDimension> {
        self.dimensions.iter().find(|d| d.kind == kind)
    }

    /// Every measurement, keyed by comparability identity.
    pub fn measurements_by_key(&self) -> BTreeMap<String, (&HealthDimension, &HealthMeasurement)> {
        let mut map = BTreeMap::new();
        for dimension in &self.dimensions {
            for measurement in &dimension.measurements {
                map.insert(
                    measurement.identity.comparability_key(),
                    (dimension, measurement),
                );
            }
        }
        map
    }

    pub fn available_dimensions(&self) -> usize {
        self.dimensions.iter().filter(|d| d.is_available()).count()
    }

    /// Derives the snapshot status from the evidence actually gathered.
    pub fn derive_status(
        dimensions: &[HealthDimension],
        world_model_status: WorldModelSnapshotStatus,
    ) -> HealthSnapshotStatus {
        if world_model_status == WorldModelSnapshotStatus::Failed {
            return HealthSnapshotStatus::Failed;
        }
        let any_incomplete = world_model_status == WorldModelSnapshotStatus::Partial
            || dimensions
                .iter()
                .any(|d| d.status != DimensionStatus::Available);
        if any_incomplete {
            HealthSnapshotStatus::Partial
        } else {
            HealthSnapshotStatus::Complete
        }
    }
}

// ------------------------------------------------------- measured repository state

/// Which repository state a piece of evidence actually describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MeasuredStateKind {
    /// Base plus the run's committed candidate. This is what an ordinary run's
    /// evaluators actually executed against.
    CandidateHead,
    /// The run changed nothing, so the evaluated workspace *was* the base
    /// commit. Evidence legitimately describes the base here — and only here.
    BaseUnchanged,
    /// The integrated result of a multi-agent execution.
    TeamFinal,
    /// The output of one node in a team plan.
    TeamNodeOutput,
}

impl MeasuredStateKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CandidateHead => "candidate_head",
            Self::BaseUnchanged => "base_unchanged",
            Self::TeamFinal => "team_final",
            Self::TeamNodeOutput => "team_node_output",
        }
    }
}

/// The exact repository state a measurement was observed on.
///
/// This exists because the obvious answer is wrong. A run record stores
/// `base_commit`, but its evaluators ran against the workspace *after* the
/// agent's patch was applied — so attaching a benchmark result to
/// `base_commit` would credit a measurement to the commit before the one it
/// describes, and every trend built from it would be off by one change.
///
/// When the state cannot be named, the evidence is excluded with a reason
/// rather than guessed at. An unattributable measurement is worse than a
/// missing one: it is a number that looks trustworthy and is not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum MeasuredRepositoryState {
    Commit {
        commit: String,
        kind: MeasuredStateKind,
    },
    Unknown {
        reason: String,
    },
}

impl MeasuredRepositoryState {
    pub fn commit(&self) -> Option<&str> {
        match self {
            Self::Commit { commit, .. } => Some(commit),
            Self::Unknown { .. } => None,
        }
    }

    pub fn kind(&self) -> Option<MeasuredStateKind> {
        match self {
            Self::Commit { kind, .. } => Some(*kind),
            Self::Unknown { .. } => None,
        }
    }

    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Commit { .. } => None,
            Self::Unknown { reason } => Some(reason),
        }
    }

    fn unknown(reason: impl Into<String>) -> Self {
        Self::Unknown {
            reason: reason.into(),
        }
    }

    /// Determines the state an ordinary run's evaluation evidence describes.
    ///
    /// - No patch record at all: the evaluated state cannot be named.
    /// - An empty patch: nothing changed, so the evaluated state is the base.
    /// - A patch that was committed: the candidate head commit.
    /// - A patch that was never committed: no nameable commit exists, so the
    ///   evidence is excluded even though the numbers are real.
    pub fn for_run(base_commit: &str, patch: Option<RunPatchState<'_>>) -> Self {
        let Some(patch) = patch else {
            return Self::unknown(
                "run has no patch record, so the evaluated repository state cannot be named",
            );
        };
        if patch.is_empty {
            return Self::Commit {
                commit: base_commit.to_string(),
                kind: MeasuredStateKind::BaseUnchanged,
            };
        }
        match patch.head_commit {
            Some(head) if !head.trim().is_empty() => Self::Commit {
                commit: head.to_string(),
                kind: MeasuredStateKind::CandidateHead,
            },
            _ => Self::unknown(
                "run produced changes that were never committed, so the evaluated state \
                 has no commit id",
            ),
        }
    }

    /// The state a completed team execution's integrated evaluation describes.
    pub fn for_team_execution(final_commit: Option<&str>) -> Self {
        match final_commit {
            Some(commit) if !commit.trim().is_empty() => Self::Commit {
                commit: commit.to_string(),
                kind: MeasuredStateKind::TeamFinal,
            },
            _ => Self::unknown(
                "team execution recorded no final commit, so its integrated evaluation \
                 describes no nameable state",
            ),
        }
    }

    /// The state one team node's evaluation describes.
    pub fn for_team_node(output_commit: Option<&str>) -> Self {
        match output_commit {
            Some(commit) if !commit.trim().is_empty() => Self::Commit {
                commit: commit.to_string(),
                kind: MeasuredStateKind::TeamNodeOutput,
            },
            _ => Self::unknown(
                "team node recorded no output commit, so its evidence describes no \
                 nameable state",
            ),
        }
    }
}

/// The patch facts needed to decide what a run's evidence measured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunPatchState<'a> {
    pub head_commit: Option<&'a str>,
    pub is_empty: bool,
}

// ------------------------------------------------------------------ materiality

/// How large a change has to be before it is called meaningful.
///
/// Deliberately tiny: a threshold table, not a policy language. Without a
/// configured threshold, a change is reported with its true magnitude and
/// simply not marked material.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MaterialityPolicy {
    /// Applied to any metric without its own threshold.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_percent: Option<f64>,
    /// Per-metric percentage thresholds, keyed by metric name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metrics: BTreeMap<String, f64>,
}

impl MaterialityPolicy {
    pub fn with_default(mut self, percent: f64) -> Self {
        self.default_percent = Some(percent);
        self
    }

    pub fn with_metric(mut self, metric: impl Into<String>, percent: f64) -> Self {
        self.metrics.insert(metric.into(), percent);
        self
    }

    pub fn threshold_for(&self, metric: &str) -> Option<f64> {
        self.metrics.get(metric).copied().or(self.default_percent)
    }

    /// Whether a percentage change clears the configured bar.
    ///
    /// With no threshold configured, nothing is claimed either way — the change
    /// is real and simply not labelled material.
    pub fn is_material(&self, metric: &str, percent_change: Option<f64>) -> bool {
        match (self.threshold_for(metric), percent_change) {
            (Some(threshold), Some(change)) => change.abs() >= threshold,
            _ => false,
        }
    }
}

// ----------------------------------------------------------------- attribution

/// How strongly evidence supports a claim that some execution caused a change.
///
/// Temporal proximity alone is `Associated` at best. Forge produces plenty of
/// commits; producing one is not the same as being shown to have caused a
/// measurement to move.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttributionLevel {
    /// Before and after are both known for the same measurement, the execution
    /// produced the later commit, and the change exceeds the declared
    /// threshold.
    Confirmed,
    /// Before and after are known and the execution produced the later commit,
    /// but no threshold declares the change significant.
    Supported,
    /// The execution produced the commit; no before/after pair supports a
    /// causal claim.
    Associated,
    /// No execution is known to have produced the commit — a human or external
    /// automation may have.
    Unknown,
}

impl AttributionLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Confirmed => "confirmed",
            Self::Supported => "supported",
            Self::Associated => "associated",
            Self::Unknown => "unknown",
        }
    }

    /// Whether the level supports describing the execution as a cause.
    pub fn is_causal(self) -> bool {
        matches!(self, Self::Confirmed | Self::Supported)
    }
}

impl std::fmt::Display for AttributionLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A Forge execution that may have contributed to a health change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionAttribution {
    pub level: AttributionLevel,
    /// The run or team execution, reusing the typed subject from Phase 5.
    pub subject: EvaluationSubject,
    pub commit: String,
    /// Why this level and not a stronger one.
    pub rationale: String,
}

// ------------------------------------------------------------------------ diff

/// What a measurement did between two snapshots.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeClassification {
    /// Moved in the direction the metric declares as better.
    Improvement,
    /// Moved against the metric's declared direction.
    Regression,
    /// Changed, but the metric has no direction — structural facts like
    /// dependency counts change without being good or bad.
    Neutral,
    /// Did not change.
    Unchanged,
    /// Measurable now, not measurable before. Not an infinite improvement.
    NewlyAvailable,
    /// Measurable before, not measurable now. Absence of a scan is not a
    /// clean result.
    NoLongerAvailable,
}

impl ChangeClassification {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Improvement => "improvement",
            Self::Regression => "regression",
            Self::Neutral => "change",
            Self::Unchanged => "unchanged",
            Self::NewlyAvailable => "newly available",
            Self::NoLongerAvailable => "no longer available",
        }
    }

    /// Classifies a movement using the metric's own declared direction.
    pub fn from_delta(direction: Direction, delta: f64) -> Self {
        if delta == 0.0 {
            return Self::Unchanged;
        }
        match direction {
            Direction::HigherIsBetter if delta > 0.0 => Self::Improvement,
            Direction::HigherIsBetter => Self::Regression,
            Direction::LowerIsBetter if delta < 0.0 => Self::Improvement,
            Direction::LowerIsBetter => Self::Regression,
            // A structural count changed. Whether that is good is a policy
            // question this layer refuses to answer.
            Direction::Neutral => Self::Neutral,
        }
    }
}

impl std::fmt::Display for ChangeClassification {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One measurement's movement between two health snapshots.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HealthChange {
    pub dimension: HealthDimensionKind,
    pub identity: MeasurementIdentity,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delta: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub percent_change: Option<f64>,
    pub classification: ChangeClassification,
    /// Whether a configured threshold calls this change significant.
    pub material: bool,
}

impl HealthChange {
    /// Renders the movement, e.g. `12.4 s → 13.2 s  (+6.5%)`.
    pub fn describe(&self) -> String {
        let unit = self
            .identity
            .unit
            .as_ref()
            .map(|u| format!(" {u}"))
            .unwrap_or_default();
        match (self.from, self.to) {
            (Some(from), Some(to)) => {
                let percent = self
                    .percent_change
                    .map(|p| format!("  ({}{:.1}%)", if p >= 0.0 { "+" } else { "" }, p))
                    .unwrap_or_default();
                format!(
                    "{}{unit} → {}{unit}{percent}",
                    format_number(from),
                    format_number(to)
                )
            }
            (None, Some(to)) => format!("— → {}{unit}", format_number(to)),
            (Some(from), None) => format!("{}{unit} → —", format_number(from)),
            (None, None) => "—".to_string(),
        }
    }
}

/// A typed comparison of two health snapshots.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RepositoryHealthDiff {
    pub from_snapshot_id: HealthSnapshotId,
    pub to_snapshot_id: HealthSnapshotId,
    pub from_commit: String,
    pub to_commit: String,
    /// How the two commits relate. A diverged pair can be compared
    /// structurally but is not a chronology.
    pub relation: SnapshotRelation,
    pub improvements: Vec<HealthChange>,
    pub regressions: Vec<HealthChange>,
    pub neutral_changes: Vec<HealthChange>,
    pub newly_available: Vec<HealthChange>,
    pub no_longer_available: Vec<HealthChange>,
    /// Executions that may have contributed, with honest confidence.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attribution: Vec<ExecutionAttribution>,
    pub algorithm_version: String,
}

impl RepositoryHealthDiff {
    pub fn is_empty(&self) -> bool {
        self.improvements.is_empty()
            && self.regressions.is_empty()
            && self.neutral_changes.is_empty()
            && self.newly_available.is_empty()
            && self.no_longer_available.is_empty()
    }

    pub fn all_changes(&self) -> impl Iterator<Item = &HealthChange> {
        self.improvements
            .iter()
            .chain(self.regressions.iter())
            .chain(self.neutral_changes.iter())
            .chain(self.newly_available.iter())
            .chain(self.no_longer_available.iter())
    }

    /// Whether the comparison describes a chronology or merely two states.
    pub fn is_chronological(&self) -> bool {
        matches!(self.relation, SnapshotRelation::Ancestor)
    }
}

// ----------------------------------------------------------------------- trend

/// The direction a series is moving.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrendDirection {
    Improving,
    Degrading,
    Stable,
    /// Moved consistently, but the metric declares no better direction.
    /// Dependency counts and interface counts live here: they change, and
    /// whether that is good is a policy question Phase 7 refuses to answer.
    Changing,
    /// Series within the dimension disagree.
    Mixed,
    /// Fewer than [`MIN_TREND_POINTS`] comparable points.
    InsufficientData,
}

impl TrendDirection {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Improving => "Improving",
            Self::Degrading => "Degrading",
            Self::Stable => "Stable",
            Self::Changing => "Changing",
            Self::Mixed => "Mixed",
            Self::InsufficientData => "InsufficientData",
        }
    }

    /// Combines per-series directions into one, without inventing a score.
    ///
    /// Disagreement stays visible as `Mixed`; it is more useful than a number
    /// that averages an improvement and a regression into nothing.
    ///
    /// `Changing` never drives the overall reading. A repository that grew two
    /// dependencies has not thereby got better or worse, and saying so would
    /// be the sort of unearned judgment this phase exists to avoid.
    pub fn combine(directions: &[TrendDirection]) -> Self {
        let known: Vec<TrendDirection> = directions
            .iter()
            .copied()
            .filter(|d| *d != Self::InsufficientData)
            .collect();
        if known.is_empty() {
            return Self::InsufficientData;
        }
        let improving = known.contains(&Self::Improving);
        let degrading = known.contains(&Self::Degrading);
        match (improving, degrading) {
            (true, true) => Self::Mixed,
            (true, false) => Self::Improving,
            (false, true) => Self::Degrading,
            (false, false) if known.contains(&Self::Mixed) => Self::Mixed,
            // Structural movement with nothing improving or degrading stays
            // visible as `Changing`. Reporting it as `Stable` would hide a real
            // change behind a word that means nothing happened.
            (false, false) if known.contains(&Self::Changing) => Self::Changing,
            (false, false) => Self::Stable,
        }
    }
}

impl std::fmt::Display for TrendDirection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One observation in a series.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrendPoint {
    pub health_snapshot_id: HealthSnapshotId,
    pub commit: String,
    pub observed_at: DateTime<Utc>,
    pub value: f64,
}

/// One comparable series and what it is doing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HealthTrend {
    pub dimension: HealthDimensionKind,
    pub identity: MeasurementIdentity,
    pub direction: TrendDirection,
    pub points: Vec<TrendPoint>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub percent_change: Option<f64>,
    /// Why this direction was reported.
    pub evidence: String,
}

/// Every trend for one repository, plus the honest overall reading.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RepositoryHealthTrends {
    pub repository: String,
    pub trends: Vec<HealthTrend>,
    /// Per-dimension roll-up, in report order.
    pub dimensions: Vec<(HealthDimensionKind, TrendDirection)>,
    pub overall: TrendDirection,
    pub snapshots_considered: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_start: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_end: Option<DateTime<Utc>>,
    pub algorithm_version: String,
}

impl RepositoryHealthTrends {
    pub fn direction_for(&self, kind: HealthDimensionKind) -> Option<TrendDirection> {
        self.dimensions
            .iter()
            .find(|(dimension, _)| *dimension == kind)
            .map(|(_, direction)| *direction)
    }
}

// ---------------------------------------------------------------------- events

/// Health lifecycle events, subject to a health snapshot rather than a run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HealthEvent {
    pub health_snapshot_id: HealthSnapshotId,
    pub seq: u64,
    pub timestamp: DateTime<Utc>,
    pub payload: HealthEventPayload,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HealthEventPayload {
    HealthBuildStarted {
        repository: String,
        commit: String,
        world_model_snapshot_id: WorldModelSnapshotId,
    },
    HealthDimensionCollected {
        dimension: HealthDimensionKind,
        status: DimensionStatus,
        measurements: u64,
    },
    HealthBuildCompleted {
        status: HealthSnapshotStatus,
        dimensions_available: u64,
    },
    HealthBuildFailed {
        error: String,
    },
    HealthDiffCreated {
        from: HealthSnapshotId,
        to: HealthSnapshotId,
        improvements: u64,
        regressions: u64,
    },
    TrendComputed {
        overall: TrendDirection,
        series: u64,
        algorithm_version: String,
    },
}

impl HealthEventPayload {
    pub fn event_type(&self) -> &'static str {
        match self {
            Self::HealthBuildStarted { .. } => "HealthBuildStarted",
            Self::HealthDimensionCollected { .. } => "HealthDimensionCollected",
            Self::HealthBuildCompleted { .. } => "HealthBuildCompleted",
            Self::HealthBuildFailed { .. } => "HealthBuildFailed",
            Self::HealthDiffCreated { .. } => "HealthDiffCreated",
            Self::TrendComputed { .. } => "TrendComputed",
        }
    }
}

// --------------------------------------------------------------------- helpers

/// Percentage change, or `None` when the baseline is zero.
///
/// Dividing by a zero baseline yields infinity, which would be reported as an
/// enormous regression when the honest answer is that percentage change is not
/// defined here.
pub fn percent_change(from: f64, to: f64) -> Option<f64> {
    if from == 0.0 || !from.is_finite() || !to.is_finite() {
        return None;
    }
    Some((to - from) / from.abs() * 100.0)
}

/// Renders a measurement without trailing noise.
pub fn format_number(value: f64) -> String {
    if value == value.trunc() && value.abs() < 1e15 {
        format!("{}", value as i64)
    } else {
        format!("{value:.2}")
    }
}

fn short_commit(commit: &str) -> String {
    commit.chars().take(7).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commit(seed: char) -> String {
        std::iter::repeat_n(seed, 40).collect()
    }

    fn identity(metric: &str, direction: Direction) -> MeasurementIdentity {
        MeasurementIdentity::new(metric, direction, "tests")
    }

    fn snapshot(commit_hash: &str, dimensions: Vec<HealthDimension>) -> RepositoryHealthSnapshot {
        RepositoryHealthSnapshot {
            health_snapshot_id: HealthSnapshotId::sequential(1),
            repository: "forge".into(),
            commit: commit_hash.to_string(),
            world_model_snapshot_id: WorldModelSnapshotId::sequential(1),
            created_at: Utc::now(),
            schema_version: HEALTH_SCHEMA_VERSION.into(),
            status: HealthSnapshotStatus::Complete,
            dimensions,
            provenance: HealthProvenance {
                builder_version: HEALTH_BUILDER_VERSION.into(),
                world_model_snapshot_id: WorldModelSnapshotId::sequential(1),
                world_model_status: WorldModelSnapshotStatus::Complete,
                window_start: None,
                runs_considered: 0,
            },
        }
    }

    // ---------------------------------------------------------- observation

    #[test]
    fn a_window_measurement_carries_its_denominator() {
        let window = ObservationScope::window(commit('a'), Utc::now(), 12);
        assert_eq!(window.observations(), Some(12));
        assert!(window.is_window());
        assert!(window.describe().contains("12 observations"));
    }

    #[test]
    fn a_point_measurement_has_no_denominator() {
        let point = ObservationScope::point(commit('a'));
        assert_eq!(point.observations(), None);
        assert!(!point.is_window());
    }

    #[test]
    fn window_dimensions_are_distinguished_from_point_dimensions() {
        assert!(HealthDimensionKind::FailureFrequency.is_window_dimension());
        assert!(HealthDimensionKind::TestReliability.is_window_dimension());
        assert!(!HealthDimensionKind::DependencyCount.is_window_dimension());
        assert!(!HealthDimensionKind::Complexity.is_window_dimension());
    }

    // -------------------------------------------------------- comparability

    #[test]
    fn measurements_differing_in_configuration_are_not_the_same_series() {
        // `cargo test --lib` and `cargo test --workspace` both produce a test
        // duration and measure different things.
        let narrow = identity("test_duration_ms", Direction::LowerIsBetter)
            .with_fingerprint("fingerprint-lib");
        let broad = identity("test_duration_ms", Direction::LowerIsBetter)
            .with_fingerprint("fingerprint-workspace");
        assert!(!narrow.is_comparable_with(&broad));
    }

    #[test]
    fn measurements_differing_in_unit_are_not_merged() {
        let seconds = identity("build_time", Direction::LowerIsBetter).with_unit("s");
        let millis = identity("build_time", Direction::LowerIsBetter).with_unit("ms");
        assert!(!seconds.is_comparable_with(&millis));
    }

    #[test]
    fn measurements_differing_in_direction_are_not_merged() {
        let up = identity("throughput", Direction::HigherIsBetter);
        let down = identity("throughput", Direction::LowerIsBetter);
        assert!(!up.is_comparable_with(&down));
    }

    #[test]
    fn measurements_differing_in_component_are_not_merged() {
        let storage = identity("complexity", Direction::Neutral).with_component("storage");
        let router = identity("complexity", Direction::Neutral).with_component("router");
        assert!(!storage.is_comparable_with(&router));
        assert_eq!(storage.label(), "storage/complexity");
    }

    #[test]
    fn identical_identities_share_a_series() {
        let a = identity("throughput", Direction::HigherIsBetter)
            .with_unit("MB/s")
            .with_fingerprint("f1");
        let b = identity("throughput", Direction::HigherIsBetter)
            .with_unit("MB/s")
            .with_fingerprint("f1");
        assert!(a.is_comparable_with(&b));
        assert_eq!(a.comparability_key(), b.comparability_key());
    }

    // ------------------------------------------------------------ snapshot

    #[test]
    fn a_snapshot_rejects_evidence_measured_at_another_commit() {
        // The central commit-binding rule.
        let elsewhere = HealthMeasurement::new(
            identity("dependencies", Direction::Neutral),
            41.0,
            ObservationScope::point(commit('b')),
        );
        let snapshot = snapshot(
            &commit('a'),
            vec![HealthDimension::available(
                HealthDimensionKind::DependencyCount,
                vec![elsewhere],
            )],
        );

        let err = snapshot.validate().unwrap_err();
        assert!(
            matches!(err, HealthError::MeasurementCommitMismatch { .. }),
            "{err}"
        );
    }

    #[test]
    fn a_valid_snapshot_validates() {
        let measurement = HealthMeasurement::new(
            identity("dependencies", Direction::Neutral),
            41.0,
            ObservationScope::point(commit('a')),
        );
        let snapshot = snapshot(
            &commit('a'),
            vec![HealthDimension::available(
                HealthDimensionKind::DependencyCount,
                vec![measurement],
            )],
        );
        snapshot.validate().unwrap();
    }

    #[test]
    fn a_snapshot_rejects_a_duplicated_dimension() {
        let make = || {
            HealthDimension::available(
                HealthDimensionKind::DependencyCount,
                vec![HealthMeasurement::new(
                    identity("dependencies", Direction::Neutral),
                    1.0,
                    ObservationScope::point(commit('a')),
                )],
            )
        };
        let snapshot = snapshot(&commit('a'), vec![make(), make()]);
        assert!(matches!(
            snapshot.validate(),
            Err(HealthError::DuplicateDimension(_))
        ));
    }

    #[test]
    fn an_available_dimension_must_have_measurements() {
        let mut dimension = HealthDimension::available(HealthDimensionKind::BuildTime, vec![]);
        dimension.status = DimensionStatus::Available;
        let snapshot = snapshot(&commit('a'), vec![dimension]);
        assert!(matches!(
            snapshot.validate(),
            Err(HealthError::StatusContradictsMeasurements { .. })
        ));
    }

    #[test]
    fn a_window_measurement_with_no_observations_is_rejected() {
        let measurement = HealthMeasurement::new(
            identity("failure_rate", Direction::LowerIsBetter),
            0.5,
            ObservationScope::window(commit('a'), Utc::now(), 0),
        );
        let snapshot = snapshot(
            &commit('a'),
            vec![HealthDimension::available(
                HealthDimensionKind::FailureFrequency,
                vec![measurement],
            )],
        );
        assert!(matches!(
            snapshot.validate(),
            Err(HealthError::WindowWithoutObservations { .. })
        ));
    }

    #[test]
    fn a_non_forty_character_commit_is_rejected() {
        let snapshot = snapshot("abc1234", vec![]);
        assert!(matches!(
            snapshot.validate(),
            Err(HealthError::InvalidCommit(_))
        ));
    }

    #[test]
    fn an_unavailable_dimension_is_not_a_measurement_of_zero() {
        let dimension =
            HealthDimension::unavailable(HealthDimensionKind::Memory, "no evaluator emits memory");
        assert_eq!(dimension.status, DimensionStatus::Unavailable);
        assert!(dimension.measurements.is_empty());
        assert!(!dimension.is_available());
        // Nothing anywhere reports a value.
        assert!(dimension.measurement("peak_rss").is_none());
    }

    #[test]
    fn a_partial_world_model_makes_the_health_snapshot_partial() {
        let complete = vec![HealthDimension::available(
            HealthDimensionKind::DependencyCount,
            vec![HealthMeasurement::new(
                identity("dependencies", Direction::Neutral),
                1.0,
                ObservationScope::point(commit('a')),
            )],
        )];
        assert_eq!(
            RepositoryHealthSnapshot::derive_status(&complete, WorldModelSnapshotStatus::Complete),
            HealthSnapshotStatus::Complete
        );
        assert_eq!(
            RepositoryHealthSnapshot::derive_status(&complete, WorldModelSnapshotStatus::Partial),
            HealthSnapshotStatus::Partial
        );
        assert_eq!(
            RepositoryHealthSnapshot::derive_status(&complete, WorldModelSnapshotStatus::Failed),
            HealthSnapshotStatus::Failed
        );
    }

    #[test]
    fn an_unavailable_dimension_makes_the_snapshot_partial() {
        let dimensions = vec![HealthDimension::unavailable(
            HealthDimensionKind::Memory,
            "none",
        )];
        assert_eq!(
            RepositoryHealthSnapshot::derive_status(
                &dimensions,
                WorldModelSnapshotStatus::Complete
            ),
            HealthSnapshotStatus::Partial
        );
    }

    #[test]
    fn snapshots_round_trip() {
        let snapshot = snapshot(
            &commit('a'),
            vec![HealthDimension::available(
                HealthDimensionKind::DependencyCount,
                vec![
                    HealthMeasurement::new(
                        identity("dependencies", Direction::Neutral),
                        41.0,
                        ObservationScope::point(commit('a')),
                    )
                    .with_evidence(vec![HealthEvidence::WorldModelFact {
                        snapshot_id: WorldModelSnapshotId::sequential(1),
                        fact_id: WorldModelFactId::new("WF-abc").unwrap(),
                    }]),
                ],
            )],
        );
        let json = serde_json::to_string(&snapshot).unwrap();
        assert_eq!(
            serde_json::from_str::<RepositoryHealthSnapshot>(&json).unwrap(),
            snapshot
        );
    }

    // ------------------------------------------------------- classification

    #[test]
    fn direction_decides_improvement_and_regression() {
        assert_eq!(
            ChangeClassification::from_delta(Direction::HigherIsBetter, 10.0),
            ChangeClassification::Improvement
        );
        assert_eq!(
            ChangeClassification::from_delta(Direction::HigherIsBetter, -10.0),
            ChangeClassification::Regression
        );
        assert_eq!(
            ChangeClassification::from_delta(Direction::LowerIsBetter, -10.0),
            ChangeClassification::Improvement
        );
        assert_eq!(
            ChangeClassification::from_delta(Direction::LowerIsBetter, 10.0),
            ChangeClassification::Regression
        );
    }

    #[test]
    fn structural_counts_change_without_being_good_or_bad() {
        // Two more dependencies is a change. Whether it is a problem is a
        // policy question Phase 7 declines to answer.
        assert_eq!(
            ChangeClassification::from_delta(Direction::Neutral, 2.0),
            ChangeClassification::Neutral
        );
        assert_eq!(
            ChangeClassification::from_delta(Direction::Neutral, -2.0),
            ChangeClassification::Neutral
        );
    }

    #[test]
    fn no_movement_is_unchanged_whatever_the_direction() {
        for direction in [
            Direction::HigherIsBetter,
            Direction::LowerIsBetter,
            Direction::Neutral,
        ] {
            assert_eq!(
                ChangeClassification::from_delta(direction, 0.0),
                ChangeClassification::Unchanged
            );
        }
    }

    // -------------------------------------------------- measured repository state

    #[test]
    fn an_ordinary_candidate_run_measures_the_head_commit_not_the_base() {
        // The correction that motivates this type: evaluators ran against
        // base + patch, which is the head commit.
        let state = MeasuredRepositoryState::for_run(
            &commit('a'),
            Some(RunPatchState {
                head_commit: Some(&commit('b')),
                is_empty: false,
            }),
        );
        assert_eq!(state.commit(), Some(commit('b').as_str()));
        assert_eq!(state.kind(), Some(MeasuredStateKind::CandidateHead));
    }

    #[test]
    fn a_no_change_run_measures_the_base_commit() {
        // Nothing was applied, so the evaluated workspace really was the base.
        let state = MeasuredRepositoryState::for_run(
            &commit('a'),
            Some(RunPatchState {
                head_commit: None,
                is_empty: true,
            }),
        );
        assert_eq!(state.commit(), Some(commit('a').as_str()));
        assert_eq!(state.kind(), Some(MeasuredStateKind::BaseUnchanged));
    }

    #[test]
    fn an_uncommitted_candidate_is_excluded_rather_than_attributed_to_the_base() {
        let state = MeasuredRepositoryState::for_run(
            &commit('a'),
            Some(RunPatchState {
                head_commit: None,
                is_empty: false,
            }),
        );
        assert_eq!(state.commit(), None);
        assert!(state.reason().unwrap().contains("never committed"));
    }

    #[test]
    fn a_run_without_a_patch_record_is_excluded() {
        let state = MeasuredRepositoryState::for_run(&commit('a'), None);
        assert_eq!(state.commit(), None);
        assert!(state.reason().unwrap().contains("cannot be named"));
    }

    #[test]
    fn team_evidence_binds_to_final_and_node_commits() {
        assert_eq!(
            MeasuredRepositoryState::for_team_execution(Some(&commit('c'))).kind(),
            Some(MeasuredStateKind::TeamFinal)
        );
        assert_eq!(
            MeasuredRepositoryState::for_team_node(Some(&commit('d'))).kind(),
            Some(MeasuredStateKind::TeamNodeOutput)
        );
        assert!(
            MeasuredRepositoryState::for_team_execution(None)
                .commit()
                .is_none()
        );
        assert!(
            MeasuredRepositoryState::for_team_node(Some("  "))
                .commit()
                .is_none()
        );
    }

    #[test]
    fn measured_state_round_trips() {
        let state = MeasuredRepositoryState::for_run(
            &commit('a'),
            Some(RunPatchState {
                head_commit: Some(&commit('b')),
                is_empty: false,
            }),
        );
        let json = serde_json::to_string(&state).unwrap();
        assert_eq!(
            serde_json::from_str::<MeasuredRepositoryState>(&json).unwrap(),
            state
        );
    }

    // ---------------------------------------------------------- materiality

    #[test]
    fn a_threshold_separates_noise_from_a_material_change() {
        let policy = MaterialityPolicy::default().with_metric("build_time", 5.0);
        assert!(!policy.is_material("build_time", Some(2.0)));
        assert!(policy.is_material("build_time", Some(6.5)));
        assert!(policy.is_material("build_time", Some(-6.5)));
    }

    #[test]
    fn without_a_threshold_nothing_is_claimed_to_be_material() {
        let policy = MaterialityPolicy::default();
        assert!(!policy.is_material("build_time", Some(90.0)));
        assert_eq!(policy.threshold_for("build_time"), None);
    }

    #[test]
    fn a_default_threshold_applies_to_unlisted_metrics() {
        let policy = MaterialityPolicy::default()
            .with_default(10.0)
            .with_metric("build_time", 5.0);
        assert_eq!(policy.threshold_for("build_time"), Some(5.0));
        assert_eq!(policy.threshold_for("throughput"), Some(10.0));
        assert!(policy.is_material("throughput", Some(12.0)));
        assert!(!policy.is_material("throughput", Some(8.0)));
    }

    // ---------------------------------------------------------- attribution

    #[test]
    fn attribution_levels_separate_causal_from_correlational() {
        assert!(AttributionLevel::Confirmed.is_causal());
        assert!(AttributionLevel::Supported.is_causal());
        // Producing the commit is not evidence of causing the measurement to
        // move.
        assert!(!AttributionLevel::Associated.is_causal());
        assert!(!AttributionLevel::Unknown.is_causal());
    }

    #[test]
    fn attribution_reuses_the_typed_execution_subject() {
        let attribution = ExecutionAttribution {
            level: AttributionLevel::Associated,
            subject: EvaluationSubject::Run(RunId::sequential(1004)),
            commit: commit('b'),
            rationale: "run produced the commit".into(),
        };
        let json = serde_json::to_string(&attribution).unwrap();
        assert_eq!(
            serde_json::from_str::<ExecutionAttribution>(&json).unwrap(),
            attribution
        );
    }

    // --------------------------------------------------------------- trend

    #[test]
    fn disagreeing_series_combine_to_mixed_rather_than_a_score() {
        assert_eq!(
            TrendDirection::combine(&[TrendDirection::Improving, TrendDirection::Degrading]),
            TrendDirection::Mixed
        );
    }

    #[test]
    fn agreeing_series_combine_to_that_direction() {
        assert_eq!(
            TrendDirection::combine(&[TrendDirection::Improving, TrendDirection::Stable]),
            TrendDirection::Improving
        );
        assert_eq!(
            TrendDirection::combine(&[TrendDirection::Degrading, TrendDirection::Stable]),
            TrendDirection::Degrading
        );
        assert_eq!(
            TrendDirection::combine(&[TrendDirection::Stable, TrendDirection::Stable]),
            TrendDirection::Stable
        );
    }

    #[test]
    fn a_structural_change_never_drives_the_reading_toward_better_or_worse() {
        // Two more dependencies is not an improvement or a regression — but it
        // is also not nothing, so it must not be reported as `Stable`.
        assert_eq!(
            TrendDirection::combine(&[TrendDirection::Changing, TrendDirection::Stable]),
            TrendDirection::Changing
        );
        assert_eq!(
            TrendDirection::combine(&[TrendDirection::Changing, TrendDirection::Changing]),
            TrendDirection::Changing
        );
        // Directional signals still decide the direction.
        assert_eq!(
            TrendDirection::combine(&[TrendDirection::Changing, TrendDirection::Improving]),
            TrendDirection::Improving
        );
        assert_eq!(
            TrendDirection::combine(&[
                TrendDirection::Changing,
                TrendDirection::Improving,
                TrendDirection::Degrading
            ]),
            TrendDirection::Mixed
        );
    }

    #[test]
    fn insufficient_series_do_not_drag_a_reading_down() {
        assert_eq!(
            TrendDirection::combine(&[TrendDirection::Improving, TrendDirection::InsufficientData]),
            TrendDirection::Improving
        );
        assert_eq!(
            TrendDirection::combine(&[TrendDirection::InsufficientData]),
            TrendDirection::InsufficientData
        );
        assert_eq!(
            TrendDirection::combine(&[]),
            TrendDirection::InsufficientData
        );
    }

    // ------------------------------------------------------------- helpers

    #[test]
    fn percent_change_refuses_a_zero_baseline() {
        // Otherwise 0 → 5 reads as an infinite improvement.
        assert_eq!(percent_change(0.0, 5.0), None);
        assert_eq!(percent_change(100.0, 110.0), Some(10.0));
        assert_eq!(percent_change(100.0, 90.0), Some(-10.0));
    }

    #[test]
    fn percent_change_handles_negative_baselines_by_magnitude() {
        assert_eq!(percent_change(-100.0, -90.0), Some(10.0));
    }

    #[test]
    fn numbers_render_without_trailing_noise() {
        assert_eq!(format_number(41.0), "41");
        assert_eq!(format_number(4720.3), "4720.30");
    }

    #[test]
    fn events_carry_a_health_subject_not_a_run_id() {
        let event = HealthEvent {
            health_snapshot_id: HealthSnapshotId::sequential(12),
            seq: 1,
            timestamp: Utc::now(),
            payload: HealthEventPayload::HealthBuildStarted {
                repository: "forge".into(),
                commit: commit('a'),
                world_model_snapshot_id: WorldModelSnapshotId::sequential(11),
            },
        };
        assert_eq!(event.payload.event_type(), "HealthBuildStarted");
        assert_eq!(event.health_snapshot_id.as_str(), "H-0012");

        let json = serde_json::to_string(&event).unwrap();
        assert_eq!(serde_json::from_str::<HealthEvent>(&json).unwrap(), event);
    }
}
