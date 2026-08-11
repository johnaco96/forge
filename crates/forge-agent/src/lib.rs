//! Coding agents as interchangeable engineering workers.
//!
//! ```text
//! forge-cli ──▶ AgentRegistry ──▶ dyn AgentAdapter ──▶ Claude Code / Codex / Pi
//! ```
//!
//! Nothing above this crate names a specific agent, and nothing in this crate
//! judges an agent's work.

#![deny(rust_2018_idioms)]

pub mod adapter;
pub mod claude;
pub mod error;
pub mod prompt;
pub mod registry;

pub use adapter::{AgentAdapter, RunContext};
pub use claude::ClaudeAdapter;
pub use error::{AgentError, AgentResult};
pub use prompt::build_agent_prompt;
pub use registry::{AgentRegistry, Availability};
