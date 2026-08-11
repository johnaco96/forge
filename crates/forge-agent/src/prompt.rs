//! The instruction Forge gives a coding agent.
//!
//! One function builds the prompt for every agent. It is deliberately not
//! harness-specific: the same instruction goes to Claude Code, Codex, or
//! anything else, because comparing agents is only meaningful if they were
//! asked the same thing in the same words.
//!
//! It is also deliberately plain string assembly. There is no planning step,
//! no model call, and no branching on anything but the task's own contents, so
//! the prompt for a given task is byte-identical every time — which is what
//! makes a run reproducible and a comparison fair.

use forge_core::task::EngineeringTask;
use forge_core::workspace::Workspace;

/// Builds the instruction for one task in one workspace.
///
/// The prompt tells the agent five things: what to achieve, what it must not
/// break, where it is allowed to work, that its own claims are not evidence,
/// and what will be run against its work.
pub fn build_agent_prompt(task: &EngineeringTask, workspace: &Workspace) -> String {
    let mut prompt = String::new();

    prompt.push_str(&format!("# Engineering task {}\n\n", task.task_id));

    prompt.push_str("## Objective\n\n");
    prompt.push_str(task.objective.trim());
    prompt.push_str("\n\n");

    prompt.push_str("## Constraints\n\n");
    if task.constraints.is_empty() {
        prompt.push_str(
            "No explicit constraints were specified. Apply the standards already \
             evident in this repository.\n\n",
        );
    } else {
        prompt.push_str(
            "These are invariants, not suggestions. A change that violates any of \
             them is not acceptable, however well it meets the objective.\n\n",
        );
        for constraint in &task.constraints {
            prompt.push_str(&format!("- {}\n", constraint.trim()));
        }
        prompt.push('\n');
    }

    prompt.push_str("## Your workspace\n\n");
    prompt.push_str(&format!(
        "You are working in an isolated Git worktree created for this task:\n\n\
         - Directory:   {}\n\
         - Branch:      {}\n\
         - Base commit: {}\n\n",
        workspace.path.display(),
        workspace.branch,
        workspace.base_commit
    ));
    prompt.push_str(
        "Inside that directory you may read and modify any file, and you should run \
         whatever development commands the work calls for — building, testing, \
         linting, running the code — to check your own progress.\n\n\
         Do not modify anything outside that directory. It is a disposable copy of \
         the repository that exists so your work cannot affect anyone else's; \
         nothing you do elsewhere on this machine will be captured, and changing \
         files outside it will invalidate the run.\n\n",
    );

    prompt.push_str("## How your work will be judged\n\n");
    prompt.push_str(
        "You are not the judge of this work. When you stop, an independent system \
         reads the diff out of Git and runs its own checks against it. Your summary \
         of what you did is recorded, but it carries no weight in the result: \
         stating that the tests pass does nothing unless they actually pass when \
         run.\n\n",
    );

    let checks = task.evaluation.checks();
    if checks.is_empty() {
        prompt.push_str(
            "No specific commands were configured for this task, so the change will \
             be assessed on its own merits. Use the repository's own conventions to \
             verify your work.\n\n",
        );
    } else {
        prompt.push_str("These commands will be run against your workspace:\n\n");
        for (name, spec) in &checks {
            prompt.push_str(&format!("- {name}: `{}`\n", spec.command));
        }
        prompt.push_str(
            "\nMake them pass by making the code correct. Do not delete, skip, weaken, \
             or narrow tests to get a green result, and do not modify the commands \
             themselves — that produces a change that measures well and is worth \
             nothing.\n\n",
        );
    }

    prompt.push_str("## Finishing\n\n");
    prompt.push_str(
        "Leave your work in the working tree. You do not need to commit, stage, \
         push, or open a pull request; everything in the workspace is captured \
         either way.\n\n\
         If you conclude the task cannot or should not be done, stop and say so \
         plainly rather than making an unrelated change. A run that reports an \
         honest obstacle is more useful than one that produces a change nobody \
         asked for.\n",
    );

    prompt
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_core::ids::{RunId, TaskId};
    use forge_core::integrity::ProtectionPolicy;
    use forge_core::task::{CommandSpec, EvaluationSpec, TaskMetadata};
    use forge_core::workspace::WorkspaceKind;
    use std::path::PathBuf;

    fn workspace() -> Workspace {
        Workspace::new(
            RunId::sequential(1),
            WorkspaceKind::Worktree,
            PathBuf::from("/repo/.forge/worktrees/R-0001"),
            "forge/R-0001",
            "a73cf2100000000000000000000000000000000",
        )
    }

    fn task() -> EngineeringTask {
        EngineeringTask {
            task_id: TaskId::sequential(1042),
            repository: "distributed-runtime".into(),
            objective: "Improve checkpoint write throughput".into(),
            constraints: vec![
                "All existing tests must pass".into(),
                "Recovery semantics cannot change".into(),
            ],
            evaluation: EvaluationSpec {
                tests: Some(CommandSpec::new("cargo test --workspace")),
                benchmark: Some(CommandSpec::new("./bench/checkpoint.sh").into()),
                ..Default::default()
            },
            protection: ProtectionPolicy::default(),
            metadata: TaskMetadata::default(),
            classification: Default::default(),
            components: Vec::new(),
            tags: Vec::new(),
        }
    }

    #[test]
    fn the_prompt_states_the_objective_and_every_constraint() {
        let prompt = build_agent_prompt(&task(), &workspace());
        assert!(prompt.contains("Improve checkpoint write throughput"));
        assert!(prompt.contains("- All existing tests must pass"));
        assert!(prompt.contains("- Recovery semantics cannot change"));
        assert!(prompt.contains("T-1042"));
    }

    #[test]
    fn the_prompt_scopes_the_agent_to_its_workspace() {
        let prompt = build_agent_prompt(&task(), &workspace());
        assert!(prompt.contains("/repo/.forge/worktrees/R-0001"));
        assert!(prompt.contains("forge/R-0001"));
        assert!(prompt.contains("a73cf2100000000000000000000000000000000"));
        assert!(prompt.contains("Do not modify anything outside that directory"));
    }

    #[test]
    fn the_prompt_permits_inspection_modification_and_commands() {
        let prompt = build_agent_prompt(&task(), &workspace());
        assert!(prompt.contains("read and modify any file"));
        assert!(prompt.contains("run \nwhatever") || prompt.contains("run whatever"));
    }

    #[test]
    fn the_prompt_says_forge_evaluates_independently() {
        let prompt = build_agent_prompt(&task(), &workspace());
        assert!(prompt.contains("You are not the judge of this work"));
        assert!(prompt.contains("carries no weight"));
    }

    #[test]
    fn the_prompt_lists_the_checks_and_forbids_gaming_them() {
        let prompt = build_agent_prompt(&task(), &workspace());
        assert!(prompt.contains("`cargo test --workspace`"));
        assert!(prompt.contains("`./bench/checkpoint.sh`"));
        assert!(prompt.contains("Do not delete, skip, weaken"));
    }

    #[test]
    fn a_task_without_constraints_or_checks_still_produces_a_usable_prompt() {
        let mut task = task();
        task.constraints.clear();
        task.evaluation = EvaluationSpec::default();

        let prompt = build_agent_prompt(&task, &workspace());
        assert!(prompt.contains("No explicit constraints"));
        assert!(prompt.contains("No specific commands were configured"));
        // The scoping and trust-boundary statements are never optional.
        assert!(prompt.contains("Do not modify anything outside that directory"));
        assert!(prompt.contains("You are not the judge of this work"));
    }

    /// Reproducibility and fair comparison both depend on this.
    #[test]
    fn prompt_generation_is_deterministic() {
        let (task, workspace) = (task(), workspace());
        let first = build_agent_prompt(&task, &workspace);
        for _ in 0..5 {
            assert_eq!(build_agent_prompt(&task, &workspace), first);
        }
    }

    #[test]
    fn the_prompt_does_not_depend_on_which_agent_will_receive_it() {
        // There is no agent parameter, and there should not be one: two agents
        // given different instructions cannot be meaningfully compared.
        let prompt = build_agent_prompt(&task(), &workspace());
        for name in ["Claude", "claude", "Codex", "anthropic"] {
            assert!(
                !prompt.contains(name),
                "prompt names a specific agent: {name}"
            );
        }
    }
}
