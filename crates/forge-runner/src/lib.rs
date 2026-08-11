//! The run pipeline.
//!
//! ```text
//!   validate task
//!         ↓
//!   resolve base commit
//!         ↓
//!   create run record ────────────────┐
//!         ↓                           │
//!   provision worktree                │  every step from here on
//!         ↓                           │  persists what it knows,
//!   invoke agent  ← untrusted         │  including its own failure
//!         ↓                           │
//! ─────── TRUST BOUNDARY ───────      │
//!         ↓                           │
//!   capture patch from Git            │
//!         ↓                           │
//!   run Forge's own evaluators        │
//!         ↓                           │
//!   derive outcome ───────────────────┘
//! ```
//!
//! This crate sits above every other one and belongs to none of them: it is the
//! engine a CLI, an API, or a scheduler would each drive. Keeping it out of
//! `forge-cli` is what makes the whole pipeline testable against a fake agent,
//! without spawning a binary or spending a token.
//!
//! Two rules shape the error handling. Before a run id exists, a failure is an
//! error and nothing is recorded — a missing executable says nothing about an
//! agent's engineering ability and does not belong in the ledger. After a run
//! id exists, *every* failure produces a persisted run whose events explain
//! what happened, because a run that died halfway is itself evidence.

#![deny(rust_2018_idioms)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use forge_agent::adapter::{AgentAdapter, RunContext};
use forge_core::agent::AgentConfig;
use forge_core::config::{ForgeConfig, Layout};
use forge_core::events::{EventPayload, EventSink, RecordingSink};
use forge_core::ids::AgentId;
use forge_core::integrity::EvaluationIntegrity;
use forge_core::patch::{PatchPolicy, PatchWarning};
use forge_core::result::{Evaluation, Verdict};
use forge_core::run::{AgentExecution, AgentRun, PatchSummary, RunOutcome, RunStatus};
use forge_core::security::SecurityPosture;
use forge_core::task::EngineeringTask;
use forge_core::workspace::Workspace;
use forge_eval::{EvalContext, EvaluatorSet};
use forge_executor::{
    EnvPolicy, ProcessRunner, WorkspaceProvider, WorktreeProvider, capture_candidate_patch,
};
use forge_git::Repository;
use forge_store::Store;

pub mod error;

pub use error::{RunnerError, RunnerResult};

/// What to run, and how.
#[derive(Debug, Clone)]
pub struct RunRequest {
    pub task: EngineeringTask,
    pub agent_id: String,
    /// Revision to start from. Defaults to `HEAD`.
    pub base_rev: Option<String>,
    /// Overrides the configured agent timeout.
    pub timeout: Option<Duration>,
    /// Overrides `workspaces.keep_after_run`.
    pub keep_workspace: Option<bool>,
}

impl RunRequest {
    pub fn new(task: EngineeringTask, agent_id: impl Into<String>) -> Self {
        Self {
            task,
            agent_id: agent_id.into(),
            base_rev: None,
            timeout: None,
            keep_workspace: None,
        }
    }
}

/// Everything a caller needs to report on a finished run.
#[derive(Debug, Clone)]
pub struct RunReport {
    pub run: AgentRun,
    /// Forge's own evaluation. `None` when the run never reached it.
    pub evaluation: Option<Evaluation>,
    /// Where the workspace is, if it was kept.
    pub workspace_path: Option<PathBuf>,
    pub workspace_kept: bool,
    pub branch: Option<String>,
    pub events_recorded: usize,
    /// True when the repository had uncommitted changes at the time of the run,
    /// which the agent could not see.
    pub base_was_dirty: bool,
}

impl RunReport {
    pub fn outcome(&self) -> RunOutcome {
        self.run.outcome.unwrap_or(RunOutcome::Errored)
    }
}

/// Drives runs against one repository.
pub struct Runner {
    repository: Repository,
    layout: Layout,
    config: ForgeConfig,
    store: Store,
}

impl Runner {
    pub fn new(repository: Repository, config: ForgeConfig, store: Store) -> Self {
        let layout = Layout::new(repository.root().to_path_buf());
        Self {
            repository,
            layout,
            config,
            store,
        }
    }

