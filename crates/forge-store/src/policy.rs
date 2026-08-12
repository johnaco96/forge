//! Persistence for engineering policy.
//!
//! Policies are immutable in everything that affects execution; only their
//! lifecycle status moves. The fingerprint excludes status by construction, so
//! it is verified on every load — a stored record whose behaviour no longer
//! hashes to its recorded identity is corruption, not a policy.
//!
//! Two responsibilities, kept apart in the same way `health` keeps them apart:
//!
//! - **Evidence collection** ([`Store::policy_run_evidence`]) reads the
//!   engineering record the ledger already holds, in the typed shape evidence
//!   assembly needs. It classifies nothing and interprets nothing — deciding
//!   what is eligible is the resolver's job, and doing it in SQL would make the
//!   exclusions invisible.
//! - **Policy persistence** stores immutable `P-*`, `PP-*`, `PD-*`, and `PX-*`
//!   records plus the mutable pointer to the active policy.

use chrono::{DateTime, Utc};
use forge_core::ids::{
    HealthSnapshotId, PolicyDecisionId, PolicyExperimentId, PolicyId, PolicyProposalId, RunId,
};
use forge_core::optimization::{
    ExperimentArm, ExperimentAssignment, ExperimentMembership, PolicyDecision, PolicyEvent,
    PolicyEventSubject, PolicyEvidenceSnapshot, PolicyExperiment, PolicyExperimentStatus,
    PolicyProposal, PolicySelectionSource, ShadowDecision,
};
use forge_core::policy::{EngineeringPolicy, PolicyStatus};
use forge_core::run::AgentRun;
use forge_core::task::TaskRevisionId;
use sqlx::Row;

use crate::{Store, StoreError, StoreResult};

const POLICY_COUNTER: &str = "engineering_policy";
const PROPOSAL_COUNTER: &str = "policy_proposal";
const DECISION_COUNTER: &str = "policy_decision";
const EXPERIMENT_COUNTER: &str = "policy_experiment";

/// One entry in a repository's policy lineage.
#[derive(Debug, Clone, PartialEq)]
pub struct PolicyHistoryEntry {
    pub policy_id: PolicyId,
    pub parent_policy_id: Option<PolicyId>,
    pub status: PolicyStatus,
    pub provenance: String,
    pub fingerprint: String,
    pub created_at: DateTime<Utc>,
    pub is_active: bool,
}

/// One run's engineering record, plus the policy facts recorded against it.
///
/// Deliberately carries the whole [`AgentRun`] rather than a flattened subset:
/// which fields count as evidence is an eligibility decision, and pre-selecting
/// them here would move that decision into the store.
#[derive(Debug, Clone, PartialEq)]
pub struct PolicyRunEvidence {
    pub run: AgentRun,
    /// Scope, taken from the immutable task revision rather than inferred.
    pub repository: String,
    pub task_revision_id: TaskRevisionId,
    /// The policy that governed the run, when one did. `None` for every
    /// Phase 0–7 execution, and that stays true.
    pub policy_id: Option<PolicyId>,
    pub policy_fingerprint: Option<String>,
    pub experiment: Option<ExperimentMembership>,
    /// How the strategy was chosen, from the decision record. `None` when no
    /// policy decision governed the run.
    pub decision_source: Option<PolicySelectionSource>,
    pub manual_override: Option<String>,
}

/// Candidate evidence for one cutoff.
///
/// The two lists are returned together on purpose. A caller reconstructing a
/// historical proposal must be able to say "these runs existed and these did
/// not", and a query that simply omitted the future would leave no trace that
/// anything had been left out.
#[derive(Debug, Clone, PartialEq)]
pub struct PolicyRunEvidenceSet {
    /// Runs that existed at the cutoff, most recent first.
    pub available: Vec<PolicyRunEvidence>,
    /// Runs the ledger holds now that did not exist at the cutoff.
    pub after_cutoff: Vec<RunId>,
    /// Runs before the cutoff that fell outside the caller's explicit cap.
    pub beyond_limit: Vec<RunId>,
}

impl Store {
    pub async fn next_policy_id(&self) -> StoreResult<PolicyId> {
        Ok(PolicyId::sequential(
            self.next_counter(POLICY_COUNTER).await?,
        ))
    }

    pub async fn next_policy_proposal_id(&self) -> StoreResult<PolicyProposalId> {
        Ok(PolicyProposalId::sequential(
            self.next_counter(PROPOSAL_COUNTER).await?,
        ))
    }

    pub async fn next_policy_decision_id(&self) -> StoreResult<PolicyDecisionId> {
        Ok(PolicyDecisionId::sequential(
            self.next_counter(DECISION_COUNTER).await?,
        ))
    }

    pub async fn next_policy_experiment_id(&self) -> StoreResult<PolicyExperimentId> {
        Ok(PolicyExperimentId::sequential(
            self.next_counter(EXPERIMENT_COUNTER).await?,
        ))
    }

    // ---------------------------------------------------------------- policies

    /// Records an immutable policy.
    ///
    /// Re-inserting an identical record succeeds. Re-inserting different
    /// behaviour under the same id is refused: a policy that governed a
    /// historical execution must still describe what governed it.
    pub async fn insert_policy(&self, policy: &EngineeringPolicy) -> StoreResult<()> {
        if let Some(existing) = self.policy_by_id(&policy.policy_id).await? {
            // Status may legitimately have moved on; every other historical
            // fact (including provenance and lineage) is immutable.
            let mut attempted = policy.clone();
            attempted.status = existing.status;
            return if existing == attempted {
                Ok(())
            } else {
                Err(StoreError::Corrupt(format!(
                    "policy {} already exists with different behaviour; \
                     policy records are immutable",
                    policy.policy_id
                )))
            };
        }

        sqlx::query(
            "INSERT INTO engineering_policies (
                 policy_id, repository, parent_policy_id, schema_version, status,
                 provenance, fingerprint, optimizer_version, proposal_id, created_at, record_json
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        )
        .bind(policy.policy_id.as_str())
        .bind(&policy.repository)
        .bind(policy.parent_policy_id.as_ref().map(|id| id.as_str()))
        .bind(&policy.schema_version)
        .bind(policy.status.as_str())
        .bind(policy.provenance.as_str())
        .bind(policy.fingerprint())
        .bind(policy.optimizer_version.as_deref())
        .bind(policy.proposal_id.as_ref().map(|id| id.as_str()))
        .bind(policy.created_at.to_rfc3339())
        .bind(serde_json::to_string(policy)?)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// Installs the first repository policy, pointer, and creation event in one
    /// transaction. Historical runs remain untouched and policy-null.
    pub async fn install_bootstrap_policy(
        &self,
        policy: &EngineeringPolicy,
        event: &PolicyEvent,
    ) -> StoreResult<()> {
        if policy.provenance != forge_core::policy::PolicyProvenance::Bootstrap
            || policy.status != PolicyStatus::Active
            || policy.parent_policy_id.is_some()
            || event.subject != PolicyEventSubject::Policy(policy.policy_id.clone())
            || !matches!(
                &event.payload,
                forge_core::optimization::PolicyEventPayload::PolicyCreated {
                    fingerprint,
                    ..
                } if fingerprint == &policy.fingerprint()
            )
        {
            return Err(StoreError::Corrupt(
                "bootstrap policy or event does not describe an initial active policy".into(),
            ));
        }
        let mut transaction = self.pool().begin().await?;
        let current: Option<String> =
            sqlx::query_scalar("SELECT policy_id FROM policy_current WHERE repository = ?1")
                .bind(&policy.repository)
                .fetch_optional(&mut *transaction)
                .await?;
        if current.is_some() {
            return Err(StoreError::Corrupt(format!(
                "repository `{}` already has an active policy",
                policy.repository
            )));
        }
        sqlx::query(
            "INSERT INTO engineering_policies (
                 policy_id, repository, parent_policy_id, schema_version, status,
                 provenance, fingerprint, optimizer_version, proposal_id, created_at, record_json
             ) VALUES (?1, ?2, NULL, ?3, ?4, ?5, ?6, NULL, NULL, ?7, ?8)",
        )
        .bind(policy.policy_id.as_str())
        .bind(&policy.repository)
        .bind(&policy.schema_version)
        .bind(policy.status.as_str())
        .bind(policy.provenance.as_str())
        .bind(policy.fingerprint())
        .bind(policy.created_at.to_rfc3339())
        .bind(serde_json::to_string(policy)?)
        .execute(&mut *transaction)
        .await?;
        sqlx::query("INSERT INTO policy_current (repository, policy_id) VALUES (?1, ?2)")
            .bind(&policy.repository)
            .bind(policy.policy_id.as_str())
            .execute(&mut *transaction)
            .await?;
        insert_policy_event(&mut transaction, event).await?;
        transaction.commit().await?;
        Ok(())
    }

