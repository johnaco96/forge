//! Deterministic end-to-end tests for `forge team`.
//!
//! Every agent executable is a local shell stub. The tests exercise the real
//! adapters, worktrees, ordinary run pipeline, team scheduler, final evaluator,
//! and SQLite ledger without network access or model calls.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use forge_core::events::EvaluationSubject;
use forge_core::ids::{RunId, TeamExecutionId, TeamNodeId};
use forge_core::run::ExecutionProvenance;
use forge_core::team::{PlanSourceKind, ReviewDecision, TeamNodeStatus, TeamOutcome, TeamStatus};
use forge_store::Store;
use tempfile::TempDir;

const APPROVE_ENVELOPE: &str = r#"{"is_error":false,"subtype":"success","result":"{\"decision\":\"approve\",\"findings\":[]}","session_id":"team-claude","total_cost_usd":0.001,"num_turns":1,"usage":{"input_tokens":40,"output_tokens":10},"permission_denials":[],"terminal_reason":"completed","type":"result","duration_ms":10}"#;
const REQUEST_CHANGES_ENVELOPE: &str = r#"{"is_error":false,"subtype":"success","result":"{\"decision\":\"request_changes\",\"findings\":[{\"category\":\"correctness\",\"severity\":\"high\",\"explanation\":\"The change needs revision.\"}]}","session_id":"team-review","total_cost_usd":0.001,"num_turns":1,"usage":{"input_tokens":40,"output_tokens":10},"permission_denials":[],"terminal_reason":"completed","type":"result","duration_ms":10}"#;
const CODEX_STREAM: &str = r#"{"type":"thread.started","thread_id":"team-codex"}
{"type":"turn.started"}
{"type":"item.completed","item":{"id":"item-1","type":"file_change","changes":[{"path":"value.txt","kind":"update"}],"status":"completed"}}
{"type":"item.completed","item":{"id":"item-2","type":"agent_message","text":"Implemented the assigned team node."}}
{"type":"turn.completed","usage":{"input_tokens":80,"cached_input_tokens":20,"output_tokens":20,"reasoning_output_tokens":5}}"#;

struct Stub {
    path: PathBuf,
    invocations: PathBuf,
}

impl Stub {
    fn claude(dir: &Path, name: &str, envelope: &str) -> Self {
        Self::claude_with_body(dir, name, envelope, "true")
    }

    fn claude_with_value(dir: &Path, name: &str, envelope: &str, value: u64) -> Self {
        Self::claude_with_body(
            dir,
            name,
            envelope,
            &format!("printf '%s\\n' '{value}' > value.txt"),
        )
    }

    fn claude_with_body(dir: &Path, name: &str, envelope: &str, body: &str) -> Self {
        let path = dir.join(name);
        let invocations = dir.join(format!("{name}.args"));
        let script = format!(
            "#!/bin/sh\n\
             printf '%s\\n' '--- invocation ---' >> '{args}'\n\
             for arg in \"$@\"; do printf '%s\\n' \"$arg\" >> '{args}'; done\n\
             {body}\n\
             cat <<'FORGE_JSON'\n{envelope}\nFORGE_JSON\n",
            args = invocations.display(),
        );
        write_executable(&path, &script);
        Self { path, invocations }
    }

    fn codex(dir: &Path, name: &str, value: u64) -> Self {
        let path = dir.join(name);
        let invocations = dir.join(format!("{name}.args"));
        let script = format!(
            "#!/bin/sh\n\
             printf '%s\\n' '--- invocation ---' >> '{args}'\n\
             for arg in \"$@\"; do printf '%s\\n' \"$arg\" >> '{args}'; done\n\
             printf '%s\\n' '{value}' > value.txt\n\
             cat <<'FORGE_JSONL'\n{stream}\nFORGE_JSONL\n",
            args = invocations.display(),
            stream = CODEX_STREAM,
        );
        write_executable(&path, &script);
        Self { path, invocations }
    }

    fn invocation_text(&self) -> String {
        std::fs::read_to_string(&self.invocations).unwrap_or_default()
    }
}

