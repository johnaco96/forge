//! Provider-neutral repository world-model extraction.

mod builder;
mod error;
mod history;
mod rust;

pub use builder::{
    ExtractionContext, WorldModelBuildReport, WorldModelBuilder, WorldModelExtractor,
    snapshot_relation,
};
pub use error::{WorldBuildError, WorldBuildResult};
pub use history::TaskHistoryExtractor;
pub use rust::RustWorkspaceExtractor;
