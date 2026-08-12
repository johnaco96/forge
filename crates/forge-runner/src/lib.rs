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

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use forge_agent::adapter::{AgentAdapter, RunContext};
use forge_core::agent::AgentConfig;
use forge_core::config::{ForgeConfig, Layout};
use forge_core::events::{EventPayload, EventSink, RecordingSink};
use forge_core::experiment::{
    Comparison, ComparisonInput, Experiment, ExperimentEventPayload, ExperimentRecordingSink,
};
use forge_core::ids::{AgentId, ExperimentId};
use forge_core::integrity::EvaluationIntegrity;
use forge_core::optimization::{
    PolicyDecision, PolicyEvent, PolicyEventPayload, PolicyEventSubject, ShadowDecision,
};
use forge_core::patch::{PatchPolicy, PatchWarning};
use forge_core::policy::{ExecutionStrategy, PolicyBounds};
use forge_core::result::{Evaluation, Verdict};
use forge_core::run::{
    AgentExecution, AgentRun, ExecutionProvenance, PatchSummary, RunOutcome, RunStatus,
    SelectionSource,
};
use forge_core::security::SecurityPosture;
use forge_core::task::EngineeringTask;
use forge_core::workspace::Workspace;
use forge_core::world::WorldModelContext;
use forge_eval::{EvalContext, EvaluationEngine, EvaluationPlan};
use forge_executor::{
    EnvPolicy, ProcessRunner, WorkspaceProvider, WorktreeProvider, capture_candidate_patch,
};
use forge_git::Repository;
use forge_policy::{ensure_bootstrap_policy, resolve_execution_policy};
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
    /// Must be asserted explicitly by the caller. The generic runner defaults
    /// to unknown rather than guessing that a custom adapter is live.
    pub execution_provenance: ExecutionProvenance,
    pub selection_source: SelectionSource,
    /// Exact user choice that policy routing must not replace.
    pub manual_policy_override: Option<String>,
}

/// A base commit resolved by Forge before execution begins.
///
/// The inner hash is private so callers cannot accidentally label an arbitrary
/// revision as resolved. Experiments pass one instance to every participant
/// and never re-read `HEAD` between runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedBaseCommit(String);

impl ResolvedBaseCommit {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// What to execute as one competitive experiment.
#[derive(Debug, Clone)]
pub struct ExperimentRequest {
    pub task: EngineeringTask,
    /// Revision resolved once before the experiment is persisted. Defaults to
    /// `HEAD`.
    pub base_rev: Option<String>,
    pub timeout: Option<Duration>,
    pub keep_workspace: Option<bool>,
}

impl ExperimentRequest {
    pub fn new(task: EngineeringTask) -> Self {
        Self {
            task,
            base_rev: None,
            timeout: None,
            keep_workspace: None,
        }
    }
}

/// One requested participant and its provider-specific adapter.
pub struct Competitor<'a> {
    pub agent_id: String,
    pub adapter: &'a dyn AgentAdapter,
    pub execution_provenance: ExecutionProvenance,
}

impl<'a> Competitor<'a> {
    pub fn new(agent_id: impl Into<String>, adapter: &'a dyn AgentAdapter) -> Self {
        Self {
            agent_id: agent_id.into(),
            adapter,
            execution_provenance: ExecutionProvenance::Unknown,
        }
    }

    pub fn with_execution_provenance(mut self, provenance: ExecutionProvenance) -> Self {
        self.execution_provenance = provenance;
        self
    }
}

