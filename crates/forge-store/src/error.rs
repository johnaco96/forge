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
        "store `{path}` is in use by another Forge process; stop active jobs and retry the operation"
    )]
    Busy { path: String },

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

    #[error(
        "team execution `{team_execution_id}` is bound to immutable plan `{existing}` and cannot be rewritten as `{attempted}`"
    )]
    TeamPlanConflict {
        team_execution_id: String,
        existing: String,
        attempted: String,
    },

    #[error("completed team execution `{team_execution_id}` is immutable")]
    TeamExecutionFinalized { team_execution_id: String },

    #[error("the run evaluation table cannot store team evaluation `{team_execution_id}`")]
    TeamEvaluationInRunTable { team_execution_id: String },

    #[error("evaluation event subject `{subject}` does not match run envelope `{run_id}`")]
    EvaluationEventSubjectConflict { run_id: String, subject: String },

    #[error(
        "evaluation event subject `{subject}` does not match team envelope `{team_execution_id}`"
    )]
    TeamEvaluationEventSubjectConflict {
        team_execution_id: String,
        subject: String,
    },

    #[error("world-model snapshot `{snapshot_id}` is immutable and differs from the stored record")]
    WorldModelSnapshotConflict { snapshot_id: String },

    #[error(
        "team node attempt `{team_execution_id}/{node_id}/{attempt}` is already linked to run `{existing}` and cannot be changed to `{attempted}`"
    )]
    TeamRunLinkConflict {
        team_execution_id: String,
        node_id: String,
        attempt: u64,
        existing: String,
        attempted: String,
    },

    #[error(
        "team artifact `{artifact_id}` already has content hash `{existing}` and cannot be rewritten as `{attempted}`"
    )]
    TeamArtifactConflict {
        artifact_id: String,
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
