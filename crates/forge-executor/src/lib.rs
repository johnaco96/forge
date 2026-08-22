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
pub mod resource;
pub mod sandbox;
pub mod workspace;

pub use error::{ExecError, ExecResult};
pub use process::{CredentialPolicy, ExecOutcome, ExecRequest, ProcessRunner, find_executable};
pub use resource::{DiskCapacity, DiskPreflightPolicy, DiskWatch, capacity, preflight_disk};
pub use sandbox::{
    DockerSandbox, EnvPolicy, ExecutionSandbox, Redactor, SandboxedInvocation,
    preflight_sandbox_config, preflight_sandbox_evaluator_tool, preflight_sandbox_executable,
};
pub use workspace::{
    PatchCapture, RETAINED_IGNORED_EXCLUSIONS, WorkspaceProvider, WorktreeProvider,
    capture_candidate_patch, capture_patch,
};