struct Fixture {
    temp: TempDir,
    repo: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("distributed-runtime");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "--quiet", "--initial-branch=main"]);
        git(&repo, &["config", "user.email", "forge@example.invalid"]);
        git(&repo, &["config", "user.name", "Forge Tests"]);
        std::fs::write(repo.join("value.txt"), "1\n").unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "--quiet", "-m", "initial commit"]);
        let fixture = Self { temp, repo };
        assert!(fixture.forge(&["init"]).status.success());
        fixture
    }

    fn forge(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_forge"))
            .args(args)
            .current_dir(&self.repo)
            .output()
            .unwrap()
    }

    fn configure(&self, claude: &Stub, codex: &Stub, synthetic: bool) {
        let path = self.repo.join(".forge/config.toml");
        let mut config = std::fs::read_to_string(&path).unwrap();
        let provenance = if synthetic {
            "execution_provenance = \"synthetic\"\n"
        } else {
            ""
        };
        config.push_str(&format!(
            "\n[agents.claude]\n{provenance}executable = \"{}\"\n\
             \n[agents.codex]\n{provenance}executable = \"{}\"\n",
            claude.path.display(),
            codex.path.display(),
        ));
        std::fs::write(path, config).unwrap();
    }

    fn mark_agents_synthetic(&self) {
        let path = self.repo.join(".forge/config.toml");
        let config = std::fs::read_to_string(&path)
            .unwrap()
            .replace(
                "\n[agents.claude]\nexecutable",
                "\n[agents.claude]\nexecution_provenance = \"synthetic\"\nexecutable",
            )
            .replace(
                "\n[agents.codex]\nexecutable",
                "\n[agents.codex]\nexecution_provenance = \"synthetic\"\nexecutable",
            );
        std::fs::write(path, config).unwrap();
    }

    fn lower_routing_threshold(&self) {
        let path = self.repo.join(".forge/config.toml");
        let config = std::fs::read_to_string(&path)
            .unwrap()
            .replace("minimum_total_evidence = 10", "minimum_total_evidence = 6");
        std::fs::write(path, config).unwrap();
    }

    fn write_task(&self, name: &str, contents: &str) -> String {
        let relative = format!(".forge/tasks/{name}");
        std::fs::write(self.repo.join(&relative), contents).unwrap();
        relative
    }

    async fn store(&self) -> Store {
        Store::open(self.repo.join(".forge/forge.db"))
            .await
            .unwrap()
    }
}

fn root_task() -> &'static str {
    "task_id: T-1042\n\
     repository: distributed-runtime\n\
     objective: Raise the recorded value in value.txt to two\n\
     constraints:\n\
     \x20 - value.txt must remain a single integer\n\
     classification:\n\
     \x20 category: debugging\n\
     \x20 language: rust\n\
     \x20 domain: concurrency\n\
     \x20 difficulty: medium\n\
     components:\n\
     \x20 - scheduler\n\
     evaluation:\n\
     \x20 tests:\n\
     \x20   command: grep -q '^2$' value.txt\n\
     \x20 lint:\n\
     \x20   command: 'true'\n"
}

fn successful_team_task() -> String {
    format!(
        "{}team:\n\
         \x20 nodes:\n\
         \x20   - id: inspect\n\
         \x20     objective: Inspect why the value is stale\n\
         \x20     execution: analysis\n\
         \x20     outputs: [structured_findings]\n\
         \x20     assignment:\n\
         \x20       strategy: explicit\n\
         \x20       agent: claude\n\
         \x20   - id: implement\n\
         \x20     objective: Implement the value correction\n\
         \x20     execution: implementation\n\
         \x20     depends_on: [inspect]\n\
         \x20     inputs: [structured_findings]\n\
         \x20     outputs: [candidate_patch, candidate_commit, evaluation]\n\
         \x20     assignment:\n\
         \x20       strategy: explicit\n\
         \x20       agent: codex\n\
         \x20   - id: review\n\
         \x20     objective: Review the candidate and recovery semantics\n\
         \x20     execution: review\n\
         \x20     depends_on: [implement]\n\
         \x20     inputs: [candidate_patch, candidate_commit, evaluation]\n\
         \x20     outputs: [review]\n\
         \x20     assignment:\n\
         \x20       strategy: explicit\n\
         \x20       agent: claude\n",
        root_task()
    )
}

fn implementation_review_task() -> String {
    format!(
        "{}team:\n\
         \x20 nodes:\n\
         \x20   - id: implement\n\
         \x20     objective: Implement the value correction\n\
         \x20     execution: implementation\n\
         \x20     outputs: [candidate_patch, candidate_commit]\n\
         \x20     assignment: {{ strategy: explicit, agent: codex }}\n\
         \x20   - id: review\n\
         \x20     objective: Review the candidate\n\
         \x20     execution: review\n\
         \x20     depends_on: [implement]\n\
         \x20     inputs: [candidate_patch]\n\
         \x20     outputs: [review]\n\
         \x20     assignment: {{ strategy: explicit, agent: claude }}\n",
        root_task()
    )
}

