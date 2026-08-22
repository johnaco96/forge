//! Git operations Forge depends on: repositories, worktrees, and diffs.
//!
//! Worktrees are the V0 isolation mechanism. They give every agent an identical
//! starting state, keep agents out of the user's working tree, and make the
//! resulting change a plain diff.

#![deny(rust_2018_idioms)]

pub mod diff;
pub mod error;
pub mod repository;
pub mod worktree;

#[cfg(test)]
pub(crate) mod test_support;

pub use diff::{
    DiffStat, cached_patch, capture_workspace_patch, commit_staged_workspace, commit_workspace,
    patch_between, stage_candidate_patch, stat_between, workspace_contains_secret, workspace_delta,
};
pub use error::{GitError, GitResult};
pub use repository::Repository;
pub use worktree::{Worktree, WorktreeManager, validate_worktree_git_link};
