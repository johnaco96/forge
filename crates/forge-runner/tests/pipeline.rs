//! End-to-end pipeline tests against a fake agent.
//!
//! Every test here drives the full run pipeline — worktree, agent, patch
//! capture, evaluation, persistence — without invoking a real model. The fake
//! agent is a `dyn AgentAdapter` whose behavior each test dictates, which is
//! what lets the failure paths be exercised deliberately rather than hoped for.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use forge_agent::adapter::{AgentAdapter, RunContext};
use forge_agent::error::{AgentError, AgentResult};
use forge_core::agent::{AdapterStatus, AgentDescriptor};
use forge_core::config::ForgeConfig;
use forge_core::events::EventPayload;
use forge_core::ids::{AgentId, PolicyId, TaskId, WorldModelFactId, WorldModelSnapshotId};
use forge_core::integrity::ProtectionPolicy;
use forge_core::policy::{PolicyProvenance, PolicyStatus};
use forge_core::result::{EvaluatorKind, Verdict};
use forge_core::run::{
    AgentExecution, AgentExecutionStatus, ExecutionProvenance, RunOutcome, RunStatus,
    SelectionSource, Usage,
};
use forge_core::task::{
    BenchmarkSpec, CommandSpec, EngineeringTask, EvaluationSpec, NamedCommand, TaskMetadata,
};
use forge_core::world::{
    Component, EvidenceConfidence, ExtractorIdentity, ExtractorRecord, ExtractorStatus,
    FactMetadata, RepositoryPath, SourceLocation, WORLD_MODEL_SCHEMA_VERSION, WorldEntityKind,
    WorldModelFacts, WorldModelProvenance, WorldModelProvenanceSource, WorldModelSnapshot,
    WorldModelSnapshotSource, WorldModelSnapshotStatus,
};
use forge_git::Repository;
use forge_runner::{RunRequest, Runner, RunnerError};
use forge_store::Store;

// ---------------------------------------------------------------- fixtures

struct Fixture {
    _temp: tempfile::TempDir,
    repo: Repository,
    config: ForgeConfig,
}

impl Fixture {
    /// A repository with a passing test script, so evaluation has something
    /// real to run without needing a toolchain.
    fn new() -> Self {
        let temp = tempfile::tempdir().expect("temp dir");
        let root = temp.path().join("distributed-runtime");
        std::fs::create_dir_all(&root).unwrap();

        git(&root, &["init", "--quiet", "--initial-branch=main"]);
        git(&root, &["config", "user.email", "forge@example.invalid"]);
        git(&root, &["config", "user.name", "Forge Tests"]);
        std::fs::write(root.join("README.md"), "# distributed-runtime\n").unwrap();
        std::fs::write(root.join("value.txt"), "1\n").unwrap();
        std::fs::write(root.join(".gitignore"), "/target\n").unwrap();
        std::fs::create_dir_all(root.join("tests")).unwrap();
        std::fs::write(root.join("tests/value.sh"), "grep -q '^2$' value.txt\n").unwrap();
        std::fs::write(
            root.join("task.yaml"),
            "evaluation:\n  tests: grep -q '^2$' value.txt\n",
        )
        .unwrap();
        git(&root, &["add", "-A"]);
        git(&root, &["commit", "--quiet", "-m", "initial commit"]);

        let repo = Repository::open(&root).unwrap();
        let mut config = ForgeConfig::default_for("distributed-runtime");
        config.defaults.timeout_secs = 60;

        Self {
            _temp: temp,
            repo,
            config,
        }
    }

    fn root(&self) -> &Path {
        self.repo.root()
    }

    async fn runner(&self) -> Runner {
        let store = Store::open(self.root().join(".forge/forge.db"))
            .await
            .expect("store");
        Runner::new(self.repo.clone(), self.config.clone(), store)
    }

    fn read(&self, relative: &str) -> String {
        std::fs::read_to_string(self.root().join(relative)).unwrap_or_default()
    }
}

fn git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("git");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn task(checks: &[(&str, &str)]) -> EngineeringTask {
    let mut evaluation = EvaluationSpec::default();
    for (name, command) in checks {
        let spec = CommandSpec::new(*command);
        match *name {
            "tests" => evaluation.tests = Some(spec),
            "benchmark" => evaluation.benchmark = Some(spec.into()),
            "lint" => evaluation.lint = Some(spec),
            _ => unreachable!("unexpected check"),
        }
    }
    EngineeringTask {
        task_id: TaskId::sequential(1042),
        repository: "distributed-runtime".into(),
        objective: "Raise the recorded value in value.txt to two".into(),
        constraints: vec!["value.txt must remain a single integer".into()],
        evaluation,
        protection: ProtectionPolicy::default(),
        metadata: TaskMetadata::default(),
        classification: Default::default(),
        components: Vec::new(),
        tags: Vec::new(),
    }
}

// --------------------------------------------------------------- fake agent

/// What the fake agent should do when invoked.
#[derive(Clone)]
enum Behavior {
    /// Write `contents` to `path` inside the workspace, then exit `exit_code`.
    Edit {
        path: String,
        contents: String,
        exit_code: i32,
    },
    /// Apply several writes/deletions in one invocation.
    Mutate {
        changes: Vec<Mutation>,
        exit_code: i32,
    },
    /// Change nothing, exit zero.
    NoOp,
    /// Report having been killed at its timeout.
    TimedOut,
    /// Fail to run at all.
    Unavailable,
    /// Fail during `prepare`, before any workspace exists.
    NotInstalled,
    /// Write outside the workspace, to demonstrate the isolation limit.
    EscapeAttempt { absolute_path: PathBuf },
}

#[derive(Clone)]
enum Mutation {
    Write { path: String, contents: String },
    Delete { path: String },
}

struct FakeAgent {
    behavior: Behavior,
    /// Prompts the agent was given, for asserting the contract.
    prompts: Arc<Mutex<Vec<String>>>,
    /// Whether it claims success regardless of what it did.
    self_report: String,
}

