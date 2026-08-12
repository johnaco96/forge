pub type HealthBuildResult<T> = Result<T, HealthBuildError>;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HealthBuildError {
    /// The only world model available describes a different commit.
    ///
    /// Substituting an ancestor would make the snapshot a claim about a commit
    /// nobody measured, so this is refused rather than approximated.
    #[error(
        "no exact world model for commit {requested}; the nearest available snapshot \
         describes {found}. Run `forge world build` at this commit first."
    )]
    WorldModelNotExact { requested: String, found: String },

    #[error("world model {snapshot_id} failed to build, so health cannot be measured from it")]
    WorldModelFailed { snapshot_id: String },

    #[error("constructed health snapshot is invalid: {0}")]
    InvalidSnapshot(String),
}
