use std::path::PathBuf;

pub type WorldBuildResult<T> = Result<T, WorldBuildError>;

#[derive(Debug, thiserror::Error)]
pub enum WorldBuildError {
    #[error(transparent)]
    Core(#[from] forge_core::WorldModelError),
    #[error(transparent)]
    Git(#[from] forge_git::GitError),
    #[error(transparent)]
    Store(#[from] forge_store::StoreError),
    #[error("failed to read `{path}`")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse `{path}`: {message}")]
    Parse { path: PathBuf, message: String },
    #[error("repository path `{0}` resolves outside the repository or through a symlink")]
    UnsafeRepositoryPath(PathBuf),
    #[error("world-model extraction is disabled in `.forge/config.toml`")]
    Disabled,
    #[error("world-model extraction requires a clean repository checkout")]
    DirtyRepository,
    #[error("world-model extraction requested commit `{requested}` but the checkout is `{head}`")]
    CheckoutCommitMismatch { requested: String, head: String },
    #[error("extractor `{extractor}` failed: {message}")]
    Extractor { extractor: String, message: String },
}
