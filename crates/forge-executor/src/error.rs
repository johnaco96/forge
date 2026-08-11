use std::path::PathBuf;

pub type ExecResult<T> = Result<T, ExecError>;

#[derive(Debug, thiserror::Error)]
pub enum ExecError {
    #[error("failed to start `{program}`")]
    Spawn {
        program: String,
        #[source]
        source: std::io::Error,
    },

    #[error("failed while waiting for a command to finish")]
    Wait(#[source] std::io::Error),

    #[error("working directory `{0}` does not exist")]
    MissingWorkingDirectory(PathBuf),

    #[error(transparent)]
    Git(#[from] forge_git::GitError),

    #[error("{context}")]
    Io {
        context: String,
        #[source]
        source: std::io::Error,
    },
}
