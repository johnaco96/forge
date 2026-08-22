//! One attempt by one agent at one task.
//!
//! `AgentRun` is the record Forge keeps whether the attempt succeeded, failed,
//! or was cancelled. Its lifecycle is an explicit state machine so that partial
//! and interrupted runs are still well-formed records rather than gaps.
//!
//! # Three statuses, deliberately not one
//!
//! A run carries three separate judgments, and collapsing them into a single
//! `success` boolean would destroy the distinctions Forge exists to measure:
//!
//! - [`RunStatus`] — where the run got to in Forge's pipeline.
//! - [`AgentExecutionStatus`] — how the agent process itself ended.
//! - [`RunOutcome`] — what Forge concluded about the resulting change.
//!
//! These genuinely diverge. An agent can exit non-zero, or time out, and still
//! leave a patch that passes every check. A timeout or cancellation remains
//! ineligible for PASS even when that partial patch measures green. An agent
//! can exit cleanly having written something broken. All of those states must
//! remain representable without ambiguity.

use std::collections::BTreeMap;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::agent::AgentConfig;
use crate::ids::{ExperimentId, RoutingDecisionId, RunId, TaskId};
use crate::integrity::EvaluationIntegrity;
use crate::patch::{ExcludedEntry, PatchWarning};
use crate::result::Verdict;
use crate::security::SecurityPosture;
use crate::world::WorldModelContextReference;

/// How a run's agent execution was produced.
///
/// This is explicit trust provenance, not something Forge infers from an
/// agent name, executable path, or harness metadata. Historical rows that
/// predate this field deserialize as [`Unknown`](Self::Unknown).
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionProvenance {
    /// A genuine agent execution intended to solve an engineering task.
    Live,
    /// A deterministic fake/stub execution used to validate Forge itself.
    Synthetic,
    /// Evidence imported from outside this Forge ledger.
    Imported,
    /// Provenance could not be established without guessing.
    #[default]
    Unknown,
}

impl ExecutionProvenance {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Synthetic => "synthetic",
            Self::Imported => "imported",
            Self::Unknown => "unknown",
        }
    }
}

impl std::fmt::Display for ExecutionProvenance {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Who selected the agent, independent of how trustworthy its execution is.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum SelectionSource {
    #[default]
    Manual,
    Automatic {
        decision_id: RoutingDecisionId,
        router_version: String,
        evidence_fingerprint: String,
    },
    Competition {
        experiment_id: ExperimentId,
    },
}

impl SelectionSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Automatic { .. } => "auto",
            Self::Competition { .. } => "competition",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RunError {
    #[error("invalid run transition: {from} -> {to}")]
    InvalidTransition { from: RunStatus, to: RunStatus },
}

/// Where a run reached in Forge's pipeline.
///
/// Says nothing about quality: a `Completed` run may have failed every check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    /// Recorded, not yet started.
    Pending,
    /// Workspace being provisioned.
    Preparing,
    /// Agent executing.
    Running,
    /// Agent done; Forge is evaluating the result independently.
    Evaluating,
    /// Reached the end of the pipeline.
    Completed,
    /// Could not be carried through the pipeline (setup error, crash).
    Failed,
    Cancelled,
}

impl RunStatus {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }

    /// Transitions allowed by the lifecycle.
    ///
    /// Failure and cancellation are reachable from any non-terminal state: an
    /// agent can die at any point, and the record must still close cleanly.
    pub fn can_transition_to(self, next: RunStatus) -> bool {
        use RunStatus::*;
        if self.is_terminal() {
            return false;
        }
        if matches!(next, Failed | Cancelled) {
            return true;
        }
        matches!(
            (self, next),
            (Pending, Preparing)
                | (Preparing, Running)
                | (Running, Evaluating)
                | (Running, Completed)
                | (Evaluating, Completed)
        )
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Preparing => "preparing",
            Self::Running => "running",
            Self::Evaluating => "evaluating",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

impl std::fmt::Display for RunStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How the agent process itself ended.
///
/// Entirely about the process, never about the quality of its work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentExecutionStatus {
    /// Exited zero.
    Completed,
    /// Ran to completion but exited non-zero. The workspace may still hold a
    /// perfectly good patch.
    NonZeroExit,
    /// Killed at its timeout. Any partial work it left behind is still
    /// evaluated.
    TimedOut,
    /// Forge could not start the agent at all. Nothing to evaluate.
    StartFailed,
    Cancelled,
}

