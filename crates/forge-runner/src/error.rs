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

    #[error("competitive execution requires at least two agents")]
    TooFewCompetitors,

    #[error("competitive execution contains duplicate agent `{0}`")]
    DuplicateCompetitor(String),

    #[error(transparent)]
    Agent(#[from] forge_agent::AgentError),

    #[error(transparent)]
    Git(#[from] forge_git::GitError),

    #[error(transparent)]
    Exec(#[from] forge_executor::ExecError),

    #[error(transparent)]
    Store(#[from] forge_store::StoreError),

    #[error(transparent)]
    Policy(#[from] forge_policy::PolicyRuntimeError),

    #[error("policy execution strategy is unavailable on this path: {0}")]
    PolicyStrategy(String),

    #[error("run lifecycle error: {0}")]
    Lifecycle(#[from] forge_core::run::RunError),
}
