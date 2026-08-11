pub type StoreResult<T> = Result<T, StoreError>;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error(transparent)]
    Database(#[from] sqlx::Error),

    #[error(transparent)]
    Migration(#[from] sqlx::migrate::MigrateError),

    #[error(transparent)]
    Serde(#[from] serde_json::Error),

    #[error(transparent)]
    RoutingContract(#[from] forge_core::RoutingContractError),

    /// A stored value could not be interpreted. Indicates the ledger was
    /// written by something other than Forge, or was edited by hand.
    #[error("ledger contains an unreadable value: {0}")]
    Corrupt(String),

    #[error("{0} was not found in the experience ledger")]
    NotFound(String),

    #[error(
        "run `{run_id}` is already bound to task revision `{existing}` and cannot be rebound to `{attempted}`"
    )]
    TaskRevisionConflict {
        run_id: String,
        existing: String,
        attempted: String,
    },

    #[error(
        "run `{run_id}` is already recorded as `{existing}` provenance and cannot be changed to `{attempted}`"
    )]
    ProvenanceConflict {
        run_id: String,
        existing: String,
        attempted: String,
    },

    #[error(
        "run `{run_id}` is already recorded with `{existing}` selection and cannot be changed to `{attempted}`"
    )]
    SelectionSourceConflict {
        run_id: String,
        existing: String,
        attempted: String,
    },

    #[error("{context}")]
    Io {
        context: String,
        #[source]
        source: std::io::Error,
    },
}