/// Typed operational failures which are not evidence that candidate code is
/// wrong. Multiple facts may coexist with a patch/integrity/evaluation result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InfrastructureFailureKind {
    DiskExhausted,
    MemoryLimitExceeded,
    CpuLimitExceeded,
    SandboxUnavailable,
    NetworkPolicyViolation,
    CredentialUnavailable,
    CredentialPolicyViolation,
    EvaluatorToolUnavailable,
    WorkspaceCleanupFailed,
    StoreUnavailable,
}

impl InfrastructureFailureKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DiskExhausted => "disk_exhausted",
            Self::MemoryLimitExceeded => "memory_limit_exceeded",
            Self::CpuLimitExceeded => "cpu_limit_exceeded",
            Self::SandboxUnavailable => "sandbox_unavailable",
            Self::NetworkPolicyViolation => "network_policy_violation",
            Self::CredentialUnavailable => "credential_unavailable",
            Self::CredentialPolicyViolation => "credential_policy_violation",
            Self::EvaluatorToolUnavailable => "evaluator_tool_unavailable",
            Self::WorkspaceCleanupFailed => "workspace_cleanup_failed",
            Self::StoreUnavailable => "store_unavailable",
        }
    }
}

/// One independently observable infrastructure fact about a run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("{kind:?}: {detail}")]
pub struct InfrastructureFailure {
    pub kind: InfrastructureFailureKind,
    pub detail: String,
}

impl InfrastructureFailure {
    pub fn new(kind: InfrastructureFailureKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: detail.into(),
        }
    }
}

impl AgentExecutionStatus {
    /// Whether the agent ran far enough to have possibly changed something.
    pub fn produced_work(self) -> bool {
        !matches!(self, Self::StartFailed)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::NonZeroExit => "non_zero_exit",
            Self::TimedOut => "timed_out",
            Self::StartFailed => "start_failed",
            Self::Cancelled => "cancelled",
        }
    }

    /// Wording for the run report.
    pub fn describe(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::NonZeroExit => "exited non-zero",
            Self::TimedOut => "timed out",
            Self::StartFailed => "failed to start",
            Self::Cancelled => "cancelled",
        }
    }
}

impl std::fmt::Display for AgentExecutionStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What the agent process did.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentExecution {
    pub status: AgentExecutionStatus,
    /// `None` when the process was killed rather than exiting on its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub duration_ms: u64,
    /// Captured output, written to the run's artifact directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdout_path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stderr_path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Usage::is_empty")]
    pub usage: Usage,
    /// The agent's own account of what it did.
    ///
    /// **Untrusted.** Recorded as trajectory data because it is useful for
    /// understanding a run, and never consulted when deciding an outcome. An
    /// agent claiming "all tests pass" is not evidence that any test ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub self_report: Option<String>,
    /// Harness-specific details, kept opaque so no adapter needs a core change
    /// to record what it knows.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub harness_metadata: BTreeMap<String, String>,
    /// Operational failures observed while this process ran. These do not
    /// overwrite independent candidate-integrity or evaluation facts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub infrastructure_failures: Vec<InfrastructureFailure>,
}

impl AgentExecution {
    /// Builds a record for an agent Forge could not start.
    pub fn start_failed(at: DateTime<Utc>) -> Self {
        Self {
            status: AgentExecutionStatus::StartFailed,
            exit_code: None,
            timed_out: false,
            started_at: at,
            finished_at: at,
            duration_ms: 0,
            stdout_path: None,
            stderr_path: None,
            usage: Usage::default(),
            self_report: None,
            harness_metadata: BTreeMap::new(),
            infrastructure_failures: Vec::new(),
        }
    }

    /// Classifies a finished process. Exit code and timeout are the only
    /// inputs; nothing the agent said is consulted.
    pub fn classify(exit_code: Option<i32>, timed_out: bool) -> AgentExecutionStatus {
        if timed_out {
            AgentExecutionStatus::TimedOut
        } else if exit_code == Some(0) {
            AgentExecutionStatus::Completed
        } else {
            AgentExecutionStatus::NonZeroExit
        }
    }
}

