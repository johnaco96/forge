//! Competitive orchestration tests using deterministic, no-network agents.

use std::path::Path;
use std::process::Command;

use async_trait::async_trait;
use forge_agent::adapter::{AgentAdapter, RunContext};
use forge_agent::error::{AgentError, AgentResult};
use forge_core::agent::{AdapterStatus, AgentDescriptor};
use forge_core::config::ForgeConfig;
use forge_core::ids::{AgentId, TaskId};
use forge_core::integrity::{IntegrityStatus, ProtectionPolicy};
use forge_core::run::{AgentExecution, AgentExecutionStatus, RunOutcome, SelectionSource, Usage};
use forge_core::task::{CommandSpec, EngineeringTask, EvaluationSpec, TaskMetadata};
use forge_git::Repository;
use forge_runner::{Competitor, ExperimentRequest, Runner, RunnerError};
use forge_store::Store;

struct Fixture {
    _temp: tempfile::TempDir,
    repo: Repository,
    config: ForgeConfig,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("distributed-runtime");
        std::fs::create_dir_all(root.join("tests")).unwrap();
        git(&root, &["init", "--quiet", "--initial-branch=main"]);
        git(&root, &["config", "user.email", "forge@example.invalid"]);
        git(&root, &["config", "user.name", "Forge Tests"]);
        std::fs::write(root.join("value.txt"), "1\n").unwrap();
        std::fs::write(root.join("tests/value.sh"), "grep -q '^2$' value.txt\n").unwrap();
        git(&root, &["add", "-A"]);
        git(&root, &["commit", "--quiet", "-m", "initial"]);
        let repo = Repository::open(&root).unwrap();
        let mut config = ForgeConfig::default_for("distributed-runtime");
        config.defaults.timeout_secs = 10;
        Self {
            _temp: temp,
            repo,
            config,
        }
    }

    async fn runner(&self) -> Runner {
        let store = Store::open(self.repo.root().join(".forge/forge.db"))
            .await
            .unwrap();
        Runner::new(self.repo.clone(), self.config.clone(), store)
    }
}

