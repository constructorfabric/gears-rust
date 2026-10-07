//! Sub-second TTLs against a `leaseDurationSeconds` `int32` field (DESIGN.md §2.10).
//!
//! `Lease.spec.leaseDurationSeconds` is an `int32` in **seconds**, but cluster
//! lock/election TTLs are `Duration`s and the rate-limiting pattern holds locks for
//! sub-second windows. So the interop-facing field is a rounded-**up** over-estimate
//! ([`lease_duration_seconds`]) — nothing that reads only the standard field ever
//! thinks a still-held lock is free — while the exact requested TTL rides the
//! `ttl-ms` annotation ([`ttl_ms`]), which the §2.9 expiry test actually uses. The
//! asymmetry is one-directional: a foreign reader is too conservative, never too
//! aggressive, which is the only safe direction to disagree.

use std::time::Duration;

use cluster_sdk::{ClusterError, ProviderErrorKind};
use k8s_openapi::api::coordination::v1::Lease;

/// The bound on a token-fenced write's re-read/retry loop when the API server keeps
/// answering `409` while our holder is still present — a benign concurrent metadata
/// update on the same Lease, not a steal (§5.4). A steal changes `holderIdentity` and
/// is caught on the first re-read, so a retry only ever re-runs against a Lease still
/// bearing our token. Shared by the lock's `renew`/`release` and the leader's
/// `renew`/`resign` token paths so the bound is defined once.
pub const HOLDER_WRITE_MAX_ATTEMPTS: usize = 3;

/// The error a token-fenced write raises when it exhausts
/// [`HOLDER_WRITE_MAX_ATTEMPTS`] still `409`-ing with **our holder still present** —
/// persistent benign churn on an object essentially only the holder writes, so
/// near-impossible in practice (§5.4).
///
/// It is a **retryable** `Provider` error, deliberately neither
/// [`ClusterError::LockExpired`] nor `Ok`: every re-read confirmed the claim is still
/// ours, so "you lost it" (`LockExpired` on a renew) would be a false loss and "done"
/// (`Ok` on a release/resign, with the holder never cleared) would be a false success
/// that silently wedges the name until its TTL lapses. Reporting a retryable fault
/// lets the caller re-issue the operation, and keeps renew and release/resign
/// consistent on this branch. The `resourceVersion` CAS guarantees no successor was
/// clobbered on the way here.
#[must_use]
pub fn holder_write_exhausted(name: &str) -> ClusterError {
    ClusterError::Provider {
        kind: ProviderErrorKind::ResourceExhausted,
        message: format!(
            "the token-fenced guarded write for `{name}` was still rejected as a conflict after \
             {HOLDER_WRITE_MAX_ATTEMPTS} attempts while the claim was still held; unchanged and \
             retryable"
        ),
    }
}

/// The non-empty `holderIdentity` of a `Lease`, or `None` when the spec or the field
/// is absent or blank (§2.4). The one implementation both the leader watcher and the
/// lock-release watcher read a holder through, so "who holds this?" is decided one way.
#[must_use]
pub fn holder_of(lease: &Lease) -> Option<String> {
    lease
        .spec
        .as_ref()
        .and_then(|spec| spec.holder_identity.clone())
        .filter(|holder| !holder.is_empty())
}

/// A fresh fence for one acquisition (§5.8.1): a random `u64`, this backend's
/// substitute for a monotonic counter. A single guarded `Lease` write has no counter
/// to increment, so the fence is drawn per acquisition. It approximates the guarantee
/// a monotonic one gives where it matters: a name that lapsed and was re-acquired
/// draws a *fresh* fence, so a stale holder's token will not match the successor's
/// claim -- a probabilistic bound (a 2^-64 collision aside), not a monotonic column's
/// certainty, which is more than enough for a coordination primitive. It mirrors the
/// redis and postgres natives' fences, and is **not** a globally monotonic fencing
/// token (ADR-002 declines those) -- only the discriminator `renew`/`release`/`resign`
/// predicate on.
#[must_use]
pub fn fresh_fence() -> u64 {
    use rand::RngExt as _;
    rand::rng().random::<u64>()
}

/// The `holderIdentity` string a lease is stored under and every token-fenced write
/// predicates on: `<owner>#<fence>` (§5.1, §5.8.1).
///
/// **Composed, never parsed** on the hot path -- both halves reach the writer already,
/// so an owner containing `#` is no hazard -- and a pure function of a
/// [`LeaseToken`](cluster_sdk::LeaseToken)'s identity fields, which is what lets a
/// caller holding only the token reconstruct the exact value the acquiring instance
/// wrote and have any replica fence on it identically (§5.8.1, invariant I7).
#[must_use]
pub fn holder_string(owner: &str, fence: u64) -> String {
    format!("{owner}#{fence}")
}

/// The `leaseDurationSeconds` (`int32`) to write for a `ttl`: `max(1, ceil(ttl))`.
///
/// Always a safe over-estimate — a 750 ms TTL becomes `1`, never `0`, so a foreign
/// reader cannot conclude the lock is free while this plugin still holds it.
///
/// # Errors
///
/// Returns [`ClusterError::InvalidConfig`] when `ttl` rounds up beyond `i32::MAX`
/// seconds (~68 years): a `Duration` can express it but the `int32` field cannot.
pub fn lease_duration_seconds(ttl: Duration) -> Result<i32, ClusterError> {
    // ceil to whole seconds without floating point: (ms + 999) / 1000, floored at 1.
    let millis = ttl.as_millis();
    let secs_ceil = millis.div_ceil(1000).max(1);
    i32::try_from(secs_ceil).map_err(|_| ClusterError::InvalidConfig {
        reason: format!(
            "ttl {ttl:?} exceeds the Lease `leaseDurationSeconds` int32 ceiling (~68 years)"
        ),
    })
}