/// Forge's conclusion about a run.
///
/// Derived from the change and Forge's own measurements — never from the
/// agent's exit code, which is recorded separately in [`AgentExecution`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunOutcome {
    /// A change was produced and every check Forge ran passed.
    Passed,
    /// A change was produced and at least one check failed.
    Failed,
    /// A change was produced but the evidence is incomplete — no checks
    /// configured, or a check that could not be executed.
    Inconclusive,
    /// The agent ran but left the workspace unchanged. Explicitly not a pass:
    /// nothing was attempted, so nothing was verified.
    NoChange,
    /// Forge could not carry the run through its pipeline.
    Errored,
}

impl RunOutcome {
    /// Decides the outcome of a run.
    ///
    /// A normal pass requires three things at once: a candidate patch exists,
    /// Forge's own checks passed, and the evaluation's inputs were not
    /// tampered with. The ordering of these rules is the policy:
    ///
    /// 1. If Forge could not run the agent, nothing else is meaningful.
    /// 2. If no change was produced, there is nothing to judge — an unchanged
    ///    repository trivially passes its own tests, and reporting that as
    ///    success would be the single most misleading thing Forge could do.
    /// 3. A failing check fails the run, whatever else happened. Tampering
    ///    does not rescue a change that broke anyway.
    /// 4. A passing check only passes the run if the evaluation was still
    ///    intact. Deleting a failing test and reporting green measures
    ///    nothing, so the honest answer is that nothing was concluded —
    ///    `Inconclusive`, with the integrity status carrying the detail.
    ///
    /// Note what is absent: the agent's exit status. An agent that crashed
    /// after writing a correct patch passes; an agent that exited cleanly
    /// having broken the build fails.
    pub fn derive(
        execution: Option<&AgentExecution>,
        patch: Option<&PatchSummary>,
        evaluation: Option<Verdict>,
        integrity: Option<&EvaluationIntegrity>,
    ) -> Self {
        let Some(execution) = execution else {
            return Self::Errored;
        };
        if !execution.status.produced_work() {
            return Self::Errored;
        }
        let outcome = match patch {
            None => Self::NoChange,
            Some(patch) if patch.is_empty() => Self::NoChange,
            Some(_) => {
                let intact = integrity.is_none_or(EvaluationIntegrity::is_acceptable);
                match evaluation {
                    Some(Verdict::Fail) => Self::Failed,
                    Some(Verdict::Pass) if intact => Self::Passed,
                    // Measured green, but the measurement was compromised.
                    Some(Verdict::Pass) => Self::Inconclusive,
                    Some(Verdict::Inconclusive) | None => Self::Inconclusive,
                }
            }
        };
        if outcome == Self::Passed
            && matches!(
                execution.status,
                AgentExecutionStatus::TimedOut | AgentExecutionStatus::Cancelled
            )
        {
            Self::Inconclusive
        } else {
            outcome
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::Failed => "failed",
            Self::Inconclusive => "inconclusive",
            Self::NoChange => "no_change",
            Self::Errored => "errored",
        }
    }

    /// Report wording.
    pub fn describe(self) -> &'static str {
        match self {
            Self::Passed => "PASS",
            Self::Failed => "FAIL",
            Self::Inconclusive => "INCONCLUSIVE",
            Self::NoChange => "NO CHANGE",
            Self::Errored => "ERROR",
        }
    }

    /// Whether a shell should treat this run as a success.
    pub fn is_success(self) -> bool {
        self == Self::Passed
    }
}

impl std::fmt::Display for RunOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Token and cost accounting, when the harness reports it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
}

impl Usage {
    pub fn is_empty(&self) -> bool {
        self.input_tokens.is_none() && self.output_tokens.is_none() && self.cost_usd.is_none()
    }

    pub fn total_tokens(&self) -> Option<u64> {
        match (self.input_tokens, self.output_tokens) {
            (None, None) => None,
            (a, b) => Some(a.unwrap_or(0) + b.unwrap_or(0)),
        }
    }
}

