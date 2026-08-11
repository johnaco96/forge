//! An isolated repository environment an agent is allowed to modify.
//!
//! This is the description of a workspace, not the machinery that creates one.
//! Provisioning lives in `forge-executor` (backed by `forge-git` today,
//! containers later), which keeps the isolation mechanism swappable without
//! touching the domain model.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::ids::RunId;

/// The isolation mechanism backing a workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceKind {
    /// A Git worktree on the local filesystem.
    Worktree,
    /// A plain directory copy, for repositories where worktrees do not apply.
    Directory,
    /// A container. Not implemented yet.
    Container,
}

/// One isolated checkout. Agent workspaces belong to a run; Forge may also
/// create a detached workspace solely to evaluate an integrated candidate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Workspace {
    /// The run this workspace belongs to, absent only for a Forge-owned final
    /// evaluation workspace that never executes an agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<RunId>,
    pub kind: WorkspaceKind,
    /// Absolute path to the workspace root.
    pub path: PathBuf,
    /// Branch the agent's work lands on.
    pub branch: String,
    /// The commit every competing agent starts from.
    pub base_commit: String,
}

impl Workspace {
    pub fn new(
        run_id: RunId,
        kind: WorkspaceKind,
        path: PathBuf,
        branch: impl Into<String>,
        base_commit: impl Into<String>,
    ) -> Self {
        Self {
            run_id: Some(run_id),
            kind,
            path,
            branch: branch.into(),
            base_commit: base_commit.into(),
        }
    }

    pub fn for_evaluation(
        kind: WorkspaceKind,
        path: PathBuf,
        branch: impl Into<String>,
        base_commit: impl Into<String>,
    ) -> Self {
        Self {
            run_id: None,
            kind,
            path,
            branch: branch.into(),
            base_commit: base_commit.into(),
        }
    }
}