fn write_executable(path: &Path, contents: &str) {
    std::fs::write(path, contents).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

fn git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[tokio::test]
async fn controlled_team_passes_with_handoffs_final_evaluation_and_baseline() {
    let fixture = Fixture::new();
    let claude = Stub::claude(fixture.temp.path(), "claude-team", APPROVE_ENVELOPE);
    let codex = Stub::codex(fixture.temp.path(), "codex-team", 2);
    fixture.configure(&claude, &codex, true);
    let ordinary = fixture.write_task("ordinary.yaml", root_task());
    let baseline = fixture.forge(&["run", &ordinary, "--agent", "codex"]);
    assert!(
        baseline.status.success(),
        "{}\n{}",
        stdout(&baseline),
        stderr(&baseline)
    );
    let task = fixture.write_task("team.yaml", &successful_team_task());

    let output = fixture.forge(&["team", &task]);
    let text = stdout(&output);
    assert!(output.status.success(), "{text}\n{}", stderr(&output));
    for expected in [
        "Forge team TE-0001",
        "inspect",
        "implement",
        "review",
        "APPROVE",
        "Final evaluation",
        "tests",
        "lint",
        "PASS",
        "Single-agent baseline",
        "R-0001",
    ] {
        assert!(text.contains(expected), "missing {expected:?}:\n{text}");
    }

    let store = fixture.store().await;
    let team = store
        .load_team_execution(&TeamExecutionId::sequential(1))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(team.status, TeamStatus::Completed);
    assert_eq!(team.outcome, Some(TeamOutcome::Passed));
    assert_eq!(team.plan_provenance.source, PlanSourceKind::Explicit);
    assert_eq!(team.execution_provenance, ExecutionProvenance::Synthetic);
    assert_eq!(team.nodes.len(), 3);
    assert_eq!(team.artifacts.len(), 5);
    assert_eq!(team.run_ids().len(), 3);
    assert_eq!(team.resources.agent_run_count, 3);
    assert_eq!(team.resources.failed_attempt_count, 0);
    assert_eq!(team.resources.total_tokens, Some(200));
    assert_eq!(team.resources.known_cost_usd, None);
    assert_eq!(
        team.baseline_comparison
            .as_ref()
            .and_then(|comparison| comparison.baseline_run_id.as_ref()),
        Some(&RunId::sequential(1))
    );
    let inspect = team.node(&TeamNodeId::new("inspect").unwrap()).unwrap();
    let implement = team.node(&TeamNodeId::new("implement").unwrap()).unwrap();
    let review = team.node(&TeamNodeId::new("review").unwrap()).unwrap();
    assert_eq!(inspect.status, TeamNodeStatus::Succeeded);
    assert_eq!(implement.status, TeamNodeStatus::Succeeded);
    assert_eq!(review.status, TeamNodeStatus::Succeeded);
    assert_eq!(implement.input_commit, inspect.output_commit);
    assert_eq!(review.input_commit, implement.output_commit);
    assert_eq!(implement.input_artifact_ids, inspect.output_artifact_ids);
    assert_eq!(
        review.review.as_ref().unwrap().decision,
        ReviewDecision::Approve
    );
    assert_eq!(
        team.final_evaluation.as_ref().unwrap().verdict,
        forge_core::Verdict::Pass
    );
    assert_eq!(
        team.final_evaluation
            .as_ref()
            .unwrap()
            .evaluation
            .as_ref()
            .unwrap()
            .subject,
        EvaluationSubject::TeamExecution(TeamExecutionId::sequential(1))
    );
    for run_id in team.run_ids() {
        assert!(store.load_run(&run_id).await.unwrap().is_some());
    }
    assert_eq!(store.run_count().await.unwrap(), 4);
    let implementation_run_id = implement.run_ids.first().unwrap();
    let run_evaluation_events = store
        .events_for(implementation_run_id)
        .await
        .unwrap()
        .into_iter()
        .filter(|event| event.payload.evaluation_subject().is_some())
        .collect::<Vec<_>>();
    assert!(!run_evaluation_events.is_empty());
    assert!(run_evaluation_events.iter().all(|event| {
        event.payload.evaluation_subject()
            == Some(&EvaluationSubject::Run(implementation_run_id.clone()))
    }));
    let team_evaluation_events = store
        .evaluation_events_for_team(&TeamExecutionId::sequential(1))
        .await
        .unwrap();
    assert_eq!(team_evaluation_events.len(), 6);
    assert!(team_evaluation_events.iter().all(|event| {
        event.evaluation_subject()
            == Some(&EvaluationSubject::TeamExecution(
                TeamExecutionId::sequential(1),
            ))
    }));
    assert_eq!(
        store
            .team_events_for(&TeamExecutionId::sequential(1))
            .await
            .unwrap()
            .last()
            .unwrap()
            .seq as usize,
        store
            .team_events_for(&TeamExecutionId::sequential(1))
            .await
            .unwrap()
            .len()
    );

    let claude_invocations = claude.invocation_text();
    assert!(claude_invocations.contains("worktrees/R-0002"));
    assert!(claude_invocations.contains("worktrees/R-0004"));
    assert!(claude_invocations.contains("Dependency artifact"));
    let codex_invocations = codex.invocation_text();
    assert!(codex_invocations.contains("worktrees/R-0001"));
    assert!(codex_invocations.contains("worktrees/R-0003"));
    assert_eq!(
        std::fs::read_to_string(fixture.repo.join("value.txt")).unwrap(),
        "1\n"
    );
}

#[tokio::test]
async fn failed_implementation_blocks_review_but_independent_sibling_continues() {
    let fixture = Fixture::new();
    let claude = Stub::claude(fixture.temp.path(), "claude-failure", APPROVE_ENVELOPE);
    let codex = Stub::codex(fixture.temp.path(), "codex-failure", 3);
    fixture.configure(&claude, &codex, true);
    let task = fixture.write_task(
        "failure.yaml",
        &format!(
            "{}team:\n\
             \x20 nodes:\n\
             \x20   - id: inspect\n\
             \x20     objective: Inspect the stale value\n\
             \x20     execution: analysis\n\
             \x20     outputs: [structured_findings]\n\
             \x20     assignment: {{ strategy: explicit, agent: claude }}\n\
             \x20   - id: implement\n\
             \x20     objective: Implement the correction\n\
             \x20     execution: implementation\n\
             \x20     depends_on: [inspect]\n\
             \x20     inputs: [structured_findings]\n\
             \x20     assignment: {{ strategy: explicit, agent: codex }}\n\
             \x20   - id: review\n\
             \x20     objective: Review the failed candidate\n\
             \x20     execution: review\n\
             \x20     depends_on: [implement]\n\
             \x20     assignment: {{ strategy: explicit, agent: claude }}\n\
             \x20   - id: zz-sibling\n\
             \x20     objective: Independently inspect documentation impact\n\
             \x20     execution: analysis\n\
             \x20     outputs: [structured_findings]\n\
             \x20     assignment: {{ strategy: explicit, agent: claude }}\n",
            root_task()
        ),
    );

    let output = fixture.forge(&["team", &task]);
    let text = stdout(&output);
    assert_eq!(output.status.code(), Some(2), "{text}\n{}", stderr(&output));
    assert!(text.contains("failed"), "{text}");
    assert!(text.contains("blocked"), "{text}");

    let team = fixture
        .store()
        .await
        .load_team_execution(&TeamExecutionId::sequential(1))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        team.node(&TeamNodeId::new("implement").unwrap())
            .unwrap()
            .status,
        TeamNodeStatus::Failed
    );
    assert_eq!(
        team.node(&TeamNodeId::new("review").unwrap())
            .unwrap()
            .status,
        TeamNodeStatus::Blocked
    );
    assert_eq!(
        team.node(&TeamNodeId::new("zz-sibling").unwrap())
            .unwrap()
            .status,
        TeamNodeStatus::Succeeded
    );
    assert_eq!(team.artifacts.len(), 2);
    assert_eq!(team.run_ids().len(), 3);
    let invocations = claude.invocation_text();
    assert!(invocations.contains("worktrees/R-0001"));
    assert!(invocations.contains("worktrees/R-0003"));
    assert!(
        team.baseline_comparison
            .as_ref()
            .unwrap()
            .baseline_run_id
            .is_none()
    );
}

