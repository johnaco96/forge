//! End-to-end `forge run` tests.
//!
//! These drive the real `forge` binary through the real Claude adapter, with a
//! stub standing in for the `claude` executable. That covers the parts a fake
//! `AgentAdapter` cannot: argument construction, the JSON result contract, exit
//! codes, and the printed report — without a network call or a token spent.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

/// A stub `claude` that records how it was invoked, optionally edits the
/// workspace, and prints a result envelope matching the real one.
struct StubClaude {
    path: PathBuf,
    args_file: PathBuf,
}

/// A stub `codex` that emits the documented `codex exec --json` JSONL stream.
struct StubCodex {
    path: PathBuf,
    args_file: PathBuf,
}

impl StubCodex {
    fn new(dir: &Path, name: &str, body: &str, stream: &str, exit_code: i32) -> Self {
        let path = dir.join(name);
        let args_file = dir.join(format!("{name}.args"));
        let script = format!(
            "#!/bin/sh\n\
             # Stub Codex CLI. No network or model process is started.\n\
             : > '{args}'\n\
             for arg in \"$@\"; do printf '%s\\n' \"$arg\" >> '{args}'; done\n\
             {body}\n\
             cat <<'FORGE_JSONL'\n{stream}\nFORGE_JSONL\n\
             exit {exit_code}\n",
            args = args_file.display(),
        );
        std::fs::write(&path, script).expect("write Codex stub");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                .expect("chmod Codex stub");
        }
        Self { path, args_file }
    }

    fn recorded_args(&self) -> Vec<String> {
        std::fs::read_to_string(&self.args_file)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn recorded_raw(&self) -> String {
        std::fs::read_to_string(&self.args_file).unwrap_or_default()
    }
}

impl StubClaude {
    /// `body` is shell run inside the workspace, standing in for the agent's
    /// edits. `envelope` is what the stub prints on stdout.
    fn new(dir: &Path, name: &str, body: &str, envelope: &str, exit_code: i32) -> Self {
        let path = dir.join(name);
        let args_file = dir.join(format!("{name}.args"));
        let script = format!(
            "#!/bin/sh\n\
             # Stub Claude Code. Records its arguments, then acts.\n\
             : > '{args}'\n\
             for arg in \"$@\"; do printf '%s\\n' \"$arg\" >> '{args}'; done\n\
             {body}\n\
             cat <<'FORGE_JSON'\n{envelope}\nFORGE_JSON\n\
             exit {exit_code}\n",
            args = args_file.display(),
        );
        std::fs::write(&path, script).expect("write stub");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                .expect("chmod stub");
        }
        Self { path, args_file }
    }

    /// The arguments Forge invoked the stub with, one per line.
    ///
    /// The prompt is multi-line, so it spans several entries here; use
    /// [`Self::recorded_raw`] to inspect its contents.
    fn recorded_args(&self) -> Vec<String> {
        std::fs::read_to_string(&self.args_file)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// Everything the stub was invoked with, as written.
    fn recorded_raw(&self) -> String {
        std::fs::read_to_string(&self.args_file).unwrap_or_default()
    }
}

const SUCCESS_ENVELOPE: &str = r#"{"is_error":false,"subtype":"success","result":"I updated the value.","session_id":"stub-session","total_cost_usd":0.0123,"num_turns":3,"usage":{"input_tokens":100,"output_tokens":20,"cache_read_input_tokens":5000},"permission_denials":[],"terminal_reason":"completed","type":"result","duration_ms":1234}"#;

const CODEX_SUCCESS_STREAM: &str = r#"{"type":"thread.started","thread_id":"stub-codex-thread"}
{"type":"turn.started"}
{"type":"item.completed","item":{"id":"item-1","type":"command_execution","command":"edit value.txt","status":"completed"}}
{"type":"item.completed","item":{"id":"item-2","type":"file_change","changes":[{"path":"value.txt","kind":"update"}],"status":"completed"}}
{"type":"item.completed","item":{"id":"item-3","type":"agent_message","text":"I updated the value."}}
{"type":"turn.completed","usage":{"input_tokens":200,"cached_input_tokens":150,"output_tokens":40,"reasoning_output_tokens":10}}"#;

