//! Interpreting health snapshots: diffs and trends.
//!
//! Strictly separated from measurement collection. The analyzer reads immutable
//! snapshots and never writes to them, so re-interpreting history under a new
//! algorithm can never corrupt the evidence that history was built from. That
//! separation is what will make Phase 8 possible without rewriting the past.
//!
//! Everything here is deterministic arithmetic over typed values. There is no
//! model, no LLM, and no statistical inference — the rules are simple enough to
//! state in a sentence each, and are stated below.

use std::collections::{BTreeMap, BTreeSet};

use forge_core::health::{
    AttributionLevel, ChangeClassification, ExecutionAttribution, HealthChange, HealthDimension,
    HealthDimensionKind, HealthMeasurement, HealthTrend, MIN_TREND_POINTS, MaterialityPolicy,
    MeasurementIdentity, RepositoryHealthDiff, RepositoryHealthSnapshot, RepositoryHealthTrends,
    TREND_ALGORITHM_VERSION, TrendDirection, TrendPoint, percent_change,
};
use forge_core::result::Direction;
use forge_core::world::SnapshotRelation;

/// Percentage movement below which a series is reported as `Stable`.
///
/// Applies only when no materiality threshold is configured for the metric.
/// Without some floor, a 0.01% wobble in a build time would be reported as a
/// degrading repository, which is exactly the noise this phase must not
/// manufacture.
pub const DEFAULT_TREND_EPSILON_PERCENT: f64 = 1.0;

/// Compares two health snapshots.
///
/// The classification rules, in full:
///
/// - Present in both, and the metric declares a direction: the sign of the
///   delta against that direction decides improvement or regression.
/// - Present in both, no declared direction: reported as a neutral change.
/// - Present only in the later snapshot: `NewlyAvailable`. Never an infinite
///   improvement — a benchmark that did not exist before has not improved.
/// - Present only in the earlier snapshot: `NoLongerAvailable`. A security scan
///   that stopped running is not a repository with no findings.
///
/// `relation` records how the commits relate. Diverged commits can be compared
/// structurally, and the caller is expected to say so rather than describing
/// the result as a chronology.
pub fn diff(
    from: &RepositoryHealthSnapshot,
    to: &RepositoryHealthSnapshot,
    relation: SnapshotRelation,
    materiality: &MaterialityPolicy,
) -> RepositoryHealthDiff {
    let before = from.measurements_by_key();
    let after = to.measurements_by_key();

    let mut improvements = Vec::new();
    let mut regressions = Vec::new();
    let mut neutral_changes = Vec::new();
    let mut newly_available = Vec::new();
    let mut no_longer_available = Vec::new();

    let keys: BTreeSet<&String> = before.keys().chain(after.keys()).collect();

    for key in keys {
        match (before.get(key), after.get(key)) {
            (Some((dimension, old)), Some((_, new))) => {
                let change = compare(dimension.kind, old, new, materiality);
                match change.classification {
                    ChangeClassification::Improvement => improvements.push(change),
                    ChangeClassification::Regression => regressions.push(change),
                    // Unchanged values are recorded as neutral so that "we
                    // measured it and it held steady" stays visible; silence
                    // would be indistinguishable from not measuring.
                    ChangeClassification::Neutral | ChangeClassification::Unchanged => {
                        neutral_changes.push(change)
                    }
                    _ => neutral_changes.push(change),
                }
            }
            (None, Some((dimension, new))) => newly_available.push(HealthChange {
                dimension: dimension.kind,
                identity: new.identity.clone(),
                from: None,
                to: Some(new.value),
                delta: None,
                percent_change: None,
                classification: ChangeClassification::NewlyAvailable,
                material: false,
            }),
            (Some((dimension, old)), None) => no_longer_available.push(HealthChange {
                dimension: dimension.kind,
                identity: old.identity.clone(),
                from: Some(old.value),
                to: None,
                delta: None,
                percent_change: None,
                classification: ChangeClassification::NoLongerAvailable,
                material: false,
            }),
            (None, None) => unreachable!("key came from one of the two maps"),
        }
    }

    RepositoryHealthDiff {
        from_snapshot_id: from.health_snapshot_id.clone(),
        to_snapshot_id: to.health_snapshot_id.clone(),
        from_commit: from.commit.clone(),
        to_commit: to.commit.clone(),
        relation,
        improvements,
        regressions,
        neutral_changes,
        newly_available,
        no_longer_available,
        attribution: Vec::new(),
        algorithm_version: TREND_ALGORITHM_VERSION.to_string(),
    }
}

