pub type AgentResult<T> = Result<T, AgentError>;

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("no agent named `{0}` is known; run `forge agent list` to see the available agents")]
    UnknownAgent(String),

    #[error("no adapter for `{agent}` yet: {reason}")]
    NotImplemented { agent: String, reason: String },

    #[error("`{agent}` requires `{executable}`, which was not found on PATH")]
    ExecutableNotFound { agent: String, executable: String },

    #[error("`{agent}` is not usable: {reason}")]
    Unavailable { agent: String, reason: String },

    #[error(transparent)]
    Exec(#[from] forge_executor::ExecError),

    /// The harness produced output Forge could not interpret.
    #[error("could not interpret output from `{agent}`: {reason}")]
    Protocol { agent: String, reason: String },
}
