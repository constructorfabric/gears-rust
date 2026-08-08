//! The optional `WAIT <replicas> <timeout>` an operator can append to writes
//! whose outcome a failover must not silently undo (DESIGN.md §3.6).
//!
//! Its own module because both primitives use it and neither owns it: the cache
//! applies it to its conditional writes and the lock applies it to acquisition,
//! and a second copy would be free to disagree about what a short count means.
//!
//! **`WAIT` does not upgrade anything.** Per ADR-009 it narrows the Sentinel
//! failover window and leaves the declaration alone — `WAIT 1` reduces but does
//! not eliminate the window in which an acknowledged write is lost to a
//! promotion, and `CacheConsistency` is deliberately two-valued. Nothing in this
//! module touches `consistency()` or `features().linearizable`.

use cluster_sdk::{ClusterError, ProviderErrorKind};
use fred::clients::Pool;
use fred::interfaces::ServerInterface;

use crate::redis_error::map_redis_error;

/// The `WAIT` policy an operator configured, or the absence of one.
#[derive(Debug, Clone, Copy)]
pub struct WaitPolicy {
    /// How many replicas must acknowledge.
    pub replicas: u32,
    /// How long to wait for them, in milliseconds.
    pub timeout_ms: u64,
}

/// Issues `WAIT <replicas> <timeout>` when a policy is configured, and reports a
/// short count as an error.
///
/// The short-count arm is the whole reason this is not a fire-and-forget call.
/// `WAIT` does not *error* when it times out — it returns however many replicas
/// acknowledged — so a caller that ignored the count would have opted into a
/// durability guarantee and then silently not received it, which is worse than
/// never having offered the option.
///
/// # Errors
/// [`ClusterError::Provider`] with [`ProviderErrorKind::ResourceExhausted`] when
/// fewer than `replicas` acknowledged before the deadline, and whatever
/// [`map_redis_error`] makes of a failing `WAIT`.
pub async fn wait_for_replicas(
    pool: &Pool,
    policy: Option<WaitPolicy>,
) -> Result<(), ClusterError> {
    let Some(policy) = policy else {
        return Ok(());
    };
    let timeout = i64::try_from(policy.timeout_ms).unwrap_or(i64::MAX);
    let acked: i64 = pool
        .wait(i64::from(policy.replicas), timeout)
        .await
        .map_err(map_redis_error)?;
    if acked < i64::from(policy.replicas) {
        return Err(ClusterError::Provider {
            kind: ProviderErrorKind::ResourceExhausted,
            message: format!(
                "WAIT {} {}ms: only {acked} replica(s) acknowledged the write before the \
                 deadline. The write is on the primary but is not yet replicated, so a \
                 failover now could lose it (DESIGN.md sec 3.6)",
                policy.replicas, policy.timeout_ms,
            ),
        });
    }
    Ok(())
}
