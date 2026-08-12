//! Execution machinery: running processes and provisioning workspaces.
//!
//! Two boundaries live here, both of which the design expects to be replaced
//! later without disturbing anything above them:
//!
//! - [`WorkspaceProvider`] — Git worktrees now, containers later.
//! - [`EnvPolicy`] — environment filtering now, real sandboxing later.

#![deny(rust_2018_idioms)]

pub mod error;
pub mod process;
pub mod sandbox;
pub mod workspace;

pub use error::{ExecError, ExecResult};
pub use process::{ExecOutcome, ExecRequest, ProcessRunner, find_executable};
pub use sandbox::{EnvPolicy, Redactor};
pub use workspace::{
    PatchCapture, RETAINED_IGNORED_EXCLUSIONS, WorkspaceProvider, WorktreeProvider,
    capture_candidate_patch, capture_patch,
};
