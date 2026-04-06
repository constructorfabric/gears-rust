//! Shared exponential-backoff helper used by both [`crate::client`] and [`crate::rpc_retry`].

use std::time::Duration;

/// Compute exponential backoff with jitter, clamped to `max_backoff`.
///
/// Formula: `base * 2^(attempt-1)`, capped at `max_backoff`, then `jitter_factor * raw` is
/// added and the result is clamped to `max_backoff` again so that `max_backoff` is always a
/// strict upper bound even after jitter.
///
/// The `jitter_factor` parameter (typically in `[0.0, 0.25]`) is passed in so the function
/// is pure and can be tested deterministically without touching an RNG.
pub fn compute_backoff(
    base: Duration,
    max_backoff: Duration,
    attempt: u32,
    jitter_factor: f64,
) -> Duration {
    let exp = i32::try_from(attempt.saturating_sub(1)).unwrap_or(i32::MAX);
    let factor = 2_f64.powi(exp);
    let raw = if factor.is_finite() {
        base.mul_f64(factor).min(max_backoff)
    } else {
        max_backoff
    };
    (raw + raw.mul_f64(jitter_factor)).min(max_backoff)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "backoff_tests.rs"]
mod tests;
