// Created: 2026-08-12 by Constructor Tech
//! String interning for the `&'static str` fields the frozen error model keeps.
//!
//! DESIGN-DEPLOYABLE-GEAR §12.1 notes that two independent places need this for
//! the same reason, and argues for one helper rather than two ad-hoc leaks:
//!
//! - [`ClusterError::ProfileNotBound`](crate::ClusterError::ProfileNotBound),
//!   [`CapabilityNotMet`](crate::ClusterError::CapabilityNotMet),
//!   [`Unsupported`](crate::ClusterError::Unsupported) and
//!   [`InvalidName`](crate::ClusterError::InvalidName) carry `&'static str`, and
//!   the error model is **frozen** (invariant I3). A name arriving over the wire,
//!   or read from runtime config, must be promoted rather than the variant
//!   widened to `String`.
//! - `provider_name()` on the three backend traits returns `&'static str`, while
//!   a remote backend learns its provider from a descriptor at runtime (§5.5).
//!
//! Interning is therefore the mechanism that keeps both signatures unchanged
//! across the process boundary — which is what makes invariant I2 ("`resolve()`
//! becoming `async` is the only SDK signature change") survive contact with a
//! wire that speaks `String`.
//!
//! # It leaks, deliberately and boundedly
//!
//! The interned set is drawn from the cluster gear's configured profile and
//! provider names plus the capability and feature identifiers compiled into the
//! SDK — all fixed for the life of a process, and all already `&'static str` on
//! the local path. It is **not** a general-purpose intern table: nothing keyed by
//! a lock name, an election name or a cache key may reach it, because those are
//! unbounded (the same cardinality rule as invariant I15).

use std::collections::HashSet;
use std::sync::{Mutex, OnceLock, PoisonError};

/// The interned set.
///
/// A `Mutex` rather than a lock-free structure on purpose: interning happens on
/// configuration, descriptor-load and error-decode paths, never on a
/// coordination hot path.
fn interned() -> &'static Mutex<HashSet<&'static str>> {
    static INTERNED: OnceLock<Mutex<HashSet<&'static str>>> = OnceLock::new();
    INTERNED.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Promotes `value` to a `&'static str`, reusing a previously interned copy.
///
/// Poison-tolerant: the guarded section performs no fallible work, so a poisoned
/// lock still carries a consistent table and the interning proceeds. An error
/// decode is the last place that should turn a panic elsewhere into a second
/// panic here.
#[must_use]
pub fn intern(value: &str) -> &'static str {
    let mut set = interned().lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(existing) = set.get(value) {
        return existing;
    }
    let leaked: &'static str = Box::leak(value.to_owned().into_boxed_str());
    set.insert(leaked);
    leaked
}

/// The interned copy of `value` if it has already been interned — **without**
/// interning it.
///
/// This is the lookup for a name that did not come from a bounded set. The
/// profile registry needs it: an unknown profile arriving in a request must not
/// be promoted, or a caller looping over made-up names would grow the table
/// without bound, which is exactly the cardinality rule the module
/// documentation states. A name that *was* registered at some point in this
/// process is still recoverable here, so the error can name it.
#[must_use]
pub fn intern_existing(value: &str) -> Option<&'static str> {
    let set = interned().lock().unwrap_or_else(PoisonError::into_inner);
    set.get(value).copied()
}

#[cfg(test)]
mod tests {
    use super::{intern, intern_existing};

    #[test]
    fn interning_the_same_value_twice_yields_one_allocation() {
        let first = intern("orders-profile");
        let second = intern(&String::from("orders-profile"));
        assert!(
            std::ptr::eq(first, second),
            "a repeated intern must reuse the leaked copy, not leak a second one"
        );
    }

    #[test]
    fn interned_value_equals_its_input() {
        assert_eq!(intern("postgres"), "postgres");
        assert_ne!(intern("postgres"), intern("standalone"));
    }

    #[test]
    fn lookup_finds_an_interned_value_and_does_not_intern_a_new_one() {
        let interned = intern("bounded-lookup-profile");
        assert!(
            intern_existing("bounded-lookup-profile")
                .is_some_and(|found| std::ptr::eq(found, interned)),
            "an already-interned value is recoverable"
        );

        assert!(
            intern_existing("never-interned-profile").is_none(),
            "an unknown value must not be found"
        );
        assert!(
            intern_existing("never-interned-profile").is_none(),
            "and the failed lookup must not have interned it"
        );
    }

    #[test]
    fn an_already_static_value_still_round_trips() {
        // The decoder cannot tell whether a name originated locally or on the
        // wire, so interning a value that is already `'static` must be benign.
        const NAME: &str = "already-static";
        assert_eq!(intern(NAME), NAME);
    }
}
