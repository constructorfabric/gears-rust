//! Unique-identity minting for the SDK default backends.
//!
//! Leader candidacy, lock holdership, and instance registration each need a
//! process-unique identity that distinguishes this participant's claim from a
//! foreign one written to the same cache key. The backends mint a fresh
//! [`Uuid`](uuid::Uuid) v4 rather than accepting an injected identity: the
//! identity is an implementation detail of the CAS protocol (the stored holder
//! marker), not part of the public contract, so generating it internally keeps
//! the constructor signatures `(cache)`-only.

use uuid::Uuid;

/// Mints a fresh, process-unique identity string (a v4 UUID).
///
/// Used as the cache-stored holder marker for a leader candidacy, a lock
/// acquisition, or an auto-assigned service instance id. Collisions are
/// cryptographically improbable, so a freshly minted id reliably distinguishes
/// this participant's claim from any other's.
pub(super) fn fresh_id() -> String {
    Uuid::new_v4().to_string()
}

#[cfg(test)]
mod tests {
    use super::fresh_id;

    #[test]
    fn fresh_ids_are_unique() {
        let a = fresh_id();
        let b = fresh_id();
        assert_ne!(a, b, "two freshly minted identities must differ");
        assert!(!a.is_empty());
    }
}
