//! The experience ledger: everything Forge remembers.
//!
//! Forge's long-term value comes from what it retains, so this crate errs
//! toward keeping more than is needed today. Trajectories are append-only, raw
//! metrics are stored beside normalized dimensions, and full records are kept
//! as JSON next to the indexed columns — a future schema should be derivable
//! from history rather than requiring runs to be repeated.

#![deny(rust_2018_idioms)]

pub mod error;
pub mod experience;
pub mod routing;
pub mod sqlite;

pub use error::{StoreError, StoreResult};
pub use experience::{
    AgentStatistics, AgentTaskOutcomes, CohortStatistics, EXPORT_SCHEMA_VERSION,
    ExperimentHistoryEntry, ExperimentRunHistory, ExportRecord, FailedEvaluatorSummary,
    FailureFilter, FailureSummary, HistoryFilter, RunHistoryEntry, TaskExperience, TaskSimilarity,
};
pub use forge_core::task::TaskRevisionId;
pub use sqlite::{RunSummary, Store};