fn compare(
    dimension: HealthDimensionKind,
    old: &HealthMeasurement,
    new: &HealthMeasurement,
    materiality: &MaterialityPolicy,
) -> HealthChange {
    let delta = new.value - old.value;
    let percent = percent_change(old.value, new.value);
    let classification = ChangeClassification::from_delta(new.identity.direction, delta);

    HealthChange {
        dimension,
        identity: new.identity.clone(),
        from: Some(old.value),
        to: Some(new.value),
        delta: Some(delta),
        percent_change: percent,
        classification,
        material: materiality.is_material(&new.identity.metric, percent),
    }
}

/// Attaches executions that may have contributed to a diff.
///
/// Attribution is deliberately conservative. Producing the later commit earns
/// `Associated` and nothing more: Forge produces many commits, and producing
/// one is not evidence of having caused a particular measurement to move.
/// `Supported` requires a before/after pair for the same measurement;
/// `Confirmed` additionally requires the movement to clear a declared
/// threshold. Temporal proximity alone never raises the level.
pub fn attribute(
    diff: &mut RepositoryHealthDiff,
    producing_executions: &[(forge_core::events::EvaluationSubject, String)],
) {
    let has_paired_regression = diff
        .regressions
        .iter()
        .any(|change| change.from.is_some() && change.to.is_some());
    let has_material_regression = diff.regressions.iter().any(|change| change.material);

    for (subject, commit) in producing_executions {
        // Only an execution that produced the *later* commit is a candidate.
        if *commit != diff.to_commit {
            continue;
        }
        let (level, rationale) = if has_material_regression {
            (
                AttributionLevel::Confirmed,
                "produced the compared commit; a paired measurement moved beyond its \
                 declared threshold"
                    .to_string(),
            )
        } else if has_paired_regression {
            (
                AttributionLevel::Supported,
                "produced the compared commit; a paired before/after measurement moved, \
                 with no threshold declaring it significant"
                    .to_string(),
            )
        } else {
            (
                AttributionLevel::Associated,
                "produced the compared commit; no paired measurement supports a causal claim"
                    .to_string(),
            )
        };
        diff.attribution.push(ExecutionAttribution {
            level,
            subject: subject.clone(),
            commit: commit.clone(),
            rationale,
        });
    }

    if diff.attribution.is_empty() {
        // A commit with no known Forge execution is ordinary. Humans and
        // external automation commit too, and the health record stays valid.
        diff.attribution.push(ExecutionAttribution {
            level: AttributionLevel::Unknown,
            subject: forge_core::events::EvaluationSubject::Run(
                forge_core::ids::RunId::sequential(0),
            ),
            commit: diff.to_commit.clone(),
            rationale: "no Forge execution is recorded as having produced this commit".to_string(),
        });
        // The placeholder subject would be misleading; drop it and leave the
        // list empty rather than inventing a run.
        diff.attribution.clear();
    }
}

/// Computes trends across a chronologically ordered series of snapshots.
///
/// The caller is responsible for supplying snapshots that form one ancestry
/// chain, oldest first; the analyzer will not silently mix branches.
///
/// The rule for one series, stated in full: take the first and last comparable
/// values, compute percentage change, and compare its magnitude against the
/// metric's materiality threshold (or [`DEFAULT_TREND_EPSILON_PERCENT`]).
/// Below the threshold is `Stable`. Above it, the metric's declared direction
/// decides `Improving` or `Degrading`; a metric with no declared direction is
/// `Changing`. Fewer than [`MIN_TREND_POINTS`] points is `InsufficientData`.
///
/// This is a net-change rule, not a regression fit. A series that rises and
/// falls back reads as `Stable`, which is honest but coarse; recording the
/// points alongside the direction lets a reader see the shape themselves.
pub fn trends(
    repository: &str,
    snapshots: &[RepositoryHealthSnapshot],
    materiality: &MaterialityPolicy,
) -> RepositoryHealthTrends {
    let mut series: BTreeMap<String, (MeasurementIdentity, HealthDimensionKind, Vec<TrendPoint>)> =
        BTreeMap::new();

    for snapshot in snapshots {
        for dimension in &snapshot.dimensions {
            for measurement in &dimension.measurements {
                let key = measurement.identity.comparability_key();
                let entry = series
                    .entry(key)
                    .or_insert_with(|| (measurement.identity.clone(), dimension.kind, Vec::new()));
                entry.2.push(TrendPoint {
                    health_snapshot_id: snapshot.health_snapshot_id.clone(),
                    commit: snapshot.commit.clone(),
                    observed_at: snapshot.created_at,
                    value: measurement.value,
                });
            }
        }
    }

    let mut trends: Vec<HealthTrend> = series
        .into_values()
        .map(|(identity, dimension, points)| {
            classify_series(identity, dimension, points, materiality)
        })
        .collect();
    trends.sort_by(|a, b| {
        a.dimension
            .as_str()
            .cmp(b.dimension.as_str())
            .then_with(|| a.identity.label().cmp(&b.identity.label()))
    });

    // Roll up per dimension, in the roadmap's report order.
    let mut dimensions = Vec::new();
    for kind in HealthDimensionKind::ALL {
        let directions: Vec<TrendDirection> = trends
            .iter()
            .filter(|trend| trend.dimension == kind)
            .map(|trend| trend.direction)
            .collect();
        if directions.is_empty() {
            continue;
        }
        dimensions.push((kind, TrendDirection::combine(&directions)));
    }

    let overall = TrendDirection::combine(
        &dimensions
            .iter()
            .map(|(_, direction)| *direction)
            .collect::<Vec<_>>(),
    );

    RepositoryHealthTrends {
        repository: repository.to_string(),
        trends,
        dimensions,
        overall,
        snapshots_considered: snapshots.len() as u64,
        window_start: snapshots.first().map(|s| s.created_at),
        window_end: snapshots.last().map(|s| s.created_at),
        algorithm_version: TREND_ALGORITHM_VERSION.to_string(),
    }
}