struct Fixture {
    temp: TempDir,
    repo: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().expect("temp dir");
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
            .expect("run forge")
    }

    fn stub(&self, body: &str) -> StubClaude {
        self.stub_with("claude-stub", body, SUCCESS_ENVELOPE, 0)
    }

    fn stub_with(&self, name: &str, body: &str, envelope: &str, exit_code: i32) -> StubClaude {
        StubClaude::new(self.temp.path(), name, body, envelope, exit_code)
    }

    fn codex_stub(&self, body: &str) -> StubCodex {
        self.codex_stub_with("codex-stub", body, CODEX_SUCCESS_STREAM, 0)
    }

    fn codex_stub_with(&self, name: &str, body: &str, stream: &str, exit_code: i32) -> StubCodex {
        StubCodex::new(self.temp.path(), name, body, stream, exit_code)
    }

    /// Points `[agents.claude]` at the stub.
    fn use_stub(&self, stub: &StubClaude) {
        let config_path = self.repo.join(".forge/config.toml");
        let mut config = std::fs::read_to_string(&config_path).unwrap();
        config.push_str(&format!(
            "\n[agents.claude]\nexecutable = \"{}\"\n",
            stub.path.display()
        ));
        std::fs::write(&config_path, config).unwrap();
    }

    /// Points `[agents.codex]` at a local no-network stub.
    fn use_codex_stub(&self, stub: &StubCodex) {
        let config_path = self.repo.join(".forge/config.toml");
        let mut config = std::fs::read_to_string(&config_path).unwrap();
        config.push_str(&format!(
            "\n[agents.codex]\nexecutable = \"{}\"\n",
            stub.path.display()
        ));
        std::fs::write(&config_path, config).unwrap();
    }

    fn write_task(&self, name: &str, body: &str) -> String {
        let relative = format!(".forge/tasks/{name}");
        std::fs::write(self.repo.join(&relative), body).unwrap();
        relative
    }

    fn read(&self, relative: &str) -> String {
        std::fs::read_to_string(self.repo.join(relative)).unwrap_or_default()
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

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn task_yaml(checks: &str) -> String {
    format!(
        "task_id: T-1042\n\
         repository: distributed-runtime\n\
         objective: Raise the recorded value in value.txt to two\n\
         constraints:\n\
         \x20 - value.txt must remain a single integer\n\
         evaluation:\n{checks}"
    )
}

// ------------------------------------------------------------------ tests

#[test]
fn a_passing_run_reports_agent_and_evaluation_separately() {
    let fixture = Fixture::new();
    let stub = fixture.stub("echo 2 > value.txt");
    fixture.use_stub(&stub);
    let task = fixture.write_task(
        "raise.yaml",
        &task_yaml("  tests:\n    command: grep -q '^2$' value.txt\n"),
    );

    let output = fixture.forge(&["run", &task, "--agent", "claude"]);
    let text = stdout(&output);

    assert!(output.status.success(), "{}\n{}", text, stderr(&output));
    assert!(text.contains("Forge run R-0001"), "{text}");

    // The two judgments appear in separate blocks and never merge.
    assert!(text.contains("Agent execution"), "{text}");
    assert!(
        text.contains("Evaluation (run by Forge, not by the agent)"),
        "{text}"
    );
    assert!(text.contains("tests"), "{text}");
    assert!(text.contains("PASS"), "{text}");

    // Patch, usage, and cost all surface.
    assert!(text.contains("1 file changed"), "{text}");
    assert!(text.contains("$0.0123"), "{text}");
    assert!(
        text.contains("5,120"),
        "cache tokens should be counted: {text}"
    );
    assert!(text.contains("Security posture"), "{text}");
    assert!(text.contains("Host containment"), "{text}");
    assert!(text.contains("none"), "{text}");
    assert!(text.contains("bypassPermissions"), "{text}");
    assert!(text.contains("no host containment"), "{text}");
    assert!(text.contains("Evaluation integrity"), "{text}");
    assert!(text.contains("clean"), "{text}");

    // The user's working tree is untouched.
    assert_eq!(fixture.read("value.txt"), "1\n");
}

#[test]
fn the_adapter_invokes_claude_with_the_documented_contract() {
    let fixture = Fixture::new();
    let stub = fixture.stub("echo 2 > value.txt");
    fixture.use_stub(&stub);
    let task = fixture.write_task("raise.yaml", &task_yaml("  tests:\n    command: true\n"));

    assert!(fixture.forge(&["run", &task]).status.success());

    let args = stub.recorded_args();
    assert!(args.contains(&"--print".to_string()), "{args:?}");
    assert!(args.contains(&"--output-format".to_string()), "{args:?}");
    assert!(args.contains(&"json".to_string()), "{args:?}");
    assert!(args.contains(&"--permission-mode".to_string()), "{args:?}");

    // The prompt is passed as a single argument carrying the whole task
    // contract: objective, constraints, workspace scope, and trust boundary.
    let invocation = stub.recorded_raw();
    assert!(
        invocation.contains("# Engineering task T-1042"),
        "{invocation}"
    );
    assert!(invocation.contains("Raise the recorded value in value.txt to two"));
    assert!(invocation.contains("value.txt must remain a single integer"));
    assert!(invocation.contains("You are not the judge of this work"));
    assert!(invocation.contains("Do not modify anything outside that directory"));
    assert!(invocation.contains("worktrees/R-0001"));
}

#[test]
fn codex_runs_end_to_end_through_the_same_pipeline_without_network() {
    let fixture = Fixture::new();
    let stub = fixture.codex_stub("echo 2 > value.txt");
    fixture.use_codex_stub(&stub);
    let task = fixture.write_task(
        "raise-codex.yaml",
        &task_yaml("  tests:\n    command: grep -q '^2$' value.txt\n"),
    );

    let output = fixture.forge(&["run", &task, "--agent", "codex"]);
    let text = stdout(&output);
    assert!(output.status.success(), "{text}\n{}", stderr(&output));
    assert!(text.contains("codex (codex-cli)"), "{text}");
    assert!(text.contains("240 (200 in / 40 out)"), "{text}");
    assert!(
        text.contains("sandbox=workspace-write, approval=never"),
        "{text}"
    );
    assert!(text.contains("Evaluation integrity"), "{text}");
    assert!(text.contains("clean"), "{text}");
    assert!(text.contains("PASS"), "{text}");
    assert_eq!(fixture.read("value.txt"), "1\n");

    let args = stub.recorded_args();
    assert!(args.contains(&"exec".to_string()), "{args:?}");
    for expected in [
        "--json",
        "--sandbox",
        "workspace-write",
        "--ask-for-approval",
        "never",
        "--cd",
    ] {
        assert!(args.contains(&expected.to_string()), "{args:?}");
    }
    let invocation = stub.recorded_raw();
    assert!(invocation.contains("# Engineering task T-1042"));
    assert!(invocation.contains("You are not the judge of this work"));
    assert!(invocation.contains("worktrees/R-0001"));

    let stdout_log = fixture.read(".forge/runs/R-0001/agent.stdout.log");
    assert!(stdout_log.contains("stub-codex-thread"), "{stdout_log}");
}

#[test]
fn a_nonzero_codex_exit_is_separate_from_a_passing_forge_outcome() {
    let fixture = Fixture::new();
    let stub = fixture.codex_stub_with(
        "codex-nonzero",
        "echo 2 > value.txt",
        CODEX_SUCCESS_STREAM,
        7,
    );
    fixture.use_codex_stub(&stub);
    let task = fixture.write_task(
        "raise-codex.yaml",
        &task_yaml("  tests:\n    command: grep -q '^2$' value.txt\n"),
    );

    let output = fixture.forge(&["run", &task, "--agent", "codex"]);
    let text = stdout(&output);
    assert!(output.status.success(), "{text}\n{}", stderr(&output));
    assert!(text.contains("exited non-zero"), "{text}");
    assert!(text.contains("Exit code  7"), "{text}");
    assert!(text.contains("PASS"), "{text}");
}

#[test]
fn a_codex_timeout_is_enforced_and_recorded() {
    let fixture = Fixture::new();
    let stub = fixture.codex_stub_with("codex-hangs", "sleep 30", CODEX_SUCCESS_STREAM, 0);
    fixture.use_codex_stub(&stub);
    let task = fixture.write_task(
        "raise-codex.yaml",
        &task_yaml("  tests:\n    command: true\n"),
    );

    let started = std::time::Instant::now();
    let output = fixture.forge(&["run", &task, "--agent", "codex", "--timeout-secs", "1"]);
    let text = stdout(&output);
    assert_eq!(output.status.code(), Some(2), "{text}\n{}", stderr(&output));
    assert!(text.contains("timed out"), "{text}");
    assert!(text.contains("NO CHANGE"), "{text}");
    assert!(started.elapsed() < std::time::Duration::from_secs(10));
}

#[test]
fn a_missing_codex_executable_fails_before_workspace_creation() {
    let fixture = Fixture::new();
    let config_path = fixture.repo.join(".forge/config.toml");
    let mut config = std::fs::read_to_string(&config_path).unwrap();
    config.push_str("\n[agents.codex]\nexecutable = \"forge-codex-definitely-not-installed\"\n");
    std::fs::write(&config_path, config).unwrap();
    let task = fixture.write_task(
        "raise-codex.yaml",
        &task_yaml("  tests:\n    command: true\n"),
    );

    let output = fixture.forge(&["run", &task, "--agent", "codex"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("PATH"), "{}", stderr(&output));
    assert!(!fixture.repo.join(".forge/worktrees/R-0001").exists());
}

#[test]
fn a_failing_evaluation_does_not_pass_however_the_agent_reports() {
    // The stub always claims "I updated the value." and exits zero.
    let fixture = Fixture::new();
    let stub = fixture.stub("echo 999 > value.txt");
    fixture.use_stub(&stub);
    let task = fixture.write_task(
        "raise.yaml",
        &task_yaml("  tests:\n    command: grep -q '^2$' value.txt\n"),
    );

    let output = fixture.forge(&["run", &task]);
    let text = stdout(&output);

    assert_eq!(output.status.code(), Some(2), "{text}");
    assert!(text.contains("FAIL"), "{text}");
    // Agent execution still reports honestly that the process completed.
    assert!(text.contains("completed"), "{text}");
}

#[test]
fn an_agent_that_changes_nothing_does_not_pass() {
    let fixture = Fixture::new();
    let stub = fixture.stub("true");
    fixture.use_stub(&stub);
    let task = fixture.write_task("raise.yaml", &task_yaml("  tests:\n    command: true\n"));

    let output = fixture.forge(&["run", &task]);
    let text = stdout(&output);

    assert_eq!(output.status.code(), Some(2), "{text}");
    assert!(text.contains("NO CHANGE"), "{text}");
    assert!(text.contains("left the workspace unchanged"), "{text}");
}

#[test]
fn a_nonzero_agent_exit_with_a_passing_patch_still_passes() {
    let fixture = Fixture::new();
    let stub = fixture.stub_with("crashy", "echo 2 > value.txt", SUCCESS_ENVELOPE, 1);
    fixture.use_stub(&stub);
    let task = fixture.write_task(
        "raise.yaml",
        &task_yaml("  tests:\n    command: grep -q '^2$' value.txt\n"),
    );

    let output = fixture.forge(&["run", &task]);
    let text = stdout(&output);

    assert!(output.status.success(), "{text}");
    assert!(text.contains("exited non-zero"), "{text}");
    assert!(text.contains("PASS"), "{text}");
}

#[test]
fn unparseable_agent_output_does_not_fail_the_run() {
    let fixture = Fixture::new();
    let stub = fixture.stub_with("noisy", "echo 2 > value.txt", "not json at all", 0);
    fixture.use_stub(&stub);
    let task = fixture.write_task(
        "raise.yaml",
        &task_yaml("  tests:\n    command: grep -q '^2$' value.txt\n"),
    );

    let output = fixture.forge(&["run", &task]);
    let text = stdout(&output);
    assert!(output.status.success(), "{text}\n{}", stderr(&output));
    assert!(text.contains("PASS"), "{text}");
}

#[test]
fn a_missing_claude_executable_is_reported_before_any_work_starts() {
    let fixture = Fixture::new();
    let config_path = fixture.repo.join(".forge/config.toml");
    let mut config = std::fs::read_to_string(&config_path).unwrap();
    config.push_str("\n[agents.claude]\nexecutable = \"forge-claude-not-installed\"\n");
    std::fs::write(&config_path, config).unwrap();

    let task = fixture.write_task("raise.yaml", &task_yaml("  tests:\n    command: true\n"));
    let output = fixture.forge(&["run", &task]);

    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("PATH"), "{}", stderr(&output));
    // Nothing was provisioned.
    assert!(!fixture.repo.join(".forge/worktrees/R-0001").exists());
}

#[test]
fn an_invalid_task_is_rejected_before_the_agent_is_invoked() {
    let fixture = Fixture::new();
    let stub = fixture.stub("echo 2 > value.txt");
    fixture.use_stub(&stub);
    let task = fixture.write_task(
        "broken.yaml",
        "task_id: T-1\nrepository: distributed-runtime\nobjective: \"\"\n",
    );

    let output = fixture.forge(&["run", &task]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("objective must not be empty"),
        "{}",
        stderr(&output)
    );
    assert!(
        stub.recorded_args().is_empty(),
        "the agent should not have run"
    );
}

#[test]
fn an_unknown_agent_is_refused_with_a_pointer_to_the_listing() {
    let fixture = Fixture::new();
    let task = fixture.write_task("raise.yaml", &task_yaml("  tests:\n    command: true\n"));

    let output = fixture.forge(&["run", &task, "--agent", "gpt-9"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("forge agent list"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn an_agent_without_an_adapter_says_so() {
    let fixture = Fixture::new();
    let task = fixture.write_task("raise.yaml", &task_yaml("  tests:\n    command: true\n"));

    let output = fixture.forge(&["run", &task, "--agent", "pi"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("no adapter"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn running_outside_an_initialized_repository_explains_what_to_do() {
    let temp = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_forge"))
        .args(["run", "task.yaml"])
        .current_dir(temp.path())
        .output()
        .expect("run forge");
    assert!(!output.status.success());
    let message = stderr(&output);
    assert!(
        message.contains("forge init") || message.contains("Git repository"),
        "{message}"
    );
}

#[test]
fn the_run_is_recorded_with_its_artifacts() {
    let fixture = Fixture::new();
    let stub = fixture.stub("echo 2 > value.txt");
    fixture.use_stub(&stub);
    let task = fixture.write_task(
        "raise.yaml",
        &task_yaml("  tests:\n    command: grep -q '^2$' value.txt\n"),
    );

    assert!(fixture.forge(&["run", &task]).status.success());

    let run_dir = fixture.repo.join(".forge/runs/R-0001");
    assert!(run_dir.join("patch.diff").exists(), "diff not written");
    assert!(run_dir.join("prompt.txt").exists(), "prompt not recorded");
    assert!(
        run_dir.join("agent.stdout.log").exists(),
        "agent output not captured"
    );
    assert!(
        run_dir.join("checks/tests.log").exists(),
        "check output not captured"
    );

    assert!(fixture.repo.join(".forge/forge.db").exists());
    assert!(
        fixture
            .read(".forge/runs/R-0001/patch.diff")
            .contains("value.txt"),
        "diff should describe the change"
    );
    // The agent's work survives on its own branch.
    let branches = Command::new("git")
        .arg("-C")
        .arg(&fixture.repo)
        .args(["branch", "--list", "forge/R-0001"])
        .output()
        .unwrap();
    assert!(!String::from_utf8_lossy(&branches.stdout).trim().is_empty());
}

#[test]
fn the_workspace_can_be_kept_for_inspection() {
    let fixture = Fixture::new();
    let stub = fixture.stub("echo 2 > value.txt");
    fixture.use_stub(&stub);
    let task = fixture.write_task("raise.yaml", &task_yaml("  tests:\n    command: true\n"));

    let output = fixture.forge(&["run", &task, "--keep-workspace"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(stdout(&output).contains("(kept)"), "{}", stdout(&output));
    assert_eq!(
        std::fs::read_to_string(fixture.repo.join(".forge/worktrees/R-0001/value.txt")).unwrap(),
        "2\n"
    );
}

#[test]
fn a_task_naming_another_repository_is_refused() {
    let fixture = Fixture::new();
    let stub = fixture.stub("echo 2 > value.txt");
    fixture.use_stub(&stub);
    let task = fixture.write_task(
        "foreign.yaml",
        "task_id: T-2\nrepository: some-other-repo\nobjective: Do something useful here\n",
    );

    let output = fixture.forge(&["run", &task]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("some-other-repo"),
        "{}",
        stderr(&output)
    );
}