impl FakeAgent {
    fn new(behavior: Behavior) -> Self {
        Self {
            behavior,
            prompts: Arc::new(Mutex::new(Vec::new())),
            self_report: "I completed the task and all tests pass.".to_string(),
        }
    }

    fn prompts(&self) -> Vec<String> {
        self.prompts.lock().unwrap().clone()
    }
}

#[async_trait]
impl AgentAdapter for FakeAgent {
    fn descriptor(&self) -> AgentDescriptor {
        AgentDescriptor {
            agent_id: AgentId::new("claude").unwrap(),
            display_name: "Fake Agent".into(),
            harness: "fake".into(),
            executable: None,
            default_model: None,
            capabilities: vec![],
            adapter_status: AdapterStatus::Implemented,
        }
    }

    async fn prepare(&self) -> AgentResult<()> {
        match self.behavior {
            Behavior::NotInstalled => Err(AgentError::ExecutableNotFound {
                agent: "claude".into(),
                executable: "claude".into(),
            }),
            _ => Ok(()),
        }
    }

    async fn execute(&self, ctx: &RunContext<'_>) -> AgentResult<AgentExecution> {
        let prompt =
            forge_agent::build_agent_prompt_with_context(ctx.task, ctx.workspace, ctx.world_model);
        self.prompts.lock().unwrap().push(prompt.clone());
        ctx.events.emit(EventPayload::PromptSubmitted { prompt });

        let now = chrono::Utc::now();
        let mut execution = AgentExecution {
            status: AgentExecutionStatus::Completed,
            exit_code: Some(0),
            timed_out: false,
            started_at: now,
            finished_at: now,
            duration_ms: 5,
            stdout_path: None,
            stderr_path: None,
            usage: Usage {
                input_tokens: Some(1000),
                output_tokens: Some(50),
                cost_usd: Some(0.02),
            },
            // Always claims success. Nothing downstream may believe it.
            self_report: Some(self.self_report.clone()),
            harness_metadata: Default::default(),
            infrastructure_failures: Vec::new(),
        };

        match &self.behavior {
            Behavior::Edit {
                path,
                contents,
                exit_code,
            } => {
                std::fs::write(ctx.workspace.path.join(path), contents).unwrap();
                execution.exit_code = Some(*exit_code);
                execution.status = AgentExecution::classify(Some(*exit_code), false);
            }
            Behavior::Mutate { changes, exit_code } => {
                for change in changes {
                    match change {
                        Mutation::Write { path, contents } => {
                            std::fs::write(ctx.workspace.path.join(path), contents).unwrap();
                        }
                        Mutation::Delete { path } => {
                            std::fs::remove_file(ctx.workspace.path.join(path)).unwrap();
                        }
                    }
                }
                execution.exit_code = Some(*exit_code);
                execution.status = AgentExecution::classify(Some(*exit_code), false);
            }
            Behavior::NoOp => {}
            Behavior::TimedOut => {
                execution.timed_out = true;
                execution.exit_code = None;
                execution.status = AgentExecutionStatus::TimedOut;
            }
            Behavior::Unavailable => {
                return Err(AgentError::Unavailable {
                    agent: "claude".into(),
                    reason: "the harness crashed on startup".into(),
                });
            }
            Behavior::EscapeAttempt { absolute_path } => {
                let _ = std::fs::write(absolute_path, "written by the agent\n");
            }
            Behavior::NotInstalled => unreachable!("rejected in prepare"),
        }

        ctx.events.emit(EventPayload::AgentFinished {
            status: execution.status,
            exit_code: execution.exit_code,
            timed_out: execution.timed_out,
            duration_ms: execution.duration_ms,
            stdout_path: None,
            stderr_path: None,
        });
        Ok(execution)
    }
}

fn edits(path: &str, contents: &str) -> FakeAgent {
    FakeAgent::new(Behavior::Edit {
        path: path.into(),
        contents: contents.into(),
        exit_code: 0,
    })
}

fn mutates(changes: Vec<Mutation>) -> FakeAgent {
    FakeAgent::new(Behavior::Mutate {
        changes,
        exit_code: 0,
    })
}

fn protected_test_task() -> EngineeringTask {
    let mut task = task(&[(
        "tests",
        "for test_file in tests/*.sh; do [ -e \"$test_file\" ] || continue; sh \"$test_file\"; done",
    )]);
    task.protection = ProtectionPolicy::new(vec!["tests/**".into()], vec![]);
    task
}

// ------------------------------------------------------------- happy path

