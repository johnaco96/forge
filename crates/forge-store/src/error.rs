pub type StoreResult<T> = Result<T, StoreError>;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error(transparent)]
    Database(#[from] sqlx::Error),

    #[error(transparent)]
    Migration(#[from] sqlx::migrate::MigrateError),

    #[error(transparent)]
    Serde(#[from] serde_json::Error),

    /// A stored value could not be interpreted. Indicates the ledger was
    /// written by something other than Forge, or was edited by hand.
    #[error("ledger contains an unreadable value: {0}")]
    Corrupt(String),

    #[error("{context}")]
    Io {
        context: String,
        #[source]
        source: std::io::Error,
    },
}
