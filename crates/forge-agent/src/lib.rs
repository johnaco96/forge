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
pub mod codex;
pub mod error;
pub mod prompt;
pub mod registry;

pub use adapter::{AgentAdapter, RunContext};
pub use claude::{
    ClaudeAdapter, EXECUTION_PROTOCOL_SETTING as CLAUDE_EXECUTION_PROTOCOL_SETTING,
    NATIVE_EXECUTION_PROTOCOL as CLAUDE_NATIVE_EXECUTION_PROTOCOL,
    OUTER_OCI_EXECUTION_PROTOCOL as CLAUDE_OUTER_OCI_EXECUTION_PROTOCOL,
};
pub use codex::{
    CodexAdapter, EXECUTION_PROTOCOL_SETTING as CODEX_EXECUTION_PROTOCOL_SETTING,
    NATIVE_EXECUTION_PROTOCOL as CODEX_NATIVE_EXECUTION_PROTOCOL,
    OUTER_OCI_EXECUTION_PROTOCOL as CODEX_OUTER_OCI_EXECUTION_PROTOCOL,
};
pub use error::{AgentError, AgentResult};
pub use prompt::{build_agent_prompt, build_agent_prompt_with_context};
pub use registry::{AgentRegistry, Availability};