    /// Loads a policy, verifying its identity.
    pub async fn policy_by_id(&self, id: &PolicyId) -> StoreResult<Option<EngineeringPolicy>> {
        let row = sqlx::query(
            "SELECT record_json, status, fingerprint FROM engineering_policies
             WHERE policy_id = ?1",
        )
        .bind(id.as_str())
        .fetch_optional(self.pool())
        .await?;

        let Some(row) = row else {
            return Ok(None);
        };
        let mut policy: EngineeringPolicy =
            serde_json::from_str(&row.try_get::<String, _>("record_json")?)?;

        // The behaviour must still hash to the identity recorded with it.
        let recorded: String = row.try_get("fingerprint")?;
        if policy.fingerprint() != recorded {
            return Err(StoreError::Corrupt(format!(
                "policy {id} no longer matches its recorded fingerprint",
            )));
        }
        // Status lives in its own column, because it is the one thing that moves.
        policy.status = parse_enum(&row.try_get::<String, _>("status")?)?;
        Ok(Some(policy))
    }

    /// Moves a policy's lifecycle status.
    ///
    /// Validated against the lifecycle: a `Draft` cannot become `Active` by a
    /// status write, however the caller asks.
    pub async fn set_policy_status(&self, id: &PolicyId, next: PolicyStatus) -> StoreResult<()> {
        if next == PolicyStatus::Active {
            return Err(StoreError::Corrupt(
                "Active status is written only by the transactional promotion or rollback path"
                    .into(),
            ));
        }
        let policy = self
            .policy_by_id(id)
            .await?
            .ok_or_else(|| StoreError::NotFound(format!("policy {id}")))?;
        if policy.status != next && !policy.status.can_transition_to(next) {
            return Err(StoreError::Corrupt(format!(
                "invalid policy transition for {id}: {} -> {next}",
                policy.status
            )));
        }
        sqlx::query("UPDATE engineering_policies SET status = ?2 WHERE policy_id = ?1")
            .bind(id.as_str())
            .bind(next.as_str())
            .execute(self.pool())
            .await?;
        Ok(())
    }

    /// The policy currently governing a repository.
    pub async fn active_policy(&self, repository: &str) -> StoreResult<Option<EngineeringPolicy>> {
        let id: Option<String> =
            sqlx::query_scalar("SELECT policy_id FROM policy_current WHERE repository = ?1")
                .bind(repository)
                .fetch_optional(self.pool())
                .await?;
        match id {
            Some(raw) => {
                let id =
                    PolicyId::new(raw).map_err(|error| StoreError::Corrupt(error.to_string()))?;
                self.policy_by_id(&id).await
            }
            None => Ok(None),
        }
    }

    /// Atomically promotes a tested candidate and records the durable event.
    /// Gate evaluation and human approval happen in `forge-policy`; this store
    /// boundary guarantees that a failed write cannot move only the pointer.
    pub async fn promote_policy(
        &self,
        repository: &str,
        expected_active: &PolicyId,
        candidate: &PolicyId,
        proposal_id: &PolicyProposalId,
        event: &PolicyEvent,
    ) -> StoreResult<()> {
        let approved = matches!(
            &event.payload,
            forge_core::optimization::PolicyEventPayload::PolicyPromoted {
                from_policy_id,
                to_policy_id,
                approved_by,
            } if from_policy_id == expected_active
                && to_policy_id == candidate
                && !approved_by.trim().is_empty()
        );
        if event.subject != PolicyEventSubject::Policy(candidate.clone()) || !approved {
            return Err(StoreError::Corrupt(
                "promotion event does not match the requested pointer change or approval".into(),
            ));
        }
        let proposal = self
            .policy_proposal_by_id(proposal_id)
            .await?
            .ok_or_else(|| StoreError::NotFound(format!("proposal {proposal_id}")))?;
        if proposal.repository != repository
            || proposal.active_policy_id != *expected_active
            || proposal.candidate_policy_id != *candidate
            || !proposal.recommendation.permits_promotion()
            || !proposal.satisfies_hard_constraints()
        {
            return Err(StoreError::Corrupt(format!(
                "proposal {proposal_id} does not authorize promotion from {expected_active} to {candidate}"
            )));
        }
        let mut transaction = self.pool().begin().await?;
        let current: Option<String> =
            sqlx::query_scalar("SELECT policy_id FROM policy_current WHERE repository = ?1")
                .bind(repository)
                .fetch_optional(&mut *transaction)
                .await?;
        if current.as_deref() != Some(expected_active.as_str()) {
            return Err(StoreError::Corrupt(format!(
                "active policy changed before promotion; expected {expected_active}, found {}",
                current.as_deref().unwrap_or("none")
            )));
        }
        let candidate_row = sqlx::query(
            "SELECT repository, parent_policy_id, status FROM engineering_policies
             WHERE policy_id = ?1",
        )
        .bind(candidate.as_str())
        .fetch_optional(&mut *transaction)
        .await?
        .ok_or_else(|| StoreError::NotFound(format!("policy {candidate}")))?;
        if candidate_row.try_get::<String, _>("repository")? != repository
            || candidate_row
                .try_get::<Option<String>, _>("parent_policy_id")?
                .as_deref()
                != Some(expected_active.as_str())
            || candidate_row.try_get::<String, _>("status")? != PolicyStatus::Canary.as_str()
        {
            return Err(StoreError::Corrupt(format!(
                "policy {candidate} is not a canary successor of {expected_active}"
            )));
        }
        let experiment_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM policy_experiments
             WHERE repository = ?1 AND control_policy_id = ?2 AND candidate_policy_id = ?3
               AND status = ?4",
        )
        .bind(repository)
        .bind(expected_active.as_str())
        .bind(candidate.as_str())
        .bind(PolicyExperimentStatus::Concluded.as_str())
        .fetch_one(&mut *transaction)
        .await?;
        if experiment_count == 0 {
            return Err(StoreError::Corrupt(format!(
                "candidate {candidate} has no concluded control/candidate experiment"
            )));
        }