    pub fn repository(&self) -> &Repository {
        &self.repository
    }

    pub fn config(&self) -> &ForgeConfig {
        &self.config
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    /// Builds the agent configuration a run will be recorded under.
    pub fn agent_config(&self, request: &RunRequest) -> RunnerResult<AgentConfig> {
        let settings = self.config.agent(&request.agent_id);
        let agent_id = AgentId::new(request.agent_id.clone())
            .map_err(|source| RunnerError::InvalidAgentId(source.to_string()))?;

        let mut config = AgentConfig::new(agent_id, harness_for(&request.agent_id));
        config.model = settings.model.clone();
        config.timeout_secs = Some(
            request
                .timeout
                .map(|t| t.as_secs())
                .unwrap_or_else(|| self.config.timeout_secs_for(&request.agent_id)),
        );
        config.settings = settings.settings.clone();
        if let Some(executable) = &settings.executable {
            config
                .settings
                .insert("executable".to_string(), executable.clone());
        }
        if !settings.extra_args.is_empty() {
            config
                .settings
                .insert("extra_args".to_string(), settings.extra_args.join(" "));
        }
        Ok(config)
    }

    /// Runs one task through one agent, end to end.
    pub async fn execute(
        &self,
        request: RunRequest,
        adapter: &dyn AgentAdapter,
    ) -> RunnerResult<RunReport> {
        // --- Before a run exists: failures here are errors, not records. ---

        request.task.validate()?;

        if request.task.repository != self.config.repository.name {
            return Err(RunnerError::WrongRepository {
                task_repository: request.task.repository.clone(),
                configured: self.config.repository.name.clone(),
            });
        }

        // Checked before provisioning so a misconfigured agent costs nothing.
        adapter.prepare().await?;

        let base_rev = request.base_rev.as_deref().unwrap_or("HEAD");
        let base_commit = self.repository.resolve(base_rev)?;
        let base_was_dirty = !self.repository.is_clean().unwrap_or(true);

        let agent_config = self.agent_config(&request)?;
        let run_id = self.store.next_run_id().await?;

        // --- From here on, every path persists a run. ---

        let mut run = AgentRun::new(
            run_id.clone(),
            request.task.task_id.clone(),
            agent_config,
            &base_commit,
        );
        run.security = Some(SecurityPosture::current(adapter.security()));
        let artifacts_dir = self.layout.run_dir(&run_id);
        run.artifacts.directory = Some(artifacts_dir.clone());

        self.store.upsert_task(&request.task).await?;
        self.store.save_run(&run, None).await?;

        let sink = RecordingSink::new(run_id.clone());
        sink.emit(EventPayload::RunStarted {
            task_id: request.task.task_id.clone(),
            agent_id: request.agent_id.clone(),
            base_commit: base_commit.clone(),
        });

        let keep_workspace = request
            .keep_workspace
            .unwrap_or(self.config.workspaces.keep_after_run);

        let result = self
            .run_inner(
                &request,
                &mut run,
                &base_commit,
                &artifacts_dir,
                &sink,
                adapter,
            )
            .await;

        let (evaluation, workspace) = match result {
            Ok(outcome) => outcome,
            Err(err) => {
                // A failure after the run exists is recorded, not thrown away.
                tracing::warn!(run = %run_id, %err, "run failed");
                sink.emit(EventPayload::RunFailed {
                    reason: err.to_string(),
                });
                if !run.status.is_terminal() {
                    let _ = run.fail(err.to_string());
                }
                run.outcome = Some(RunOutcome::Errored);
                self.persist(&run, None, &sink).await?;
                return Ok(self.report(run, None, None, false, base_was_dirty, &sink));
            }
        };

        // Tear down only on a clean pipeline: a failed run's workspace is the
        // most useful thing to look at, and the branch alone does not preserve
        // the state the agent left behind.
        let outcome = run.outcome.unwrap_or(RunOutcome::Errored);
        let should_keep = keep_workspace || outcome == RunOutcome::Errored;
        if let Some(workspace) = &workspace
            && !should_keep
        {
            match self.provider(keep_workspace) {
                Ok(provider) => {
                    if let Err(err) = provider.teardown(workspace) {
                        tracing::warn!(%err, "could not remove workspace");
                    }
                }
                Err(err) => tracing::warn!(%err, "could not build workspace provider"),
            }
        }

        self.persist(&run, evaluation.as_ref(), &sink).await?;

        Ok(self.report(
            run,
            evaluation,
            workspace,
            should_keep,
            base_was_dirty,
            &sink,
        ))
    }

    /// The pipeline proper, split out so its `?` failures all land in one place.
    async fn run_inner(
        &self,
        request: &RunRequest,
        run: &mut AgentRun,
        base_commit: &str,
        artifacts_dir: &Path,
        sink: &RecordingSink,
        adapter: &dyn AgentAdapter,
    ) -> RunnerResult<(Option<Evaluation>, Option<Workspace>)> {
        // 1. Isolated workspace.
        run.transition_to(RunStatus::Preparing)?;
        let provider = self.provider(true)?;
        let workspace = provider.provision(&run.run_id, base_commit, sink)?;
        run.workspace_path = Some(workspace.path.clone());
        run.branch = Some(workspace.branch.clone());
        self.store.save_run(run, None).await?;

        // 2. The agent. Untrusted from here until the patch is read back.
        run.transition_to(RunStatus::Running)?;
        self.store.save_run(run, None).await?;

        let timeout = request
            .timeout
            .or_else(|| run.agent.timeout_secs.map(Duration::from_secs));
        let ctx = RunContext::new(
            &run.run_id,
            &request.task,
            &workspace,
            &run.agent,
            sink,
            artifacts_dir.to_path_buf(),
        )
        .with_timeout(timeout);

        match adapter.execute(&ctx).await {
            Ok(execution) => run.execution = Some(execution),
            Err(err) => {
                // The agent could not be run. Record it and stop; there is
                // nothing to evaluate.
                run.execution = Some(AgentExecution::start_failed(chrono::Utc::now()));
                return Err(RunnerError::Agent(err))
                    .inspect_err(|_| tracing::warn!(run = %run.run_id, "agent execution failed"));
            }
        }

        // 3. Read the change out of Git. Everything below this line is measured.
        let (patch, integrity, warnings) =
            self.capture(&workspace, &request.task, run, artifacts_dir, sink)?;
        run.patch = Some(patch);
        run.integrity = Some(integrity);
        run.warnings = warnings;

        // 4. Forge's own evaluation.
        run.transition_to(RunStatus::Evaluating)?;
        self.store.save_run(run, None).await?;

        let evaluation = self
            .evaluate(&request.task, &workspace, run, artifacts_dir, sink)
            .await;
        run.evaluation_verdict = evaluation.as_ref().map(|e| e.verdict);

        // 5. Conclude.
        let outcome = run.finalize_outcome();
        sink.emit(EventPayload::RunCompleted {
            outcome,
            duration_ms: run
                .total_duration()
                .and_then(|d| d.num_milliseconds().try_into().ok())
                .unwrap_or(0),
        });
        run.transition_to(RunStatus::Completed)?;

        Ok((evaluation, Some(workspace)))
    }

    /// Reads the agent's change out of Git and records it.
    fn capture(
        &self,
        workspace: &Workspace,
        task: &EngineeringTask,
        run: &AgentRun,
        artifacts_dir: &Path,
        sink: &RecordingSink,
    ) -> RunnerResult<(PatchSummary, EvaluationIntegrity, Vec<PatchWarning>)> {
        let diff_path = artifacts_dir.join("patch.diff");
        let message = format!("forge {}: {}", run.run_id, run.task_id);
        let mut policy = PatchPolicy::default();
        if let Some(metrics_file) = task
            .evaluation
            .benchmark
            .as_ref()
            .and_then(|benchmark| benchmark.metrics_file.as_ref())
        {
            policy = policy.with_excluded_path(metrics_file);
        }
        let captured =
            capture_candidate_patch(workspace, Some(&diff_path), Some(&message), &policy)?;
        let integrity = task.protection.check(&captured.delta)?;
        let mut warnings = captured.candidate.warnings.clone();
        warnings.extend(integrity.warnings());
        let patch = captured.summary;

        sink.emit(EventPayload::PatchCaptured {
            files_changed: patch.files_changed,
            insertions: patch.insertions,
            deletions: patch.deletions,
            diff_path: patch.diff_path.clone(),
        });
        Ok((patch, integrity, warnings))
    }

    /// Runs the task's configured checks against the workspace.
    ///
    /// Returns `None` only when the task declares no checks at all; a check
    /// that fails or cannot run still produces an evaluation, because "we
    /// measured and it failed" and "we could not measure" are different facts.
    async fn evaluate(
        &self,
        task: &EngineeringTask,
        workspace: &Workspace,
        run: &AgentRun,
        artifacts_dir: &Path,
        sink: &RecordingSink,
    ) -> Option<Evaluation> {
        let evaluators = EvaluatorSet::from_task(task);
        if evaluators.is_empty() {
            return None;
        }

        // Evaluation commands get the conservative environment: they run code
        // an agent just wrote, and have no business seeing credentials.
        let runner = ProcessRunner::new(EnvPolicy::conservative());
        let ctx = EvalContext::new(workspace, task, &runner, sink)
            .with_default_timeout(Some(Duration::from_secs(self.config.defaults.timeout_secs)))
            .with_artifacts_dir(artifacts_dir);

        Some(evaluators.run(run.run_id.clone(), &ctx).await)
    }

    fn provider(&self, keep: bool) -> RunnerResult<WorktreeProvider> {
        Ok(WorktreeProvider::new(
            self.repository.clone(),
            self.layout.worktrees_root(&self.config),
            &self.config.workspaces.branch_prefix,
        )?
        .keep_after_run(keep))
    }

    /// Writes everything gathered to the ledger.
    ///
    /// Events are flushed last and unconditionally: the trajectory is the part
    /// that explains a failure, so it must survive one.
    async fn persist(
        &self,
        run: &AgentRun,
        evaluation: Option<&Evaluation>,
        sink: &RecordingSink,
    ) -> RunnerResult<()> {
        self.store.save_run(run, None).await?;
        if let Some(patch) = &run.patch {
            self.store.record_patch(&run.run_id, patch).await?;
        }
        if let Some(evaluation) = evaluation {
            self.store.record_evaluation(evaluation).await?;
        }
        self.store.append_events(&sink.events()).await?;
        Ok(())
    }

    fn report(
        &self,
        run: AgentRun,
        evaluation: Option<Evaluation>,
        workspace: Option<Workspace>,
        workspace_kept: bool,
        base_was_dirty: bool,
        sink: &RecordingSink,
    ) -> RunReport {
        RunReport {
            workspace_path: workspace
                .map(|w| w.path)
                .or_else(|| workspace_kept.then(|| run.workspace_path.clone()).flatten()),
            branch: run.branch.clone(),
            evaluation,
            workspace_kept,
            base_was_dirty,
            events_recorded: sink.len(),
            run,
        }
    }
}

/// The harness an agent id runs under.
///
/// A lookup table rather than adapter knowledge: the runner never needs to know
/// what a harness *is*, only how to label the configuration a run was recorded
/// under.
fn harness_for(agent_id: &str) -> String {
    match agent_id {
        "claude" => "claude-code",
        "codex" => "codex-cli",
        other => other,
    }
    .to_string()
}

/// Summarizes an evaluation for display, preserving per-check detail.
pub fn check_lines(evaluation: &Evaluation) -> Vec<(String, Verdict, String)> {
    evaluation
        .checks
        .iter()
        .map(|check| {
            let detail = if check.verdict == Verdict::Pass {
                format!("{}ms", check.duration_ms)
            } else {
                match check.exit_code {
                    Some(code) => format!("exit {code}, {}ms", check.duration_ms),
                    None => format!("{}ms", check.duration_ms),
                }
            };
            (check.name.clone(), check.verdict, detail)
        })
        .collect()
}
