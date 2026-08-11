//! A deliberately incomplete crate, used as a Forge smoke-test fixture.
//!
//! `median` is unimplemented and its tests fail. A run against this repository
//! succeeds only if the agent actually implements the function and Forge's own
//! `cargo test` then passes.

/// Returns the median of `values`, or `None` if there are no values.
///
/// For an even number of values, returns the mean of the two middle ones.
pub fn median(values: &mut [f64]) -> Option<f64> {
    todo!("implement median")
}