fn git(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn task() -> EngineeringTask {
    EngineeringTask {
        task_id: TaskId::sequential(1042),
        repository: "distributed-runtime".into(),
        objective: "Raise value.txt to two".into(),
        constraints: vec!["value.txt stays an integer".into()],
        evaluation: EvaluationSpec {
            tests: Some(CommandSpec::new("grep -q '^2$' value.txt")),
            lint: Some(CommandSpec::new("true")),
            ..Default::default()
        },
        protection: ProtectionPolicy::new(vec!["tests/**".into()], Vec::new()),
        metadata: TaskMetadata::default(),
        classification: Default::default(),
        components: Vec::new(),
        tags: Vec::new(),
    }
}

#[derive(Clone)]
enum Behavior {
    Write { path: String, contents: String },
    NoChange,
    TimedOut,
    ExecutionError,
}

struct FakeAgent {
    id: String,
    behavior: Behavior,
    cost_usd: Option<f64>,
}

impl FakeAgent {
    fn writes(id: &str, path: &str, contents: &str) -> Self {
        Self {
            id: id.into(),
            behavior: Behavior::Write {
                path: path.into(),
                contents: contents.into(),
            },
            cost_usd: (id == "claude").then_some(0.01),
        }
    }

    fn with_behavior(id: &str, behavior: Behavior) -> Self {
        Self {
            id: id.into(),
            behavior,
            cost_usd: None,
        }
    }
}

#[async_trait]
impl AgentAdapter for FakeAgent {
    fn descriptor(&self) -> AgentDescriptor {
        AgentDescriptor {
            agent_id: AgentId::new(&self.id).unwrap(),
            display_name: format!("Fake {}", self.id),
            harness: "fake".into(),
            executable: None,
            default_model: None,
            capabilities: Vec::new(),
            adapter_status: AdapterStatus::Implemented,
        }
    }

    async fn prepare(&self) -> AgentResult<()> {
        Ok(())
    }

    async fn execute(&self, ctx: &RunContext<'_>) -> AgentResult<AgentExecution> {
        if matches!(self.behavior, Behavior::ExecutionError) {
            return Err(AgentError::Unavailable {
                agent: self.id.clone(),
                reason: "deterministic execution failure".into(),
            });
        }
        if let Behavior::Write { path, contents } = &self.behavior {
            let target = ctx.workspace.path.join(path);
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(target, contents).unwrap();
        }
        let now = chrono::Utc::now();
        let timed_out = matches!(self.behavior, Behavior::TimedOut);
        Ok(AgentExecution {
            status: if timed_out {
                AgentExecutionStatus::TimedOut
            } else {
                AgentExecutionStatus::Completed
            },
            exit_code: (!timed_out).then_some(0),
            timed_out,
            started_at: now,
            finished_at: now,
            duration_ms: if timed_out { 10_000 } else { 5 },
            stdout_path: None,
            stderr_path: None,
            usage: Usage {
                input_tokens: Some(if self.id == "claude" { 100 } else { 200 }),
                output_tokens: Some(20),
                cost_usd: self.cost_usd,
            },
            self_report: None,
            harness_metadata: Default::default(),
        })
    }
}

async fn compete(
    runner: &Runner,
    left: &FakeAgent,
    right: &FakeAgent,
) -> forge_runner::ExperimentReport {
    runner
        .compete(
            ExperimentRequest::new(task()),
            vec![
                Competitor::new(&left.id, left),
                Competitor::new(&right.id, right),
            ],
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn two_successful_agents_share_one_base_but_use_isolated_worktrees() {
    let fixture = Fixture::new();
    let runner = fixture.runner().await;
    let base = fixture.repo.head_commit().unwrap();
    let claude = FakeAgent::writes("claude", "value.txt", "2\n");
    let codex = FakeAgent::writes("codex", "value.txt", "2\n");

    let report = compete(&runner, &claude, &codex).await;

    assert_eq!(report.execution_strategy, "sequential");
    assert_eq!(report.runs.len(), 2);
    assert!(
        report
            .runs
            .iter()
            .all(|run| run.outcome() == RunOutcome::Passed)
    );
    assert!(report.runs.iter().all(|run| run.run.base_commit == base));
    assert!(report.runs.iter().all(|run| matches!(
        &run.run.selection_source,
        SelectionSource::Competition { experiment_id }
            if experiment_id == &report.experiment.experiment_id
    )));
    assert_ne!(report.runs[0].branch, report.runs[1].branch);
    assert_ne!(
        report.runs[0].run.workspace_path,
        report.runs[1].run.workspace_path
    );
    assert_eq!(
        std::fs::read_to_string(fixture.repo.root().join("value.txt")).unwrap(),
        "1\n"
    );

    let stored = runner
        .store()
        .load_experiment(&report.experiment.experiment_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.run_ids.len(), 2);
    assert_eq!(stored.base_commit, base);
    let event_types = runner
        .store()
        .experiment_events_for(&stored.experiment_id)
        .await
        .unwrap()
        .into_iter()
        .map(|event| event.event_type().to_string())
        .collect::<Vec<_>>();
    assert_eq!(
        event_types,
        vec![
            "ExperimentStarted",
            "ParticipantRunStarted",
            "ParticipantRunCompleted",
            "ParticipantRunStarted",
            "ParticipantRunCompleted",
            "ExperimentCompleted",
        ]
    );
}

#[tokio::test]
async fn a_pass_and_a_fail_are_both_preserved() {
    let fixture = Fixture::new();
    let runner = fixture.runner().await;
    let pass = FakeAgent::writes("claude", "value.txt", "2\n");
    let fail = FakeAgent::writes("codex", "value.txt", "999\n");

    let report = compete(&runner, &pass, &fail).await;
    assert_eq!(report.runs[0].outcome(), RunOutcome::Passed);
    assert_eq!(report.runs[1].outcome(), RunOutcome::Failed);
    assert_eq!(runner.store().run_count().await.unwrap(), 2);
}

#[tokio::test]
async fn a_pass_and_a_timeout_form_a_completed_experiment() {
    let fixture = Fixture::new();
    let runner = fixture.runner().await;
    let pass = FakeAgent::writes("claude", "value.txt", "2\n");
    let timeout = FakeAgent::with_behavior("codex", Behavior::TimedOut);

    let report = compete(&runner, &pass, &timeout).await;
    assert_eq!(report.runs[0].outcome(), RunOutcome::Passed);
    assert_eq!(
        report.runs[1].run.execution.as_ref().unwrap().status,
        AgentExecutionStatus::TimedOut
    );
    assert_eq!(report.runs[1].outcome(), RunOutcome::NoChange);
    assert_eq!(
        report.experiment.status,
        forge_core::experiment::ExperimentStatus::Completed
    );
}

#[tokio::test]
async fn no_change_and_integrity_violation_remain_distinct_evidence() {
    let fixture = Fixture::new();
    let runner = fixture.runner().await;
    let no_change = FakeAgent::with_behavior("claude", Behavior::NoChange);
    let violates = FakeAgent::writes("codex", "tests/value.sh", "true\n");

    let report = compete(&runner, &no_change, &violates).await;
    assert_eq!(report.runs[0].outcome(), RunOutcome::NoChange);
    assert_eq!(
        report.runs[1].run.integrity.as_ref().unwrap().status,
        IntegrityStatus::Modified
    );
    assert_eq!(report.runs[1].outcome(), RunOutcome::Failed);
}

#[tokio::test]
async fn one_agent_execution_error_does_not_erase_the_other_result() {
    let fixture = Fixture::new();
    let runner = fixture.runner().await;
    let broken = FakeAgent::with_behavior("claude", Behavior::ExecutionError);
    let pass = FakeAgent::writes("codex", "value.txt", "2\n");

    let report = compete(&runner, &broken, &pass).await;
    assert_eq!(report.runs[0].outcome(), RunOutcome::Errored);
    assert_eq!(report.runs[1].outcome(), RunOutcome::Passed);
    assert_eq!(report.experiment.run_ids.len(), 2);
}

#[tokio::test]
async fn duplicate_agents_and_too_few_agents_fail_before_an_experiment_exists() {
    let fixture = Fixture::new();
    let runner = fixture.runner().await;
    let claude = FakeAgent::writes("claude", "value.txt", "2\n");

    let duplicate = runner
        .compete(
            ExperimentRequest::new(task()),
            vec![
                Competitor::new("claude", &claude),
                Competitor::new("claude", &claude),
            ],
        )
        .await
        .unwrap_err();
    assert!(matches!(duplicate, RunnerError::DuplicateCompetitor(_)));

    let too_few = runner
        .compete(
            ExperimentRequest::new(task()),
            vec![Competitor::new("claude", &claude)],
        )
        .await
        .unwrap_err();
    assert!(matches!(too_few, RunnerError::TooFewCompetitors));
    assert_eq!(runner.store().experiment_count().await.unwrap(), 0);
}
