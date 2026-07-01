//! The shared constructor safety guard for the consistency-sensitive default
//! backends (`cpt-cf-clst-algo-sdk-default-backends-constructor-guard`).
//!
//! The CAS-based leader-election and lock defaults preserve their safety
//! guarantee (at-most-one-leader / correctness-grade exclusion) only over a
//! **linearizable** cache. Both expose the same constructor pair, so the
//! reject-by-default and warn-on-opt-in logic lives here once:
//!
//! - [`reject_weak_consistency`] backs the default-safe `new(cache)` —
//!   `Err(InvalidConfig)` when the cache is eventually consistent
//!   (`inst-cg-default`/`inst-cg-reject`).
//! - [`warn_weak_consistency`] backs `new_allow_weak_consistency(cache)` — the
//!   instantiation-time split-brain warning (`inst-cg-weak`/`inst-cg-warn`).

use cluster_sdk::cache::CacheConsistency;
use cluster_sdk::error::ClusterError;

/// Rejects an eventually-consistent cache for a consistency-sensitive default
/// backend named `backend` (the default-safe construction path).
///
/// # Errors
/// Returns [`ClusterError::InvalidConfig`] when `consistency` is
/// [`CacheConsistency::EventuallyConsistent`]; the message points the operator
/// at the explicit `new_allow_weak_consistency` opt-in.
pub(super) fn reject_weak_consistency(
    consistency: CacheConsistency,
    backend: &'static str,
) -> Result<(), ClusterError> {
    if consistency == CacheConsistency::EventuallyConsistent {
        return Err(ClusterError::InvalidConfig {
            reason: format!(
                "{backend} requires a linearizable cache to preserve its safety guarantee, but \
                 the supplied cache declares EventuallyConsistent. Route this primitive to a \
                 linearizable backend, or construct via `new_allow_weak_consistency` to opt in \
                 and accept the split-brain risk."
            ),
        });
    }
    Ok(())
}

/// Emits the split-brain acknowledgement warning for the explicit
/// weak-consistency opt-in path of the backend named `backend`.
///
/// Always logs at instantiation (DESIGN §3.11): selecting the opt-in
/// constructor is itself the acknowledgement that the safety guarantee is
/// waived. The `weak_consistency` field records whether the supplied cache is
/// actually eventually consistent (the case the warning protects against) or a
/// linearizable cache the caller chose to bypass the guard for anyway.
pub(super) fn warn_weak_consistency(consistency: CacheConsistency, backend: &'static str) {
    tracing::warn!(
        backend,
        weak_consistency = consistency == CacheConsistency::EventuallyConsistent,
        "{backend} constructed via new_allow_weak_consistency: its safety guarantee is waived; \
         an eventually-consistent cache may produce split-brain (dual leaders / dual lock \
         holders) under partition"
    );
}

#[cfg(test)]
mod tests {
    use tracing_test::traced_test;

    use super::{reject_weak_consistency, warn_weak_consistency};
    use cluster_sdk::cache::CacheConsistency;
    use cluster_sdk::error::ClusterError;

    #[test]
    fn rejects_eventually_consistent() {
        assert!(matches!(
            reject_weak_consistency(CacheConsistency::EventuallyConsistent, "TestBackend"),
            Err(ClusterError::InvalidConfig { .. })
        ));
    }

    #[test]
    fn accepts_linearizable() {
        assert!(reject_weak_consistency(CacheConsistency::Linearizable, "TestBackend").is_ok());
    }

    #[traced_test]
    #[test]
    fn warn_emits_split_brain_warning_with_weak_consistency_flag() {
        warn_weak_consistency(CacheConsistency::EventuallyConsistent, "TestBackend");
        assert!(logs_contain("weak_consistency=true"));
        assert!(logs_contain("split-brain"));

        warn_weak_consistency(CacheConsistency::Linearizable, "TestBackend");
        assert!(logs_contain("weak_consistency=false"));
    }
}