/// The change a run produced, summarized.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PatchSummary {
    pub base_commit: String,
    /// Commit the agent left the workspace on, if it committed anything.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_commit: Option<String>,
    pub files_changed: u64,
    pub insertions: u64,
    pub deletions: u64,
    /// Files with no line counts, which are almost always build output that
    /// should have been ignored. Tracked separately because they make
    /// `lines_changed` meaningless without explaining why.
    #[serde(default)]
    pub binary_files: u64,
    /// Where the full diff was written. The diff itself is not held in memory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff_path: Option<PathBuf>,
    /// Workspace changes the patch policy declined to include, with reasons.
    ///
    /// Forge's own judgments are kept in full. Ignored files are sampled, since
    /// re-listing an agent's whole build tree stores the expansion of the
    /// repository's `.gitignore` rather than anything about the run. The exact
    /// number is always in [`Self::excluded_counts`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub excluded: Vec<ExcludedEntry>,
    /// Exact count of exclusions per reason. Never sampled.
    ///
    /// Additive: records written before this field deserialize with it empty,
    /// and [`Self::excluded_total`] falls back to the retained list for them.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub excluded_counts: BTreeMap<String, u64>,
}

impl PatchSummary {
    pub fn is_empty(&self) -> bool {
        self.files_changed == 0 && self.insertions == 0 && self.deletions == 0
    }

    /// Total lines touched; the crude change-size signal used until a better
    /// one exists.
    pub fn lines_changed(&self) -> u64 {
        self.insertions + self.deletions
    }

    /// Whether the change looks like it swept up build output.
    ///
    /// A real code change touches few or no binary files. A patch full of them
    /// almost always means the repository is missing a `.gitignore` entry, and
    /// silently committing a `target/` directory to the run branch is worth
    /// saying out loud.
    pub fn looks_like_build_output(&self) -> bool {
        self.binary_files > 0
    }

    /// How many workspace changes the policy declined to include.
    ///
    /// Reads the exact counts when present, and falls back to the retained list
    /// for records written before counts existed — where the list was complete,
    /// so its length was the exact total.
    pub fn excluded_total(&self) -> u64 {
        if self.excluded_counts.is_empty() {
            self.excluded.len() as u64
        } else {
            self.excluded_counts.values().sum()
        }
    }

    /// Whether the retained exclusion list is a sample rather than the whole set.
    pub fn exclusions_sampled(&self) -> bool {
        self.excluded_total() > self.excluded.len() as u64
    }
}

/// One agent's attempt at one task.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentRun {
    pub run_id: RunId,
    pub task_id: TaskId,
    pub agent: AgentConfig,
    /// Explicit trust provenance for routing and analytical policy.
    #[serde(default)]
    pub execution_provenance: ExecutionProvenance,
    /// Manual, automatic, or competitive selection; never execution trust.
    #[serde(default)]
    pub selection_source: SelectionSource,
    /// The commit every competing run for this task starts from.
    pub base_commit: String,
    /// Exact world-model facts supplied to this run's agent, when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub world_model_context: Option<WorldModelContextReference>,
    pub status: RunStatus,
    pub created_at: DateTime<Utc>,
    /// Set when the agent actually starts, so agent time can be separated from
    /// queueing and setup time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<DateTime<Utc>>,
    /// Why the run failed or was cancelled. Forge's reason, not the agent's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_reason: Option<String>,
    /// Typed operational causes retained in addition to `failure_reason` and
    /// candidate/evaluator facts. More than one may apply to the same run.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub infrastructure_failures: Vec<InfrastructureFailure>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// How the agent process ended.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution: Option<AgentExecution>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patch: Option<PatchSummary>,
    /// Forge's independent verdict, once evaluation has run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evaluation_verdict: Option<Verdict>,
    /// Whether the evaluation's own inputs survived the run intact.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub integrity: Option<EvaluationIntegrity>,
    /// Structured observations about the patch and the evaluation inputs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<PatchWarning>,
    /// What was and was not being protected against during this run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub security: Option<SecurityPosture>,
    /// Forge's conclusion. Set when the run finalizes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<RunOutcome>,
    #[serde(default)]
    pub artifacts: RunArtifacts,
}

/// Where a run's captured output lives on disk.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RunArtifacts {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub directory: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trajectory_path: Option<PathBuf>,
}