#[tokio::test]
async fn review_request_changes_cannot_turn_a_passing_evaluation_into_team_pass() {
    let fixture = Fixture::new();
    let claude = Stub::claude(
        fixture.temp.path(),
        "claude-request-changes",
        REQUEST_CHANGES_ENVELOPE,
    );
    let codex = Stub::codex(fixture.temp.path(), "codex-reviewed", 2);
    fixture.configure(&claude, &codex, true);
    let task = fixture.write_task("review.yaml", &implementation_review_task());

    let output = fixture.forge(&["team", &task]);
    let text = stdout(&output);
    assert_eq!(output.status.code(), Some(2), "{text}\n{}", stderr(&output));
    assert!(text.contains("REQUEST CHANGES"), "{text}");
    let team = fixture
        .store()
        .await
        .load_team_execution(&TeamExecutionId::sequential(1))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        team.final_evaluation.as_ref().unwrap().verdict,
        forge_core::Verdict::Pass
    );
    assert_eq!(team.outcome, Some(TeamOutcome::Failed));
}

#[tokio::test]
async fn insufficient_auto_routing_evidence_blocks_and_keeps_decision_lineage() {
    let fixture = Fixture::new();
    let claude = Stub::claude(fixture.temp.path(), "claude-auto-stop", APPROVE_ENVELOPE);
    let codex = Stub::codex(fixture.temp.path(), "codex-auto-stop", 2);
    fixture.configure(&claude, &codex, true);
    let task = fixture.write_task(
        "auto-stop.yaml",
        &format!(
            "{}team:\n\
             \x20 nodes:\n\
             \x20   - id: implement\n\
             \x20     objective: Implement the correction\n\
             \x20     execution: implementation\n\
             \x20     assignment: {{ strategy: auto }}\n",
            root_task()
        ),
    );

    let output = fixture.forge(&["team", &task]);
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    let store = fixture.store().await;
    let team = store
        .load_team_execution(&TeamExecutionId::sequential(1))
        .await
        .unwrap()
        .unwrap();
    let node = team.node(&TeamNodeId::new("implement").unwrap()).unwrap();
    assert_eq!(node.status, TeamNodeStatus::AssignmentBlocked);
    assert!(node.routing_decision_id.is_some());
    assert!(node.run_ids.is_empty());
    assert_eq!(team.outcome, Some(TeamOutcome::Blocked));
}

