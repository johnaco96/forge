pub type PolicyOptimizationResult<T> = Result<T, PolicyOptimizationError>;

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum PolicyOptimizationError {
    /// The candidate never reaches the evidence: an invalid policy cannot be
    /// evaluated, only refused.
    #[error("candidate policy is invalid: {0}")]
    InvalidCandidate(forge_core::policy::PolicyError),

    #[error("candidate governs `{candidate}` but the evidence is for `{evidence}`")]
    RepositoryMismatch { candidate: String, evidence: String },
}
