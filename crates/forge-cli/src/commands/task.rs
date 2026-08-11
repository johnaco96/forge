//! `forge task validate` — check a task file before spending an agent run on it.

use std::path::PathBuf;

use anyhow::Result;
use forge_core::task::EngineeringTask;

use crate::output;

pub fn validate(path: PathBuf) -> Result<()> {
    // `EngineeringTask::load` already names the file in its error, so adding
    // context here would only repeat it.
    let task = EngineeringTask::load(&path)?;
    task.validate()?;

    let checks = task.evaluation.checks();
    let check_names = if checks.is_empty() {
        "none".to_string()
    } else {
        checks
            .iter()
            .map(|(name, _)| name.clone())
            .collect::<Vec<_>>()
            .join(", ")
    };

    println!("Task {} is valid\n", task.task_id);
    println!(
        "{}",
        output::fields(&[
            ("Repository", task.repository.clone()),
            ("Objective", first_line(&task.objective)),
            ("Constraints", task.constraints.len().to_string()),
            ("Checks", check_names),
        ])
    );

    if !task.constraints.is_empty() {
        println!(
            "\n{}",
            output::section("Constraints", output::bullets(&task.constraints))
        );
    }

    if !checks.is_empty() {
        println!(
            "\n{}",
            output::section(
                "Evaluation",
                output::table(
                    &["evaluator", "policy", "command"],
                    &checks
                        .iter()
                        .map(|(name, spec)| {
                            vec![
                                name.clone(),
                                if spec.required {
                                    "required"
                                } else {
                                    "optional"
                                }
                                .to_string(),
                                spec.command.clone(),
                            ]
                        })
                        .collect::<Vec<_>>(),
                )
            )
        );
    }

    let warnings = task.warnings();
    if !warnings.is_empty() {
        println!(
            "\n{}",
            output::section("Warnings", output::bullets(&warnings))
        );
    }

    Ok(())
}

/// Objectives are often multi-line; summaries show the first line only.
fn first_line(text: &str) -> String {
    let trimmed = text.trim();
    match trimmed.split('\n').next() {
        Some(line) if line.len() < trimmed.len() => format!("{} …", line.trim()),
        Some(line) => line.trim().to_string(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multi_line_objectives_are_summarized() {
        assert_eq!(first_line("one line"), "one line");
        assert_eq!(first_line("first\nsecond"), "first …");
        assert_eq!(first_line("  padded  "), "padded");
    }
}
