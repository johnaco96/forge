use forge_core::ids::{RoutingDecisionId, TeamNodeId};

pub type TeamResult<T> = Result<T, TeamError>;

#[derive(Debug, thiserror::Error)]
pub enum TeamError {
    #[error(transparent)]
    Plan(#[from] forge_core::TeamPlanError),
    #[error(transparent)]
    Task(#[from] forge_core::TaskError),
    #[error(transparent)]
    TaskRevision(#[from] forge_core::TaskRevisionError),
    #[error(transparent)]
    Store(#[from] forge_store::StoreError),
    #[error(transparent)]
    Runner(#[from] forge_runner::RunnerError),
    #[error(transparent)]
    Agent(#[from] forge_agent::AgentError),
    #[error(transparent)]
    Router(#[from] forge_router::RouterError),
    #[error(transparent)]
    Git(#[from] forge_git::GitError),
    #[error(transparent)]
    Executor(#[from] forge_executor::ExecError),
    #[error(transparent)]
    Protection(#[from] forge_core::ProtectionError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("failed to read team task `{path}`")]
    ReadTask {
        path: std::path::PathBuf,
        source: std::io::Error,
    },
    #[error("failed to parse team task `{path}`: {message}")]
    ParseTask {
        path: std::path::PathBuf,
        message: String,
    },
    #[error("task file has no `team` plan; Phase 5 requires an explicit typed plan")]
    MissingPlan,
    #[error("team plan root objective does not exactly match the immutable root task objective")]
    RootObjectiveMismatch,
    #[error("team node `{0}` references an unavailable or ineligible agent")]
    AssignmentUnavailable(TeamNodeId),
    #[error("team node `{node}` automatic assignment stopped: {reason}")]
    AssignmentBlocked {
        node: TeamNodeId,
        reason: String,
        decision_id: Option<RoutingDecisionId>,
    },
    #[error("team node `{node}` has conflicting input commits: {commits:?}")]
    IntegrationConflict {
        node: TeamNodeId,
        commits: Vec<String>,
    },
    #[error(
        "team produced multiple terminal candidate commits requiring explicit integration: {0:?}"
    )]
    MultipleFinalCandidates(Vec<String>),
    #[error("team produced no final candidate commit")]
    NoFinalCandidate,
    #[error("team node `{0}` was missing from its execution record")]
    MissingNode(TeamNodeId),
    #[error("team plan serialization failed: {0}")]
    PlanSerialization(String),
}