#[tokio::test]
async fn a_complete_run_produces_a_patch_an_evaluation_and_a_ledger_entry() {
    let fixture = Fixture::new();
    let runner = fixture.runner().await;
    let agent = edits("value.txt", "2\n");

    let mut request = RunRequest::new(task(&[("tests", "grep -q '^2$' value.txt")]), "claude");
    request.execution_provenance = ExecutionProvenance::Synthetic;
    let report = runner.execute(request, &agent).await.unwrap();

    // The three statuses, each saying its own thing.
    assert_eq!(report.run.status, RunStatus::Completed);
    assert_eq!(
        report.run.execution.as_ref().unwrap().status,
        AgentExecutionStatus::Completed
    );
    assert_eq!(report.outcome(), RunOutcome::Passed);
    assert_eq!(report.run.evaluation_verdict, Some(Verdict::Pass));
    assert!(report.run.world_model_context.is_none());

    // The patch was read out of Git, not reported by the agent.
    let patch = report.run.patch.as_ref().unwrap();
    assert_eq!(patch.files_changed, 1);
    assert_eq!(patch.insertions, 1);
    assert_eq!(patch.base_commit, fixture.repo.head_commit().unwrap());
    assert!(patch.head_commit.is_some(), "work should be committed");

    // Everything is in the ledger.
    let store = runner.store();
    let stored = store.load_run(&report.run.run_id).await.unwrap().unwrap();
    assert_eq!(stored.outcome, Some(RunOutcome::Passed));
    assert_eq!(stored.execution_provenance, ExecutionProvenance::Synthetic);
    assert_eq!(stored.selection_source, SelectionSource::Manual);
    let (policy_id, fingerprint, decision_id) = store
        .run_policy_link(&report.run.run_id)
        .await
        .unwrap()
        .expect("Phase 8 run policy link");
    let decision = store
        .policy_decision_by_id(&decision_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        decision.source,
        forge_core::PolicySelectionSource::ActivePolicy
    );
    assert_eq!(decision.selected_policy_id, policy_id);
    assert_eq!(decision.policy_fingerprint, fingerprint);
    assert!(decision.manual_override.is_none());
    let evaluation = store
        .load_evaluation(&report.run.run_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(evaluation.verdict, Verdict::Pass);
    assert!(!evaluation.metrics.is_empty());

    let events = store.events_for(&report.run.run_id).await.unwrap();
    let types: Vec<&str> = events.iter().map(|e| e.event_type()).collect();
    for required in [
        "RunStarted",
        "WorkspaceCreated",
        "PromptSubmitted",
        "AgentFinished",
        "PatchCaptured",
        "EvaluationStarted",
        "EvaluationCompleted",
        "RunCompleted",
    ] {
        assert!(types.contains(&required), "missing {required} in {types:?}");
    }
}

#[tokio::test]
async fn an_explicit_agent_override_wins_and_remains_distinct_policy_evidence() {
    let fixture = Fixture::new();
    let runner = fixture.runner().await;
    let agent = edits("value.txt", "2\n");
    let mut request = RunRequest::new(task(&[("tests", "grep -q '^2$' value.txt")]), "claude");
    request.execution_provenance = ExecutionProvenance::Synthetic;
    request.manual_policy_override = Some("agent=claude".into());

    let report = runner.execute(request, &agent).await.unwrap();
    let (_, _, decision_id) = runner
        .store()
        .run_policy_link(&report.run.run_id)
        .await
        .unwrap()
        .unwrap();
    let decision = runner
        .store()
        .policy_decision_by_id(&decision_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        decision.source,
        forge_core::PolicySelectionSource::ManualOverride
    );
    assert_eq!(decision.manual_override.as_deref(), Some("agent=claude"));

    let active = runner
        .store()
        .active_policy("distributed-runtime")
        .await
        .unwrap()
        .unwrap();
    let candidate = active.clone();
    let resolved = forge_policy::PolicyEvidenceResolver::new(runner.store().clone())
        .with_allowed_provenance([ExecutionProvenance::Synthetic])
        .resolve(&active, &candidate, chrono::Utc::now())
        .await
        .unwrap();
    assert!(resolved.snapshot.eligible.is_empty());
    assert!(resolved.snapshot.excluded.iter().any(|excluded| {
        excluded.run_id == report.run.run_id
            && matches!(
                excluded.exclusion,
                forge_core::EvidenceExclusion::ManualOverride
            )
    }));
}

#[tokio::test]
async fn a_shadow_policy_records_only_its_unexecuted_choice() {
    let fixture = Fixture::new();
    let runner = fixture.runner().await;
    let active = runner.ensure_active_policy().await.unwrap();
    let mut shadow = active.clone();
    shadow.policy_id = PolicyId::sequential(2);
    shadow.parent_policy_id = Some(active.policy_id.clone());
    shadow.status = PolicyStatus::Shadow;
    shadow.provenance = PolicyProvenance::OptimizerProposed;
    shadow.routing.use_learned_routing = true;
    shadow.created_at = chrono::Utc::now();
    runner.store().insert_policy(&shadow).await.unwrap();

    let mut request = RunRequest::new(task(&[("tests", "grep -q '^2$' value.txt")]), "claude");
    request.execution_provenance = ExecutionProvenance::Synthetic;
    let report = runner
        .execute(request, &edits("value.txt", "2\n"))
        .await
        .unwrap();
    assert_eq!(report.outcome(), RunOutcome::Passed);

    let shadows = runner
        .store()
        .shadow_decisions("distributed-runtime", 10)
        .await
        .unwrap();
    assert_eq!(shadows.len(), 1);
    assert_eq!(shadows[0].shadow_policy_id, shadow.policy_id);

    let resolved = forge_policy::PolicyEvidenceResolver::new(runner.store().clone())
        .with_allowed_provenance([ExecutionProvenance::Synthetic])
        .resolve(&active, &shadow, chrono::Utc::now())
        .await
        .unwrap();
    assert_eq!(
        resolved
            .snapshot
            .observations_for(&shadow.fingerprint())
            .len(),
        0
    );
}

#[tokio::test]
async fn the_agent_receives_the_task_contract_in_its_prompt() {
    let fixture = Fixture::new();
    let runner = fixture.runner().await;
    let agent = edits("value.txt", "2\n");

    runner
        .execute(
            RunRequest::new(task(&[("tests", "true")]), "claude"),
            &agent,
        )
        .await
        .unwrap();

    let prompts = agent.prompts();
    assert_eq!(prompts.len(), 1);
    let prompt = &prompts[0];
    assert!(prompt.contains("Raise the recorded value in value.txt to two"));
    assert!(prompt.contains("value.txt must remain a single integer"));
    assert!(prompt.contains(".forge/worktrees/R-0001"));
    assert!(prompt.contains("You are not the judge of this work"));
}

#[tokio::test]
async fn an_exact_world_model_supplies_and_records_compact_agent_context() {
    let fixture = Fixture::new();
    let runner = fixture.runner().await;
    let commit = fixture.repo.head_commit().unwrap();
    let snapshot_id = WorldModelSnapshotId::sequential(1);
    let fact_id = WorldModelFactId::stable(WorldEntityKind::Component, "value-storage");
    let snapshot = WorldModelSnapshot {
        snapshot_id: snapshot_id.clone(),
        repository: "distributed-runtime".into(),
        commit: commit.clone(),
        created_at: chrono::Utc::now(),
        source: WorldModelSnapshotSource::Deterministic,
        schema_version: WORLD_MODEL_SCHEMA_VERSION.into(),
        status: WorldModelSnapshotStatus::Complete,
        extractors: vec![ExtractorRecord {
            identity: ExtractorIdentity::new("fixture", "1"),
            required: true,
            status: ExtractorStatus::Completed,
            facts_produced: 1,
            configuration_fingerprint: "fixture".into(),
            error: None,
        }],
        facts: WorldModelFacts {
            components: vec![Component {
                metadata: FactMetadata::new(
                    fact_id.clone(),
                    snapshot_id.clone(),
                    EvidenceConfidence::Observed,
                    WorldModelProvenance {
                        extractor: ExtractorIdentity::new("fixture", "1"),
                        source: WorldModelProvenanceSource::SourceCode {
                            location: SourceLocation::new(
                                RepositoryPath::new("value.txt").unwrap(),
                                &commit,
                            ),
                        },
                    },
                ),
                name: "value storage".into(),
                description: "Owns value.txt".into(),
                paths: vec![RepositoryPath::new("value.txt").unwrap()],
                parent: None,
                tags: Vec::new(),
                related_tasks: vec![TaskId::sequential(1042)],
            }],
            ..Default::default()
        },
    };
    runner
        .store()
        .insert_world_model_snapshot(&snapshot)
        .await
        .unwrap();
    let agent = edits("value.txt", "2\n");
    let report = runner
        .execute(
            RunRequest::new(task(&[("tests", "true")]), "claude"),
            &agent,
        )
        .await
        .unwrap();

    let reference = report.run.world_model_context.as_ref().unwrap();
    assert_eq!(reference.snapshot_id, snapshot_id);
    assert_eq!(reference.fact_ids, vec![fact_id.clone()]);
    let prompt = &agent.prompts()[0];
    assert!(prompt.contains("Repository architecture context"));
    assert!(prompt.contains(fact_id.as_str()));
    let stored = runner
        .store()
        .load_run(&report.run.run_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.world_model_context, report.run.world_model_context);
}

// ------------------------------------------------------- the trust boundary

#[tokio::test]
async fn an_agent_claiming_success_does_not_make_a_failing_run_pass() {
    // The fake always reports "all tests pass". Forge runs the tests itself.
    let fixture = Fixture::new();
    let runner = fixture.runner().await;
    let agent = edits("value.txt", "999\n");

    let report = runner
        .execute(
            RunRequest::new(task(&[("tests", "grep -q '^2$' value.txt")]), "claude"),
            &agent,
        )
        .await
        .unwrap();

    assert_eq!(report.outcome(), RunOutcome::Failed);
    assert_eq!(report.run.evaluation_verdict, Some(Verdict::Fail));
    // The claim is preserved as trajectory data, and believed by nothing.
    assert_eq!(
        report
            .run
            .execution
            .as_ref()
            .unwrap()
            .self_report
            .as_deref(),
        Some("I completed the task and all tests pass.")
    );
    assert_eq!(
        report.run.execution.as_ref().unwrap().status,
        AgentExecutionStatus::Completed
    );
}

#[tokio::test]
async fn a_nonzero_agent_exit_with_a_passing_patch_still_passes() {
    let fixture = Fixture::new();
    let runner = fixture.runner().await;
    let agent = FakeAgent::new(Behavior::Edit {
        path: "value.txt".into(),
        contents: "2\n".into(),
        exit_code: 1,
    });

    let report = runner
        .execute(
            RunRequest::new(task(&[("tests", "grep -q '^2$' value.txt")]), "claude"),
            &agent,
        )
        .await
        .unwrap();

    assert_eq!(
        report.run.execution.as_ref().unwrap().status,
        AgentExecutionStatus::NonZeroExit
    );
    assert_eq!(report.outcome(), RunOutcome::Passed);
}

#[tokio::test]
async fn a_timed_out_agent_is_still_judged_on_what_it_left_behind() {
    let fixture = Fixture::new();
    let runner = fixture.runner().await;
    let agent = FakeAgent::new(Behavior::TimedOut);

    let report = runner
        .execute(
            RunRequest::new(task(&[("tests", "true")]), "claude"),
            &agent,
        )
        .await
        .unwrap();

    let execution = report.run.execution.as_ref().unwrap();
    assert!(execution.timed_out);
    assert_eq!(execution.status, AgentExecutionStatus::TimedOut);
    // It changed nothing, so there is nothing to pass.
    assert_eq!(report.outcome(), RunOutcome::NoChange);
    assert_eq!(report.run.status, RunStatus::Completed);
}

#[tokio::test]
async fn producing_no_changes_is_not_success_even_when_every_check_passes() {
    let fixture = Fixture::new();
    let runner = fixture.runner().await;
    let agent = FakeAgent::new(Behavior::NoOp);

    let report = runner
        .execute(
            RunRequest::new(task(&[("tests", "true")]), "claude"),
            &agent,
        )
        .await
        .unwrap();

    assert_eq!(report.run.evaluation_verdict, Some(Verdict::Pass));
    assert_eq!(report.outcome(), RunOutcome::NoChange);
    assert!(report.run.patch.as_ref().unwrap().is_empty());
}

#[tokio::test]
async fn a_change_with_no_configured_checks_is_inconclusive() {
    let fixture = Fixture::new();
    let runner = fixture.runner().await;
    let agent = edits("value.txt", "2\n");

    let report = runner
        .execute(RunRequest::new(task(&[]), "claude"), &agent)
        .await
        .unwrap();

    assert_eq!(report.outcome(), RunOutcome::Inconclusive);
    assert!(report.evaluation.is_none());
}

#[tokio::test]
async fn deleting_a_failing_protected_test_cannot_turn_green_into_a_pass() {
    let fixture = Fixture::new();
    let runner = fixture.runner().await;
    let agent = mutates(vec![Mutation::Delete {
        path: "tests/value.sh".into(),
    }]);

    let report = runner
        .execute(RunRequest::new(protected_test_task(), "claude"), &agent)
        .await
        .unwrap();

    assert_eq!(report.run.evaluation_verdict, Some(Verdict::Pass));
    assert_eq!(report.outcome(), RunOutcome::Inconclusive);
    let integrity = report.run.integrity.as_ref().unwrap();
    assert_eq!(integrity.status, forge_core::IntegrityStatus::Missing);
    assert_eq!(integrity.deleted, vec!["tests/value.sh"]);
    assert!(
        report
            .run
            .warnings
            .iter()
            .any(|warning| { warning.kind == forge_core::WarningKind::ProtectedPathDeleted })
    );

    let stored = runner
        .store()
        .load_run(&report.run.run_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.integrity, report.run.integrity);
    assert_eq!(stored.warnings, report.run.warnings);
}

#[tokio::test]
async fn weakening_a_failing_protected_test_cannot_turn_green_into_a_pass() {
    let fixture = Fixture::new();
    let runner = fixture.runner().await;
    let agent = edits("tests/value.sh", "true\n");

    let report = runner
        .execute(RunRequest::new(protected_test_task(), "claude"), &agent)
        .await
        .unwrap();

    assert_eq!(report.run.evaluation_verdict, Some(Verdict::Pass));
    assert_eq!(report.outcome(), RunOutcome::Inconclusive);
    let integrity = report.run.integrity.as_ref().unwrap();
    assert_eq!(integrity.status, forge_core::IntegrityStatus::Modified);
    assert_eq!(integrity.modified, vec!["tests/value.sh"]);
}

#[tokio::test]
async fn an_explicitly_allowed_protected_test_change_is_recorded_and_judged_normally() {
    let fixture = Fixture::new();
    let runner = fixture.runner().await;
    let agent = edits("tests/value.sh", "grep -q '^1$' value.txt\n");
    let mut task = protected_test_task();
    task.protection.allowed = vec!["tests/value.sh".into()];

    let report = runner
        .execute(RunRequest::new(task, "claude"), &agent)
        .await
        .unwrap();

    assert_eq!(report.outcome(), RunOutcome::Passed);
    let integrity = report.run.integrity.as_ref().unwrap();
    assert_eq!(integrity.status, forge_core::IntegrityStatus::Clean);
    assert_eq!(integrity.allowed, vec!["tests/value.sh"]);
    assert!(
        report
            .run
            .warnings
            .iter()
            .any(|warning| { warning.kind == forge_core::WarningKind::ProtectedPathAllowed })
    );
}

#[tokio::test]
async fn a_normal_source_patch_has_clean_integrity() {
    let fixture = Fixture::new();
    let runner = fixture.runner().await;
    let agent = edits("value.txt", "2\n");

    let report = runner
        .execute(RunRequest::new(protected_test_task(), "claude"), &agent)
        .await
        .unwrap();

    assert_eq!(report.outcome(), RunOutcome::Passed);
    assert_eq!(
        report.run.integrity.as_ref().unwrap().status,
        forge_core::IntegrityStatus::Clean
    );
    assert!(report.run.integrity.as_ref().unwrap().allowed.is_empty());
}

#[tokio::test]
async fn evaluation_uses_the_task_loaded_before_the_agent_mutates_workspace_files() {
    let fixture = Fixture::new();
    let runner = fixture.runner().await;
    let agent = mutates(vec![
        Mutation::Write {
            path: "value.txt".into(),
            contents: "999\n".into(),
        },
        Mutation::Write {
            path: "task.yaml".into(),
            contents: "evaluation:\n  tests: true\n".into(),
        },
    ]);

    let report = runner
        .execute(
            RunRequest::new(task(&[("tests", "grep -q '^2$' value.txt")]), "claude"),
            &agent,
        )
        .await
        .unwrap();

    assert_eq!(report.run.evaluation_verdict, Some(Verdict::Fail));
    assert_eq!(report.outcome(), RunOutcome::Failed);
    assert_eq!(
        report
            .evaluation
            .as_ref()
            .unwrap()
            .check("tests")
            .unwrap()
            .command
            .as_deref(),
        Some("grep -q '^2$' value.txt")
    );
}

// ----------------------------------------------------------- evaluation

#[tokio::test]
async fn each_configured_check_runs_separately_and_keeps_its_evidence() {
    let fixture = Fixture::new();
    let runner = fixture.runner().await;
    let agent = edits("value.txt", "2\n");

    let report = runner
        .execute(
            RunRequest::new(
                task(&[
                    ("tests", "grep -q '^2$' value.txt"),
                    ("benchmark", "echo 'throughput: 4.72 GB/s'"),
                ]),
                "claude",
            ),
            &agent,
        )
        .await
        .unwrap();

    let evaluation = report.evaluation.as_ref().unwrap();
    assert_eq!(evaluation.checks.len(), 2);

    for name in ["tests", "benchmark"] {
        let check = evaluation.check(name).unwrap();
        assert_eq!(check.verdict, Verdict::Pass, "{name}");
        assert!(check.command.is_some(), "{name} lost its command");
        assert_eq!(check.exit_code, Some(0), "{name}");

        // Full output is kept as a run artifact, not just summarized.
        let output = check.output_path.as_ref().expect("output path");
        assert!(output.exists(), "{name} output not written");
        let body = std::fs::read_to_string(output).unwrap();
        assert!(body.contains("exit: 0"), "{body}");
    }

    let benchmark_log = std::fs::read_to_string(
        evaluation
            .check("benchmark")
            .unwrap()
            .output_path
            .as_ref()
            .unwrap(),
    )
    .unwrap();
    assert!(benchmark_log.contains("throughput: 4.72 GB/s"));
}

#[tokio::test]
async fn structured_benchmark_metrics_are_parsed_and_runtime_output_is_not_patch_content() {
    let fixture = Fixture::new();
    let runner = fixture.runner().await;
    let agent = mutates(vec![
        Mutation::Write {
            path: "value.txt".into(),
            contents: "2\n".into(),
        },
        Mutation::Write {
            path: ".forge-metrics.json".into(),
            contents: r#"{"metrics":{"forged":{"value":999,"direction":"maximize"}}}"#.into(),
        },
    ]);
    let mut task = task(&[("tests", "grep -q '^2$' value.txt")]);
    task.evaluation.benchmark = Some(
        forge_core::BenchmarkSpec::new(
            r#"printf '%s' '{"metrics":{"throughput":{"value":4720.3,"unit":"MB/s","direction":"maximize"}}}' > .forge-metrics.json"#,
        )
        .with_metrics_file(".forge-metrics.json"),
    );

    let report = runner
        .execute(RunRequest::new(task, "claude"), &agent)
        .await
        .unwrap();

    assert_eq!(report.outcome(), RunOutcome::Passed);
    let evaluation = report.evaluation.as_ref().unwrap();
    assert_eq!(evaluation.metric("throughput").unwrap().value, 4720.3);
    assert!(evaluation.metric("forged").is_none());
    let patch = report.run.patch.as_ref().unwrap();
    assert_eq!(patch.files_changed, 1);
    assert_eq!(patch.excluded[0].path, ".forge-metrics.json");
    assert!(
        std::fs::read_to_string(patch.diff_path.as_ref().unwrap())
            .unwrap()
            .contains("value.txt")
    );
    assert!(
        !std::fs::read_to_string(patch.diff_path.as_ref().unwrap())
            .unwrap()
            .contains("forge-metrics")
    );
}

#[tokio::test]
async fn multidimensional_smoke_preserves_one_deliberate_failure_and_all_other_evidence() {
    let fixture = Fixture::new();
    let runner = fixture.runner().await;
    let agent = mutates(vec![
        Mutation::Write {
            path: "value.txt".into(),
            contents: "2\n".into(),
        },
        Mutation::Write {
            path: ".benchmark.json".into(),
            contents: r#"{"metrics":{"forged":{"value":999,"direction":"maximize"}}}"#.into(),
        },
        Mutation::Write {
            path: ".complexity.json".into(),
            contents: r#"{"metrics":{"forged":{"value":0,"direction":"minimize"}}}"#.into(),
        },
    ]);

    let json_command = |path: &str, name: &str, value: u64, direction: &str| {
        format!(
            "printf '%s' '{{\"metrics\":{{\"{name}\":{{\"value\":{value},\"unit\":\"points\",\"direction\":\"{direction}\"}}}}}}' > {path}"
        )
    };
    let mut phase_two = task(&[("tests", "grep -q '^2$' value.txt"), ("lint", "true")]);
    phase_two.evaluation.benchmark = Some(
        BenchmarkSpec::new(json_command(
            ".benchmark.json",
            "throughput",
            100,
            "maximize",
        ))
        .with_metrics_file(".benchmark.json"),
    );
    phase_two.evaluation.security = Some(CommandSpec::new("true"));
    phase_two.evaluation.complexity = Some(
        BenchmarkSpec::new(json_command(
            ".complexity.json",
            "branch_points",
            3,
            "minimize",
        ))
        .with_metrics_file(".complexity.json"),
    );
    phase_two.evaluation.custom.push(NamedCommand {
        name: "api_contract".into(),
        spec: CommandSpec::new("test \"$(tr -d '\\n' < value.txt)\" = 2"),
        metrics_file: None,
    });

    let passing = runner
        .execute(RunRequest::new(phase_two.clone(), "claude"), &agent)
        .await
        .unwrap();
    let passing_evaluation = passing.evaluation.as_ref().unwrap();
    assert_eq!(passing.outcome(), RunOutcome::Passed);
    assert_eq!(passing_evaluation.verdict, Verdict::Pass);
    assert_eq!(passing_evaluation.checks.len(), 6);
    assert!(
        passing_evaluation
            .checks
            .iter()
            .all(|check| check.verdict == Verdict::Pass)
    );

    phase_two.evaluation.security = Some(CommandSpec::new(
        "echo 'deliberate fixture finding' >&2; exit 9",
    ));
    let report = runner
        .execute(RunRequest::new(phase_two, "claude"), &agent)
        .await
        .unwrap();
    let evaluation = report.evaluation.as_ref().unwrap();
    assert_eq!(report.outcome(), RunOutcome::Failed);
    assert_eq!(evaluation.verdict, Verdict::Fail);
    assert_eq!(evaluation.checks.len(), 6);
    assert_eq!(evaluation.check("security").unwrap().verdict, Verdict::Fail);
    assert_eq!(
        evaluation.check("security").unwrap().kind,
        EvaluatorKind::Security
    );
    for id in ["tests", "lint", "complexity", "benchmark", "api_contract"] {
        assert_eq!(evaluation.check(id).unwrap().verdict, Verdict::Pass, "{id}");
    }
    assert_eq!(evaluation.metric("throughput").unwrap().value, 100.0);
    assert_eq!(evaluation.metric("branch_points").unwrap().value, 3.0);
    assert!(evaluation.metric("forged").is_none());
    let patch = report.run.patch.as_ref().unwrap();
    assert_eq!(patch.files_changed, 1);
    assert_eq!(patch.excluded.len(), 2);

    let events = runner.store().events_for(&report.run.run_id).await.unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| event.event_type() == "EvaluatorStarted")
            .count(),
        6
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event.event_type() == "EvaluatorCompleted")
            .count(),
        6
    );
}

