//! Deterministic DAG scheduling over ordinary Forge agent runs.

#![deny(rust_2018_idioms)]

mod error;
mod planner;
mod scheduler;
mod task_file;

pub use error::{TeamError, TeamResult};
pub use planner::{PlanningContext, StaticTeamPlanner, TeamPlanner};
pub use scheduler::{TeamCoordinator, TeamReport, TeamRequest};
pub use task_file::load_team_task;
