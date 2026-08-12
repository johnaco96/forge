//! Turning persisted Forge evidence into an immutable health snapshot.
//!
//! The builder collects, normalizes, and attributes. It does not interpret:
//! nothing here decides whether the repository is improving, and no trend is
//! computed. That belongs to the analyzer, on the other side of the immutable
//! snapshot.
//!
//! # The commit-binding rule
//!
//! A run record stores `base_commit`, but its evaluators ran against the
//! workspace *after* the agent's patch was applied. Attaching a benchmark
//! result to `base_commit` would credit every measurement to the commit before
//! the one it describes. So every piece of evidence is resolved through
//! [`MeasuredRepositoryState`] first, and evidence whose state cannot be named
//! is excluded with a reason rather than guessed at.
//!
//! # Point-in-time versus window
//!
//! Structural facts (dependency and interface counts) and per-commit
//! measurements (build durations, benchmark values) are point-in-time: they are
//! taken only from evidence measured at exactly the target commit.
//!
//! Rates (test reliability, failure frequency) are window measurements. Their
//! window is bounded by ancestry — evidence measured at the target commit or an
//! ancestor of it — so a snapshot can never be contaminated by a descendant
//! commit that did not exist when the state it describes did.

use std::collections::BTreeMap;

use chrono::Utc;
use forge_core::health::{
    HEALTH_BUILDER_VERSION, HEALTH_SCHEMA_VERSION, HealthDimension, HealthDimensionKind,
    HealthEvent, HealthEventPayload, HealthEvidence, HealthMeasurement, HealthProvenance,
    MeasuredRepositoryState, MeasurementIdentity, ObservationScope, RepositoryHealthSnapshot,
};
use forge_core::ids::{HealthSnapshotId, RunId};
use forge_core::result::{
    CheckResult, Direction, EvaluatorExecutionStatus, EvaluatorKind, Metric, Verdict,
};
use forge_core::world::{
    InterfaceVisibility, SnapshotRelation, WorldModelSnapshot, WorldModelSnapshotStatus,
};
use forge_store::HealthRunEvidence;
use sha2::{Digest, Sha256};

use crate::error::{HealthBuildError, HealthBuildResult};

/// Units that mark a structured metric as a memory measurement.
///
/// Memory has no dedicated evaluator kind, so it is identified by unit. This is
/// a stated convention rather than an inference: an evaluator that reports
/// bytes is reporting memory, and one that does not is not guessed at.
const BYTE_UNITS: &[&str] = &["B", "KB", "MB", "GB", "KiB", "MiB", "GiB", "bytes"];

/// Metric names that carry duplication evidence, when an evaluator emits them.
const DUPLICATION_METRICS: &[&str] = &["duplicate_percentage", "duplicate_blocks"];

/// How the builder decides whether one commit precedes another.
///
/// Abstracted so window collection can be tested without a Git repository, and
/// so the rule "never consume descendant evidence" is enforced by something
/// explicit rather than by timestamps.
pub trait CommitAncestry {
    fn relation(&self, candidate: &str, target: &str) -> SnapshotRelation;
}

/// Ancestry answered by Git itself.
pub struct GitAncestry<'a> {
    pub repository: &'a forge_git::Repository,
}

impl CommitAncestry for GitAncestry<'_> {
    fn relation(&self, candidate: &str, target: &str) -> SnapshotRelation {
        forge_world::snapshot_relation(self.repository, candidate, target)
    }
}

/// Evidence the builder declined to use, and why.
#[derive(Debug, Clone, PartialEq)]
pub struct ExcludedEvidence {
    pub run_id: RunId,
    pub reason: String,
}

/// What a build produced.
#[derive(Debug, Clone, PartialEq)]
pub struct HealthBuildReport {
    pub snapshot: RepositoryHealthSnapshot,
    pub events: Vec<HealthEvent>,
    /// Evidence excluded for want of a nameable measured state.
    pub excluded: Vec<ExcludedEvidence>,
}

/// Builds immutable, commit-bound repository health snapshots.
#[derive(Debug, Clone, Default)]
pub struct RepositoryHealthBuilder;

impl RepositoryHealthBuilder {
    pub fn new() -> Self {
        Self
    }

