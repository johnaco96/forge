//! Cutoff-safe assembly of persisted policy evidence.
//!
//! The resolver classifies every run returned by the store as eligible or as
//! one typed exclusion. It also derives longitudinal values from immutable,
//! exact-commit health snapshots. The optimizer remains a pure consumer.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, TimeDelta, Utc};
use forge_core::health::{
    DimensionStatus, HealthDimensionKind, MeasuredRepositoryState, RunPatchState,
};
use forge_core::optimization::{
    EvidenceExclusion, ExcludedObservation, HealthEvidenceRef, ObservationSource,
    POLICY_EVIDENCE_VERSION, PolicyEvidenceSnapshot, PolicyObservation,
};
use forge_core::policy::{EngineeringPolicy, ObjectiveMetric};
use forge_core::run::{ExecutionProvenance, RunOutcome, RunStatus};
use forge_store::{PolicyRunEvidence, Store};

use crate::optimizer::HealthEvidenceValues;
use crate::runtime::PolicyRuntimeError;

/// Evidence plus the separately assembled health values consumed by the pure
/// optimizer.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedPolicyEvidence {
    pub snapshot: PolicyEvidenceSnapshot,
    pub health: HealthEvidenceValues,
}

/// Deterministic, store-backed evidence resolution.
#[derive(Debug, Clone)]
pub struct PolicyEvidenceResolver {
    store: Store,
    allowed_provenance: BTreeSet<ExecutionProvenance>,
    limit: u32,
}

impl PolicyEvidenceResolver {
    /// Production evidence accepts only executions explicitly recorded as
    /// live. Tests may opt synthetic evidence in explicitly.
    pub fn new(store: Store) -> Self {
        Self {
            store,
            allowed_provenance: [ExecutionProvenance::Live].into_iter().collect(),
            limit: 10_000,
        }
    }

    pub fn with_allowed_provenance(
        mut self,
        allowed: impl IntoIterator<Item = ExecutionProvenance>,
    ) -> Self {
        self.allowed_provenance = allowed.into_iter().collect();
        self
    }

    pub fn with_limit(mut self, limit: u32) -> Self {
        self.limit = limit.max(1);
        self
    }

    pub async fn resolve(
        &self,
        active: &EngineeringPolicy,
        candidate: &EngineeringPolicy,
        cutoff: DateTime<Utc>,
    ) -> Result<ResolvedPolicyEvidence, PolicyRuntimeError> {
        if active.repository != candidate.repository {
            return Err(PolicyRuntimeError::Invalid(
                "active and candidate policies govern different repositories".into(),
            ));
        }
        if active.objective != candidate.objective {
            return Err(PolicyRuntimeError::Invalid(
                "candidate changes the optimization objective".into(),
            ));
        }

        let evidence = self.store.policy_run_evidence(cutoff, self.limit).await?;
        let window_start = cutoff
            - TimeDelta::try_days(i64::from(active.objective.observation_window_days))
                .ok_or_else(|| PolicyRuntimeError::Invalid("invalid observation window".into()))?;
        let active_fingerprint = active.fingerprint();
        let candidate_fingerprint = candidate.fingerprint();

        let mut eligible = Vec::new();
        let mut excluded = Vec::new();
        for run in evidence.available {
            match self.classify(
                run,
                &active.repository,
                &active_fingerprint,
                &candidate_fingerprint,
                window_start,
            ) {
                Ok(observation) => eligible.push(observation),
                Err(exclusion) => excluded.push(exclusion),
            }
        }
        excluded.extend(
            evidence
                .after_cutoff
                .into_iter()
                .map(|run_id| ExcludedObservation {
                    run_id,
                    exclusion: EvidenceExclusion::PostCutoff,
                }),
        );
        excluded.extend(
            evidence
                .beyond_limit
                .into_iter()
                .map(|run_id| ExcludedObservation {
                    run_id,
                    exclusion: EvidenceExclusion::CollectionLimit,
                }),
        );

        // Store order is deterministic, but make the identity independent of a
        // query-plan change.
        eligible.sort_by(|left, right| left.run_id.cmp(&right.run_id));
        excluded.sort_by(|left, right| left.run_id.cmp(&right.run_id));

        let (health_refs, health) = self
            .assemble_health(
                &active.repository,
                &active.objective.terms,
                &eligible,
                &active_fingerprint,
                &candidate_fingerprint,
                cutoff,
            )
            .await?;

        Ok(ResolvedPolicyEvidence {
            snapshot: PolicyEvidenceSnapshot {
                repository: active.repository.clone(),
                cutoff,
                active_policy_id: active.policy_id.clone(),
                active_policy_fingerprint: active_fingerprint,
                candidate_policy_fingerprints: vec![candidate_fingerprint],
                eligible,
                excluded,
                health: health_refs,
                // Policy optimization does not consult a current world model;
                // exact context is recorded on each PolicyDecision instead.
                world_model_snapshot_id: None,
                evidence_version: POLICY_EVIDENCE_VERSION.to_string(),
                observation_window_days: active.objective.observation_window_days,
            },
            health,
        })
    }

