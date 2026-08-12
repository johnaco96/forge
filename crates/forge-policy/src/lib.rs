//! Deterministic policy optimization.
//!
//! ```text
//! PolicyEvidenceSnapshot ──▶ BaselineOptimizer ──▶ PolicyProposal
//!                                                       │
//!                                            evaluate_promotion
//!                                                       │
//!                                        promote · reject · rollback
//! ```
//!
//! The optimizer proposes; it never activates. Everything that changes what
//! Forge actually does passes through [`gate::evaluate_promotion`], which
//! enforces guardrails, hard constraints, evidence minimums, and the approval
//! model together.

#![deny(rust_2018_idioms)]

pub mod error;
pub mod gate;
pub mod optimizer;
pub mod resolver;
pub mod runtime;

pub use error::{PolicyOptimizationError, PolicyOptimizationResult};
pub use gate::{
    Approval, PromotionBlocker, PromotionGate, Rollback, RollbackError, evaluate_promotion,
    prepare_rollback, rollback_recommended,
};
pub use optimizer::{
    BaselineOptimizer, HealthEvidenceValues, OPTIMIZER_VERSION, OptimizationRequest,
    PolicyOptimizer, successor,
};
pub use resolver::{PolicyEvidenceResolver, ResolvedPolicyEvidence};
pub use runtime::{
    ExecutionPolicyResolution, PolicyRuntimeError, create_policy_experiment,
    ensure_bootstrap_policy, promote_proposal, resolve_execution_policy, rollback_policy,
};
