pub type EvalResult<T> = Result<T, EvalError>;

#[derive(Debug, thiserror::Error)]
pub enum EvalError {
    /// Forge could not perform the measurement.
    ///
    /// Distinct from a failing check: this says nothing about the change, only
    /// that the evidence is missing.
    #[error("check `{check}` could not be measured: {reason}")]
    NotMeasurable { check: String, reason: String },

    #[error(transparent)]
    Exec(#[from] forge_executor::ExecError),
}