fn classify_series(
    identity: MeasurementIdentity,
    dimension: HealthDimensionKind,
    points: Vec<TrendPoint>,
    materiality: &MaterialityPolicy,
) -> HealthTrend {
    if points.len() < MIN_TREND_POINTS {
        return HealthTrend {
            dimension,
            direction: TrendDirection::InsufficientData,
            percent_change: None,
            evidence: format!(
                "{} comparable measurement{}; {MIN_TREND_POINTS} required",
                points.len(),
                if points.len() == 1 { "" } else { "s" }
            ),
            identity,
            points,
        };
    }

    let first = points.first().expect("checked above").value;
    let last = points.last().expect("checked above").value;
    let percent = percent_change(first, last);
    let threshold = materiality
        .threshold_for(&identity.metric)
        .unwrap_or(DEFAULT_TREND_EPSILON_PERCENT);

    let direction = match percent {
        Some(change) if change.abs() < threshold => TrendDirection::Stable,
        Some(change) => match identity.direction {
            Direction::Neutral => TrendDirection::Changing,
            Direction::HigherIsBetter if change > 0.0 => TrendDirection::Improving,
            Direction::HigherIsBetter => TrendDirection::Degrading,
            Direction::LowerIsBetter if change < 0.0 => TrendDirection::Improving,
            Direction::LowerIsBetter => TrendDirection::Degrading,
        },
        // A zero baseline makes percentage change undefined; fall back to the
        // raw movement rather than reporting nothing.
        None if last == first => TrendDirection::Stable,
        None => match identity.direction {
            Direction::Neutral => TrendDirection::Changing,
            Direction::HigherIsBetter if last > first => TrendDirection::Improving,
            Direction::HigherIsBetter => TrendDirection::Degrading,
            Direction::LowerIsBetter if last < first => TrendDirection::Improving,
            Direction::LowerIsBetter => TrendDirection::Degrading,
        },
    };

    let fingerprint = identity
        .fingerprint
        .as_ref()
        .map(|f| format!("; identical producer fingerprint {}", &f[..f.len().min(12)]))
        .unwrap_or_default();

    HealthTrend {
        dimension,
        direction,
        percent_change: percent,
        evidence: format!("{} comparable measurements{fingerprint}", points.len()),
        identity,
        points,
    }
}

/// Selects the baseline a snapshot should be compared against.
///
/// Prefers the most recent earlier snapshot on the same ancestry chain, which
/// is the only comparison that describes an evolution. `candidates` must
/// already be filtered to ancestors of `target`; snapshots on diverged branches
/// are excluded here rather than silently compared.
pub fn nearest_ancestor_baseline<'a>(
    target: &RepositoryHealthSnapshot,
    candidates: &'a [(RepositoryHealthSnapshot, SnapshotRelation)],
) -> Option<&'a RepositoryHealthSnapshot> {
    candidates
        .iter()
        .filter(|(snapshot, relation)| {
            *relation == SnapshotRelation::Ancestor
                && snapshot.created_at <= target.created_at
                && snapshot.health_snapshot_id != target.health_snapshot_id
        })
        .max_by_key(|(snapshot, _)| snapshot.created_at)
        .map(|(snapshot, _)| snapshot)
}

/// Dimension-level roll-up for a single diff, for the report surface.
pub fn dimension_summary(diff: &RepositoryHealthDiff) -> Vec<(HealthDimensionKind, String)> {
    let mut summary: BTreeMap<HealthDimensionKind, (u64, u64, u64)> = BTreeMap::new();
    for change in diff.all_changes() {
        let entry = summary.entry(change.dimension).or_insert((0, 0, 0));
        match change.classification {
            ChangeClassification::Improvement => entry.0 += 1,
            ChangeClassification::Regression => entry.1 += 1,
            _ => entry.2 += 1,
        }
    }
    summary
        .into_iter()
        .map(|(kind, (improved, regressed, other))| {
            let text = match (improved, regressed) {
                (0, 0) => format!("{other} unchanged"),
                (i, 0) => format!("{i} improvement{}", plural(i)),
                (0, r) => format!("{r} regression{}", plural(r)),
                (i, r) => format!("{i} improvement{}, {r} regression{}", plural(i), plural(r)),
            };
            (kind, text)
        })
        .collect()
}