#[tokio::test]
async fn a_failing_check_records_why_without_stopping_the_others() {
    let fixture = Fixture::new();
    let runner = fixture.runner().await;
    let agent = edits("value.txt", "2\n");

    let report = runner
        .execute(
            RunRequest::new(
                task(&[
                    ("tests", "echo 'assertion failed: value != 3' >&2; exit 101"),
                    ("lint", "true"),
                ]),
                "claude",
            ),
            &agent,
        )
        .await
        .unwrap();

    let evaluation = report.evaluation.as_ref().unwrap();
    let tests = evaluation.check("tests").unwrap();
    assert_eq!(tests.verdict, Verdict::Fail);
    assert_eq!(tests.exit_code, Some(101));
    assert!(tests.detail.as_ref().unwrap().contains("assertion failed"));
    // The other check still ran.
    assert_eq!(evaluation.check("lint").unwrap().verdict, Verdict::Pass);
    assert_eq!(report.outcome(), RunOutcome::Failed);
}

#[tokio::test]
async fn an_evaluation_command_that_hangs_fails_at_its_timeout() {
    let fixture = Fixture::new();
    let runner = fixture.runner().await;
    let agent = edits("value.txt", "2\n");

    let mut task = task(&[]);
    let mut spec = CommandSpec::new("sleep 30");
    spec.timeout_secs = Some(1);
    task.evaluation.tests = Some(spec);

    let report = runner
        .execute(RunRequest::new(task, "claude"), &agent)
        .await
        .unwrap();

    let tests = report.evaluation.as_ref().unwrap().check("tests").unwrap();
    assert_eq!(tests.verdict, Verdict::Fail);
    assert!(tests.detail.as_ref().unwrap().contains("timed out"));
    assert_eq!(report.outcome(), RunOutcome::Failed);
}