impl AgentRun {
    pub fn new(
        run_id: RunId,
        task_id: TaskId,
        agent: AgentConfig,
        base_commit: impl Into<String>,
    ) -> Self {
        Self {
            run_id,
            task_id,
            agent,
            execution_provenance: ExecutionProvenance::Unknown,
            selection_source: SelectionSource::Manual,
            base_commit: base_commit.into(),
            world_model_context: None,
            status: RunStatus::Pending,
            created_at: Utc::now(),
            started_at: None,
            finished_at: None,
            failure_reason: None,
            infrastructure_failures: Vec::new(),
            workspace_path: None,
            branch: None,
            execution: None,
            patch: None,
            evaluation_verdict: None,
            integrity: None,
            warnings: Vec::new(),
            security: None,
            outcome: None,
            artifacts: RunArtifacts::default(),
        }
    }

    /// Advances the lifecycle, maintaining the timestamps tied to it.
    pub fn transition_to(&mut self, next: RunStatus) -> Result<(), RunError> {
        if !self.status.can_transition_to(next) {
            return Err(RunError::InvalidTransition {
                from: self.status,
                to: next,
            });
        }
        self.status = next;
        if next == RunStatus::Running && self.started_at.is_none() {
            self.started_at = Some(Utc::now());
        }
        if next.is_terminal() {
            self.finished_at = Some(Utc::now());
        }
        Ok(())
    }

    /// Terminates the run as failed, recording why.
    pub fn fail(&mut self, reason: impl Into<String>) -> Result<(), RunError> {
        self.failure_reason = Some(reason.into());
        self.outcome = Some(RunOutcome::Errored);
        self.transition_to(RunStatus::Failed)
    }

    /// Computes and stores the outcome from the evidence gathered so far.
    pub fn finalize_outcome(&mut self) -> RunOutcome {
        let outcome = if self.infrastructure_failures.is_empty() {
            RunOutcome::derive(
                self.execution.as_ref(),
                self.patch.as_ref(),
                self.evaluation_verdict,
                self.integrity.as_ref(),
            )
        } else {
            RunOutcome::Errored
        };
        self.outcome = Some(outcome);
        outcome
    }

    /// Whether the run's evaluation inputs were left intact.
    pub fn integrity_is_acceptable(&self) -> bool {
        self.integrity
            .as_ref()
            .is_none_or(EvaluationIntegrity::is_acceptable)
    }

    /// The agent's exit code, if it reported one.
    pub fn exit_code(&self) -> Option<i32> {
        self.execution.as_ref().and_then(|e| e.exit_code)
    }

    pub fn usage(&self) -> Usage {
        self.execution
            .as_ref()
            .map(|e| e.usage.clone())
            .unwrap_or_default()
    }

    /// Wall-clock time from agent start to run end.
    pub fn duration(&self) -> Option<chrono::TimeDelta> {
        Some(self.finished_at? - self.started_at?)
    }

