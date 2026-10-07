//! Jittered backoff shared by the retry loops (§4.1, §6.1).

use std::time::Duration;

/// A uniformly-random backoff in `[0, max)`.
///
/// Jitter — not a fixed delay — is what de-correlates N instances racing the same
/// key: a fixed backoff would keep them in lock-step and amplify API load on the
/// contended key exactly when it is hottest (a leader-election failover, a
/// `put_if_absent` reclaim). `max == 0` yields `Duration::ZERO`; callers reject a
/// zero budget at `build_and_start`, so this is only the degenerate guard.
// Plain `pub` (not `pub(crate)`): the module is private, so `pub(crate)` would trip
// `clippy::redundant_pub_crate` — the crate's house convention (see `config::reject_zero`).
#[must_use]
pub fn jittered_backoff(max: Duration) -> Duration {
    use rand::RngExt as _;
    let max_nanos = u64::try_from(max.as_nanos()).unwrap_or(u64::MAX);
    if max_nanos == 0 {
        return Duration::ZERO;
    }
    Duration::from_nanos(rand::rng().random_range(0..max_nanos))
}