// -------------------------------------------------------- workspace safety

#[tokio::test]
async fn the_primary_working_tree_is_never_touched() {
    let fixture = Fixture::new();
    let runner = fixture.runner().await;
    let agent = edits("value.txt", "2\n");

    let before = fixture.read("value.txt");
    let report = runner
        .execute(
            RunRequest::new(task(&[("tests", "true")]), "claude"),
            &agent,
        )
        .await
        .unwrap();

    assert_eq!(fixture.read("value.txt"), before);
    assert_eq!(fixture.read("value.txt"), "1\n");
    assert_eq!(fixture.repo.head_commit().unwrap(), report.run.base_commit);

    // The work lives on its own branch, reachable after the workspace is gone.
    let branch = report.branch.as_ref().unwrap();
    assert_eq!(branch, "forge/R-0001");
    let head = fixture.repo.resolve(branch).unwrap();
    assert_eq!(Some(head), report.run.patch.as_ref().unwrap().head_commit);
}

#[tokio::test]
async fn the_workspace_is_removed_after_a_clean_run_but_the_branch_survives() {
    let fixture = Fixture::new();
    let runner = fixture.runner().await;
    let agent = edits("value.txt", "2\n");

    let report = runner
        .execute(
            RunRequest::new(task(&[("tests", "true")]), "claude"),
            &agent,
        )
        .await
        .unwrap();

    let workspace = report.run.workspace_path.as_ref().unwrap();
    assert!(!workspace.exists(), "workspace should have been removed");
    assert!(fixture.repo.branch_exists("forge/R-0001"));
}