    /// Wall-clock time including workspace provisioning.
    pub fn total_duration(&self) -> Option<chrono::TimeDelta> {
        Some(self.finished_at? - self.created_at)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::AgentId;

    fn run() -> AgentRun {
        AgentRun::new(
            RunId::sequential(1),
            TaskId::sequential(1),
            AgentConfig::new(AgentId::new("claude").unwrap(), "claude-code"),
            "a73cf21",
        )
    }

    fn execution(status: AgentExecutionStatus) -> AgentExecution {
        let now = Utc::now();
        AgentExecution {
            status,
            exit_code: match status {
                AgentExecutionStatus::Completed => Some(0),
                AgentExecutionStatus::NonZeroExit => Some(1),
                _ => None,
            },
            timed_out: status == AgentExecutionStatus::TimedOut,
            started_at: now,
            finished_at: now,
            duration_ms: 1,
            stdout_path: None,
            stderr_path: None,
            usage: Usage::default(),
            self_report: None,
            harness_metadata: BTreeMap::new(),
            infrastructure_failures: Vec::new(),
        }
    }

    /// Runs recorded before exclusions stopped being duplicated into warnings
    /// still hold both, and must keep loading with their totals intact.
    #[test]
    fn a_pre_dedup_record_still_reads_with_its_original_totals() {
        let legacy = serde_json::json!({
            "base_commit": "c566354",
            "files_changed": 1,
            "insertions": 116,
            "deletions": 18,
            "binary_files": 0,
            "excluded": [
                {"path": "target/debug/a.o", "change": "added", "reason": "git_ignored"},
                {"path": "target/debug/b.o", "change": "added", "reason": "git_ignored"}
            ]
        });

        let summary: PatchSummary = serde_json::from_value(legacy).expect("legacy record loads");

        // No counts were written back then, so the retained list was complete
        // and its length is the exact total.
        assert!(summary.excluded_counts.is_empty());
        assert_eq!(summary.excluded_total(), 2);
        assert!(!summary.exclusions_sampled());
    }

    /// A record written now reports the true total even though the retained
    /// list is a sample.
    #[test]
    fn a_sampled_record_reports_the_true_total() {
        let summary = PatchSummary {
            base_commit: "c566354".into(),
            head_commit: None,
            files_changed: 1,
            insertions: 116,
            deletions: 18,
            binary_files: 0,
            diff_path: None,
            excluded: vec![ExcludedEntry {
                path: "target/debug/a.o".into(),
                change: crate::patch::ChangeKind::Added,
                reason: crate::patch::ExclusionReason::GitIgnored,
            }],
            excluded_counts: BTreeMap::from([("git_ignored".to_string(), 26_286)]),
        };

        assert_eq!(summary.excluded_total(), 26_286);
        assert!(summary.exclusions_sampled());

        let encoded = serde_json::to_string(&summary).expect("serializes");
        assert!(
            encoded.len() < 1024,
            "a sampled summary must stay small: {} bytes",
            encoded.len()
        );
    }

    fn patch(files: u64) -> PatchSummary {
        PatchSummary {
            base_commit: "a73cf21".into(),
            head_commit: None,
            files_changed: files,
            insertions: files * 10,
            deletions: files,
            binary_files: 0,
            diff_path: None,
            excluded: Vec::new(),
            excluded_counts: Default::default(),
        }
    }

    #[test]
    fn the_happy_path_walks_the_whole_lifecycle() {
        let mut run = run();
        assert_eq!(run.status, RunStatus::Pending);
        for next in [
            RunStatus::Preparing,
            RunStatus::Running,
            RunStatus::Evaluating,
            RunStatus::Completed,
        ] {
            run.transition_to(next).unwrap();
            assert_eq!(run.status, next);
        }
        assert!(run.started_at.is_some());
        assert!(run.finished_at.is_some());
        assert!(run.duration().is_some());
    }

    #[test]
    fn runs_without_evaluation_may_complete_directly() {
        let mut run = run();
        run.transition_to(RunStatus::Preparing).unwrap();
        run.transition_to(RunStatus::Running).unwrap();
        run.transition_to(RunStatus::Completed).unwrap();
        assert_eq!(run.status, RunStatus::Completed);
    }

    #[test]
    fn stages_cannot_be_skipped() {
        let mut run = run();
        let err = run.transition_to(RunStatus::Running).unwrap_err();
        assert_eq!(
            err,
            RunError::InvalidTransition {
                from: RunStatus::Pending,
                to: RunStatus::Running
            }
        );
        assert_eq!(run.status, RunStatus::Pending);
        assert!(run.transition_to(RunStatus::Evaluating).is_err());
    }

    #[test]
    fn a_run_can_fail_or_be_cancelled_from_any_live_state() {
        for state in [
            RunStatus::Pending,
            RunStatus::Preparing,
            RunStatus::Running,
            RunStatus::Evaluating,
        ] {
            assert!(state.can_transition_to(RunStatus::Failed), "{state}");
            assert!(state.can_transition_to(RunStatus::Cancelled), "{state}");
        }
    }

    #[test]
    fn terminal_states_are_final() {
        for state in [
            RunStatus::Completed,
            RunStatus::Failed,
            RunStatus::Cancelled,
        ] {
            assert!(state.is_terminal());
            for next in [
                RunStatus::Pending,
                RunStatus::Preparing,
                RunStatus::Running,
                RunStatus::Evaluating,
                RunStatus::Completed,
                RunStatus::Failed,
                RunStatus::Cancelled,
            ] {
                assert!(
                    !state.can_transition_to(next),
                    "{state} should not transition to {next}"
                );
            }
        }
    }

    #[test]
    fn failing_records_the_reason_and_closes_the_record() {
        let mut run = run();
        run.transition_to(RunStatus::Preparing).unwrap();
        run.fail("workspace could not be created").unwrap();
        assert_eq!(run.status, RunStatus::Failed);
        assert_eq!(
            run.failure_reason.as_deref(),
            Some("workspace could not be created")
        );
        assert_eq!(run.outcome, Some(RunOutcome::Errored));
        assert!(run.finished_at.is_some());
        assert!(run.duration().is_none());
        assert!(run.total_duration().is_some());
    }

    #[test]
    fn started_at_is_not_overwritten_by_later_transitions() {
        let mut run = run();
        run.transition_to(RunStatus::Preparing).unwrap();
        run.transition_to(RunStatus::Running).unwrap();
        let started = run.started_at;
        run.transition_to(RunStatus::Evaluating).unwrap();
        run.transition_to(RunStatus::Completed).unwrap();
        assert_eq!(run.started_at, started);
    }

    #[test]
    fn agent_status_is_classified_from_the_process_alone() {
        assert_eq!(
            AgentExecution::classify(Some(0), false),
            AgentExecutionStatus::Completed
        );
        assert_eq!(
            AgentExecution::classify(Some(1), false),
            AgentExecutionStatus::NonZeroExit
        );
        assert_eq!(
            AgentExecution::classify(None, true),
            AgentExecutionStatus::TimedOut
        );
        // A timeout is a timeout even if the process managed to report a code.
        assert_eq!(
            AgentExecution::classify(Some(0), true),
            AgentExecutionStatus::TimedOut
        );
    }

    /// The case the design calls out explicitly: a crashed agent that still
    /// left good work behind.
    #[test]
    fn a_nonzero_agent_exit_with_a_passing_patch_is_a_pass() {
        let outcome = RunOutcome::derive(
            Some(&execution(AgentExecutionStatus::NonZeroExit)),
            Some(&patch(3)),
            Some(Verdict::Pass),
            None,
        );
        assert_eq!(outcome, RunOutcome::Passed);
    }

    /// The mirror case: a clean exit proves nothing about the work.
    #[test]
    fn a_clean_agent_exit_with_failing_checks_is_a_failure() {
        let outcome = RunOutcome::derive(
            Some(&execution(AgentExecutionStatus::Completed)),
            Some(&patch(3)),
            Some(Verdict::Fail),
            None,
        );
        assert_eq!(outcome, RunOutcome::Failed);
    }

    #[test]
    fn a_timed_out_agent_that_left_a_working_patch_cannot_pass() {
        let outcome = RunOutcome::derive(
            Some(&execution(AgentExecutionStatus::TimedOut)),
            Some(&patch(1)),
            Some(Verdict::Pass),
            None,
        );
        assert_eq!(outcome, RunOutcome::Inconclusive);
    }

    #[test]
    fn a_cancelled_agent_that_left_a_working_patch_cannot_pass() {
        let outcome = RunOutcome::derive(
            Some(&execution(AgentExecutionStatus::Cancelled)),
            Some(&patch(1)),
            Some(Verdict::Pass),
            None,
        );
        assert_eq!(outcome, RunOutcome::Inconclusive);
    }

    #[test]
    fn producing_no_changes_is_never_a_pass() {
        // An unchanged repository passes its own tests trivially. Reporting
        // that as success would be actively misleading.
        for verdict in [Some(Verdict::Pass), Some(Verdict::Fail), None] {
            let outcome = RunOutcome::derive(
                Some(&execution(AgentExecutionStatus::Completed)),
                Some(&patch(0)),
                verdict,
                None,
            );
            assert_eq!(outcome, RunOutcome::NoChange, "verdict {verdict:?}");
        }

        // No patch captured at all is the same conclusion.
        let outcome = RunOutcome::derive(
            Some(&execution(AgentExecutionStatus::Completed)),
            None,
            Some(Verdict::Pass),
            None,
        );
        assert_eq!(outcome, RunOutcome::NoChange);
    }

    #[test]
    fn a_change_with_no_evaluation_is_inconclusive_not_passing() {
        let outcome = RunOutcome::derive(
            Some(&execution(AgentExecutionStatus::Completed)),
            Some(&patch(2)),
            None,
            None,
        );
        assert_eq!(outcome, RunOutcome::Inconclusive);
    }

    #[test]
    fn an_agent_that_never_started_errors_regardless_of_anything_else() {
        let outcome = RunOutcome::derive(
            Some(&execution(AgentExecutionStatus::StartFailed)),
            Some(&patch(5)),
            Some(Verdict::Pass),
            None,
        );
        assert_eq!(outcome, RunOutcome::Errored);
        assert_eq!(
            RunOutcome::derive(None, None, None, None),
            RunOutcome::Errored
        );
    }

    #[test]
    fn only_a_pass_is_a_success() {
        assert!(RunOutcome::Passed.is_success());
        for outcome in [
            RunOutcome::Failed,
            RunOutcome::Inconclusive,
            RunOutcome::NoChange,
            RunOutcome::Errored,
        ] {
            assert!(!outcome.is_success(), "{outcome}");
        }
    }

    #[test]
    fn finalizing_stores_the_derived_outcome() {
        let mut run = run();
        run.execution = Some(execution(AgentExecutionStatus::NonZeroExit));
        run.patch = Some(patch(1));
        run.evaluation_verdict = Some(Verdict::Pass);

        assert_eq!(run.finalize_outcome(), RunOutcome::Passed);
        assert_eq!(run.outcome, Some(RunOutcome::Passed));
        // The agent's own status is preserved alongside it, not overwritten.
        assert_eq!(
            run.execution.as_ref().unwrap().status,
            AgentExecutionStatus::NonZeroExit
        );
        assert_eq!(run.exit_code(), Some(1));
    }

    #[test]
    fn patch_summary_reports_change_size() {
        let patch = PatchSummary {
            base_commit: "a73cf21".into(),
            head_commit: None,
            files_changed: 3,
            insertions: 120,
            deletions: 63,
            binary_files: 0,
            diff_path: None,
            excluded: Vec::new(),
            excluded_counts: Default::default(),
        };
        assert_eq!(patch.lines_changed(), 183);
        assert!(!patch.is_empty());
        assert!(!patch.looks_like_build_output());
    }

    #[test]
    fn usage_totals_tolerate_partial_reporting() {
        assert_eq!(Usage::default().total_tokens(), None);
        let partial = Usage {
            input_tokens: Some(100),
            ..Default::default()
        };
        assert_eq!(partial.total_tokens(), Some(100));
    }

    #[test]
    fn runs_serialize_compactly_and_round_trip() {
        let run = run();
        let json = serde_json::to_string(&run).unwrap();
        assert!(!json.contains("finished_at"), "{json}");
        let back: AgentRun = serde_json::from_str(&json).unwrap();
        assert_eq!(back, run);
    }

    #[test]
    fn a_full_run_record_round_trips() {
        let mut run = run();
        run.execution = Some(execution(AgentExecutionStatus::TimedOut));
        run.patch = Some(patch(2));
        run.evaluation_verdict = Some(Verdict::Fail);
        run.finalize_outcome();

        let json = serde_json::to_string(&run).unwrap();
        let back: AgentRun = serde_json::from_str(&json).unwrap();
        assert_eq!(back, run);
        assert_eq!(back.outcome, Some(RunOutcome::Failed));
    }

    #[test]
    fn a_green_evaluation_with_compromised_inputs_cannot_pass() {
        let compromised = EvaluationIntegrity {
            status: crate::integrity::IntegrityStatus::Missing,
            deleted: vec!["tests/median.rs".into()],
            ..Default::default()
        };
        let outcome = RunOutcome::derive(
            Some(&execution(AgentExecutionStatus::Completed)),
            Some(&patch(1)),
            Some(Verdict::Pass),
            Some(&compromised),
        );
        assert_eq!(outcome, RunOutcome::Inconclusive);
    }

    #[test]
    fn an_explicitly_allowed_protected_change_can_still_pass() {
        let allowed = EvaluationIntegrity {
            allowed: vec!["tests/new_case.rs".into()],
            ..Default::default()
        };
        let outcome = RunOutcome::derive(
            Some(&execution(AgentExecutionStatus::Completed)),
            Some(&patch(1)),
            Some(Verdict::Pass),
            Some(&allowed),
        );
        assert_eq!(outcome, RunOutcome::Passed);
    }
}