    /// Constructs a health snapshot for the exact commit the world model
    /// describes.
    ///
    /// Fails rather than substituting an inexact world model: a snapshot that
    /// silently used an ancestor's structure would be a claim about a commit
    /// nobody measured.
    pub fn build(
        &self,
        health_snapshot_id: HealthSnapshotId,
        repository: &str,
        commit: &str,
        world_model: &WorldModelSnapshot,
        evidence: &[HealthRunEvidence],
        ancestry: &dyn CommitAncestry,
    ) -> HealthBuildResult<HealthBuildReport> {
        if world_model.commit != commit {
            return Err(HealthBuildError::WorldModelNotExact {
                requested: commit.to_string(),
                found: world_model.commit.clone(),
            });
        }
        if world_model.status == WorldModelSnapshotStatus::Failed {
            return Err(HealthBuildError::WorldModelFailed {
                snapshot_id: world_model.snapshot_id.to_string(),
            });
        }

        let mut events = Vec::new();
        let emit = |payload: HealthEventPayload, events: &mut Vec<HealthEvent>| {
            events.push(HealthEvent {
                health_snapshot_id: health_snapshot_id.clone(),
                seq: events.len() as u64 + 1,
                timestamp: Utc::now(),
                payload,
            });
        };

        emit(
            HealthEventPayload::HealthBuildStarted {
                repository: repository.to_string(),
                commit: commit.to_string(),
                world_model_snapshot_id: world_model.snapshot_id.clone(),
            },
            &mut events,
        );

        // Resolve what each run's evidence actually measured, once.
        let mut excluded = Vec::new();
        let mut resolved: Vec<ResolvedEvidence<'_>> = Vec::new();
        for run in evidence {
            let Some(evaluation) = &run.evaluation else {
                // A run with no independent evaluation contributes no
                // measurement. It may still count toward failure frequency,
                // which is handled from the run record itself.
                continue;
            };
            let state = MeasuredRepositoryState::for_run(
                &run.base_commit,
                run.patch.as_ref().map(|patch| patch.as_state()),
            );
            match state.commit() {
                Some(measured) => resolved.push(ResolvedEvidence {
                    run,
                    evaluation_checks: &evaluation.checks,
                    measured_commit: measured.to_string(),
                }),
                None => excluded.push(ExcludedEvidence {
                    run_id: run.run_id.clone(),
                    reason: state.reason().unwrap_or("unknown").to_string(),
                }),
            }
        }

        // Point-in-time evidence: measured at exactly this commit.
        let at_commit: Vec<&ResolvedEvidence<'_>> = resolved
            .iter()
            .filter(|item| item.measured_commit == commit)
            .collect();

        // Window evidence: measured at this commit or an ancestor of it. Never
        // a descendant — that would be evidence from a future the snapshot's
        // commit had not reached.
        let in_window: Vec<&ResolvedEvidence<'_>> = resolved
            .iter()
            .filter(|item| {
                matches!(
                    ancestry.relation(&item.measured_commit, commit),
                    SnapshotRelation::Exact | SnapshotRelation::Ancestor
                )
            })
            .collect();

        let mut dimensions = Vec::new();
        for dimension in [
            self.test_reliability(commit, &in_window),
            self.complexity(commit, &at_commit),
            self.dependency_count(commit, world_model),
            self.build_time(commit, &at_commit),
            self.runtime_performance(commit, &at_commit),
            self.memory(commit, &at_commit),
            self.security(commit, &at_commit),
            self.duplication(commit, &at_commit),
            self.api_stability(commit, world_model),
            self.failure_frequency(commit, &in_window),
            self.regression_frequency(),
        ] {
            emit(
                HealthEventPayload::HealthDimensionCollected {
                    dimension: dimension.kind,
                    status: dimension.status,
                    measurements: dimension.measurements.len() as u64,
                },
                &mut events,
            );
            dimensions.push(dimension);
        }

        let status = RepositoryHealthSnapshot::derive_status(&dimensions, world_model.status);
        let snapshot = RepositoryHealthSnapshot {
            health_snapshot_id: health_snapshot_id.clone(),
            repository: repository.to_string(),
            commit: commit.to_string(),
            world_model_snapshot_id: world_model.snapshot_id.clone(),
            created_at: Utc::now(),
            schema_version: HEALTH_SCHEMA_VERSION.to_string(),
            status,
            dimensions,
            provenance: HealthProvenance {
                builder_version: HEALTH_BUILDER_VERSION.to_string(),
                world_model_snapshot_id: world_model.snapshot_id.clone(),
                world_model_status: world_model.status,
                window_start: in_window
                    .iter()
                    .filter_map(|item| item.run.created_at.into())
                    .min(),
                runs_considered: in_window.len() as u64,
            },
        };

        snapshot
            .validate()
            .map_err(|error| HealthBuildError::InvalidSnapshot(error.to_string()))?;

        emit(
            HealthEventPayload::HealthBuildCompleted {
                status: snapshot.status,
                dimensions_available: snapshot.available_dimensions() as u64,
            },
            &mut events,
        );

        Ok(HealthBuildReport {
            snapshot,
            events,
            excluded,
        })
    }

    // ------------------------------------------------------------- window