#[tokio::test]
async fn auto_assignment_reuses_phase_four_router_and_preserves_selection_source() {
    let fixture = Fixture::new();
    let claude = Stub::claude_with_value(fixture.temp.path(), "claude-auto", APPROVE_ENVELOPE, 3);
    let codex = Stub::codex(fixture.temp.path(), "codex-auto", 2);
    fixture.configure(&claude, &codex, false);
    fixture.lower_routing_threshold();
    let ordinary = fixture.write_task("routing-history.yaml", root_task());

    // Controlled historical evidence: Codex passes and Claude produces no
    // patch. The team node itself is marked synthetic before it executes.
    for _ in 0..3 {
        assert!(
            fixture
                .forge(&["run", &ordinary, "--agent", "codex"])
                .status
                .success()
        );
        assert_eq!(
            fixture
                .forge(&["run", &ordinary, "--agent", "claude"])
                .status
                .code(),
            Some(2)
        );
    }
    fixture.mark_agents_synthetic();
    let task = fixture.write_task(
        "auto-team.yaml",
        &format!(
            "{}team:\n\
             \x20 nodes:\n\
             \x20   - id: implement\n\
             \x20     objective: Implement the correction\n\
             \x20     execution: implementation\n\
             \x20     assignment: {{ strategy: auto }}\n",
            root_task()
        ),
    );

    let output = fixture.forge(&["team", &task]);
    let text = stdout(&output);
    assert!(output.status.success(), "{text}\n{}", stderr(&output));
    let store = fixture.store().await;
    let team = store
        .load_team_execution(&TeamExecutionId::sequential(1))
        .await
        .unwrap()
        .unwrap();
    let node = team.node(&TeamNodeId::new("implement").unwrap()).unwrap();
    let assignment = node.assignment.as_ref().unwrap();
    assert_eq!(assignment.agent.agent_id.as_str(), "codex");
    assert_eq!(node.routing_decision_id, assignment.routing_decision_id);
    assert!(matches!(
        assignment.selection_source,
        forge_core::SelectionSource::Automatic { .. }
    ));
    assert_eq!(team.execution_provenance, ExecutionProvenance::Synthetic);
    let run = store.load_run(&node.run_ids[0]).await.unwrap().unwrap();
    assert_eq!(run.execution_provenance, ExecutionProvenance::Synthetic);
}