#[tokio::test]
async fn the_workspace_can_be_kept_for_inspection() {
    let fixture = Fixture::new();
    let runner = fixture.runner().await;
    let agent = edits("value.txt", "2\n");

    let mut request = RunRequest::new(task(&[("tests", "true")]), "claude");
    request.keep_workspace = Some(true);
    let report = runner.execute(request, &agent).await.unwrap();

    assert!(report.workspace_kept);
    assert!(report.run.workspace_path.as_ref().unwrap().exists());
}

#[tokio::test]
async fn run_artifacts_stay_inside_the_runs_directory() {
    let fixture = Fixture::new();
    let runner = fixture.runner().await;
    let agent = edits("value.txt", "2\n");

    let report = runner
        .execute(
            RunRequest::new(task(&[("tests", "true")]), "claude"),
            &agent,
        )
        .await
        .unwrap();

    let runs_dir = fixture.root().join(".forge/runs");
    let artifacts = report.run.artifacts.directory.as_ref().unwrap();
    assert!(artifacts.starts_with(&runs_dir), "{}", artifacts.display());
    assert_eq!(artifacts.file_name().unwrap(), "R-0001");

    let diff = report
        .run
        .patch
        .as_ref()
        .unwrap()
        .diff_path
        .as_ref()
        .unwrap();
    assert!(diff.starts_with(&runs_dir));
    assert!(std::fs::read_to_string(diff).unwrap().contains("value.txt"));
}

