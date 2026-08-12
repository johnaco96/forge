//! Longitudinal repository health.
//!
//! ```text
//! RepositoryHealthBuilder ──▶ RepositoryHealthSnapshot (immutable, commit-bound)
//!                                          │
//!                            LongitudinalAnalyzer
//!                                          │
//!                              diffs · trends · reports
//! ```
//!
//! The two halves are deliberately separate. Construction reads evidence and
//! records what it found; interpretation reads immutable snapshots and never
//! writes to them. Re-interpreting history under a new algorithm version
//! therefore cannot alter the evidence that history was built from.

#![deny(rust_2018_idioms)]

pub mod analyzer;
pub mod builder;
pub mod error;

pub use builder::{
    CommitAncestry, ExcludedEvidence, GitAncestry, HealthBuildReport, RepositoryHealthBuilder,
};
pub use error::{HealthBuildError, HealthBuildResult};

pub use analyzer::{
    DEFAULT_TREND_EPSILON_PERCENT, attribute, diff, dimension_rows, dimension_summary,
    missing_dimensions, nearest_ancestor_baseline, trends,
};
