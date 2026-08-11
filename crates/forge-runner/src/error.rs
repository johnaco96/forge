pub type RunnerResult<T> = Result<T, RunnerError>;

#[derive(Debug, thiserror::Error)]
pub enum RunnerError {
    #[error(transparent)]
    Task(#[from] forge_core::task::TaskError),

    #[error(transparent)]
    Protection(#[from] forge_core::integrity::ProtectionError),

    #[error(
        "task targets repository `{task_repository}`, but this one is configured as \
         `{configured}`"
    )]
    WrongRepository {
        task_repository: String,
        configured: String,
    },

    #[error("invalid agent id: {0}")]
    InvalidAgentId(String),

    #[error(transparent)]
    Agent(#[from] forge_agent::AgentError),

    #[error(transparent)]
    Git(#[from] forge_git::GitError),

    #[error(transparent)]
    Exec(#[from] forge_executor::ExecError),

    #[error(transparent)]
    Store(#[from] forge_store::StoreError),

    #[error("run lifecycle error: {0}")]
    Lifecycle(#[from] forge_core::run::RunError),
}