/// Documents the isolation limit rather than pretending it does not exist.
#[tokio::test]
async fn an_agent_writing_outside_its_workspace_is_not_contained_but_is_not_captured() {
    let fixture = Fixture::new();
    let runner = fixture.runner().await;
    let escape_target = fixture.root().join("escaped.txt");
    let agent = FakeAgent::new(Behavior::EscapeAttempt {
        absolute_path: escape_target.clone(),
    });

    let report = runner
        .execute(
            RunRequest::new(task(&[("tests", "true")]), "claude"),
            &agent,
        )
        .await
        .unwrap();

    // Forge does not (and before container isolation cannot) prevent this.
    assert!(
        escape_target.exists(),
        "test premise: the write should have succeeded"
    );
    // What Forge does guarantee: the escape is not part of the captured change,
    // and the run is not credited for it.
    assert_eq!(report.outcome(), RunOutcome::NoChange);
    assert!(report.run.patch.as_ref().unwrap().is_empty());
    let diff_path = report.run.patch.as_ref().unwrap().diff_path.clone();
    assert!(diff_path.is_none());
}

// ------------------------------------------------------------- failures

#[tokio::test]
async fn a_missing_agent_executable_fails_before_anything_is_provisioned() {
    let fixture = Fixture::new();
    let runner = fixture.runner().await;
    let agent = FakeAgent::new(Behavior::NotInstalled);

    let err = runner
        .execute(
            RunRequest::new(task(&[("tests", "true")]), "claude"),
            &agent,
        )
        .await
        .expect_err("should not start");

    assert!(matches!(err, RunnerError::Agent(_)), "{err}");
    // Nothing was recorded: an uninstalled CLI is not evidence about an agent.
    assert_eq!(runner.store().run_count().await.unwrap(), 0);
    assert!(!fixture.root().join(".forge/worktrees/R-0001").exists());
}

#[tokio::test]
async fn an_invalid_task_is_rejected_before_a_run_is_created() {
    let fixture = Fixture::new();
    let runner = fixture.runner().await;
    let agent = edits("value.txt", "2\n");

    let mut invalid = task(&[("tests", "true")]);
    invalid.objective = "   ".into();

    let err = runner
        .execute(RunRequest::new(invalid, "claude"), &agent)
        .await
        .expect_err("should be rejected");
    assert!(matches!(err, RunnerError::Task(_)), "{err}");
    assert_eq!(runner.store().run_count().await.unwrap(), 0);
}