fn plural(count: u64) -> &'static str {
    if count == 1 { "" } else { "s" }
}

/// Marks a dimension present in one snapshot and absent from another.
pub fn missing_dimensions(
    from: &RepositoryHealthSnapshot,
    to: &RepositoryHealthSnapshot,
) -> Vec<HealthDimensionKind> {
    let available = |snapshot: &RepositoryHealthSnapshot| -> BTreeSet<HealthDimensionKind> {
        snapshot
            .dimensions
            .iter()
            .filter(|dimension| dimension.is_available())
            .map(|dimension| dimension.kind)
            .collect()
    };
    available(from)
        .difference(&available(to))
        .copied()
        .collect()
}

/// Convenience for callers building report rows from a snapshot.
pub fn dimension_rows(snapshot: &RepositoryHealthSnapshot) -> Vec<(&'static str, String)> {
    let mut rows = Vec::new();
    for kind in HealthDimensionKind::ALL {
        let Some(dimension) = snapshot.dimension(kind) else {
            continue;
        };
        rows.push((kind.label(), describe_dimension(dimension)));
    }
    rows
}

fn describe_dimension(dimension: &HealthDimension) -> String {
    match dimension.status {
        forge_core::health::DimensionStatus::Unavailable => dimension
            .notes
            .first()
            .map(|note| format!("unavailable ({note})"))
            .unwrap_or_else(|| "unavailable".to_string()),
        status => format!(
            "{status} ({} measurement{})",
            dimension.measurements.len(),
            plural(dimension.measurements.len() as u64)
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeDelta, Utc};
    use forge_core::events::EvaluationSubject;
    use forge_core::health::{
        DimensionStatus, HEALTH_BUILDER_VERSION, HEALTH_SCHEMA_VERSION, HealthProvenance,
        HealthSnapshotStatus, ObservationScope,
    };
    use forge_core::ids::{HealthSnapshotId, RunId, WorldModelSnapshotId};
    use forge_core::world::WorldModelSnapshotStatus;

    fn commit(seed: char) -> String {
        std::iter::repeat_n(seed, 40).collect()
    }

    fn identity(metric: &str, direction: Direction) -> MeasurementIdentity {
        MeasurementIdentity::new(metric, direction, "test-evaluator")
    }

    fn measurement(
        identity: MeasurementIdentity,
        value: f64,
        commit_hash: &str,
    ) -> HealthMeasurement {
        HealthMeasurement::new(identity, value, ObservationScope::point(commit_hash))
    }

    fn snapshot(
        n: u64,
        commit_hash: &str,
        dimensions: Vec<HealthDimension>,
    ) -> RepositoryHealthSnapshot {
        RepositoryHealthSnapshot {
            health_snapshot_id: HealthSnapshotId::sequential(n),
            repository: "forge".into(),
            commit: commit_hash.to_string(),
            world_model_snapshot_id: WorldModelSnapshotId::sequential(n),
            // Ordered by construction so trend ordering is deterministic.
            created_at: Utc::now() + TimeDelta::try_seconds(n as i64).unwrap(),
            schema_version: HEALTH_SCHEMA_VERSION.into(),
            status: HealthSnapshotStatus::Complete,
            dimensions,
            provenance: HealthProvenance {
                builder_version: HEALTH_BUILDER_VERSION.into(),
                world_model_snapshot_id: WorldModelSnapshotId::sequential(n),
                world_model_status: WorldModelSnapshotStatus::Complete,
                window_start: None,
                runs_considered: 0,
            },
        }
    }

    fn build_time(n: u64, commit_hash: &str, seconds: f64) -> RepositoryHealthSnapshot {
        snapshot(
            n,
            commit_hash,
            vec![HealthDimension::available(
                HealthDimensionKind::BuildTime,
                vec![measurement(
                    identity("build_time", Direction::LowerIsBetter)
                        .with_unit("s")
                        .with_fingerprint("build-fingerprint-1"),
                    seconds,
                    commit_hash,
                )],
            )],
        )
    }

    // ---------------------------------------------------------------- diff

    #[test]
    fn a_metric_moving_its_declared_way_is_an_improvement() {
        let from = snapshot(
            1,
            &commit('a'),
            vec![HealthDimension::available(
                HealthDimensionKind::RuntimePerformance,
                vec![measurement(
                    identity("throughput", Direction::HigherIsBetter).with_unit("MB/s"),
                    4720.0,
                    &commit('a'),
                )],
            )],
        );
        let to = snapshot(
            2,
            &commit('b'),
            vec![HealthDimension::available(
                HealthDimensionKind::RuntimePerformance,
                vec![measurement(
                    identity("throughput", Direction::HigherIsBetter).with_unit("MB/s"),
                    4910.0,
                    &commit('b'),
                )],
            )],
        );

        let diff = diff(
            &from,
            &to,
            SnapshotRelation::Ancestor,
            &MaterialityPolicy::default(),
        );
        assert_eq!(diff.improvements.len(), 1);
        assert!(diff.regressions.is_empty());

        let change = &diff.improvements[0];
        assert_eq!(change.from, Some(4720.0));
        assert_eq!(change.to, Some(4910.0));
        assert!((change.percent_change.unwrap() - 4.025).abs() < 0.01);
        assert!(change.describe().contains("4720 MB/s → 4910 MB/s"));
    }

    #[test]
    fn a_metric_moving_against_its_direction_is_a_regression() {
        let diff = diff(
            &build_time(1, &commit('a'), 12.4),
            &build_time(2, &commit('b'), 13.2),
            SnapshotRelation::Ancestor,
            &MaterialityPolicy::default(),
        );
        assert_eq!(diff.regressions.len(), 1);
        let change = &diff.regressions[0];
        assert!((change.percent_change.unwrap() - 6.45).abs() < 0.01);
    }

    #[test]
    fn a_structural_count_changes_without_being_good_or_bad() {
        let make = |n: u64, c: &str, value: f64| {
            snapshot(
                n,
                c,
                vec![HealthDimension::available(
                    HealthDimensionKind::DependencyCount,
                    vec![measurement(
                        identity("dependency_count", Direction::Neutral),
                        value,
                        c,
                    )],
                )],
            )
        };
        let diff = diff(
            &make(1, &commit('a'), 41.0),
            &make(2, &commit('b'), 43.0),
            SnapshotRelation::Ancestor,
            &MaterialityPolicy::default(),
        );

        assert!(diff.improvements.is_empty());
        assert!(diff.regressions.is_empty());
        assert_eq!(diff.neutral_changes.len(), 1);
        assert_eq!(
            diff.neutral_changes[0].classification,
            ChangeClassification::Neutral
        );
        assert_eq!(diff.neutral_changes[0].delta, Some(2.0));
    }

    #[test]
    fn a_metric_that_did_not_exist_before_is_newly_available_not_infinite_improvement() {
        let from = snapshot(1, &commit('a'), vec![]);
        let to = snapshot(
            2,
            &commit('b'),
            vec![HealthDimension::available(
                HealthDimensionKind::RuntimePerformance,
                vec![measurement(
                    identity("throughput", Direction::HigherIsBetter),
                    1150.0,
                    &commit('b'),
                )],
            )],
        );

        let diff = diff(
            &from,
            &to,
            SnapshotRelation::Ancestor,
            &MaterialityPolicy::default(),
        );
        assert!(diff.improvements.is_empty());
        assert_eq!(diff.newly_available.len(), 1);
        assert_eq!(diff.newly_available[0].percent_change, None);
        assert_eq!(
            diff.newly_available[0].classification,
            ChangeClassification::NewlyAvailable
        );
    }

    #[test]
    fn a_scan_that_stopped_running_is_not_a_clean_result() {
        let from = snapshot(
            1,
            &commit('a'),
            vec![HealthDimension::available(
                HealthDimensionKind::Security,
                vec![measurement(
                    identity("security_findings", Direction::LowerIsBetter),
                    3.0,
                    &commit('a'),
                )],
            )],
        );
        let to = snapshot(2, &commit('b'), vec![]);

        let diff = diff(
            &from,
            &to,
            SnapshotRelation::Ancestor,
            &MaterialityPolicy::default(),
        );
        // Not an improvement from 3 findings to 0.
        assert!(diff.improvements.is_empty());
        assert_eq!(diff.no_longer_available.len(), 1);
        assert_eq!(diff.no_longer_available[0].from, Some(3.0));
        assert_eq!(diff.no_longer_available[0].to, None);
    }

    #[test]
    fn incomparable_measurements_are_not_merged_into_one_change() {
        // Same display name, different producer configuration.
        let from = snapshot(
            1,
            &commit('a'),
            vec![HealthDimension::available(
                HealthDimensionKind::BuildTime,
                vec![measurement(
                    identity("build_time", Direction::LowerIsBetter).with_fingerprint("lib-only"),
                    10.0,
                    &commit('a'),
                )],
            )],
        );
        let to = snapshot(
            2,
            &commit('b'),
            vec![HealthDimension::available(
                HealthDimensionKind::BuildTime,
                vec![measurement(
                    identity("build_time", Direction::LowerIsBetter).with_fingerprint("workspace"),
                    30.0,
                    &commit('b'),
                )],
            )],
        );

        let diff = diff(
            &from,
            &to,
            SnapshotRelation::Ancestor,
            &MaterialityPolicy::default(),
        );
        // Emphatically not a 200% build-time regression.
        assert!(diff.regressions.is_empty());
        assert_eq!(diff.newly_available.len(), 1);
        assert_eq!(diff.no_longer_available.len(), 1);
    }

    #[test]
    fn materiality_marks_only_changes_that_clear_the_bar() {
        let policy = MaterialityPolicy::default().with_metric("build_time", 5.0);

        let small = diff(
            &build_time(1, &commit('a'), 12.4),
            &build_time(2, &commit('b'), 12.5),
            SnapshotRelation::Ancestor,
            &policy,
        );
        assert!(!small.regressions[0].material);

        let large = diff(
            &build_time(1, &commit('a'), 12.4),
            &build_time(2, &commit('b'), 14.2),
            SnapshotRelation::Ancestor,
            &policy,
        );
        assert!(large.regressions[0].material);
    }

    #[test]
    fn a_diverged_comparison_is_not_described_as_a_chronology() {
        let diff = diff(
            &build_time(1, &commit('a'), 12.0),
            &build_time(2, &commit('b'), 13.0),
            SnapshotRelation::Stale,
            &MaterialityPolicy::default(),
        );
        assert!(!diff.is_chronological());

        let ancestral = diff_relation(SnapshotRelation::Ancestor);
        assert!(ancestral.is_chronological());
    }

    fn diff_relation(relation: SnapshotRelation) -> RepositoryHealthDiff {
        diff(
            &build_time(1, &commit('a'), 12.0),
            &build_time(2, &commit('b'), 13.0),
            relation,
            &MaterialityPolicy::default(),
        )
    }

    // --------------------------------------------------------- attribution

    #[test]
    fn producing_a_commit_alone_is_only_associated() {
        // No paired regression, so no causal claim is available.
        let mut diff = diff(
            &snapshot(1, &commit('a'), vec![]),
            &snapshot(2, &commit('b'), vec![]),
            SnapshotRelation::Ancestor,
            &MaterialityPolicy::default(),
        );
        attribute(
            &mut diff,
            &[(EvaluationSubject::Run(RunId::sequential(1004)), commit('b'))],
        );

        assert_eq!(diff.attribution.len(), 1);
        assert_eq!(diff.attribution[0].level, AttributionLevel::Associated);
        assert!(!diff.attribution[0].level.is_causal());
    }

    #[test]
    fn a_paired_regression_supports_attribution() {
        let mut diff = diff(
            &build_time(1, &commit('a'), 12.0),
            &build_time(2, &commit('b'), 13.0),
            SnapshotRelation::Ancestor,
            &MaterialityPolicy::default(),
        );
        attribute(
            &mut diff,
            &[(EvaluationSubject::Run(RunId::sequential(1004)), commit('b'))],
        );
        assert_eq!(diff.attribution[0].level, AttributionLevel::Supported);
    }

    #[test]
    fn a_material_regression_confirms_attribution() {
        let policy = MaterialityPolicy::default().with_metric("build_time", 5.0);
        let mut diff = diff(
            &build_time(1, &commit('a'), 12.0),
            &build_time(2, &commit('b'), 14.0),
            SnapshotRelation::Ancestor,
            &policy,
        );
        attribute(
            &mut diff,
            &[(
                EvaluationSubject::TeamExecution(forge_core::ids::TeamExecutionId::sequential(42)),
                commit('b'),
            )],
        );
        assert_eq!(diff.attribution[0].level, AttributionLevel::Confirmed);
        assert!(diff.attribution[0].level.is_causal());
    }

    #[test]
    fn a_human_commit_has_no_attribution_rather_than_an_invented_one() {
        let mut diff = diff(
            &build_time(1, &commit('a'), 12.0),
            &build_time(2, &commit('b'), 13.0),
            SnapshotRelation::Ancestor,
            &MaterialityPolicy::default(),
        );
        // No execution produced this commit.
        attribute(&mut diff, &[]);
        assert!(diff.attribution.is_empty());
    }

    #[test]
    fn an_execution_that_produced_another_commit_is_not_attributed() {
        let mut diff = diff(
            &build_time(1, &commit('a'), 12.0),
            &build_time(2, &commit('b'), 13.0),
            SnapshotRelation::Ancestor,
            &MaterialityPolicy::default(),
        );
        attribute(
            &mut diff,
            &[(EvaluationSubject::Run(RunId::sequential(7)), commit('c'))],
        );
        assert!(diff.attribution.is_empty());
    }

    // --------------------------------------------------------------- trend

    #[test]
    fn fewer_than_three_points_is_insufficient_data() {
        let series = vec![
            build_time(1, &commit('a'), 12.0),
            build_time(2, &commit('b'), 13.0),
        ];
        let trends = trends("forge", &series, &MaterialityPolicy::default());

        assert_eq!(trends.trends[0].direction, TrendDirection::InsufficientData);
        assert_eq!(trends.overall, TrendDirection::InsufficientData);
        assert!(trends.trends[0].evidence.contains("2 comparable"));
    }

    #[test]
    fn a_steadily_worsening_series_degrades_with_evidence() {
        let series = vec![
            build_time(1, &commit('a'), 12.4),
            build_time(2, &commit('b'), 12.8),
            build_time(3, &commit('c'), 13.7),
            build_time(4, &commit('d'), 14.2),
        ];
        let trends = trends("forge", &series, &MaterialityPolicy::default());

        let trend = &trends.trends[0];
        assert_eq!(trend.direction, TrendDirection::Degrading);
        assert_eq!(trend.points.len(), 4);
        assert!((trend.percent_change.unwrap() - 14.516).abs() < 0.01);
        assert!(trend.evidence.contains("4 comparable measurements"));
        assert!(trend.evidence.contains("fingerprint"));
        assert_eq!(trends.overall, TrendDirection::Degrading);
    }

    #[test]
    fn a_steadily_improving_series_improves() {
        let throughput = |n: u64, c: &str, value: f64| {
            snapshot(
                n,
                c,
                vec![HealthDimension::available(
                    HealthDimensionKind::RuntimePerformance,
                    vec![measurement(
                        identity("throughput", Direction::HigherIsBetter).with_unit("MB/s"),
                        value,
                        c,
                    )],
                )],
            )
        };
        let series = vec![
            throughput(1, &commit('a'), 1000.0),
            throughput(2, &commit('b'), 1150.0),
            throughput(3, &commit('c'), 1170.0),
        ];
        let trends = trends("forge", &series, &MaterialityPolicy::default());
        assert_eq!(trends.trends[0].direction, TrendDirection::Improving);
        assert_eq!(trends.overall, TrendDirection::Improving);
    }

    #[test]
    fn a_flat_series_is_stable_rather_than_noise() {
        let series = vec![
            build_time(1, &commit('a'), 12.40),
            build_time(2, &commit('b'), 12.41),
            build_time(3, &commit('c'), 12.42),
        ];
        let trends = trends("forge", &series, &MaterialityPolicy::default());
        assert_eq!(trends.trends[0].direction, TrendDirection::Stable);
    }

    #[test]
    fn a_growing_structural_count_is_changing_not_degrading() {
        let deps = |n: u64, c: &str, value: f64| {
            snapshot(
                n,
                c,
                vec![HealthDimension::available(
                    HealthDimensionKind::DependencyCount,
                    vec![measurement(
                        identity("dependency_count", Direction::Neutral),
                        value,
                        c,
                    )],
                )],
            )
        };
        let series = vec![
            deps(1, &commit('a'), 2.0),
            deps(2, &commit('b'), 3.0),
            deps(3, &commit('c'), 3.0),
        ];
        let trends = trends("forge", &series, &MaterialityPolicy::default());
        assert_eq!(trends.trends[0].direction, TrendDirection::Changing);
        // Structural growth alone must not make the repository "degrading" —
        // and must not vanish into "stable" either.
        assert_eq!(trends.overall, TrendDirection::Changing);
    }

    #[test]
    fn disagreeing_dimensions_produce_a_mixed_repository_reading() {
        let make = |n: u64, c: &str, build: f64, throughput: f64| {
            snapshot(
                n,
                c,
                vec![
                    HealthDimension::available(
                        HealthDimensionKind::BuildTime,
                        vec![measurement(
                            identity("build_time", Direction::LowerIsBetter).with_unit("s"),
                            build,
                            c,
                        )],
                    ),
                    HealthDimension::available(
                        HealthDimensionKind::RuntimePerformance,
                        vec![measurement(
                            identity("throughput", Direction::HigherIsBetter),
                            throughput,
                            c,
                        )],
                    ),
                ],
            )
        };
        let series = vec![
            make(1, &commit('a'), 100.0, 1000.0),
            make(2, &commit('b'), 108.0, 1150.0),
            make(3, &commit('c'), 116.0, 1170.0),
        ];
        let trends = trends("forge", &series, &MaterialityPolicy::default());

        assert_eq!(
            trends.direction_for(HealthDimensionKind::BuildTime),
            Some(TrendDirection::Degrading)
        );
        assert_eq!(
            trends.direction_for(HealthDimensionKind::RuntimePerformance),
            Some(TrendDirection::Improving)
        );
        // Better to say Mixed than to invent a number that hides the conflict.
        assert_eq!(trends.overall, TrendDirection::Mixed);
        assert_eq!(trends.snapshots_considered, 3);
        assert_eq!(trends.algorithm_version, TREND_ALGORITHM_VERSION);
    }

    #[test]
    fn incomparable_series_are_tracked_separately_not_averaged() {
        let mut series = vec![
            build_time(1, &commit('a'), 12.0),
            build_time(2, &commit('b'), 12.1),
            build_time(3, &commit('c'), 12.2),
        ];
        // A differently-configured build appears once; it must not join the
        // existing series.
        series.push(snapshot(
            4,
            &commit('d'),
            vec![HealthDimension::available(
                HealthDimensionKind::BuildTime,
                vec![measurement(
                    identity("build_time", Direction::LowerIsBetter)
                        .with_unit("s")
                        .with_fingerprint("different-config"),
                    99.0,
                    &commit('d'),
                )],
            )],
        ));

        let trends = trends("forge", &series, &MaterialityPolicy::default());
        assert_eq!(trends.trends.len(), 2);
        let insufficient = trends
            .trends
            .iter()
            .find(|t| t.direction == TrendDirection::InsufficientData)
            .expect("the one-off series");
        assert_eq!(insufficient.points.len(), 1);
    }

    #[test]
    fn a_materiality_threshold_raises_the_bar_for_a_trend() {
        let series = vec![
            build_time(1, &commit('a'), 100.0),
            build_time(2, &commit('b'), 101.0),
            build_time(3, &commit('c'), 103.0),
        ];
        // 3% net change: a trend by default, not a trend at a 5% bar.
        assert_eq!(
            trends("forge", &series, &MaterialityPolicy::default()).trends[0].direction,
            TrendDirection::Degrading
        );
        assert_eq!(
            trends(
                "forge",
                &series,
                &MaterialityPolicy::default().with_metric("build_time", 5.0)
            )
            .trends[0]
                .direction,
            TrendDirection::Stable
        );
    }

    // ------------------------------------------------------------ baseline

    #[test]
    fn the_nearest_ancestor_is_preferred_as_a_baseline() {
        let target = build_time(4, &commit('d'), 14.0);
        let candidates = vec![
            (
                build_time(1, &commit('a'), 12.0),
                SnapshotRelation::Ancestor,
            ),
            (
                build_time(3, &commit('c'), 13.0),
                SnapshotRelation::Ancestor,
            ),
            // Latest overall, but on a diverged branch.
            (build_time(9, &commit('z'), 99.0), SnapshotRelation::Stale),
        ];

        let baseline = nearest_ancestor_baseline(&target, &candidates).expect("a baseline");
        assert_eq!(baseline.commit, commit('c'));
    }

    #[test]
    fn a_diverged_history_yields_no_automatic_baseline() {
        let target = build_time(4, &commit('d'), 14.0);
        let candidates = vec![(build_time(1, &commit('z'), 12.0), SnapshotRelation::Stale)];
        assert!(nearest_ancestor_baseline(&target, &candidates).is_none());
    }

    // ------------------------------------------------------------- summary

    #[test]
    fn missing_dimensions_are_reported_rather_than_assumed_clean() {
        let from = snapshot(
            1,
            &commit('a'),
            vec![HealthDimension::available(
                HealthDimensionKind::Security,
                vec![measurement(
                    identity("security_findings", Direction::LowerIsBetter),
                    0.0,
                    &commit('a'),
                )],
            )],
        );
        let to = snapshot(
            2,
            &commit('b'),
            vec![HealthDimension::unavailable(
                HealthDimensionKind::Security,
                "no security evaluator ran",
            )],
        );
        assert_eq!(
            missing_dimensions(&from, &to),
            vec![HealthDimensionKind::Security]
        );
    }

    #[test]
    fn dimension_rows_state_availability_honestly() {
        let snapshot = snapshot(
            1,
            &commit('a'),
            vec![
                HealthDimension::available(
                    HealthDimensionKind::BuildTime,
                    vec![measurement(
                        identity("build_time", Direction::LowerIsBetter),
                        1.0,
                        &commit('a'),
                    )],
                ),
                HealthDimension::unavailable(HealthDimensionKind::Memory, "no evaluator emits it"),
            ],
        );
        let rows = dimension_rows(&snapshot);
        assert_eq!(rows[0], ("Build time", "available (1 measurement)".into()));
        assert_eq!(
            rows[1],
            ("Memory", "unavailable (no evaluator emits it)".into())
        );
    }

    #[test]
    fn a_partial_dimension_reports_as_partial() {
        let dimension = HealthDimension::partial(
            HealthDimensionKind::RuntimePerformance,
            vec![measurement(
                identity("throughput", Direction::HigherIsBetter),
                1.0,
                &commit('a'),
            )],
            "only one of two benchmarks reported",
        );
        assert_eq!(dimension.status, DimensionStatus::Partial);
        assert!(describe_dimension(&dimension).starts_with("partial"));
    }
}
