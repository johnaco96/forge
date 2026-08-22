//! Independent evaluation of agent-produced changes.
//!
//! Forge never trusts a coding agent's account of its own work. Everything in
//! this crate runs against the code in the workspace, after the agent has
//! finished, using commands the repository declared in advance.

#![deny(rust_2018_idioms)]

pub mod benchmark;
pub mod builtins;
pub mod command;
pub mod error;
pub mod evaluator;
pub mod set;

#[cfg(test)]
pub(crate) mod test_support;

pub use benchmark::BenchmarkEvaluator;
pub use builtins::{
    BuildEvaluator, ComplexityEvaluator, CustomEvaluator, LintEvaluator, SecurityEvaluator,
    TestEvaluator,
};
pub use command::CommandEvaluator;
pub use error::{EvalError, EvalResult};
pub use evaluator::{EvalContext, EvaluationContext, Evaluator};
pub use set::{EvaluationEngine, EvaluationPlan, EvaluatorPrerequisite, EvaluatorSet};