#[tokio::test]
async fn a_task_for_another_repository_is_refused() {
    let fixture = Fixture::new();
    let runner = fixture.runner().await;
    let agent = edits("value.txt", "2\n");

    let mut foreign = task(&[("tests", "true")]);
    foreign.repository = "some-other-repo".into();

    let err = runner
        .execute(RunRequest::new(foreign, "claude"), &agent)
        .await
        .expect_err("should be refused");
    assert!(matches!(err, RunnerError::WrongRepository { .. }), "{err}");
}

#[tokio::test]
async fn an_unknown_base_revision_is_reported_clearly() {
    let fixture = Fixture::new();
    let runner = fixture.runner().await;
    let agent = edits("value.txt", "2\n");

    let mut request = RunRequest::new(task(&[("tests", "true")]), "claude");
    request.base_rev = Some("no-such-branch".into());

    let err = runner
        .execute(request, &agent)
        .await
        .expect_err("unknown rev");
    assert!(matches!(err, RunnerError::Git(_)), "{err}");
}

#[tokio::test]
async fn an_agent_that_cannot_execute_leaves_a_recorded_failed_run() {
    let fixture = Fixture::new();
    let runner = fixture.runner().await;
    let agent = FakeAgent::new(Behavior::Unavailable);

    let report = runner
        .execute(
            RunRequest::new(task(&[("tests", "true")]), "claude"),
            &agent,
        )
        .await
        .unwrap();

    assert_eq!(report.run.status, RunStatus::Failed);
    assert_eq!(report.outcome(), RunOutcome::Errored);
    assert!(report.run.failure_reason.is_some());

    // The failure is in the ledger, with events explaining it.
    let stored = runner
        .store()
        .load_run(&report.run.run_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.status, RunStatus::Failed);
    let events = runner.store().events_for(&report.run.run_id).await.unwrap();
    assert!(events.iter().any(|e| e.event_type() == "RunFailed"));
    assert!(events.iter().any(|e| e.event_type() == "WorkspaceCreated"));

    // A failed run keeps its workspace, since that is what you debug.
    assert!(report.run.workspace_path.as_ref().unwrap().exists());
}

#[tokio::test]
async fn a_workspace_that_cannot_be_created_is_recorded_as_a_failed_run() {
    let fixture = Fixture::new();
    let runner = fixture.runner().await;
    let agent = edits("value.txt", "2\n");

    // Occupy the path the first run will want, so provisioning fails.
    let worktrees = fixture.root().join(".forge/worktrees");
    std::fs::create_dir_all(worktrees.join("R-0001")).unwrap();
    std::fs::write(worktrees.join("R-0001/blocker"), "in the way").unwrap();

    let report = runner
        .execute(
            RunRequest::new(task(&[("tests", "true")]), "claude"),
            &agent,
        )
        .await
        .unwrap();

    assert_eq!(report.run.status, RunStatus::Failed);
    assert_eq!(report.outcome(), RunOutcome::Errored);
    assert!(report.run.execution.is_none());
    assert_eq!(runner.store().run_count().await.unwrap(), 1);
}

#[tokio::test]
async fn a_ledger_that_cannot_be_opened_is_reported_rather_than_ignored() {
    let fixture = Fixture::new();
    // A directory where the database file must go.
    let db_path = fixture.root().join(".forge/forge.db");
    std::fs::create_dir_all(&db_path).unwrap();

    let err = Store::open(&db_path).await.expect_err("should not open");
    assert!(!err.to_string().is_empty(), "{err}");
}

// ------------------------------------------------------------- accounting

#[tokio::test]
async fn usage_reported_by_the_harness_reaches_the_ledger() {
    let fixture = Fixture::new();
    let runner = fixture.runner().await;
    let agent = edits("value.txt", "2\n");

    let report = runner
        .execute(
            RunRequest::new(task(&[("tests", "true")]), "claude"),
            &agent,
        )
        .await
        .unwrap();

    let usage = report.run.usage();
    assert_eq!(usage.input_tokens, Some(1000));
    assert_eq!(usage.cost_usd, Some(0.02));

    let summaries = runner.store().list_runs(10).await.unwrap();
    assert_eq!(summaries.len(), 1);
    let summary = &summaries[0];
    assert_eq!(summary.cost_usd, Some(0.02));
    assert_eq!(summary.outcome, Some(RunOutcome::Passed));
    assert_eq!(summary.agent_status, Some(AgentExecutionStatus::Completed));
    assert_eq!(summary.files_changed, Some(1));
}

#[tokio::test]
async fn concurrent_runs_get_distinct_workspaces_and_ids() {
    let fixture = Fixture::new();
    let runner = fixture.runner().await;

    let first = runner
        .execute(
            RunRequest::new(task(&[("tests", "true")]), "claude"),
            &edits("value.txt", "2\n"),
        )
        .await
        .unwrap();
    let second = runner
        .execute(
            RunRequest::new(task(&[("tests", "true")]), "claude"),
            &edits("value.txt", "3\n"),
        )
        .await
        .unwrap();

    assert_ne!(first.run.run_id, second.run.run_id);
    assert_ne!(first.branch, second.branch);
    // Both started from the same base, which is what makes them comparable.
    assert_eq!(first.run.base_commit, second.run.base_commit);
}

#[tokio::test]
async fn the_configured_timeout_reaches_the_run_record() {
    let fixture = Fixture::new();
    let runner = fixture.runner().await;
    let agent = edits("value.txt", "2\n");

    let mut request = RunRequest::new(task(&[("tests", "true")]), "claude");
    request.timeout = Some(Duration::from_secs(120));

    let report = runner.execute(request, &agent).await.unwrap();
    // The explicit request may shorten the configured hard limit, not raise it.
    assert_eq!(report.run.agent.timeout_secs, Some(60));
}