        sqlx::query(
            "UPDATE engineering_policies SET status = ?2
             WHERE policy_id = ?1 AND status = ?3",
        )
        .bind(expected_active.as_str())
        .bind(PolicyStatus::Superseded.as_str())
        .bind(PolicyStatus::Active.as_str())
        .execute(&mut *transaction)
        .await?;
        sqlx::query("UPDATE engineering_policies SET status = ?2 WHERE policy_id = ?1")
            .bind(candidate.as_str())
            .bind(PolicyStatus::Active.as_str())
            .execute(&mut *transaction)
            .await?;
        sqlx::query("UPDATE policy_current SET policy_id = ?2 WHERE repository = ?1")
            .bind(repository)
            .bind(candidate.as_str())
            .execute(&mut *transaction)
            .await?;
        insert_policy_event(&mut transaction, event).await?;
        transaction.commit().await?;
        Ok(())
    }

    /// Atomically re-points to a prior immutable policy and records why.
    pub async fn rollback_policy(
        &self,
        repository: &str,
        expected_active: &PolicyId,
        target: &PolicyId,
        event: &PolicyEvent,
    ) -> StoreResult<()> {
        let justified = matches!(
            &event.payload,
            forge_core::optimization::PolicyEventPayload::PolicyRolledBack {
                from_policy_id,
                to_policy_id,
                reason,
            } if from_policy_id == expected_active
                && to_policy_id == target
                && !reason.trim().is_empty()
        );
        if event.subject != PolicyEventSubject::Policy(target.clone()) || !justified {
            return Err(StoreError::Corrupt(
                "rollback event does not match the requested pointer change or reason".into(),
            ));
        }
        let mut transaction = self.pool().begin().await?;
        let current: Option<String> =
            sqlx::query_scalar("SELECT policy_id FROM policy_current WHERE repository = ?1")
                .bind(repository)
                .fetch_optional(&mut *transaction)
                .await?;
        if current.as_deref() != Some(expected_active.as_str()) {
            return Err(StoreError::Corrupt(format!(
                "active policy changed before rollback; expected {expected_active}, found {}",
                current.as_deref().unwrap_or("none")
            )));
        }
        let target_row =
            sqlx::query("SELECT repository, status FROM engineering_policies WHERE policy_id = ?1")
                .bind(target.as_str())
                .fetch_optional(&mut *transaction)
                .await?
                .ok_or_else(|| StoreError::NotFound(format!("policy {target}")))?;
        let target_status: PolicyStatus = parse_enum(&target_row.try_get::<String, _>("status")?)?;
        if target_row.try_get::<String, _>("repository")? != repository
            || !matches!(
                target_status,
                PolicyStatus::Superseded | PolicyStatus::RolledBack
            )
        {
            return Err(StoreError::Corrupt(format!(
                "policy {target} is not a previously active policy for `{repository}`"
            )));
        }

        sqlx::query("UPDATE engineering_policies SET status = ?2 WHERE policy_id = ?1")
            .bind(expected_active.as_str())
            .bind(PolicyStatus::RolledBack.as_str())
            .execute(&mut *transaction)
            .await?;
        sqlx::query("UPDATE engineering_policies SET status = ?2 WHERE policy_id = ?1")
            .bind(target.as_str())
            .bind(PolicyStatus::Active.as_str())
            .execute(&mut *transaction)
            .await?;
        sqlx::query("UPDATE policy_current SET policy_id = ?2 WHERE repository = ?1")
            .bind(repository)
            .bind(target.as_str())
            .execute(&mut *transaction)
            .await?;
        insert_policy_event(&mut transaction, event).await?;
        transaction.commit().await?;
        Ok(())
    }

    /// A repository's policy lineage, oldest first.
    pub async fn policy_history(
        &self,
        repository: &str,
        limit: u32,
    ) -> StoreResult<Vec<PolicyHistoryEntry>> {
        let active = sqlx::query_scalar::<_, String>(
            "SELECT policy_id FROM policy_current WHERE repository = ?1",
        )
        .bind(repository)
        .fetch_optional(self.pool())
        .await?;

        let rows = sqlx::query(
            "SELECT policy_id, parent_policy_id, status, provenance, fingerprint, created_at
             FROM engineering_policies
             WHERE repository = ?1
             ORDER BY created_at ASC, policy_id ASC
             LIMIT ?2",
        )
        .bind(repository)
        .bind(limit)
        .fetch_all(self.pool())
        .await?;

        rows.into_iter()
            .map(|row| {
                let policy_id = PolicyId::new(row.try_get::<String, _>("policy_id")?)
                    .map_err(|error| StoreError::Corrupt(error.to_string()))?;
                Ok(PolicyHistoryEntry {
                    is_active: active.as_deref() == Some(policy_id.as_str()),
                    parent_policy_id: row
                        .try_get::<Option<String>, _>("parent_policy_id")?
                        .map(PolicyId::new)
                        .transpose()
                        .map_err(|error| StoreError::Corrupt(error.to_string()))?,
                    policy_id,
                    status: parse_enum(&row.try_get::<String, _>("status")?)?,
                    provenance: row.try_get("provenance")?,
                    fingerprint: row.try_get("fingerprint")?,
                    created_at: parse_time(&row.try_get::<String, _>("created_at")?)?,
                })
            })
            .collect()
    }

    pub async fn policy_count(&self, repository: &str) -> StoreResult<u64> {
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM engineering_policies WHERE repository = ?1")
                .bind(repository)
                .fetch_one(self.pool())
                .await?;
        Ok(count as u64)
    }

    /// Most recently created policy currently observing in shadow mode.
    pub async fn shadow_policy(&self, repository: &str) -> StoreResult<Option<EngineeringPolicy>> {
        let id: Option<String> = sqlx::query_scalar(
            "SELECT p.policy_id FROM engineering_policies p
             JOIN policy_current c ON c.repository = p.repository
             WHERE p.repository = ?1 AND p.status = ?2
               AND p.parent_policy_id = c.policy_id
             ORDER BY p.created_at DESC, p.policy_id DESC LIMIT 1",
        )
        .bind(repository)
        .bind(PolicyStatus::Shadow.as_str())
        .fetch_optional(self.pool())
        .await?;
        match id {
            Some(id) => {
                self.policy_by_id(
                    &PolicyId::new(id).map_err(|error| StoreError::Corrupt(error.to_string()))?,
                )
                .await
            }
            None => Ok(None),
        }
    }

    // --------------------------------------------------------------- proposals

    /// Records an immutable proposal together with the evidence behind it.
    ///
    /// The snapshot is stored whole and its identity checked against the
    /// proposal's: a recommendation whose recorded evidence fingerprint does
    /// not match the evidence filed with it could never be re-derived, and
    /// storing the pair anyway would produce an audit trail that lies.
    pub async fn insert_policy_proposal(
        &self,
        proposal: &PolicyProposal,
        evidence: &PolicyEvidenceSnapshot,
    ) -> StoreResult<()> {
        let active = self
            .policy_by_id(&proposal.active_policy_id)
            .await?
            .ok_or_else(|| StoreError::NotFound(format!("policy {}", proposal.active_policy_id)))?;
        let candidate = self
            .policy_by_id(&proposal.candidate_policy_id)
            .await?
            .ok_or_else(|| {
                StoreError::NotFound(format!("policy {}", proposal.candidate_policy_id))
            })?;
        if active.repository != proposal.repository
            || candidate.repository != proposal.repository
            || candidate.parent_policy_id.as_ref() != Some(&active.policy_id)
        {
            return Err(StoreError::Corrupt(format!(
                "proposal {} does not describe a repository-scoped direct policy successor",
                proposal.proposal_id
            )));
        }
        if candidate.fingerprint() != proposal.candidate_fingerprint {
            return Err(StoreError::Corrupt(format!(
                "proposal {} candidate fingerprint does not match policy {}",
                proposal.proposal_id, candidate.policy_id
            )));
        }
        if proposal.objective != active.objective || candidate.objective != active.objective {
            return Err(StoreError::Corrupt(format!(
                "proposal {} attempts to change the optimizer's objective",
                proposal.proposal_id
            )));
        }
        let fingerprint = evidence.fingerprint();
        if fingerprint != proposal.evidence_fingerprint {
            return Err(StoreError::Corrupt(format!(
                "proposal {} records evidence `{}` but was filed with evidence `{fingerprint}`",
                proposal.proposal_id, proposal.evidence_fingerprint
            )));
        }
        if evidence.repository != proposal.repository {
            return Err(StoreError::Corrupt(format!(
                "proposal {} governs `{}` but its evidence describes `{}`",
                proposal.proposal_id, proposal.repository, evidence.repository
            )));
        }
        if evidence.active_policy_id != active.policy_id
            || evidence.active_policy_fingerprint != active.fingerprint()
            || !evidence
                .candidate_policy_fingerprints
                .contains(&candidate.fingerprint())
        {
            return Err(StoreError::Corrupt(format!(
                "proposal {} evidence does not identify its active and candidate policies",
                proposal.proposal_id
            )));
        }

        let mut transaction = self.pool().begin().await?;

        sqlx::query(
            "INSERT INTO policy_proposals (
                 proposal_id, repository, active_policy_id, candidate_policy_id,
                 recommendation, comparison, evidence_strength, cutoff, evidence_fingerprint,
                 optimizer_version, eligible_count, excluded_count, created_at, record_json,
                 evidence_json
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
        )
        .bind(proposal.proposal_id.as_str())
        .bind(&proposal.repository)
        .bind(proposal.active_policy_id.as_str())
        .bind(proposal.candidate_policy_id.as_str())
        .bind(proposal.recommendation.as_str())
        .bind(proposal.comparison.as_str())
        .bind(proposal.evidence_strength.as_str())
        .bind(proposal.cutoff.to_rfc3339())
        .bind(&proposal.evidence_fingerprint)
        .bind(&proposal.optimizer_version)
        .bind(proposal.eligible_observations as i64)
        .bind(proposal.excluded_observations as i64)
        .bind(proposal.created_at.to_rfc3339())
        .bind(serde_json::to_string(proposal)?)
        .bind(serde_json::to_string(evidence)?)
        .execute(&mut *transaction)
        .await?;

        for observation in &evidence.eligible {
            sqlx::query(
                "INSERT INTO policy_proposal_evidence (proposal_id, run_id, eligible, exclusion)
                 VALUES (?1, ?2, 1, NULL)
                 ON CONFLICT (proposal_id, run_id) DO NOTHING",
            )
            .bind(proposal.proposal_id.as_str())
            .bind(observation.run_id.as_str())
            .execute(&mut *transaction)
            .await?;
        }
        for excluded in &evidence.excluded {
            sqlx::query(
                "INSERT INTO policy_proposal_evidence (proposal_id, run_id, eligible, exclusion)
                 VALUES (?1, ?2, 0, ?3)
                 ON CONFLICT (proposal_id, run_id) DO NOTHING",
            )
            .bind(proposal.proposal_id.as_str())
            .bind(excluded.run_id.as_str())
            .bind(excluded.exclusion.as_str())
            .execute(&mut *transaction)
            .await?;
        }

        transaction.commit().await?;
        Ok(())
    }

    pub async fn policy_proposal_by_id(
        &self,
        id: &PolicyProposalId,
    ) -> StoreResult<Option<PolicyProposal>> {
        let record: Option<String> =
            sqlx::query_scalar("SELECT record_json FROM policy_proposals WHERE proposal_id = ?1")
                .bind(id.as_str())
                .fetch_optional(self.pool())
                .await?;
        record
            .map(|json| Ok(serde_json::from_str(&json)?))
            .transpose()
    }

    /// Proposals for a repository, most recent first.
    pub async fn policy_proposals(
        &self,
        repository: &str,
        limit: u32,
    ) -> StoreResult<Vec<PolicyProposal>> {
        let rows: Vec<String> = sqlx::query_scalar(
            "SELECT record_json FROM policy_proposals
             WHERE repository = ?1
             ORDER BY created_at DESC, proposal_id DESC
             LIMIT ?2",
        )
        .bind(repository)
        .bind(limit)
        .fetch_all(self.pool())
        .await?;
        rows.into_iter()
            .map(|json| Ok(serde_json::from_str(&json)?))
            .collect()
    }

    /// The complete evidence a proposal was computed from.
    pub async fn policy_proposal_evidence(
        &self,
        id: &PolicyProposalId,
    ) -> StoreResult<Option<PolicyEvidenceSnapshot>> {
        let record: Option<String> =
            sqlx::query_scalar("SELECT evidence_json FROM policy_proposals WHERE proposal_id = ?1")
                .bind(id.as_str())
                .fetch_optional(self.pool())
                .await?;
        record
            .map(|json| Ok(serde_json::from_str(&json)?))
            .transpose()
    }

    /// The evidence ids a proposal used, from the indexed projection.
    pub async fn policy_proposal_evidence_ids(
        &self,
        id: &PolicyProposalId,
    ) -> StoreResult<(Vec<RunId>, Vec<(RunId, String)>)> {
        let rows = sqlx::query(
            "SELECT run_id, eligible, exclusion FROM policy_proposal_evidence
             WHERE proposal_id = ?1 ORDER BY run_id",
        )
        .bind(id.as_str())
        .fetch_all(self.pool())
        .await?;

        let mut eligible = Vec::new();
        let mut excluded = Vec::new();
        for row in rows {
            let run_id = RunId::new(row.try_get::<String, _>("run_id")?)
                .map_err(|error| StoreError::Corrupt(error.to_string()))?;
            if row.try_get::<i64, _>("eligible")? == 1 {
                eligible.push(run_id);
            } else {
                excluded.push((
                    run_id,
                    row.try_get::<Option<String>, _>("exclusion")?
                        .unwrap_or_default(),
                ));
            }
        }
        Ok((eligible, excluded))
    }

    // --------------------------------------------------------------- decisions

    /// Records why an execution used the strategy it used.
    pub async fn insert_policy_decision(&self, decision: &PolicyDecision) -> StoreResult<()> {
        if !decision.is_honest() {
            return Err(StoreError::Corrupt(format!(
                "policy decision {} claims policy control while recording a manual override",
                decision.decision_id
            )));
        }
        // The decision names the behaviour it ran under. If that does not match
        // the policy it names, one of the two is wrong, and a stored decision
        // that misattributes behaviour would corrupt every later comparison.
        let selected = self
            .policy_by_id(&decision.selected_policy_id)
            .await?
            .ok_or_else(|| {
                StoreError::NotFound(format!("policy {}", decision.selected_policy_id))
            })?;
        if selected.fingerprint() != decision.policy_fingerprint {
            return Err(StoreError::Corrupt(format!(
                "policy decision {} records fingerprint `{}` for policy {}, which is `{}`",
                decision.decision_id,
                decision.policy_fingerprint,
                decision.selected_policy_id,
                selected.fingerprint()
            )));
        }
        if selected.repository != decision.repository {
            return Err(StoreError::Corrupt(format!(
                "policy decision {} is scoped to `{}` but policy {} governs `{}`",
                decision.decision_id,
                decision.repository,
                decision.selected_policy_id,
                selected.repository
            )));
        }
        let active = self
            .policy_by_id(&decision.active_policy_id)
            .await?
            .ok_or_else(|| StoreError::NotFound(format!("policy {}", decision.active_policy_id)))?;
        if active.repository != decision.repository {
            return Err(StoreError::Corrupt(format!(
                "policy decision {} names an active policy from another repository",
                decision.decision_id
            )));
        }
        let task_repository: Option<String> =
            sqlx::query_scalar("SELECT repository FROM task_revisions WHERE revision_id = ?1")
                .bind(decision.task_revision_id.as_str())
                .fetch_optional(self.pool())
                .await?;
        if task_repository.as_deref() != Some(decision.repository.as_str()) {
            return Err(StoreError::Corrupt(format!(
                "policy decision {} task revision is not scoped to `{}`",
                decision.decision_id, decision.repository
            )));
        }
        match decision.source {
            PolicySelectionSource::ActivePolicy => {
                if decision.selected_policy_id != decision.active_policy_id
                    || decision.experiment.is_some()
                    || decision.manual_override.is_some()
                {
                    return Err(StoreError::Corrupt(format!(
                        "active-policy decision {} has contradictory selection metadata",
                        decision.decision_id
                    )));
                }
            }
            PolicySelectionSource::ManualOverride => {
                if decision.selected_policy_id != decision.active_policy_id
                    || decision.experiment.is_some()
                    || decision
                        .manual_override
                        .as_deref()
                        .is_none_or(|value| value.trim().is_empty())
                {
                    return Err(StoreError::Corrupt(format!(
                        "manual policy decision {} has contradictory selection metadata",
                        decision.decision_id
                    )));
                }
            }
            PolicySelectionSource::CanaryControl | PolicySelectionSource::CanaryCandidate => {
                let membership = decision.experiment.as_ref().ok_or_else(|| {
                    StoreError::Corrupt(format!(
                        "canary policy decision {} has no experiment membership",
                        decision.decision_id
                    ))
                })?;
                let experiment = self
                    .policy_experiment_by_id(&membership.experiment_id)
                    .await?
                    .ok_or_else(|| {
                        StoreError::NotFound(format!(
                            "policy experiment {}",
                            membership.experiment_id
                        ))
                    })?;
                let expected = match decision.source {
                    PolicySelectionSource::CanaryControl => {
                        (ExperimentArm::Control, &experiment.control_policy_id)
                    }
                    PolicySelectionSource::CanaryCandidate => {
                        (ExperimentArm::Candidate, &experiment.candidate_policy_id)
                    }
                    _ => unreachable!(),
                };
                let assigned = self
                    .experiment_assignment(&experiment.experiment_id, &decision.task_revision_id)
                    .await?;
                if experiment.repository != decision.repository
                    || experiment.control_policy_id != decision.active_policy_id
                    || membership.arm != expected.0
                    || decision.selected_policy_id != *expected.1
                    || assigned != Some(expected.0)
                    || decision.manual_override.is_some()
                {
                    return Err(StoreError::Corrupt(format!(
                        "canary policy decision {} contradicts its experiment assignment",
                        decision.decision_id
                    )));
                }
            }
            PolicySelectionSource::Shadow | PolicySelectionSource::NoPolicy => {
                return Err(StoreError::Corrupt(format!(
                    "source `{}` cannot govern an executed policy decision",
                    decision.source
                )));
            }
        }

        sqlx::query(
            "INSERT INTO policy_decisions (
                 decision_id, repository, task_revision_id, active_policy_id,
                 selected_policy_id, policy_fingerprint, source, manual_override,
                 experiment_id, base_commit, created_at, record_json
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        )
        .bind(decision.decision_id.as_str())
        .bind(&decision.repository)
        .bind(decision.task_revision_id.as_str())
        .bind(decision.active_policy_id.as_str())
        .bind(decision.selected_policy_id.as_str())
        .bind(&decision.policy_fingerprint)
        .bind(decision.source.as_str())
        .bind(decision.manual_override.as_deref())
        .bind(
            decision
                .experiment
                .as_ref()
                .map(|membership| membership.experiment_id.as_str()),
        )
        .bind(decision.base_commit.as_deref())
        .bind(decision.created_at.to_rfc3339())
        .bind(serde_json::to_string(decision)?)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    pub async fn policy_decision_by_id(
        &self,
        id: &PolicyDecisionId,
    ) -> StoreResult<Option<PolicyDecision>> {
        let record: Option<String> =
            sqlx::query_scalar("SELECT record_json FROM policy_decisions WHERE decision_id = ?1")
                .bind(id.as_str())
                .fetch_optional(self.pool())
                .await?;
        record
            .map(|json| Ok(serde_json::from_str(&json)?))
            .transpose()
    }

    /// Decisions for a repository, most recent first.
    pub async fn policy_decisions(
        &self,
        repository: &str,
        limit: u32,
    ) -> StoreResult<Vec<PolicyDecision>> {
        let rows: Vec<String> = sqlx::query_scalar(
            "SELECT record_json FROM policy_decisions
             WHERE repository = ?1 ORDER BY created_at DESC, decision_id DESC LIMIT ?2",
        )
        .bind(repository)
        .bind(limit)
        .fetch_all(self.pool())
        .await?;
        rows.into_iter()
            .map(|json| Ok(serde_json::from_str(&json)?))
            .collect()
    }

    /// Links a run to the policy and decision that governed it.
    ///
    /// Only ever called for executions Phase 8 actually governed; historical
    /// runs keep NULL columns. A missing run is an error rather than a silent
    /// no-op, because "the link was written" and "no row matched" must not look
    /// the same to a caller.
    pub async fn link_run_to_policy(
        &self,
        run_id: &RunId,
        policy_id: &PolicyId,
        fingerprint: &str,
        decision_id: &PolicyDecisionId,
    ) -> StoreResult<()> {
        let decision = self
            .policy_decision_by_id(decision_id)
            .await?
            .ok_or_else(|| StoreError::NotFound(format!("policy decision {decision_id}")))?;
        if decision.selected_policy_id != *policy_id || decision.policy_fingerprint != fingerprint {
            return Err(StoreError::Corrupt(format!(
                "run {run_id} linkage does not match policy decision {decision_id}"
            )));
        }
        let row = sqlx::query(
            "SELECT task_revision_id, policy_id, policy_fingerprint, policy_decision_id
             FROM runs WHERE run_id = ?1",
        )
        .bind(run_id.as_str())
        .fetch_optional(self.pool())
        .await?
        .ok_or_else(|| StoreError::NotFound(format!("run {run_id}")))?;
        if row.try_get::<String, _>("task_revision_id")? != decision.task_revision_id.as_str() {
            return Err(StoreError::Corrupt(format!(
                "run {run_id} and policy decision {decision_id} name different task revisions"
            )));
        }
        let existing = (
            row.try_get::<Option<String>, _>("policy_id")?,
            row.try_get::<Option<String>, _>("policy_fingerprint")?,
            row.try_get::<Option<String>, _>("policy_decision_id")?,
        );
        if existing.0.is_some()
            && existing
                != (
                    Some(policy_id.to_string()),
                    Some(fingerprint.to_string()),
                    Some(decision_id.to_string()),
                )
        {
            return Err(StoreError::Corrupt(format!(
                "run {run_id} is already linked to a different policy decision"
            )));
        }
        let result = sqlx::query(
            "UPDATE runs SET policy_id = ?2, policy_fingerprint = ?3, policy_decision_id = ?4
             WHERE run_id = ?1",
        )
        .bind(run_id.as_str())
        .bind(policy_id.as_str())
        .bind(fingerprint)
        .bind(decision_id.as_str())
        .execute(self.pool())
        .await?;
        debug_assert_eq!(result.rows_affected(), 1);
        Ok(())
    }

    /// Policy linkage projected from a run without changing the historical
    /// provider-neutral run document.
    pub async fn run_policy_link(
        &self,
        run_id: &RunId,
    ) -> StoreResult<Option<(PolicyId, String, PolicyDecisionId)>> {
        let row = sqlx::query(
            "SELECT policy_id, policy_fingerprint, policy_decision_id
             FROM runs WHERE run_id = ?1",
        )
        .bind(run_id.as_str())
        .fetch_optional(self.pool())
        .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        match (
            row.try_get::<Option<String>, _>("policy_id")?,
            row.try_get::<Option<String>, _>("policy_fingerprint")?,
            row.try_get::<Option<String>, _>("policy_decision_id")?,
        ) {
            (None, None, None) => Ok(None),
            (Some(policy), Some(fingerprint), Some(decision)) => Ok(Some((
                PolicyId::new(policy).map_err(|error| StoreError::Corrupt(error.to_string()))?,
                fingerprint,
                PolicyDecisionId::new(decision)
                    .map_err(|error| StoreError::Corrupt(error.to_string()))?,
            ))),
            _ => Err(StoreError::Corrupt(format!(
                "run {run_id} has incomplete policy linkage"
            ))),
        }
    }

    // ------------------------------------------------------------------ shadow

    /// Records what a shadow policy would have chosen.
    pub async fn insert_shadow_decision(&self, shadow: &ShadowDecision) -> StoreResult<()> {
        let active = self
            .policy_by_id(&shadow.active_policy_id)
            .await?
            .ok_or_else(|| StoreError::NotFound(format!("policy {}", shadow.active_policy_id)))?;
        let shadow_policy = self
            .policy_by_id(&shadow.shadow_policy_id)
            .await?
            .ok_or_else(|| StoreError::NotFound(format!("policy {}", shadow.shadow_policy_id)))?;
        if active.repository != shadow.repository
            || shadow_policy.repository != shadow.repository
            || shadow_policy.fingerprint() != shadow.shadow_policy_fingerprint
        {
            return Err(StoreError::Corrupt(format!(
                "shadow decision {} does not match its repository-scoped policies",
                shadow.decision_id
            )));
        }
        let task_repository: Option<String> =
            sqlx::query_scalar("SELECT repository FROM task_revisions WHERE revision_id = ?1")
                .bind(shadow.task_revision_id.as_str())
                .fetch_optional(self.pool())
                .await?;
        if task_repository.as_deref() != Some(shadow.repository.as_str()) {
            return Err(StoreError::Corrupt(format!(
                "shadow decision {} task revision is not scoped to `{}`",
                shadow.decision_id, shadow.repository
            )));
        }
        sqlx::query(
            "INSERT INTO policy_shadow_decisions (
                 decision_id, repository, task_revision_id, active_policy_id,
                 shadow_policy_id, shadow_policy_fingerprint, actual_selection,
                 shadow_selection, agreed, created_at, record_json
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        )
        .bind(shadow.decision_id.as_str())
        .bind(&shadow.repository)
        .bind(shadow.task_revision_id.as_str())
        .bind(shadow.active_policy_id.as_str())
        .bind(shadow.shadow_policy_id.as_str())
        .bind(&shadow.shadow_policy_fingerprint)
        .bind(&shadow.actual_selection)
        .bind(&shadow.shadow_selection)
        .bind(i64::from(shadow.agreed))
        .bind(shadow.created_at.to_rfc3339())
        .bind(serde_json::to_string(shadow)?)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    pub async fn shadow_decisions(
        &self,
        repository: &str,
        limit: u32,
    ) -> StoreResult<Vec<ShadowDecision>> {
        let rows: Vec<String> = sqlx::query_scalar(
            "SELECT record_json FROM policy_shadow_decisions
             WHERE repository = ?1 ORDER BY created_at DESC, decision_id DESC LIMIT ?2",
        )
        .bind(repository)
        .bind(limit)
        .fetch_all(self.pool())
        .await?;
        rows.into_iter()
            .map(|json| Ok(serde_json::from_str(&json)?))
            .collect()
    }

    // ------------------------------------------------------------- experiments

    pub async fn insert_policy_experiment(&self, experiment: &PolicyExperiment) -> StoreResult<()> {
        let control = self
            .policy_by_id(&experiment.control_policy_id)
            .await?
            .ok_or_else(|| {
                StoreError::NotFound(format!("policy {}", experiment.control_policy_id))
            })?;
        let candidate = self
            .policy_by_id(&experiment.candidate_policy_id)
            .await?
            .ok_or_else(|| {
                StoreError::NotFound(format!("policy {}", experiment.candidate_policy_id))
            })?;
        if control.repository != experiment.repository
            || candidate.repository != experiment.repository
            || candidate.parent_policy_id.as_ref() != Some(&control.policy_id)
        {
            return Err(StoreError::Corrupt(format!(
                "policy experiment {} does not compare a repository-scoped direct successor",
                experiment.experiment_id
            )));
        }
        if experiment.assignment.candidate_share_percent > 100 {
            return Err(StoreError::Corrupt(format!(
                "policy experiment {} has an invalid candidate share",
                experiment.experiment_id
            )));
        }
        if let Some(proposal_id) = &experiment.proposal_id {
            let proposal = self
                .policy_proposal_by_id(proposal_id)
                .await?
                .ok_or_else(|| StoreError::NotFound(format!("proposal {proposal_id}")))?;
            if proposal.active_policy_id != control.policy_id
                || proposal.candidate_policy_id != candidate.policy_id
            {
                return Err(StoreError::Corrupt(format!(
                    "policy experiment {} does not match proposal {proposal_id}",
                    experiment.experiment_id
                )));
            }
        }
        sqlx::query(
            "INSERT INTO policy_experiments (
                 experiment_id, repository, control_policy_id, candidate_policy_id,
                 assignment_version, candidate_share_percent, status, started_at, concluded_at,
                 proposal_id, record_json
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        )
        .bind(experiment.experiment_id.as_str())
        .bind(&experiment.repository)
        .bind(experiment.control_policy_id.as_str())
        .bind(experiment.candidate_policy_id.as_str())
        .bind(&experiment.assignment.version)
        .bind(experiment.assignment.candidate_share_percent as i64)
        .bind(experiment.status.as_str())
        .bind(experiment.started_at.to_rfc3339())
        .bind(experiment.concluded_at.map(|at| at.to_rfc3339()))
        .bind(experiment.proposal_id.as_ref().map(|id| id.as_str()))
        .bind(serde_json::to_string(experiment)?)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    pub async fn policy_experiment_by_id(
        &self,
        id: &PolicyExperimentId,
    ) -> StoreResult<Option<PolicyExperiment>> {
        let row = sqlx::query(
            "SELECT record_json, status, concluded_at FROM policy_experiments
             WHERE experiment_id = ?1",
        )
        .bind(id.as_str())
        .fetch_optional(self.pool())
        .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let mut experiment: PolicyExperiment =
            serde_json::from_str(&row.try_get::<String, _>("record_json")?)?;
        // Status and conclusion time are the two things that move; everything
        // else about an experiment is fixed when it starts.
        experiment.status = parse_enum(&row.try_get::<String, _>("status")?)?;
        experiment.concluded_at = row
            .try_get::<Option<String>, _>("concluded_at")?
            .as_deref()
            .map(parse_time)
            .transpose()?;
        Ok(Some(experiment))
    }

    /// The open experiment for a repository, if one is running.
    pub async fn active_policy_experiment(
        &self,
        repository: &str,
    ) -> StoreResult<Option<PolicyExperiment>> {
        let id: Option<String> = sqlx::query_scalar(
            "SELECT experiment_id FROM policy_experiments
             WHERE repository = ?1 AND status = ?2
             ORDER BY started_at DESC LIMIT 1",
        )
        .bind(repository)
        .bind(PolicyExperimentStatus::Running.as_str())
        .fetch_optional(self.pool())
        .await?;
        match id {
            Some(raw) => {
                let id = PolicyExperimentId::new(raw)
                    .map_err(|error| StoreError::Corrupt(error.to_string()))?;
                self.policy_experiment_by_id(&id).await
            }
            None => Ok(None),
        }
    }

    /// Experiments for a repository, most recent first.
    pub async fn policy_experiments(
        &self,
        repository: &str,
        limit: u32,
    ) -> StoreResult<Vec<PolicyExperiment>> {
        let ids: Vec<String> = sqlx::query_scalar(
            "SELECT experiment_id FROM policy_experiments
             WHERE repository = ?1 ORDER BY started_at DESC, experiment_id DESC LIMIT ?2",
        )
        .bind(repository)
        .bind(limit)
        .fetch_all(self.pool())
        .await?;

        let mut experiments = Vec::with_capacity(ids.len());
        for raw in ids {
            let id = PolicyExperimentId::new(raw)
                .map_err(|error| StoreError::Corrupt(error.to_string()))?;
            if let Some(experiment) = self.policy_experiment_by_id(&id).await? {
                experiments.push(experiment);
            }
        }
        Ok(experiments)
    }

    /// Concludes or cancels an experiment.
    pub async fn set_policy_experiment_status(
        &self,
        id: &PolicyExperimentId,
        status: PolicyExperimentStatus,
        concluded_at: Option<DateTime<Utc>>,
    ) -> StoreResult<()> {
        let experiment = self
            .policy_experiment_by_id(id)
            .await?
            .ok_or_else(|| StoreError::NotFound(format!("policy experiment {id}")))?;
        let valid = experiment.status == status
            || matches!(
                (experiment.status, status),
                (
                    PolicyExperimentStatus::Running,
                    PolicyExperimentStatus::ExecutionComplete
                        | PolicyExperimentStatus::Concluded
                        | PolicyExperimentStatus::Cancelled
                ) | (
                    PolicyExperimentStatus::ExecutionComplete,
                    PolicyExperimentStatus::Concluded | PolicyExperimentStatus::Cancelled
                )
            );
        if !valid {
            return Err(StoreError::Corrupt(format!(
                "invalid policy experiment transition for {id}: {} -> {status}",
                experiment.status
            )));
        }
        if status == PolicyExperimentStatus::Running && concluded_at.is_some() {
            return Err(StoreError::Corrupt(format!(
                "running policy experiment {id} cannot have a conclusion time"
            )));
        }
        let result = sqlx::query(
            "UPDATE policy_experiments SET status = ?2, concluded_at = ?3
             WHERE experiment_id = ?1",
        )
        .bind(id.as_str())
        .bind(status.as_str())
        .bind(concluded_at.map(|at| at.to_rfc3339()))
        .execute(self.pool())
        .await?;
        if result.rows_affected() == 0 {
            return Err(StoreError::NotFound(format!("policy experiment {id}")));
        }
        Ok(())
    }

    /// Records a deterministic arm assignment.
    ///
    /// Re-recording is a no-op and the stored arm is returned, so an assignment
    /// can never silently flip between two executions of the same task revision.
    pub async fn record_experiment_assignment(
        &self,
        assignment: &ExperimentAssignment,
    ) -> StoreResult<ExperimentArm> {
        if let Some(row) = sqlx::query(
            "SELECT arm, assignment_version FROM policy_experiment_assignments
             WHERE experiment_id = ?1 AND task_revision_id = ?2",
        )
        .bind(assignment.experiment_id.as_str())
        .bind(assignment.task_revision_id.as_str())
        .fetch_optional(self.pool())
        .await?
        {
            let stored_arm: ExperimentArm = parse_enum(&row.try_get::<String, _>("arm")?)?;
            let stored_version: String = row.try_get("assignment_version")?;
            if stored_arm != assignment.arm || stored_version != assignment.assignment_version {
                return Err(StoreError::Corrupt(format!(
                    "policy experiment {} task {} already has a different immutable assignment",
                    assignment.experiment_id, assignment.task_revision_id
                )));
            }
            return Ok(stored_arm);
        }

        let experiment = self
            .policy_experiment_by_id(&assignment.experiment_id)
            .await?
            .ok_or_else(|| {
                StoreError::NotFound(format!("policy experiment {}", assignment.experiment_id))
            })?;
        if !experiment.is_open() {
            return Err(StoreError::Corrupt(format!(
                "policy experiment {} is not running",
                experiment.experiment_id
            )));
        }
        let expected = experiment.arm_for(&assignment.task_revision_id);
        if assignment.assignment_version != experiment.assignment.version
            || assignment.arm != expected
        {
            return Err(StoreError::Corrupt(format!(
                "assignment for experiment {} task {} does not match its deterministic rule",
                assignment.experiment_id, assignment.task_revision_id
            )));
        }
        let assigned = self
            .experiment_assignment_count(&assignment.experiment_id)
            .await?;
        if !experiment
            .budget
            .permits(assigned, 0, assignment.assigned_at)
        {
            return Err(StoreError::Corrupt(format!(
                "policy experiment {} has exhausted its task or time budget",
                assignment.experiment_id
            )));
        }

        sqlx::query(
            "INSERT INTO policy_experiment_assignments (
                 experiment_id, task_revision_id, arm, assignment_version, assigned_at
             ) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT (experiment_id, task_revision_id) DO NOTHING",
        )
        .bind(assignment.experiment_id.as_str())
        .bind(assignment.task_revision_id.as_str())
        .bind(assignment.arm.as_str())
        .bind(&assignment.assignment_version)
        .bind(assignment.assigned_at.to_rfc3339())
        .execute(self.pool())
        .await?;

        // Return what is now recorded. A concurrent writer can only have
        // inserted the same deterministic arm under the experiment's immutable
        // rule; if it did not, the check below catches ledger corruption.
        let stored: String = sqlx::query_scalar(
            "SELECT arm FROM policy_experiment_assignments
             WHERE experiment_id = ?1 AND task_revision_id = ?2",
        )
        .bind(assignment.experiment_id.as_str())
        .bind(assignment.task_revision_id.as_str())
        .fetch_one(self.pool())
        .await?;
        let stored = parse_enum(&stored)?;
        if stored != expected {
            return Err(StoreError::Corrupt(format!(
                "policy experiment {} stored a non-deterministic arm",
                assignment.experiment_id
            )));
        }
        Ok(stored)
    }

    pub async fn experiment_assignment(
        &self,
        experiment_id: &PolicyExperimentId,
        task_revision_id: &TaskRevisionId,
    ) -> StoreResult<Option<ExperimentArm>> {
        let stored: Option<String> = sqlx::query_scalar(
            "SELECT arm FROM policy_experiment_assignments
             WHERE experiment_id = ?1 AND task_revision_id = ?2",
        )
        .bind(experiment_id.as_str())
        .bind(task_revision_id.as_str())
        .fetch_optional(self.pool())
        .await?;
        stored.map(|arm| parse_enum(&arm)).transpose()
    }

    /// Attaches a run to an experiment arm.
    pub async fn record_experiment_observation(
        &self,
        experiment_id: &PolicyExperimentId,
        run_id: &RunId,
        arm: ExperimentArm,
    ) -> StoreResult<()> {
        let experiment = self
            .policy_experiment_by_id(experiment_id)
            .await?
            .ok_or_else(|| StoreError::NotFound(format!("policy experiment {experiment_id}")))?;
        let row = sqlx::query(
            "SELECT r.task_revision_id, r.policy_id, r.policy_decision_id, d.source
             FROM runs r
             LEFT JOIN policy_decisions d ON d.decision_id = r.policy_decision_id
             WHERE r.run_id = ?1",
        )
        .bind(run_id.as_str())
        .fetch_optional(self.pool())
        .await?
        .ok_or_else(|| StoreError::NotFound(format!("run {run_id}")))?;
        let task_revision =
            TaskRevisionId::from_stored(row.try_get::<String, _>("task_revision_id")?)
                .map_err(|error| StoreError::Corrupt(error.to_string()))?;
        let assigned = self
            .experiment_assignment(experiment_id, &task_revision)
            .await?;
        if assigned != Some(arm) {
            return Err(StoreError::Corrupt(format!(
                "run {run_id} arm does not match its persisted experiment assignment"
            )));
        }
        let expected_policy = match arm {
            ExperimentArm::Control => &experiment.control_policy_id,
            ExperimentArm::Candidate => &experiment.candidate_policy_id,
        };
        let policy: Option<String> = row.try_get("policy_id")?;
        let source: Option<String> = row.try_get("source")?;
        let expected_source = match arm {
            ExperimentArm::Control => PolicySelectionSource::CanaryControl,
            ExperimentArm::Candidate => PolicySelectionSource::CanaryCandidate,
        };
        if policy.as_deref() != Some(expected_policy.as_str())
            || source.as_deref() != Some(expected_source.as_str())
        {
            return Err(StoreError::Corrupt(format!(
                "run {run_id} was not a policy-controlled {arm} execution"
            )));
        }
        sqlx::query(
            "INSERT INTO policy_experiment_observations (experiment_id, run_id, arm, recorded_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (experiment_id, run_id) DO NOTHING",
        )
        .bind(experiment_id.as_str())
        .bind(run_id.as_str())
        .bind(arm.as_str())
        .bind(Utc::now().to_rfc3339())
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// Observations recorded on each arm, for budget enforcement and reporting.
    pub async fn experiment_observations(
        &self,
        experiment_id: &PolicyExperimentId,
    ) -> StoreResult<Vec<(RunId, ExperimentArm)>> {
        let rows = sqlx::query(
            "SELECT run_id, arm FROM policy_experiment_observations
             WHERE experiment_id = ?1 ORDER BY recorded_at, run_id",
        )
        .bind(experiment_id.as_str())
        .fetch_all(self.pool())
        .await?;
        rows.into_iter()
            .map(|row| {
                Ok((
                    RunId::new(row.try_get::<String, _>("run_id")?)
                        .map_err(|error| StoreError::Corrupt(error.to_string()))?,
                    parse_enum(&row.try_get::<String, _>("arm")?)?,
                ))
            })
            .collect()
    }

    /// How many task revisions the experiment has assigned.
    pub async fn experiment_assignment_count(
        &self,
        experiment_id: &PolicyExperimentId,
    ) -> StoreResult<u32> {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM policy_experiment_assignments WHERE experiment_id = ?1",
        )
        .bind(experiment_id.as_str())
        .fetch_one(self.pool())
        .await?;
        Ok(count as u32)
    }

    // ---------------------------------------------------------------- evidence

    /// Every run the ledger holds, split by whether it existed at `cutoff`.
    ///
    /// Deliberately unfiltered beyond the cutoff split: the resolver must give
    /// a reason for every observation it does not use, and a `WHERE` clause
    /// that quietly dropped rows would make those reasons unrecordable.
    pub async fn policy_run_evidence(
        &self,
        cutoff: DateTime<Utc>,
        limit: u32,
    ) -> StoreResult<PolicyRunEvidenceSet> {
        let cutoff_text = cutoff.to_rfc3339();

        let after_cutoff: Vec<String> = sqlx::query_scalar(
            "SELECT run_id FROM runs
             WHERE COALESCE(finished_at, created_at) > ?1
             ORDER BY COALESCE(finished_at, created_at) DESC, run_id DESC",
        )
        .bind(&cutoff_text)
        .fetch_all(self.pool())
        .await?;
        let after_cutoff = after_cutoff
            .into_iter()
            .map(|raw| RunId::new(raw).map_err(|error| StoreError::Corrupt(error.to_string())))
            .collect::<StoreResult<Vec<_>>>()?;

        let beyond_limit: Vec<String> = sqlx::query_scalar(
            "SELECT run_id FROM runs
             WHERE COALESCE(finished_at, created_at) <= ?1
             ORDER BY COALESCE(finished_at, created_at) DESC, run_id DESC
             LIMIT -1 OFFSET ?2",
        )
        .bind(&cutoff_text)
        .bind(limit)
        .fetch_all(self.pool())
        .await?;
        let beyond_limit = beyond_limit
            .into_iter()
            .map(|raw| RunId::new(raw).map_err(|error| StoreError::Corrupt(error.to_string())))
            .collect::<StoreResult<Vec<_>>>()?;

        let rows = sqlx::query(
            "SELECT r.record_json AS record_json,
                    r.task_revision_id AS task_revision_id,
                    r.policy_id AS policy_id,
                    r.policy_fingerprint AS policy_fingerprint,
                    tr.repository AS repository,
                    d.source AS decision_source,
                    d.manual_override AS manual_override,
                    d.experiment_id AS experiment_id,
                    o.arm AS experiment_arm
             FROM runs r
             JOIN task_revisions tr ON tr.revision_id = r.task_revision_id
             LEFT JOIN policy_decisions d ON d.decision_id = r.policy_decision_id
             LEFT JOIN policy_experiment_observations o
               ON o.run_id = r.run_id
              AND o.experiment_id = d.experiment_id
              AND o.recorded_at <= ?1
             WHERE COALESCE(r.finished_at, r.created_at) <= ?1
             ORDER BY COALESCE(r.finished_at, r.created_at) DESC, r.run_id DESC
             LIMIT ?2",
        )
        .bind(&cutoff_text)
        .bind(limit)
        .fetch_all(self.pool())
        .await?;

        let mut available = Vec::with_capacity(rows.len());
        for row in rows {
            let run: AgentRun = serde_json::from_str(&row.try_get::<String, _>("record_json")?)?;
            let task_revision_id =
                TaskRevisionId::from_stored(row.try_get::<String, _>("task_revision_id")?)
                    .map_err(|error| StoreError::Corrupt(error.to_string()))?;

            let experiment = match (
                row.try_get::<Option<String>, _>("experiment_id")?,
                row.try_get::<Option<String>, _>("experiment_arm")?,
            ) {
                (Some(id), Some(arm)) => Some(ExperimentMembership {
                    experiment_id: PolicyExperimentId::new(id)
                        .map_err(|error| StoreError::Corrupt(error.to_string()))?,
                    arm: parse_enum(&arm)?,
                }),
                _ => None,
            };

            available.push(PolicyRunEvidence {
                run,
                repository: row.try_get("repository")?,
                task_revision_id,
                policy_id: row
                    .try_get::<Option<String>, _>("policy_id")?
                    .map(PolicyId::new)
                    .transpose()
                    .map_err(|error| StoreError::Corrupt(error.to_string()))?,
                policy_fingerprint: row.try_get("policy_fingerprint")?,
                experiment,
                decision_source: row
                    .try_get::<Option<String>, _>("decision_source")?
                    .as_deref()
                    .map(parse_enum)
                    .transpose()?,
                manual_override: row.try_get("manual_override")?,
            });
        }

        Ok(PolicyRunEvidenceSet {
            available,
            after_cutoff,
            beyond_limit,
        })
    }

    /// Health snapshot identities available at `cutoff`, oldest first.
    ///
    /// Ids and commits only. What a snapshot *measured* is health's business;
    /// what policy needs to know is which measurements existed in time.
    pub async fn policy_health_evidence(
        &self,
        repository: &str,
        cutoff: DateTime<Utc>,
        limit: u32,
    ) -> StoreResult<Vec<(HealthSnapshotId, String, DateTime<Utc>)>> {
        let rows = sqlx::query(
            "SELECT health_snapshot_id, commit_hash, created_at
             FROM repository_health_snapshots
             WHERE repository = ?1 AND created_at <= ?2 AND status = 'complete'
             ORDER BY created_at ASC, health_snapshot_id ASC
             LIMIT ?3",
        )
        .bind(repository)
        .bind(cutoff.to_rfc3339())
        .bind(limit)
        .fetch_all(self.pool())
        .await?;

        rows.into_iter()
            .map(|row| {
                Ok((
                    HealthSnapshotId::new(row.try_get::<String, _>("health_snapshot_id")?)
                        .map_err(|error| StoreError::Corrupt(error.to_string()))?,
                    row.try_get("commit_hash")?,
                    parse_time(&row.try_get::<String, _>("created_at")?)?,
                ))
            })
            .collect()
    }

    // ------------------------------------------------------------------ events

    pub async fn append_policy_events(&self, events: &[PolicyEvent]) -> StoreResult<usize> {
        if events.is_empty() {
            return Ok(0);
        }
        let mut transaction = self.pool().begin().await?;
        let mut written = 0;
        for event in events {
            let result = sqlx::query(
                "INSERT INTO policy_events (
                     subject_kind, subject_id, seq, timestamp, event_type, data_json
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT (subject_kind, subject_id, seq) DO NOTHING",
            )
            .bind(event.subject.kind())
            .bind(event.subject.id())
            .bind(event.seq as i64)
            .bind(event.timestamp.to_rfc3339())
            .bind(event.payload.event_type())
            .bind(serde_json::to_string(&event.payload)?)
            .execute(&mut *transaction)
            .await?;
            written += result.rows_affected() as usize;
        }
        transaction.commit().await?;
        Ok(written)
    }

    pub async fn policy_events(
        &self,
        subject: &PolicyEventSubject,
    ) -> StoreResult<Vec<PolicyEvent>> {
        let rows = sqlx::query(
            "SELECT seq, timestamp, data_json FROM policy_events
             WHERE subject_kind = ?1 AND subject_id = ?2 ORDER BY seq",
        )
        .bind(subject.kind())
        .bind(subject.id())
        .fetch_all(self.pool())
        .await?;

        rows.into_iter()
            .map(|row| {
                Ok(PolicyEvent {
                    subject: subject.clone(),
                    seq: row.try_get::<i64, _>("seq")? as u64,
                    timestamp: parse_time(&row.try_get::<String, _>("timestamp")?)?,
                    payload: serde_json::from_str(&row.try_get::<String, _>("data_json")?)?,
                })
            })
            .collect()
    }

    /// The next event sequence number for a subject.
    pub async fn next_policy_event_seq(&self, subject: &PolicyEventSubject) -> StoreResult<u64> {
        let max: Option<i64> = sqlx::query_scalar(
            "SELECT MAX(seq) FROM policy_events WHERE subject_kind = ?1 AND subject_id = ?2",
        )
        .bind(subject.kind())
        .bind(subject.id())
        .fetch_one(self.pool())
        .await?;
        Ok(max.unwrap_or(0) as u64 + 1)
    }
}

/// Reads a column written with a type's `as_str()` back into that type.
///
/// Only used for enums whose `as_str()` and serde discriminant agree, which is
/// asserted by test rather than assumed.
fn parse_enum<T: serde::de::DeserializeOwned>(raw: &str) -> StoreResult<T> {
    Ok(serde_json::from_str(
        &serde_json::Value::String(raw.to_string()).to_string(),
    )?)
}

fn parse_time(raw: &str) -> StoreResult<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw)
        .map(|time| time.with_timezone(&Utc))
        .map_err(|error| StoreError::Corrupt(format!("invalid timestamp `{raw}`: {error}")))
}

