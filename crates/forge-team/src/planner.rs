use async_trait::async_trait;
use forge_core::{TaskRevision, TeamPlan};

use crate::TeamResult;

#[derive(Debug, Clone, Default)]
pub struct PlanningContext {
    pub repository: String,
}

#[async_trait]
pub trait TeamPlanner: Send + Sync {
    async fn plan(&self, task: &TaskRevision, context: &PlanningContext) -> TeamResult<TeamPlan>;
}

#[derive(Debug, Clone)]
pub struct StaticTeamPlanner {
    plan: TeamPlan,
}

impl StaticTeamPlanner {
    pub fn new(plan: TeamPlan) -> Self {
        Self { plan }
    }
}

#[async_trait]
impl TeamPlanner for StaticTeamPlanner {
    async fn plan(&self, _task: &TaskRevision, _context: &PlanningContext) -> TeamResult<TeamPlan> {
        Ok(self.plan.clone())
    }
}
