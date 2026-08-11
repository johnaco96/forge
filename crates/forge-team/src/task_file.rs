use std::path::Path;

use forge_core::{EngineeringTask, TEAM_PLAN_VERSION, TeamPlan, TeamPlanNode};
use serde::Deserialize;

use crate::{TeamError, TeamResult};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TeamSection {
    #[serde(default = "default_plan_version")]
    plan_version: String,
    nodes: Vec<TeamPlanNode>,
}

fn default_plan_version() -> String {
    TEAM_PLAN_VERSION.into()
}

/// Loads one ordinary root task plus an explicit typed team plan from the same
/// YAML/JSON document. The `team` key is removed before the accepted task
/// parser runs, so ordinary `EngineeringTask` remains unchanged.
pub fn load_team_task(path: impl AsRef<Path>) -> TeamResult<(EngineeringTask, TeamPlan)> {
    let path = path.as_ref();
    let raw = std::fs::read_to_string(path).map_err(|source| TeamError::ReadTask {
        path: path.to_path_buf(),
        source,
    })?;
    let mut value: serde_json::Value = match extension(path).as_deref() {
        Some("json") => serde_json::from_str(&raw).map_err(|error| TeamError::ParseTask {
            path: path.to_path_buf(),
            message: error.to_string(),
        })?,
        Some("yaml" | "yml") => {
            serde_yaml_ng::from_str(&raw).map_err(|error| TeamError::ParseTask {
                path: path.to_path_buf(),
                message: error.to_string(),
            })?
        }
        _ => {
            return Err(TeamError::ParseTask {
                path: path.to_path_buf(),
                message: "expected a .yaml, .yml, or .json file".into(),
            });
        }
    };
    let object = value.as_object_mut().ok_or_else(|| TeamError::ParseTask {
        path: path.to_path_buf(),
        message: "top-level task document must be an object".into(),
    })?;
    let team_value = object.remove("team").ok_or(TeamError::MissingPlan)?;
    let team: TeamSection =
        serde_json::from_value(team_value).map_err(|error| TeamError::ParseTask {
            path: path.to_path_buf(),
            message: format!("invalid `team` plan: {error}"),
        })?;
    let task: EngineeringTask =
        serde_json::from_value(value).map_err(|error| TeamError::ParseTask {
            path: path.to_path_buf(),
            message: error.to_string(),
        })?;
    task.validate()?;
    Ok((
        task.clone(),
        TeamPlan {
            plan_version: team.plan_version,
            root_objective: task.objective,
            nodes: team.nodes,
        },
    ))
}

fn extension(path: &Path) -> Option<String> {
    path.extension()
        .map(|extension| extension.to_string_lossy().to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_yaml_plan_uses_id_alias_and_preserves_root_task_semantics() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("task.yaml");
        std::fs::write(
            &path,
            "task_id: T-1\n\
             repository: forge\n\
             objective: Repair the fixture\n\
             evaluation: {}\n\
             team:\n\
             \x20 nodes:\n\
             \x20   - id: inspect\n\
             \x20     objective: Inspect the fixture\n\
             \x20     execution: analysis\n\
             \x20     assignment: { strategy: explicit, agent: claude }\n",
        )
        .unwrap();
        let (task, plan) = load_team_task(path).unwrap();
        assert_eq!(task.task_id.as_str(), "T-1");
        assert_eq!(plan.root_objective, task.objective);
        assert_eq!(plan.nodes[0].node_id.as_str(), "inspect");
        plan.validate().unwrap();
    }

    #[test]
    fn missing_and_untyped_plans_are_rejected_before_execution() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing.yaml");
        std::fs::write(
            &missing,
            "task_id: T-1\nrepository: forge\nobjective: Repair the fixture\n",
        )
        .unwrap();
        assert!(matches!(
            load_team_task(missing),
            Err(TeamError::MissingPlan)
        ));

        let unknown = dir.path().join("unknown.yaml");
        std::fs::write(
            &unknown,
            "task_id: T-1\n\
             repository: forge\n\
             objective: Repair the fixture\n\
             team:\n\
             \x20 nodes:\n\
             \x20   - id: inspect\n\
             \x20     objective: Inspect\n\
             \x20     execution: analysis\n\
             \x20     persona: architect\n\
             \x20     assignment: { strategy: explicit, agent: claude }\n",
        )
        .unwrap();
        assert!(matches!(
            load_team_task(unknown),
            Err(TeamError::ParseTask { .. })
        ));
    }
}