async fn insert_policy_event(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    event: &PolicyEvent,
) -> StoreResult<()> {
    sqlx::query(
        "INSERT INTO policy_events (
             subject_kind, subject_id, seq, timestamp, event_type, data_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    )
    .bind(event.subject.kind())
    .bind(event.subject.id())
    .bind(event.seq as i64)
    .bind(event.timestamp.to_rfc3339())
    .bind(event.payload.event_type())
    .bind(serde_json::to_string(&event.payload)?)
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use forge_core::agent::AgentConfig;
    use forge_core::config::ForgeConfig;
    use forge_core::ids::{AgentId, RunId, TaskId};
    use forge_core::optimization::{
        AssignmentRule, ExperimentAssignment, ExperimentBudget, PolicyEventPayload,
        PolicyExperiment,
    };
    use forge_core::policy::{EngineeringPolicy, PolicyBounds, PolicyProvenance};
    use forge_core::run::{AgentRun, ExecutionProvenance, SelectionSource};
    use forge_core::task::{EngineeringTask, TaskMetadata};
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

    use super::*;

    fn bootstrap_event(policy: &EngineeringPolicy) -> PolicyEvent {
        PolicyEvent {
            subject: PolicyEventSubject::Policy(policy.policy_id.clone()),
            seq: 1,
            timestamp: policy.created_at,
            payload: PolicyEventPayload::PolicyCreated {
                provenance: policy.provenance.as_str().into(),
                fingerprint: policy.fingerprint(),
            },
        }
    }

    fn task(number: u64) -> EngineeringTask {
        EngineeringTask {
            task_id: TaskId::sequential(number),
            repository: "forge".into(),
            objective: "exercise policy persistence".into(),
            constraints: Vec::new(),
            evaluation: Default::default(),
            protection: Default::default(),
            metadata: TaskMetadata::default(),
            classification: Default::default(),
            components: Vec::new(),
            tags: Vec::new(),
        }
    }

    #[tokio::test]
    async fn bootstrap_pointer_history_and_immutable_provenance_are_durable() {
        let store = Store::open_in_memory().await.unwrap();
        let config = ForgeConfig::default_for("forge");
        let policy = EngineeringPolicy::bootstrap_from_config(
            store.next_policy_id().await.unwrap(),
            &config,
        );
        policy.validate(&PolicyBounds::for_config(&config)).unwrap();
        store
            .install_bootstrap_policy(&policy, &bootstrap_event(&policy))
            .await
            .unwrap();

        assert_eq!(store.active_policy("forge").await.unwrap().unwrap(), policy);
        let history = store.policy_history("forge", 10).await.unwrap();
        assert_eq!(history.len(), 1);
        assert!(history[0].is_active);
        assert_eq!(
            store
                .policy_events(&PolicyEventSubject::Policy(policy.policy_id.clone()))
                .await
                .unwrap()
                .len(),
            1
        );

        let mut rewritten = policy.clone();
        rewritten.provenance = PolicyProvenance::Imported;
        assert!(store.insert_policy(&rewritten).await.is_err());
    }

    #[tokio::test]
    async fn experiment_assignments_are_persisted_and_cannot_flip() {
        let store = Store::open_in_memory().await.unwrap();
        let config = ForgeConfig::default_for("forge");
        let active = EngineeringPolicy::bootstrap_from_config(
            store.next_policy_id().await.unwrap(),
            &config,
        );
        store
            .install_bootstrap_policy(&active, &bootstrap_event(&active))
            .await
            .unwrap();
        let mut candidate = active.clone();
        candidate.policy_id = store.next_policy_id().await.unwrap();
        candidate.parent_policy_id = Some(active.policy_id.clone());
        candidate.status = PolicyStatus::Canary;
        candidate.provenance = PolicyProvenance::OptimizerProposed;
        candidate.context.max_world_facts -= 1;
        store.insert_policy(&candidate).await.unwrap();
        let experiment = PolicyExperiment {
            experiment_id: store.next_policy_experiment_id().await.unwrap(),
            repository: "forge".into(),
            control_policy_id: active.policy_id.clone(),
            candidate_policy_id: candidate.policy_id.clone(),
            assignment: AssignmentRule::new(50),
            budget: ExperimentBudget::default(),
            status: PolicyExperimentStatus::Running,
            started_at: Utc::now(),
            concluded_at: None,
            proposal_id: None,
        };
        store.insert_policy_experiment(&experiment).await.unwrap();
        let revision = store.upsert_task(&task(1)).await.unwrap();
        let expected = experiment.arm_for(&revision);
        let assignment = ExperimentAssignment {
            experiment_id: experiment.experiment_id.clone(),
            task_revision_id: revision,
            arm: expected,
            assignment_version: experiment.assignment.version.clone(),
            assigned_at: Utc::now(),
        };
        assert_eq!(
            store
                .record_experiment_assignment(&assignment)
                .await
                .unwrap(),
            expected
        );
        let mut flip = assignment;
        flip.arm = match expected {
            ExperimentArm::Control => ExperimentArm::Candidate,
            ExperimentArm::Candidate => ExperimentArm::Control,
        };
        assert!(store.record_experiment_assignment(&flip).await.is_err());
    }

    #[tokio::test]
    async fn a_phase_seven_database_migrates_without_rewriting_its_run() {
        let temp = tempfile::tempdir().unwrap();
        let old_migrations = temp.path().join("old-migrations");
        std::fs::create_dir(&old_migrations).unwrap();
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        for name in [
            "0001_init.sql",
            "0002_run_outcome.sql",
            "0003_experiments.sql",
            "0004_evaluator_results.sql",
            "0005_experience_queries.sql",
            "0006_immutable_task_revisions.sql",
            "0007_execution_provenance.sql",
            "0008_routing_decisions.sql",
            "0009_team_executions.sql",
            "0010_world_model.sql",
            "0011_repository_health.sql",
        ] {
            std::fs::copy(
                manifest.join("migrations").join(name),
                old_migrations.join(name),
            )
            .unwrap();
        }
        let database = temp.path().join("forge.db");
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(&database)
                    .create_if_missing(true)
                    .foreign_keys(true),
            )
            .await
            .unwrap();
        sqlx::migrate::Migrator::new(old_migrations)
            .await
            .unwrap()
            .run(&pool)
            .await
            .unwrap();

        let task = task(7);
        let revision = TaskRevisionId::for_definition("phase-seven-task");
        sqlx::query(
            "INSERT INTO tasks (task_id, repository, objective, definition_json, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
        )
        .bind(task.task_id.as_str())
        .bind(&task.repository)
        .bind(&task.objective)
        .bind(serde_json::to_string(&task).unwrap())
        .bind("2026-01-01T00:00:00Z")
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO task_revisions (
                 revision_id, task_id, repository, objective, definition_json, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )
        .bind(revision.as_str())
        .bind(task.task_id.as_str())
        .bind(&task.repository)
        .bind(&task.objective)
        .bind(serde_json::to_string(&task).unwrap())
        .bind("2026-01-01T00:00:00Z")
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("UPDATE tasks SET current_revision_id = ?2 WHERE task_id = ?1")
            .bind(task.task_id.as_str())
            .bind(revision.as_str())
            .execute(&pool)
            .await
            .unwrap();
        let agent = AgentConfig::new(AgentId::new("legacy").unwrap(), "legacy");
        sqlx::query(
            "INSERT INTO agent_configs (
                 fingerprint, agent_id, harness, tools_json, settings_json, first_seen_at
             ) VALUES (?1, ?2, ?3, '[]', '{}', ?4)",
        )
        .bind(agent.fingerprint())
        .bind(agent.agent_id.as_str())
        .bind(&agent.harness)
        .bind("2026-01-01T00:00:00Z")
        .execute(&pool)
        .await
        .unwrap();
        let mut run = AgentRun::new(
            RunId::sequential(1),
            task.task_id.clone(),
            agent,
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        );
        run.execution_provenance = ExecutionProvenance::Unknown;
        run.selection_source = SelectionSource::Manual;
        sqlx::query(
            "INSERT INTO runs (
                 run_id, task_id, agent_id, config_fingerprint, base_commit, status,
                 created_at, record_json, task_revision_id, execution_provenance,
                 selection_source
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        )
        .bind(run.run_id.as_str())
        .bind(run.task_id.as_str())
        .bind(run.agent.agent_id.as_str())
        .bind(run.agent.fingerprint())
        .bind(&run.base_commit)
        .bind(run.status.as_str())
        .bind(run.created_at.to_rfc3339())
        .bind(serde_json::to_string(&run).unwrap())
        .bind(revision.as_str())
        .bind(run.execution_provenance.as_str())
        .bind(run.selection_source.as_str())
        .execute(&pool)
        .await
        .unwrap();
        pool.close().await;

        let migrated = Store::open(&database).await.unwrap();
        assert_eq!(
            migrated.load_run(&run.run_id).await.unwrap(),
            Some(run.clone())
        );
        assert!(
            migrated
                .run_policy_link(&run.run_id)
                .await
                .unwrap()
                .is_none()
        );
        assert!(migrated.active_policy("forge").await.unwrap().is_none());
        assert_eq!(migrated.policy_count("forge").await.unwrap(), 0);
    }
}