    fn classify(
        &self,
        item: PolicyRunEvidence,
        repository: &str,
        active_fingerprint: &str,
        candidate_fingerprint: &str,
        window_start: DateTime<Utc>,
    ) -> Result<PolicyObservation, ExcludedObservation> {
        let run_id = item.run.run_id.clone();
        let exclude = |exclusion| ExcludedObservation {
            run_id: run_id.clone(),
            exclusion,
        };

        if item.repository != repository {
            return Err(exclude(EvidenceExclusion::WrongRepository));
        }
        let observed_at = item.run.finished_at.unwrap_or(item.run.created_at);
        if observed_at < window_start {
            return Err(exclude(EvidenceExclusion::OutsideObservationWindow));
        }
        if item.manual_override.is_some()
            || item.decision_source == Some(forge_core::PolicySelectionSource::ManualOverride)
        {
            return Err(exclude(EvidenceExclusion::ManualOverride));
        }
        if !self
            .allowed_provenance
            .contains(&item.run.execution_provenance)
        {
            return Err(exclude(EvidenceExclusion::DisallowedProvenance {
                provenance: item.run.execution_provenance,
            }));
        }
        if item.run.status != RunStatus::Completed
            || item.run.outcome.is_none()
            || item.run.outcome == Some(RunOutcome::Errored)
        {
            return Err(exclude(EvidenceExclusion::InfrastructureFailure));
        }
        let Some(policy_fingerprint) = item.policy_fingerprint.as_deref() else {
            return Err(exclude(EvidenceExclusion::MissingPolicyIdentity));
        };
        if item.policy_id.is_none() {
            return Err(exclude(EvidenceExclusion::MissingPolicyIdentity));
        }
        if policy_fingerprint != active_fingerprint && policy_fingerprint != candidate_fingerprint {
            return Err(exclude(EvidenceExclusion::PolicyMismatch));
        }
        let source = item
            .decision_source
            .map(|source| source.observation_source())
            .ok_or_else(|| exclude(EvidenceExclusion::MissingPolicyIdentity))?;
        if !source.is_policy_controlled() {
            return Err(exclude(EvidenceExclusion::ManualOverride));
        }
        if matches!(
            source,
            ObservationSource::CanaryCandidate | ObservationSource::CanaryControl
        ) && item.experiment.is_none()
        {
            return Err(exclude(EvidenceExclusion::IncomparableConfiguration {
                detail: "canary decision has no cutoff-admissible experiment observation".into(),
            }));
        }
        let Some(integrity) = item.run.integrity.as_ref() else {
            return Err(exclude(EvidenceExclusion::IncomparableConfiguration {
                detail: "run has no integrity measurement".into(),
            }));
        };
        let measured = MeasuredRepositoryState::for_run(
            &item.run.base_commit,
            item.run.patch.as_ref().map(|patch| RunPatchState {
                head_commit: patch.head_commit.as_deref(),
                is_empty: patch.is_empty(),
            }),
        );
        let Some(measured_commit) = measured.commit() else {
            return Err(exclude(EvidenceExclusion::MissingMeasuredCommit));
        };
        let usage = item.run.usage();
        Ok(PolicyObservation {
            run_id,
            task_revision_id: item.task_revision_id,
            policy_id: item.policy_id,
            policy_fingerprint: item.policy_fingerprint,
            source,
            experiment: item.experiment,
            provenance: item.run.execution_provenance,
            outcome: item.run.outcome.expect("checked above"),
            integrity_clean: integrity.is_acceptable(),
            config_fingerprint: item.run.agent.fingerprint(),
            runtime_ms: item
                .run
                .total_duration()
                .and_then(|duration| duration.num_milliseconds().try_into().ok()),
            cost_usd: usage.cost_usd,
            tokens: usage.total_tokens(),
            patch_lines: item.run.patch.as_ref().map(|patch| patch.lines_changed()),
            measured_commit: Some(measured_commit.to_string()),
            observed_at,
        })
    }