impl RunRequest {
    pub fn new(task: EngineeringTask, agent_id: impl Into<String>) -> Self {
        Self {
            task,
            agent_id: agent_id.into(),
            base_rev: None,
            timeout: None,
            keep_workspace: None,
            execution_provenance: ExecutionProvenance::Unknown,
            selection_source: SelectionSource::Manual,
            manual_policy_override: None,
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

/// A completed experiment and the ordinary run reports it groups.
#[derive(Debug, Clone)]
pub struct ExperimentReport {
    pub experiment: Experiment,
    pub runs: Vec<RunReport>,
    pub experiment_events_recorded: usize,
    pub execution_strategy: &'static str,
}

/// Drives runs against one repository.
pub struct Runner {
    repository: Repository,
    layout: Layout,
    config: ForgeConfig,
    store: Store,
}

struct PipelineInputs<'a> {
    artifacts_dir: &'a Path,
    sink: &'a RecordingSink,
    adapter: &'a dyn AgentAdapter,
    evaluation_plan: &'a EvaluationPlan,
    experiment_id: Option<&'a ExperimentId>,
    world_model: Option<&'a WorldModelContext>,
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

    /// Ensures the repository has the behavior-preserving Phase 8 bootstrap
    /// and returns the current immutable policy.
    pub async fn ensure_active_policy(&self) -> RunnerResult<forge_core::EngineeringPolicy> {
        Ok(ensure_bootstrap_policy(&self.store, &self.config).await?)
    }

    /// Resolves a revision once into the immutable commit every participant
    /// must share.
    pub fn resolve_base(&self, revision: Option<&str>) -> RunnerResult<ResolvedBaseCommit> {
        Ok(ResolvedBaseCommit(
            self.repository.resolve(revision.unwrap_or("HEAD"))?,
        ))
    }

    /// Builds the agent configuration a run will be recorded under.
    pub fn agent_config(&self, request: &RunRequest) -> RunnerResult<AgentConfig> {
        self.agent_config_with_timeout_cap(request, None)
    }

    fn agent_config_with_timeout_cap(
        &self,
        request: &RunRequest,
        policy_timeout_secs: Option<u64>,
    ) -> RunnerResult<AgentConfig> {
        let settings = self.config.agent(&request.agent_id);
        let agent_id = AgentId::new(request.agent_id.clone())
            .map_err(|source| RunnerError::InvalidAgentId(source.to_string()))?;

        let mut config = AgentConfig::new(agent_id, harness_for(&request.agent_id));
        config.model = settings.model.clone();
        let requested = request
            .timeout
            .map(|timeout| timeout.as_secs())
            .unwrap_or_else(|| self.config.timeout_secs_for(&request.agent_id));
        config.timeout_secs = Some(
            policy_timeout_secs
                .map(|policy| requested.min(policy))
                .unwrap_or(requested),
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
        self.validate_task(&request.task)?;
        // Checked before provisioning so a misconfigured agent costs nothing.
        adapter.prepare().await?;
        let base_commit = self.resolve_base(request.base_rev.as_deref())?;
        let base_was_dirty = !self.repository.is_clean().unwrap_or(true);

        self.execute_resolved(request, adapter, &base_commit, None, None, base_was_dirty)
            .await
    }

    /// Runs independent participants sequentially through the ordinary run
    /// pipeline. Adapters are all preflighted and the base is resolved once
    /// before the experiment record exists, so a bad configuration cannot
    /// create a half-started competition.
    pub async fn compete(
        &self,
        request: ExperimentRequest,
        competitors: Vec<Competitor<'_>>,
    ) -> RunnerResult<ExperimentReport> {
        self.validate_task(&request.task)?;
        if competitors.len() < 2 {
            return Err(RunnerError::TooFewCompetitors);
        }
        let mut seen = HashSet::new();
        for competitor in &competitors {
            if !seen.insert(competitor.agent_id.clone()) {
                return Err(RunnerError::DuplicateCompetitor(
                    competitor.agent_id.clone(),
                ));
            }
        }

        // Resolve the shared repository state before any adapter can execute.
        let base_commit = self.resolve_base(request.base_rev.as_deref())?;
        let base_was_dirty = !self.repository.is_clean().unwrap_or(true);

        // A missing executable is configuration evidence, not an engineering
        // result. Preflight every participant before creating the experiment.
        for competitor in &competitors {
            competitor.adapter.prepare().await?;
        }

        self.store.upsert_task(&request.task).await?;
        let experiment_id = self.store.next_experiment_id().await?;
        let agents = competitors
            .iter()
            .map(|competitor| competitor.agent_id.clone())
            .collect::<Vec<_>>();
        let mut experiment = Experiment::new(
            experiment_id.clone(),
            request.task.task_id.clone(),
            request.task.repository.clone(),
            base_commit.as_str(),
            agents.clone(),
        );
        let experiment_sink = ExperimentRecordingSink::new(experiment_id.clone());
        experiment_sink.emit(ExperimentEventPayload::ExperimentStarted {
            task_id: request.task.task_id.clone(),
            repository: request.task.repository.clone(),
            base_commit: base_commit.as_str().to_string(),
            agents,
        });
        self.store.save_experiment(&experiment).await?;
        self.store
            .append_experiment_events(&experiment_sink.events())
            .await?;

        let mut reports = Vec::with_capacity(competitors.len());
        for competitor in competitors {
            let mut run_request = RunRequest::new(request.task.clone(), &competitor.agent_id);
            run_request.timeout = request.timeout;
            run_request.keep_workspace = request.keep_workspace;
            run_request.execution_provenance = competitor.execution_provenance;
            run_request.selection_source = SelectionSource::Competition {
                experiment_id: experiment_id.clone(),
            };
            run_request.manual_policy_override = Some("competitive execution".into());

            let result = self
                .execute_resolved(
                    run_request,
                    competitor.adapter,
                    &base_commit,
                    Some(&experiment_id),
                    Some(&experiment_sink),
                    base_was_dirty,
                )
                .await;
            let report = match result {
                Ok(report) => report,
                Err(error) => {
                    experiment.fail(error.to_string());
                    experiment_sink.emit(ExperimentEventPayload::ExperimentFailed {
                        reason: error.to_string(),
                    });
                    // The original infrastructure error remains primary. These
                    // best-effort writes preserve as much partial evidence as
                    // the ledger still accepts.
                    let _ = self.store.save_experiment(&experiment).await;
                    let _ = self
                        .store
                        .append_experiment_events(&experiment_sink.events())
                        .await;
                    return Err(error);
                }
            };

            experiment.record_run(report.run.run_id.clone());
            experiment_sink.emit(ExperimentEventPayload::ParticipantRunCompleted {
                run_id: report.run.run_id.clone(),
                agent_id: competitor.agent_id,
                outcome: report.outcome(),
            });
            reports.push(report);
            self.store.save_experiment(&experiment).await?;
            self.store
                .append_experiment_events(&experiment_sink.events())
                .await?;
        }

        let comparison_inputs = reports
            .iter()
            .map(|report| ComparisonInput::new(&report.run, report.evaluation.as_ref()))
            .collect::<Vec<_>>();
        let comparison = Comparison::from_runs(experiment_id, &comparison_inputs);
        experiment.complete(comparison);
        experiment_sink.emit(ExperimentEventPayload::ExperimentCompleted {
            run_count: reports.len(),
        });
        self.store.save_experiment(&experiment).await?;
        self.store
            .append_experiment_events(&experiment_sink.events())
            .await?;

        Ok(ExperimentReport {
            experiment,
            runs: reports,
            experiment_events_recorded: experiment_sink.len(),
            execution_strategy: "sequential",
        })
    }

    fn validate_task(&self, task: &EngineeringTask) -> RunnerResult<()> {
        task.validate()?;
        if task.repository != self.config.repository.name {
            return Err(RunnerError::WrongRepository {
                task_repository: task.repository.clone(),
                configured: self.config.repository.name.clone(),
            });
        }
        Ok(())
    }

    /// Executes the ordinary run pipeline from an already-resolved base.
    async fn execute_resolved(
        &self,
        request: RunRequest,
        adapter: &dyn AgentAdapter,
        base_commit: &ResolvedBaseCommit,
        experiment_id: Option<&ExperimentId>,
        experiment_sink: Option<&ExperimentRecordingSink>,
        base_was_dirty: bool,
    ) -> RunnerResult<RunReport> {
        let task_revision_id = self.store.upsert_task(&request.task).await?;
        let policy = resolve_execution_policy(
            &self.store,
            &self.config,
            &task_revision_id,
            request.manual_policy_override.as_deref(),
            chrono::Utc::now(),
        )
        .await?;
        let bounds = PolicyBounds::for_config(&self.config);
        policy
            .selected
            .validate(&bounds)
            .map_err(|error| RunnerError::PolicyStrategy(error.to_string()))?;
        if policy.source.policy_controlled_execution()
            && policy.selected.execution == ExecutionStrategy::Team
        {
            return Err(RunnerError::PolicyStrategy(
                "team policy requires the existing `forge team` path and a validated task plan"
                    .into(),
            ));
        }

        let agent_config = self.agent_config_with_timeout_cap(
            &request,
            Some(policy.selected.resources.timeout_secs),
        )?;
        // Resolve trusted evaluation configuration before any candidate code
        // executes. The resulting plan is never rebuilt from the workspace.
        let evaluation_plan = EvaluationPlan::resolve(&request.task);
        let world_model = if self.config.world_model.enabled {
            self.store
                .world_context_for_policy(
                    &request.task,
                    base_commit.as_str(),
                    policy.selected.context.max_world_facts as usize,
                    policy.selected.context.selection_strategy,
                    policy.selected.context.include_failure_history,
                )
                .await?
        } else {
            None
        };
        let health_snapshot_id = self
            .store
            .health_snapshot_for_commit(&request.task.repository, base_commit.as_str())
            .await?
            .map(|snapshot| snapshot.health_snapshot_id);
        let decision_id = self.store.next_policy_decision_id().await?;
        let mut explanation = policy.explanation.clone();
        explanation.push(format!(
            "executed agent `{}` with timeout cap {}s and {} context fact(s)",
            request.agent_id,
            policy.selected.resources.timeout_secs,
            world_model
                .as_ref()
                .map(|context| context.facts.len())
                .unwrap_or(0)
        ));
        if let SelectionSource::Automatic { decision_id, .. } = &request.selection_source {
            explanation.push(format!("routing decision {decision_id} selected the agent"));
        }
        let decision = PolicyDecision {
            decision_id: decision_id.clone(),
            repository: request.task.repository.clone(),
            created_at: chrono::Utc::now(),
            task_revision_id: task_revision_id.clone(),
            base_commit: Some(base_commit.as_str().to_string()),
            active_policy_id: policy.active.policy_id.clone(),
            selected_policy_id: policy.selected.policy_id.clone(),
            policy_fingerprint: policy.selected.fingerprint(),
            source: policy.source,
            manual_override: request.manual_policy_override.clone(),
            experiment: policy.experiment.clone(),
            world_model_snapshot_id: world_model
                .as_ref()
                .map(|context| context.snapshot_id.clone()),
            context_fact_ids: world_model
                .as_ref()
                .map(|context| {
                    context
                        .facts
                        .iter()
                        .map(|fact| fact.id.to_string())
                        .collect()
                })
                .unwrap_or_default(),
            health_snapshot_id,
            evidence_cutoff: None,
            evidence_fingerprint: None,
            optimizer_version: policy.selected.optimizer_version.clone(),
            explanation,
        };
        self.store.insert_policy_decision(&decision).await?;
        if let Some(shadow_policy) = self.store.shadow_policy(&request.task.repository).await? {
            let shadow = ShadowDecision::new(
                self.store.next_policy_decision_id().await?,
                &request.task.repository,
                task_revision_id.clone(),
                shadow_policy.policy_id.clone(),
                shadow_policy.fingerprint(),
                policy.active.policy_id.clone(),
                policy_selection_label(&policy.selected, &request.agent_id),
                policy_selection_label(&shadow_policy, &request.agent_id),
            );
            self.store.insert_shadow_decision(&shadow).await?;
            let subject = PolicyEventSubject::Policy(shadow_policy.policy_id.clone());
            self.store
                .append_policy_events(&[PolicyEvent {
                    seq: self.store.next_policy_event_seq(&subject).await?,
                    subject,
                    timestamp: shadow.created_at,
                    payload: PolicyEventPayload::ShadowDecisionRecorded {
                        shadow_policy_id: shadow_policy.policy_id,
                        agreed: shadow.agreed,
                    },
                }])
                .await?;
        }
        let run_id = self.store.next_run_id().await?;

        // --- From here on, every path persists a run. ---

        let mut run = AgentRun::new(
            run_id.clone(),
            request.task.task_id.clone(),
            agent_config,
            base_commit.as_str(),
        );
        run.execution_provenance = request.execution_provenance;
        run.selection_source = request.selection_source.clone();
        run.world_model_context = world_model.as_ref().map(Into::into);
        run.security = Some(SecurityPosture::current(adapter.security()));
        let artifacts_dir = self.layout.run_dir(&run_id);
        run.artifacts.directory = Some(artifacts_dir.clone());

        self.store
            .save_run_at_task_revision(&run, experiment_id, &task_revision_id)
            .await?;
        self.store
            .link_run_to_policy(
                &run_id,
                &policy.selected.policy_id,
                &decision.policy_fingerprint,
                &decision_id,
            )
            .await?;
        if let Some(membership) = &policy.experiment {
            let subject = PolicyEventSubject::Experiment(membership.experiment_id.clone());
            self.store
                .append_policy_events(&[PolicyEvent {
                    seq: self.store.next_policy_event_seq(&subject).await?,
                    subject,
                    timestamp: decision.created_at,
                    payload: PolicyEventPayload::PolicyExperimentAssigned {
                        task_revision_id: task_revision_id.clone(),
                        arm: membership.arm,
                    },
                }])
                .await?;
        }

        if let Some(experiment_sink) = experiment_sink {
            experiment_sink.emit(ExperimentEventPayload::ParticipantRunStarted {
                run_id: run_id.clone(),
                agent_id: request.agent_id.clone(),
            });
        }

        let sink = RecordingSink::new(run_id.clone());
        sink.emit(EventPayload::RunStarted {
            task_id: request.task.task_id.clone(),
            agent_id: request.agent_id.clone(),
            base_commit: base_commit.as_str().to_string(),
        });

        let keep_workspace = request
            .keep_workspace
            .unwrap_or(self.config.workspaces.keep_after_run);

        let inputs = PipelineInputs {
            artifacts_dir: &artifacts_dir,
            sink: &sink,
            adapter,
            evaluation_plan: &evaluation_plan,
            experiment_id,
            world_model: world_model.as_ref(),
        };
        let result = self.run_inner(&request, &mut run, &inputs).await;

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
                self.persist(&run, None, &sink, experiment_id).await?;
                self.record_policy_experiment_observation(&run, policy.experiment.as_ref())
                    .await?;
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

        self.persist(&run, evaluation.as_ref(), &sink, experiment_id)
            .await?;
        self.record_policy_experiment_observation(&run, policy.experiment.as_ref())
            .await?;

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
        inputs: &PipelineInputs<'_>,
    ) -> RunnerResult<(Option<Evaluation>, Option<Workspace>)> {
        // 1. Isolated workspace.
        run.transition_to(RunStatus::Preparing)?;
        let provider = self.provider(true)?;
        let workspace = provider.provision(&run.run_id, &run.base_commit, inputs.sink)?;
        run.workspace_path = Some(workspace.path.clone());
        run.branch = Some(workspace.branch.clone());
        self.store.save_run(run, inputs.experiment_id).await?;

        // 2. The agent. Untrusted from here until the patch is read back.
        run.transition_to(RunStatus::Running)?;
        self.store.save_run(run, inputs.experiment_id).await?;

        let timeout = request
            .timeout
            .or_else(|| run.agent.timeout_secs.map(Duration::from_secs));
        let ctx = RunContext::new(
            &run.run_id,
            &request.task,
            &workspace,
            &run.agent,
            inputs.sink,
            inputs.artifacts_dir.to_path_buf(),
        )
        .with_world_model(inputs.world_model)
        .with_timeout(timeout);

        match inputs.adapter.execute(&ctx).await {
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
        let (patch, integrity, warnings) = self.capture(
            &workspace,
            &request.task,
            run,
            inputs.artifacts_dir,
            inputs.sink,
        )?;
        run.patch = Some(patch);
        run.integrity = Some(integrity);
        run.warnings = warnings;

        // 4. Forge's own evaluation.
        run.transition_to(RunStatus::Evaluating)?;
        self.store.save_run(run, inputs.experiment_id).await?;

        let evaluation = self
            .evaluate(
                &request.task,
                inputs.evaluation_plan,
                &workspace,
                run,
                inputs.artifacts_dir,
                inputs.sink,
            )
            .await;
        run.evaluation_verdict = evaluation.as_ref().map(|e| e.verdict);

        // 5. Conclude.
        let outcome = run.finalize_outcome();
        inputs.sink.emit(EventPayload::RunCompleted {
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
        for metrics_file in task.evaluation.metrics_files() {
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
        evaluation_plan: &EvaluationPlan,
        workspace: &Workspace,
        run: &AgentRun,
        artifacts_dir: &Path,
        sink: &RecordingSink,
    ) -> Option<Evaluation> {
        if evaluation_plan.is_empty() {
            return None;
        }

        // Evaluation commands get the conservative environment: they run code
        // an agent just wrote, and have no business seeing credentials.
        let runner = ProcessRunner::new(EnvPolicy::conservative());
        let ctx = EvalContext::new(workspace, task, &runner, sink)
            .with_patch(
                run.patch
                    .as_ref()
                    .expect("patch captured before evaluation"),
            )
            .with_default_timeout(Some(Duration::from_secs(self.config.defaults.timeout_secs)))
            .with_artifacts_dir(artifacts_dir);

        Some(EvaluationEngine::execute(evaluation_plan, run.run_id.clone(), &ctx).await)
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
        experiment_id: Option<&ExperimentId>,
    ) -> RunnerResult<()> {
        self.store.save_run(run, experiment_id).await?;
        if let Some(patch) = &run.patch {
            self.store.record_patch(&run.run_id, patch).await?;
        }
        if let Some(evaluation) = evaluation {
            self.store.record_evaluation(evaluation).await?;
        }
        self.store.append_events(&sink.events()).await?;
        Ok(())
    }

    async fn record_policy_experiment_observation(
        &self,
        run: &AgentRun,
        membership: Option<&forge_core::ExperimentMembership>,
    ) -> RunnerResult<()> {
        let Some(membership) = membership else {
            return Ok(());
        };
        self.store
            .record_experiment_observation(&membership.experiment_id, &run.run_id, membership.arm)
            .await?;
        let subject = PolicyEventSubject::Experiment(membership.experiment_id.clone());
        self.store
            .append_policy_events(&[PolicyEvent {
                seq: self.store.next_policy_event_seq(&subject).await?,
                subject,
                timestamp: run.finished_at.unwrap_or_else(chrono::Utc::now),
                payload: PolicyEventPayload::PolicyExperimentObservationAdded {
                    run_id: run.run_id.clone(),
                    arm: membership.arm,
                },
            }])
            .await?;
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

fn policy_selection_label(policy: &forge_core::EngineeringPolicy, actual_agent: &str) -> String {
    format!(
        "agent={} routing={} execution={} context={} max_facts={} timeout={}s",
        if policy.routing.use_learned_routing {
            "learned"
        } else {
            actual_agent
        },
        if policy.routing.use_learned_routing {
            "learned"
        } else {
            "configured"
        },
        policy.execution.as_str(),
        policy.context.selection_strategy.as_str(),
        policy.context.max_world_facts,
        policy.resources.timeout_secs,
    )
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
