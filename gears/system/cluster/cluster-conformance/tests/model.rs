// Created: 2026-06-24 by Constructor Tech
//! Property/model-based cache conformance: a `proptest`-generated op sequence is
//! replayed against the reference model and a backend, asserting the contract's
//! value/presence and version invariants hold under every interleaving.
//!
//! The strategy lives here (not in the library) because `proptest` is a
//! dev-dependency; the library exposes the runtime-agnostic replay engine
//! ([`replay_against_model`]) this test drives. A plugin reuses this exact
//! pattern against its real backend by swapping the factory.

use std::sync::Arc;

use cluster_conformance::fixture::MemCache;
use cluster_conformance::{CacheOp, VersionTarget, replay_against_model};
use cluster_sdk::cache::ClusterCacheBackend;
use proptest::collection::vec;
use proptest::prelude::*;
use proptest::test_runner::TestRunner;

/// A small key space so create/CAS/delete operations collide frequently — the
/// regime where version/CAS races surface.
fn key() -> impl Strategy<Value = String> {
    prop_oneof![Just("k0"), Just("k1"), Just("k2")].prop_map(str::to_owned)
}

/// Tiny values so equality checks are cheap and overwrites with identical bytes
/// (which must still bump the version) are common.
fn value() -> impl Strategy<Value = Vec<u8>> {
    prop_oneof![Just(vec![b'a']), Just(vec![b'b']), Just(vec![b'c'])]
}

/// One operation drawn uniformly across the alphabet.
fn op() -> impl Strategy<Value = CacheOp> {
    prop_oneof![
        (key(), value()).prop_map(|(key, value)| CacheOp::Put { key, value }),
        (key(), value()).prop_map(|(key, value)| CacheOp::PutIfAbsent { key, value }),
        (
            key(),
            prop_oneof![Just(VersionTarget::Current), Just(VersionTarget::Stale)],
            value(),
        )
            .prop_map(|(key, target, value)| CacheOp::CompareAndSwap {
                key,
                target,
                value
            }),
        key().prop_map(|key| CacheOp::Delete { key }),
    ]
}

#[test]
fn cache_invariants_hold_under_random_op_sequences() {
    // A current-thread runtime with time enabled: `MemCache` spawns a TTL
    // sweeper on construction. Reused across cases; each case builds (and drops)
    // its own backend, so the sweeper self-terminates via its weak reference.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");
    let mut runner = TestRunner::default();
    runner
        .run(&vec(op(), 1..40), |ops| {
            let result = rt.block_on(replay_against_model(&ops, || {
                MemCache::linearizable() as Arc<dyn ClusterCacheBackend>
            }));
            prop_assert!(result.is_ok(), "{}", result.err().unwrap_or_default());
            Ok(())
        })
        .expect("the linearizable MemCache must satisfy every cache invariant");
}
