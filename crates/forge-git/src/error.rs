use std::path::PathBuf;

pub type GitResult<T> = Result<T, GitError>;

#[derive(Debug, thiserror::Error)]
pub enum GitError {
    #[error("`git` was not found on PATH")]
    GitNotFound,

    #[error("`{0}` is not inside a Git repository")]
    NotARepository(PathBuf),

    #[error("`{requested}` is inside a repository but is not its root (`{root}`)")]
    NotRepositoryRoot { requested: PathBuf, root: PathBuf },

    #[error("unknown revision `{0}`")]
    UnknownRevision(String),

    #[error("{command} failed{}: {stderr}", .code.map(|c| format!(" with exit code {c}")).unwrap_or_default())]
    CommandFailed {
        command: String,
        code: Option<i32>,
        stderr: String,
    },

    #[error("invalid workspace name `{name}`: {reason}")]
    InvalidWorkspaceName { name: String, reason: String },

    /// Raised when an operation would touch a path outside the directory Forge
    /// is allowed to manage. Always a bug or an attack, never routine.
    #[error("refusing to operate on `{path}`: {reason}")]
    UnsafePath { path: PathBuf, reason: String },

    #[error("workspace `{0}` already exists")]
    WorkspaceExists(PathBuf),

    #[error("{context}")]
    Io {
        context: String,
        #[source]
        source: std::io::Error,
    },
}