    /// Pass rate of required test evaluators over comparable runs.
    ///
    /// Evaluators whose execution errored are excluded: a test command Forge
    /// could not run says nothing about the repository, and counting it as a
    /// failure would turn infrastructure trouble into an engineering
    /// regression.
    fn test_reliability(&self, commit: &str, window: &[&ResolvedEvidence<'_>]) -> HealthDimension {
        let mut passed = 0u64;
        let mut total = 0u64;
        let mut evidence = Vec::new();

        for item in window {
            for check in item.evaluation_checks {
                if check.kind != EvaluatorKind::Test || !check.required {
                    continue;
                }
                if check.execution_status == EvaluatorExecutionStatus::Error {
                    continue;
                }
                total += 1;
                if check.verdict == Verdict::Pass {
                    passed += 1;
                }
                evidence.push(HealthEvidence::Run {
                    run_id: item.run.run_id.clone(),
                });
            }
        }

        if total == 0 {
            return HealthDimension::unavailable(
                HealthDimensionKind::TestReliability,
                "no comparable required test evaluations in the observation window",
            );
        }

        let scope = ObservationScope::window(commit, Utc::now(), total);
        let measurements = vec![
            HealthMeasurement::new(
                MeasurementIdentity::new(
                    "test_pass_rate",
                    Direction::HigherIsBetter,
                    "evaluation-history",
                ),
                passed as f64 / total as f64,
                scope.clone(),
            )
            .with_evidence(evidence.clone()),
            HealthMeasurement::new(
                MeasurementIdentity::new(
                    "test_evaluations_passed",
                    Direction::HigherIsBetter,
                    "evaluation-history",
                ),
                passed as f64,
                scope,
            )
            .with_evidence(evidence),
        ];
        HealthDimension::available(HealthDimensionKind::TestReliability, measurements)
    }

    /// Share of comparable runs whose outcome was a measured failure.
    ///
    /// `Errored` runs are excluded from the denominator entirely: Forge failing
    /// to carry a run through its pipeline is not evidence about the
    /// repository.
    fn failure_frequency(&self, commit: &str, window: &[&ResolvedEvidence<'_>]) -> HealthDimension {
        use forge_core::run::RunOutcome;

        let mut failed = 0u64;
        let mut inconclusive = 0u64;
        let mut total = 0u64;
        let mut evidence = Vec::new();

        for item in window {
            match item.run.outcome {
                Some(RunOutcome::Errored) | None => continue,
                Some(outcome) => {
                    total += 1;
                    if outcome == RunOutcome::Failed {
                        failed += 1;
                    }
                    if outcome == RunOutcome::Inconclusive {
                        inconclusive += 1;
                    }
                    evidence.push(HealthEvidence::Run {
                        run_id: item.run.run_id.clone(),
                    });
                }
            }
        }

        if total == 0 {
            return HealthDimension::unavailable(
                HealthDimensionKind::FailureFrequency,
                "no comparable engineering outcomes in the observation window",
            );
        }

        let scope = ObservationScope::window(commit, Utc::now(), total);
        HealthDimension::available(
            HealthDimensionKind::FailureFrequency,
            vec![
                HealthMeasurement::new(
                    MeasurementIdentity::new(
                        "run_failure_rate",
                        Direction::LowerIsBetter,
                        "run-history",
                    ),
                    failed as f64 / total as f64,
                    scope.clone(),
                )
                .with_evidence(evidence.clone()),
                HealthMeasurement::new(
                    MeasurementIdentity::new(
                        "run_inconclusive_rate",
                        Direction::LowerIsBetter,
                        "run-history",
                    ),
                    inconclusive as f64 / total as f64,
                    scope,
                )
                .with_evidence(evidence),
            ],
        )
    }

    /// Agent-created regressions need paired before/after evidence, which only
    /// exists once two snapshots are compared.
    ///
    /// Populating this from a single snapshot would mean inferring causality
    /// from one observation, which the attribution model exists to prevent.
    fn regression_frequency(&self) -> HealthDimension {
        HealthDimension::unavailable(
            HealthDimensionKind::RegressionFrequency,
            "requires paired before/after measurements; computed by the analyzer at diff time",
        )
    }

    // ------------------------------------------------------- point in time

    fn complexity(&self, commit: &str, at_commit: &[&ResolvedEvidence<'_>]) -> HealthDimension {
        let measurements = metrics_from(at_commit, commit, |check| {
            check.kind == EvaluatorKind::Complexity
        });
        if measurements.is_empty() {
            return HealthDimension::unavailable(
                HealthDimensionKind::Complexity,
                "no complexity evaluator reported structured metrics at this commit",
            );
        }
        HealthDimension::available(HealthDimensionKind::Complexity, measurements)
    }

    /// Dependency edges from the exact world model.
    ///
    /// Internal and external are not split: the Phase 6 dependency fact records
    /// a relationship kind, not an internal/external distinction, and inventing
    /// one from entity names would be a guess.
    fn dependency_count(&self, commit: &str, world: &WorldModelSnapshot) -> HealthDimension {
        let dependencies = &world.facts.dependencies;
        if dependencies.is_empty() && world.facts.components.is_empty() {
            return HealthDimension::unavailable(
                HealthDimensionKind::DependencyCount,
                "the world model records no dependency facts",
            );
        }

        let evidence: Vec<HealthEvidence> = dependencies
            .iter()
            .map(|dependency| HealthEvidence::WorldModelFact {
                snapshot_id: world.snapshot_id.clone(),
                fact_id: dependency.metadata.id.clone(),
            })
            .collect();

        let mut by_kind: BTreeMap<&'static str, u64> = BTreeMap::new();
        for dependency in dependencies {
            *by_kind
                .entry(dependency_kind_name(dependency.dependency_kind))
                .or_default() += 1;
        }

        let mut measurements = vec![
            HealthMeasurement::new(
                MeasurementIdentity::new("dependency_count", Direction::Neutral, "world-model"),
                dependencies.len() as f64,
                ObservationScope::point(commit),
            )
            .with_evidence(evidence),
        ];
        for (kind, count) in by_kind {
            measurements.push(HealthMeasurement::new(
                MeasurementIdentity::new(
                    format!("dependency_count.{kind}"),
                    Direction::Neutral,
                    "world-model",
                ),
                count as f64,
                ObservationScope::point(commit),
            ));
        }
        HealthDimension::available(HealthDimensionKind::DependencyCount, measurements)
    }

    /// Durations of build, test, and lint evaluators.
    ///
    /// Each duration's identity carries a fingerprint derived from the exact
    /// command, so `cargo test --lib` and `cargo test --workspace` never join
    /// the same series however alike their names look.
    fn build_time(&self, commit: &str, at_commit: &[&ResolvedEvidence<'_>]) -> HealthDimension {
        let mut measurements = Vec::new();
        for item in at_commit {
            for check in item.evaluation_checks {
                if !matches!(
                    check.kind,
                    EvaluatorKind::Build | EvaluatorKind::Test | EvaluatorKind::Lint
                ) {
                    continue;
                }
                if check.execution_status == EvaluatorExecutionStatus::Error {
                    continue;
                }
                measurements.push(
                    HealthMeasurement::new(
                        MeasurementIdentity::new(
                            format!("{}_duration_ms", check.kind.as_str()),
                            Direction::LowerIsBetter,
                            format!("evaluator:{}", check.name),
                        )
                        .with_unit("ms")
                        .with_fingerprint(evaluator_fingerprint(check)),
                        check.duration_ms as f64,
                        ObservationScope::point(commit),
                    )
                    .with_evidence(vec![HealthEvidence::Run {
                        run_id: item.run.run_id.clone(),
                    }]),
                );
            }
        }

        if measurements.is_empty() {
            return HealthDimension::unavailable(
                HealthDimensionKind::BuildTime,
                "no comparable build, test, or lint durations at this commit",
            );
        }
        HealthDimension::available(HealthDimensionKind::BuildTime, measurements)
    }

    /// Structured benchmark metrics only. Nothing is scraped from output.
    fn runtime_performance(
        &self,
        commit: &str,
        at_commit: &[&ResolvedEvidence<'_>],
    ) -> HealthDimension {
        let measurements: Vec<HealthMeasurement> = metrics_from(at_commit, commit, |check| {
            check.kind == EvaluatorKind::Benchmark
        })
        .into_iter()
        .filter(|measurement| !is_byte_unit(measurement.identity.unit.as_deref()))
        .collect();

        if measurements.is_empty() {
            return HealthDimension::unavailable(
                HealthDimensionKind::RuntimePerformance,
                "no structured benchmark metrics at this commit",
            );
        }
        HealthDimension::available(HealthDimensionKind::RuntimePerformance, measurements)
    }

    /// Structured metrics reported in byte units.
    fn memory(&self, commit: &str, at_commit: &[&ResolvedEvidence<'_>]) -> HealthDimension {
        let measurements: Vec<HealthMeasurement> = metrics_from(at_commit, commit, |_| true)
            .into_iter()
            .filter(|measurement| is_byte_unit(measurement.identity.unit.as_deref()))
            .collect();

        if measurements.is_empty() {
            return HealthDimension::unavailable(
                HealthDimensionKind::Memory,
                "no evaluator reported a metric in byte units at this commit",
            );
        }
        HealthDimension::available(HealthDimensionKind::Memory, measurements)
    }

    /// Security evaluator outcomes and any structured counts they emit.
    ///
    /// No universal severity score is invented: a scanner's own findings stay
    /// its own, and only the pass/fail Forge observed is normalized.
    fn security(&self, commit: &str, at_commit: &[&ResolvedEvidence<'_>]) -> HealthDimension {
        let mut measurements = Vec::new();
        for item in at_commit {
            for check in item.evaluation_checks {
                if check.kind != EvaluatorKind::Security
                    || check.execution_status == EvaluatorExecutionStatus::Error
                {
                    continue;
                }
                measurements.push(
                    HealthMeasurement::new(
                        MeasurementIdentity::new(
                            "security_evaluator_passed",
                            Direction::HigherIsBetter,
                            format!("evaluator:{}", check.name),
                        )
                        .with_fingerprint(evaluator_fingerprint(check)),
                        f64::from(u8::from(check.verdict == Verdict::Pass)),
                        ObservationScope::point(commit),
                    )
                    .with_evidence(vec![HealthEvidence::Run {
                        run_id: item.run.run_id.clone(),
                    }]),
                );
                measurements.extend(metrics_to_measurements(
                    &check.metrics,
                    commit,
                    &item.run.run_id,
                    check,
                ));
            }
        }

        if measurements.is_empty() {
            return HealthDimension::unavailable(
                HealthDimensionKind::Security,
                "no security evaluator ran at this commit",
            );
        }
        HealthDimension::available(HealthDimensionKind::Security, measurements)
    }

    /// Duplication only when an evaluator actually emits it.
    fn duplication(&self, commit: &str, at_commit: &[&ResolvedEvidence<'_>]) -> HealthDimension {
        let measurements: Vec<HealthMeasurement> = metrics_from(at_commit, commit, |_| true)
            .into_iter()
            .filter(|measurement| {
                DUPLICATION_METRICS.contains(&measurement.identity.metric.as_str())
            })
            .collect();

        if measurements.is_empty() {
            return HealthDimension::unavailable(
                HealthDimensionKind::Duplication,
                "no evaluator emits structured duplication metrics",
            );
        }
        HealthDimension::available(HealthDimensionKind::Duplication, measurements)
    }

    /// Interface counts from the exact world model.
    ///
    /// Counts only. Whether an interface change is a regression needs a
    /// contract or compatibility rule, and is decided at diff time.
    fn api_stability(&self, commit: &str, world: &WorldModelSnapshot) -> HealthDimension {
        let interfaces = &world.facts.interfaces;
        if interfaces.is_empty() {
            return HealthDimension::unavailable(
                HealthDimensionKind::ApiStability,
                "the world model records no interface facts",
            );
        }

        let public = interfaces
            .iter()
            .filter(|interface| interface.visibility == InterfaceVisibility::Public)
            .count();

        let evidence: Vec<HealthEvidence> = interfaces
            .iter()
            .map(|interface| HealthEvidence::WorldModelFact {
                snapshot_id: world.snapshot_id.clone(),
                fact_id: interface.metadata.id.clone(),
            })
            .collect();

        HealthDimension::available(
            HealthDimensionKind::ApiStability,
            vec![
                HealthMeasurement::new(
                    MeasurementIdentity::new("interface_count", Direction::Neutral, "world-model"),
                    interfaces.len() as f64,
                    ObservationScope::point(commit),
                )
                .with_evidence(evidence),
                HealthMeasurement::new(
                    MeasurementIdentity::new(
                        "public_interface_count",
                        Direction::Neutral,
                        "world-model",
                    ),
                    public as f64,
                    ObservationScope::point(commit),
                ),
            ],
        )
    }
}

/// One run's evidence with its measured state resolved.
struct ResolvedEvidence<'a> {
    run: &'a HealthRunEvidence,
    evaluation_checks: &'a [CheckResult],
    measured_commit: String,
}

fn metrics_from(
    evidence: &[&ResolvedEvidence<'_>],
    commit: &str,
    accept: impl Fn(&CheckResult) -> bool,
) -> Vec<HealthMeasurement> {
    let mut measurements = Vec::new();
    for item in evidence {
        for check in item.evaluation_checks {
            if !accept(check) || check.execution_status == EvaluatorExecutionStatus::Error {
                continue;
            }
            measurements.extend(metrics_to_measurements(
                &check.metrics,
                commit,
                &item.run.run_id,
                check,
            ));
        }
    }
    measurements
}

fn metrics_to_measurements(
    metrics: &[Metric],
    commit: &str,
    run_id: &RunId,
    check: &CheckResult,
) -> Vec<HealthMeasurement> {
    metrics
        .iter()
        .filter(|metric| metric.value.is_finite())
        .map(|metric| {
            let mut identity = MeasurementIdentity::new(
                metric.name.clone(),
                metric.direction,
                format!("evaluator:{}", check.name),
            )
            .with_fingerprint(evaluator_fingerprint(check));
            if let Some(unit) = &metric.unit {
                identity = identity.with_unit(unit.clone());
            }
            HealthMeasurement::new(identity, metric.value, ObservationScope::point(commit))
                .with_evidence(vec![HealthEvidence::Metric {
                    run_id: run_id.clone(),
                    metric: metric.name.clone(),
                }])
        })
        .collect()
}

/// Identity of the producing configuration.
///
/// Includes the exact command, because the command *is* the configuration for
/// a command-backed evaluator, and two different commands are two different
/// measurements.
fn evaluator_fingerprint(check: &CheckResult) -> String {
    let mut digest = Sha256::new();
    digest.update(check.name.as_bytes());
    digest.update([0]);
    digest.update(check.kind.as_str().as_bytes());
    digest.update([0]);
    digest.update(check.command.as_deref().unwrap_or("").as_bytes());
    format!("{:x}", digest.finalize())[..16].to_string()
}

fn is_byte_unit(unit: Option<&str>) -> bool {
    unit.is_some_and(|unit| BYTE_UNITS.contains(&unit))
}

fn dependency_kind_name(kind: forge_core::world::DependencyKind) -> &'static str {
    use forge_core::world::DependencyKind::*;
    match kind {
        Imports => "imports",
        Calls => "calls",
        Implements => "implements",
        Reads => "reads",
        Writes => "writes",
        PublishesTo => "publishes_to",
        SubscribesTo => "subscribes_to",
        DependsOn => "depends_on",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_core::health::DimensionStatus;
    use forge_core::ids::WorldModelSnapshotId;
    use forge_core::result::Evaluation;
    use forge_core::run::{RunOutcome, RunStatus};
    use forge_core::world::{
        ExtractorIdentity, ExtractorRecord, ExtractorStatus, WorldModelFacts,
        WorldModelSnapshotSource,
    };
    use forge_store::PatchFacts;

    fn commit(seed: char) -> String {
        std::iter::repeat_n(seed, 40).collect()
    }

    /// Ancestry declared explicitly, so window rules are tested without Git.
    struct FakeAncestry {
        /// (candidate, target) pairs where candidate is an ancestor of target.
        ancestors: Vec<(String, String)>,
    }

    impl CommitAncestry for FakeAncestry {
        fn relation(&self, candidate: &str, target: &str) -> SnapshotRelation {
            if candidate == target {
                return SnapshotRelation::Exact;
            }
            if self
                .ancestors
                .iter()
                .any(|(a, t)| a == candidate && t == target)
            {
                SnapshotRelation::Ancestor
            } else {
                SnapshotRelation::Stale
            }
        }
    }

    fn world_model(commit_hash: &str, status: WorldModelSnapshotStatus) -> WorldModelSnapshot {
        WorldModelSnapshot {
            snapshot_id: WorldModelSnapshotId::sequential(1),
            repository: "forge".into(),
            commit: commit_hash.to_string(),
            created_at: Utc::now(),
            source: WorldModelSnapshotSource::Deterministic,
            schema_version: forge_core::world::WORLD_MODEL_SCHEMA_VERSION.into(),
            status,
            extractors: vec![ExtractorRecord {
                identity: ExtractorIdentity::new("test-extractor", "1"),
                required: status != WorldModelSnapshotStatus::Partial,
                status: if status == WorldModelSnapshotStatus::Partial {
                    ExtractorStatus::Failed
                } else {
                    ExtractorStatus::Completed
                },
                facts_produced: 0,
                configuration_fingerprint: "fp".into(),
                error: None,
            }],
            facts: WorldModelFacts::default(),
        }
    }

    fn check(
        name: &str,
        kind: EvaluatorKind,
        verdict: Verdict,
        metrics: Vec<Metric>,
    ) -> CheckResult {
        CheckResult {
            name: name.into(),
            kind,
            required: true,
            verdict,
            execution_status: EvaluatorExecutionStatus::Completed,
            command: Some(format!("run {name}")),
            exit_code: Some(0),
            duration_ms: 120,
            detail: None,
            output_path: None,
            metrics,
            warnings: Vec::new(),
            execution_error: None,
        }
    }

    fn evidence(
        run: u64,
        base: &str,
        head: Option<&str>,
        empty: bool,
        checks: Vec<CheckResult>,
    ) -> HealthRunEvidence {
        let run_id = RunId::sequential(run);
        HealthRunEvidence {
            evaluation: Some(Evaluation::from_checks(
                run_id.clone(),
                checks,
                Utc::now(),
                Utc::now(),
            )),
            run_id,
            base_commit: base.to_string(),
            patch: Some(PatchFacts {
                head_commit: head.map(str::to_string),
                is_empty: empty,
            }),
            status: RunStatus::Completed,
            outcome: Some(RunOutcome::Passed),
            agent_id: "claude".into(),
            config_fingerprint: "cfg".into(),
            created_at: Utc::now(),
            finished_at: Some(Utc::now()),
        }
    }

    fn build(
        commit_hash: &str,
        world: &WorldModelSnapshot,
        evidence: &[HealthRunEvidence],
        ancestry: &dyn CommitAncestry,
    ) -> HealthBuildResult<HealthBuildReport> {
        RepositoryHealthBuilder::new().build(
            HealthSnapshotId::sequential(1),
            "forge",
            commit_hash,
            world,
            evidence,
            ancestry,
        )
    }

    fn no_ancestors() -> FakeAncestry {
        FakeAncestry {
            ancestors: Vec::new(),
        }
    }

    // -------------------------------------------------- commit binding

    /// The correction this phase turns on: candidate evidence belongs to the
    /// head commit, never to the base the run started from.
    #[test]
    fn candidate_metrics_bind_to_the_head_commit_not_the_base() {
        let world = world_model(&commit('b'), WorldModelSnapshotStatus::Complete);
        let run = evidence(
            1,
            &commit('a'),
            Some(&commit('b')),
            false,
            vec![check(
                "benchmark",
                EvaluatorKind::Benchmark,
                Verdict::Pass,
                vec![Metric::new(
                    "throughput",
                    1000.0,
                    "benchmark",
                    Direction::HigherIsBetter,
                )],
            )],
        );

        // Health at the head commit sees the measurement.
        let report = build(
            &commit('b'),
            &world,
            std::slice::from_ref(&run),
            &no_ancestors(),
        )
        .unwrap();
        let performance = report
            .snapshot
            .dimension(HealthDimensionKind::RuntimePerformance)
            .unwrap();
        assert_eq!(performance.status, DimensionStatus::Available);
        assert_eq!(performance.measurements[0].value, 1000.0);

        // Health at the base commit does not.
        let base_world = world_model(&commit('a'), WorldModelSnapshotStatus::Complete);
        let base_report = build(&commit('a'), &base_world, &[run], &no_ancestors()).unwrap();
        assert_eq!(
            base_report
                .snapshot
                .dimension(HealthDimensionKind::RuntimePerformance)
                .unwrap()
                .status,
            DimensionStatus::Unavailable
        );
    }

    #[test]
    fn a_no_change_run_binds_its_evidence_to_the_base_commit() {
        // Nothing was applied, so the evaluated workspace really was the base.
        let world = world_model(&commit('a'), WorldModelSnapshotStatus::Complete);
        let run = evidence(
            1,
            &commit('a'),
            None,
            true,
            vec![check(
                "benchmark",
                EvaluatorKind::Benchmark,
                Verdict::Pass,
                vec![Metric::new(
                    "throughput",
                    900.0,
                    "benchmark",
                    Direction::HigherIsBetter,
                )],
            )],
        );

        let report = build(&commit('a'), &world, &[run], &no_ancestors()).unwrap();
        assert_eq!(
            report
                .snapshot
                .dimension(HealthDimensionKind::RuntimePerformance)
                .unwrap()
                .measurements[0]
                .value,
            900.0
        );
        assert!(report.excluded.is_empty());
    }

    #[test]
    fn evidence_whose_measured_state_is_unknown_is_excluded_with_a_reason() {
        let world = world_model(&commit('a'), WorldModelSnapshotStatus::Complete);
        // Changes that were never committed have no nameable commit.
        let run = evidence(
            1,
            &commit('a'),
            None,
            false,
            vec![check(
                "benchmark",
                EvaluatorKind::Benchmark,
                Verdict::Pass,
                vec![Metric::new(
                    "throughput",
                    999.0,
                    "benchmark",
                    Direction::HigherIsBetter,
                )],
            )],
        );

        let report = build(&commit('a'), &world, &[run], &no_ancestors()).unwrap();
        assert_eq!(report.excluded.len(), 1);
        assert!(report.excluded[0].reason.contains("never committed"));
        // And the number does not sneak into the base commit's health.
        assert_eq!(
            report
                .snapshot
                .dimension(HealthDimensionKind::RuntimePerformance)
                .unwrap()
                .status,
            DimensionStatus::Unavailable
        );
    }

    #[test]
    fn a_run_without_a_patch_record_is_excluded() {
        let world = world_model(&commit('a'), WorldModelSnapshotStatus::Complete);
        let mut run = evidence(1, &commit('a'), None, true, vec![]);
        run.patch = None;

        let report = build(&commit('a'), &world, &[run], &no_ancestors()).unwrap();
        assert_eq!(report.excluded.len(), 1);
        assert!(report.excluded[0].reason.contains("cannot be named"));
    }

    #[test]
    fn descendant_evidence_never_contaminates_an_earlier_snapshot() {
        // A run measured at C must not appear in health for its ancestor A.
        let world = world_model(&commit('a'), WorldModelSnapshotStatus::Complete);
        let future = evidence(
            1,
            &commit('b'),
            Some(&commit('c')),
            false,
            vec![check("tests", EvaluatorKind::Test, Verdict::Pass, vec![])],
        );

        // A is an ancestor of C, not the other way round.
        let ancestry = FakeAncestry {
            ancestors: vec![(commit('a'), commit('c'))],
        };
        let report = build(&commit('a'), &world, &[future], &ancestry).unwrap();

        assert_eq!(
            report
                .snapshot
                .dimension(HealthDimensionKind::TestReliability)
                .unwrap()
                .status,
            DimensionStatus::Unavailable
        );
    }

    #[test]
    fn ancestor_evidence_is_included_in_window_dimensions() {
        let world = world_model(&commit('c'), WorldModelSnapshotStatus::Complete);
        let earlier = evidence(
            1,
            &commit('a'),
            Some(&commit('b')),
            false,
            vec![check("tests", EvaluatorKind::Test, Verdict::Pass, vec![])],
        );
        let ancestry = FakeAncestry {
            ancestors: vec![(commit('b'), commit('c'))],
        };

        let report = build(&commit('c'), &world, &[earlier], &ancestry).unwrap();
        let reliability = report
            .snapshot
            .dimension(HealthDimensionKind::TestReliability)
            .unwrap();
        assert_eq!(reliability.status, DimensionStatus::Available);
        // Window measurements carry their denominator.
        assert_eq!(reliability.measurements[0].scope.observations(), Some(1));
    }

    // ------------------------------------------------ world model linkage

    #[test]
    fn an_inexact_world_model_is_refused_rather_than_substituted() {
        let world = world_model(&commit('a'), WorldModelSnapshotStatus::Complete);
        let error = build(&commit('b'), &world, &[], &no_ancestors()).unwrap_err();
        assert!(
            matches!(error, HealthBuildError::WorldModelNotExact { .. }),
            "{error}"
        );
        assert!(error.to_string().contains("forge world build"));
    }

    #[test]
    fn a_partial_world_model_makes_the_health_snapshot_partial() {
        let world = world_model(&commit('a'), WorldModelSnapshotStatus::Partial);
        let report = build(&commit('a'), &world, &[], &no_ancestors()).unwrap();
        assert_eq!(
            report.snapshot.status,
            forge_core::health::HealthSnapshotStatus::Partial
        );
        assert_eq!(
            report.snapshot.provenance.world_model_status,
            WorldModelSnapshotStatus::Partial
        );
    }

    #[test]
    fn the_snapshot_records_the_exact_world_model_it_used() {
        let world = world_model(&commit('a'), WorldModelSnapshotStatus::Complete);
        let report = build(&commit('a'), &world, &[], &no_ancestors()).unwrap();
        assert_eq!(report.snapshot.world_model_snapshot_id, world.snapshot_id);
        assert_eq!(
            report.snapshot.provenance.world_model_snapshot_id,
            world.snapshot_id
        );
        assert_eq!(
            report.snapshot.provenance.builder_version,
            HEALTH_BUILDER_VERSION
        );
    }

    // ------------------------------------------------------- dimensions

    #[test]
    fn an_infrastructure_error_is_excluded_from_test_reliability() {
        let world = world_model(&commit('b'), WorldModelSnapshotStatus::Complete);
        let mut broken = check("tests", EvaluatorKind::Test, Verdict::Inconclusive, vec![]);
        broken.execution_status = EvaluatorExecutionStatus::Error;

        let run = evidence(1, &commit('a'), Some(&commit('b')), false, vec![broken]);
        let report = build(&commit('b'), &world, &[run], &no_ancestors()).unwrap();

        // Forge failing to run a test says nothing about the repository.
        assert_eq!(
            report
                .snapshot
                .dimension(HealthDimensionKind::TestReliability)
                .unwrap()
                .status,
            DimensionStatus::Unavailable
        );
    }

    #[test]
    fn test_reliability_reports_its_numerator_and_denominator() {
        let world = world_model(&commit('b'), WorldModelSnapshotStatus::Complete);
        let run = evidence(
            1,
            &commit('a'),
            Some(&commit('b')),
            false,
            vec![
                check("tests", EvaluatorKind::Test, Verdict::Pass, vec![]),
                check("more-tests", EvaluatorKind::Test, Verdict::Fail, vec![]),
            ],
        );

        let report = build(&commit('b'), &world, &[run], &no_ancestors()).unwrap();
        let reliability = report
            .snapshot
            .dimension(HealthDimensionKind::TestReliability)
            .unwrap();
        assert_eq!(
            reliability.measurement("test_pass_rate").unwrap().value,
            0.5
        );
        assert_eq!(
            reliability
                .measurement("test_pass_rate")
                .unwrap()
                .scope
                .observations(),
            Some(2)
        );
    }

    #[test]
    fn differently_configured_evaluators_get_different_identities() {
        let world = world_model(&commit('b'), WorldModelSnapshotStatus::Complete);
        let mut narrow = check("tests", EvaluatorKind::Test, Verdict::Pass, vec![]);
        narrow.command = Some("cargo test --lib".into());
        let mut broad = check("tests", EvaluatorKind::Test, Verdict::Pass, vec![]);
        broad.command = Some("cargo test --workspace".into());

        let run = evidence(
            1,
            &commit('a'),
            Some(&commit('b')),
            false,
            vec![narrow, broad],
        );
        let report = build(&commit('b'), &world, &[run], &no_ancestors()).unwrap();

        let durations = report
            .snapshot
            .dimension(HealthDimensionKind::BuildTime)
            .unwrap();
        assert_eq!(durations.measurements.len(), 2);
        assert!(
            !durations.measurements[0]
                .identity
                .is_comparable_with(&durations.measurements[1].identity),
            "differently configured commands must not share a series"
        );
    }

    #[test]
    fn memory_is_identified_by_byte_units_and_kept_out_of_performance() {
        let world = world_model(&commit('b'), WorldModelSnapshotStatus::Complete);
        let mut peak = Metric::new("peak_rss", 844.0, "benchmark", Direction::LowerIsBetter);
        peak.unit = Some("MB".into());
        let throughput = Metric::new("throughput", 4.72, "benchmark", Direction::HigherIsBetter);

        let run = evidence(
            1,
            &commit('a'),
            Some(&commit('b')),
            false,
            vec![check(
                "benchmark",
                EvaluatorKind::Benchmark,
                Verdict::Pass,
                vec![peak, throughput],
            )],
        );
        let report = build(&commit('b'), &world, &[run], &no_ancestors()).unwrap();

        let memory = report
            .snapshot
            .dimension(HealthDimensionKind::Memory)
            .unwrap();
        assert_eq!(memory.measurements.len(), 1);
        assert_eq!(memory.measurements[0].identity.metric, "peak_rss");

        let performance = report
            .snapshot
            .dimension(HealthDimensionKind::RuntimePerformance)
            .unwrap();
        assert_eq!(performance.measurements.len(), 1);
        assert_eq!(performance.measurements[0].identity.metric, "throughput");
    }

    #[test]
    fn duplication_stays_unavailable_without_structured_evidence() {
        let world = world_model(&commit('a'), WorldModelSnapshotStatus::Complete);
        let report = build(&commit('a'), &world, &[], &no_ancestors()).unwrap();
        let duplication = report
            .snapshot
            .dimension(HealthDimensionKind::Duplication)
            .unwrap();
        assert_eq!(duplication.status, DimensionStatus::Unavailable);
        assert!(duplication.measurements.is_empty());
    }

    #[test]
    fn regression_frequency_is_not_populated_from_one_snapshot() {
        // Causality needs a pair; one observation cannot supply it.
        let world = world_model(&commit('a'), WorldModelSnapshotStatus::Complete);
        let report = build(&commit('a'), &world, &[], &no_ancestors()).unwrap();
        let regressions = report
            .snapshot
            .dimension(HealthDimensionKind::RegressionFrequency)
            .unwrap();
        assert_eq!(regressions.status, DimensionStatus::Unavailable);
        assert!(regressions.notes[0].contains("paired before/after"));
    }

    #[test]
    fn security_records_the_observed_verdict_without_inventing_a_score() {
        let world = world_model(&commit('b'), WorldModelSnapshotStatus::Complete);
        let run = evidence(
            1,
            &commit('a'),
            Some(&commit('b')),
            false,
            vec![check(
                "audit",
                EvaluatorKind::Security,
                Verdict::Pass,
                vec![],
            )],
        );
        let report = build(&commit('b'), &world, &[run], &no_ancestors()).unwrap();

        let security = report
            .snapshot
            .dimension(HealthDimensionKind::Security)
            .unwrap();
        assert_eq!(
            security
                .measurement("security_evaluator_passed")
                .unwrap()
                .value,
            1.0
        );
    }

    // ----------------------------------------------------------- events

    #[test]
    fn the_build_emits_lifecycle_events_subject_to_the_health_snapshot() {
        let world = world_model(&commit('a'), WorldModelSnapshotStatus::Complete);
        let report = build(&commit('a'), &world, &[], &no_ancestors()).unwrap();

        let types: Vec<&str> = report
            .events
            .iter()
            .map(|event| event.payload.event_type())
            .collect();
        assert_eq!(types.first(), Some(&"HealthBuildStarted"));
        assert_eq!(types.last(), Some(&"HealthBuildCompleted"));
        assert_eq!(
            types
                .iter()
                .filter(|t| **t == "HealthDimensionCollected")
                .count(),
            HealthDimensionKind::ALL.len()
        );
        // Subject is the health snapshot, never a run.
        assert!(
            report
                .events
                .iter()
                .all(|event| event.health_snapshot_id.as_str() == "H-0001")
        );
        // Sequence numbers are monotonic from 1.
        assert_eq!(report.events[0].seq, 1);
    }

    #[test]
    fn every_dimension_is_represented_even_when_unavailable() {
        let world = world_model(&commit('a'), WorldModelSnapshotStatus::Complete);
        let report = build(&commit('a'), &world, &[], &no_ancestors()).unwrap();
        assert_eq!(
            report.snapshot.dimensions.len(),
            HealthDimensionKind::ALL.len()
        );
        // Nothing was measurable, so nothing claims to be.
        assert_eq!(report.snapshot.available_dimensions(), 0);
        report.snapshot.validate().unwrap();
    }
}