    #[allow(clippy::too_many_arguments)]
    async fn assemble_health(
        &self,
        repository: &str,
        terms: &[forge_core::ObjectiveTerm],
        observations: &[PolicyObservation],
        active_fingerprint: &str,
        candidate_fingerprint: &str,
        cutoff: DateTime<Utc>,
    ) -> Result<(Vec<HealthEvidenceRef>, HealthEvidenceValues), PolicyRuntimeError> {
        let commits_by_arm = |fingerprint: &str| {
            observations
                .iter()
                .filter(|observation| {
                    observation.policy_fingerprint.as_deref() == Some(fingerprint)
                })
                .filter_map(|observation| observation.measured_commit.clone())
                .collect::<BTreeSet<_>>()
        };
        let active_commits = commits_by_arm(active_fingerprint);
        let candidate_commits = commits_by_arm(candidate_fingerprint);
        let ids = self
            .store
            .policy_health_evidence(repository, cutoff, u32::MAX)
            .await?;

        type Values = BTreeMap<(HealthDimensionKind, String), Vec<f64>>;
        let mut active_values = Values::new();
        let mut candidate_values = Values::new();
        let mut active_snapshots = BTreeSet::new();
        let mut candidate_snapshots = BTreeSet::new();
        let mut refs = Vec::new();
        let objective_directions = terms
            .iter()
            .filter_map(|term| match &term.metric {
                ObjectiveMetric::RepositoryHealth { dimension } => {
                    Some((*dimension, term.direction))
                }
                _ => None,
            })
            .collect::<BTreeMap<_, _>>();

        for (id, commit, observed_at) in ids {
            let on_active = active_commits.contains(&commit);
            let on_candidate = candidate_commits.contains(&commit);
            if !on_active && !on_candidate {
                continue;
            }
            let snapshot = self.store.load_health_snapshot(&id).await?.ok_or_else(|| {
                PolicyRuntimeError::Invalid(format!("missing health snapshot {id}"))
            })?;
            if snapshot.repository != repository || snapshot.commit != commit {
                return Err(PolicyRuntimeError::Invalid(format!(
                    "health snapshot {id} is not scoped to its indexed repository and commit"
                )));
            }
            // Exact commit equality with an eligible execution is stronger
            // than an ancestry inference and stays reproducible if HEAD moves.
            refs.push(HealthEvidenceRef {
                health_snapshot_id: id.clone(),
                commit: commit.clone(),
                observed_at,
            });
            if on_active {
                active_snapshots.insert(id.clone());
            }
            if on_candidate {
                candidate_snapshots.insert(id.clone());
            }
            for dimension in &snapshot.dimensions {
                if dimension.status != DimensionStatus::Available {
                    continue;
                }
                for measurement in &dimension.measurements {
                    if objective_directions.get(&dimension.kind)
                        != Some(&measurement.identity.direction)
                    {
                        continue;
                    }
                    let key = (dimension.kind, measurement.identity.comparability_key());
                    if on_active {
                        active_values
                            .entry(key.clone())
                            .or_default()
                            .push(measurement.value);
                    }
                    if on_candidate {
                        candidate_values
                            .entry(key)
                            .or_default()
                            .push(measurement.value);
                    }
                }
            }
        }

        refs.sort_by(|left, right| left.health_snapshot_id.cmp(&right.health_snapshot_id));
        refs.dedup_by(|left, right| left.health_snapshot_id == right.health_snapshot_id);

        let mut baseline = Vec::new();
        let mut candidate = Vec::new();
        for term in terms {
            let ObjectiveMetric::RepositoryHealth { dimension } = &term.metric else {
                continue;
            };
            let shared = active_values
                .keys()
                .filter(|(kind, key)| {
                    kind == dimension && candidate_values.contains_key(&(*kind, key.clone()))
                })
                .cloned()
                .collect::<Vec<_>>();
            // Multiple producer identities are different measurements, not a
            // bag of numbers that may be averaged together.
            if shared.len() != 1 {
                continue;
            }
            let key = &shared[0];
            let active_arm = &active_values[key];
            let candidate_arm = &candidate_values[key];
            baseline.push((*dimension, mean(active_arm)));
            candidate.push((*dimension, mean(candidate_arm)));
        }

        Ok((
            refs,
            HealthEvidenceValues {
                baseline,
                candidate,
                snapshots: active_snapshots.len().min(candidate_snapshots.len()) as u64,
            },
        ))
    }
}

fn mean(values: &[f64]) -> f64 {
    values.iter().sum::<f64>() / values.len() as f64
}