/// The exact requested TTL in whole milliseconds, for the `ttl-ms` annotation.
///
/// # Errors
///
/// Returns [`ClusterError::InvalidConfig`] on the same over-`i32::MAX`-seconds TTL
/// [`lease_duration_seconds`] rejects, so the two encodings can never disagree about
/// acceptability.
pub fn ttl_ms(ttl: Duration) -> Result<u64, ClusterError> {
    // Reuse the seconds ceiling as the acceptability gate, so a TTL is either
    // representable in both encodings or rejected by both.
    lease_duration_seconds(ttl)?;
    // After that gate, milliseconds fit u64 comfortably (< i32::MAX * 1000).
    Ok(u64::try_from(ttl.as_millis()).unwrap_or(u64::MAX))
}

/// Rejects an election TTL below `min_election_ttl`, naming the write rate it would
/// otherwise generate (DESIGN.md §2.10). Locks have no such floor — a lock's writes
/// are per-acquisition, not per-interval.
///
/// `max_missed` is the election's missed-renewal budget; the derived renewal
/// interval is `ttl / (max_missed + 1)`, and the rate the message reports is its
/// reciprocal.
///
/// # Errors
///
/// Returns [`ClusterError::InvalidConfig`] when `ttl < min_election_ttl`.
pub fn check_election_ttl_floor(
    ttl: Duration,
    min_election_ttl: Duration,
    max_missed: u32,
) -> Result<(), ClusterError> {
    if ttl >= min_election_ttl {
        return Ok(());
    }
    let renewal = ttl / (max_missed + 1);
    let rate = if renewal.is_zero() {
        f64::INFINITY
    } else {
        1.0 / renewal.as_secs_f64()
    };
    Err(ClusterError::InvalidConfig {
        reason: format!(
            "election ttl {ttl:?} is below min_election_ttl {min_election_ttl:?}: its derived \
             renewal rate is ~{rate:.1} writes/sec against the API server"
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::{
        HOLDER_WRITE_MAX_ATTEMPTS, check_election_ttl_floor, holder_write_exhausted,
        lease_duration_seconds, ttl_ms,
    };
    use std::time::Duration;

    #[test]
    fn holder_write_exhausted_is_a_retryable_named_fault() {
        // The retry-exhaustion terminal must be retryable (so a caller re-issues) and
        // must NOT be LockExpired/Ok — every re-read confirmed the claim was still held.
        let err = holder_write_exhausted("ledger");
        assert!(
            err.is_retryable(),
            "exhaustion is a transient conflict, retryable: {err:?}"
        );
        let cluster_sdk::ClusterError::Provider { message, .. } = &err else {
            panic!("expected a Provider error, got {err:?}");
        };
        assert!(message.contains("ledger"), "names the resource: {message}");
        assert!(
            message.contains(&HOLDER_WRITE_MAX_ATTEMPTS.to_string()),
            "names the attempt bound: {message}"
        );
    }

    #[test]
    fn ceil_table() {
        let cases = [
            (Duration::from_millis(1), 1),
            (Duration::from_millis(750), 1),
            (Duration::from_secs(1), 1),
            (Duration::from_millis(1001), 2),
            (Duration::from_secs(29), 29),
            (Duration::from_millis(29_001), 30),
        ];
        for (ttl, expected) in cases {
            assert_eq!(lease_duration_seconds(ttl).unwrap(), expected, "{ttl:?}");
        }
    }

    #[test]
    fn ttl_ms_is_exact() {
        assert_eq!(ttl_ms(Duration::from_millis(750)).unwrap(), 750);
        assert_eq!(ttl_ms(Duration::from_millis(1)).unwrap(), 1);
        assert_eq!(ttl_ms(Duration::from_secs(30)).unwrap(), 30_000);
    }

    #[test]
    fn over_i32_max_seconds_is_invalid_config() {
        let huge = Duration::from_secs(u64::from(u32::MAX)); // > i32::MAX seconds
        assert!(matches!(
            lease_duration_seconds(huge),
            Err(cluster_sdk::ClusterError::InvalidConfig { .. })
        ));
        assert!(ttl_ms(huge).is_err());
    }

    #[test]
    fn election_floor_rejects_and_names_the_rate() {
        // 1 s TTL, budget 2 → 333 ms renewal → ~3 writes/sec.
        let err = check_election_ttl_floor(Duration::from_secs(1), Duration::from_secs(5), 2)
            .unwrap_err();
        let cluster_sdk::ClusterError::InvalidConfig { reason } = err else {
            panic!("expected InvalidConfig");
        };
        assert!(reason.contains("writes/sec"), "{reason}");
        // At or above the floor is fine.
        assert!(
            check_election_ttl_floor(Duration::from_secs(5), Duration::from_secs(5), 2).is_ok()
        );
        assert!(
            check_election_ttl_floor(Duration::from_secs(30), Duration::from_secs(5), 2).is_ok()
        );
    }
}
