// Created: 2026-08-13 by Constructor Tech
//! Tests for [`RemoteClusterClient`](super::RemoteClusterClient) that need no
//! server.
//!
//! What is asserted here is everything about the client that must be true
//! *before* anything is reachable — which is most of invariant I6. The behaviour
//! against a live gear is exercised end to end in
//! `cluster/tests/remote_backends.rs`, because only the gear crate can serve the
//! four services.

use std::sync::Arc;

use super::RemoteClusterClient;
use crate::cache::CacheConsistency;
use crate::client::ClusterClient;
use crate::error::ClusterError;

/// TEST-NET-1 (RFC 5737), reserved for documentation and guaranteed not to be a
/// live host. Naming it in an endpoint is the strongest available statement that
/// nothing here connects: if construction touched the network, this address is
/// what it would hang or fail on.
const UNROUTABLE: &str = "http://192.0.2.1:9999";

/// A runtime whose reactor is *entered* but never driven.
///
/// This is how "touches no network" is asserted, and the shape is a measured
/// finding rather than a preference: `Endpoint::connect_lazy` needs a Tokio
/// reactor **context** and panics without one (hyper-util's connector asks for
/// the handle at construction). It still performs no I/O, and entering the
/// runtime without ever calling `block_on` is what proves it — nothing under the
/// guard can await, poll a future, or run a task, because nothing is driving one.
fn entered() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a current-thread runtime")
}

#[test]
fn connect_lazy_touches_no_network() {
    // No `block_on` anywhere below: the reactor is entered but never driven, so a
    // constructor that performed I/O could not complete here, and neither could
    // the three factory methods. An unroutable endpoint makes the point twice
    // over.
    let runtime = entered();
    let _reactor = runtime.enter();
    let client = RemoteClusterClient::connect_lazy(UNROUTABLE).expect("a valid endpoint");

    assert!(client.cache_backend("orders").is_ok());
    assert!(client.lock_backend("orders").is_ok());
    assert!(client.leader_election_backend("orders").is_ok());
}

#[test]
fn a_factory_call_names_no_bound_profile_and_still_succeeds() {
    // Nothing is validated at factory time and nothing is fetched: the profile is
    // a request parameter, not a wiring parameter (DESIGN section 3.1). A profile
    // the server does not bind produces a handle whose first *call* reports
    // `ProfileNotBound` - the same answer a bound-then-removed profile gives.
    let runtime = entered();
    let _reactor = runtime.enter();
    let client = RemoteClusterClient::connect_lazy(UNROUTABLE).expect("a valid endpoint");
    assert!(client.cache_backend("no-such-profile").is_ok());
}

#[test]
fn an_invalid_endpoint_is_the_only_construction_failure() {
    let err = RemoteClusterClient::connect_lazy("not a uri at all")
        .expect_err("a malformed endpoint cannot build a channel");
    assert!(
        matches!(err, ClusterError::InvalidConfig { .. }),
        "expected InvalidConfig, got: {err}"
    );
}

#[test]
fn the_sync_accessors_fail_safe_before_a_descriptor_is_fetched() {
    // The accessors read the descriptor cache and cannot fetch (DESIGN section
    // 5.5), so what they answer with nothing cached is a real decision: the
    // *weaker* reading in every case, so a declared requirement fails rather than
    // being falsely satisfied.
    let runtime = entered();
    let _reactor = runtime.enter();
    let client = RemoteClusterClient::connect_lazy(UNROUTABLE).expect("a valid endpoint");

    let cache = client.cache_backend("orders").expect("a handle");
    assert_eq!(cache.consistency(), CacheConsistency::EventuallyConsistent);
    assert!(!cache.features().prefix_watch);
    assert_eq!(cache.provider_name(), "unknown");

    let lock = client.lock_backend("orders").expect("a handle");
    assert!(!lock.features().linearizable);
    assert_eq!(lock.provider_name(), "unknown");

    let leader = client.leader_election_backend("orders").expect("a handle");
    assert!(!leader.features().linearizable);
    assert_eq!(leader.provider_name(), "unknown");
}

#[test]
fn every_factory_hands_back_a_trait_object() {
    // Invariant I4: no consumer names a `Remote*Backend`. The types are private to
    // the crate, so the only thing that *can* come back is the trait object -
    // these bindings are the compile-time statement of it.
    let runtime = entered();
    let _reactor = runtime.enter();
    let client = RemoteClusterClient::connect_lazy(UNROUTABLE).expect("a valid endpoint");

    let _cache: Arc<dyn crate::cache::ClusterCacheBackend> =
        client.cache_backend("orders").expect("a handle");
    let _lock: Arc<dyn crate::lock::DistributedLockBackend> =
        client.lock_backend("orders").expect("a handle");
    let _leader: Arc<dyn crate::leader::LeaderElectionBackend> =
        client.leader_election_backend("orders").expect("a handle");
}

#[tokio::test]
async fn an_unreachable_cluster_is_a_retryable_provider_error() {
    // The exit criterion: a transport failure with no canonical body decodes as
    // `Provider{ConnectionLost}` and is therefore retryable, so an unreachable
    // cluster gear behaves for a consumer exactly like an unreachable Postgres -
    // same recovery path, no new consumer branch (DESIGN section 6.9).
    //
    // Port 1 on loopback refuses immediately, which keeps the test fast and makes
    // the failure a connection error rather than a timeout.
    let client = RemoteClusterClient::connect_lazy("http://127.0.0.1:1").expect("a valid endpoint");
    let cache = client.cache_backend("orders").expect("a handle");

    let err = cache
        .get("ledger")
        .await
        .expect_err("nothing is listening there");
    assert!(
        matches!(
            err,
            ClusterError::Provider {
                kind: crate::error::ProviderErrorKind::ConnectionLost,
                ..
            }
        ),
        "expected Provider{{ConnectionLost}}, got: {err}"
    );
    assert!(err.is_retryable(), "and it must be retryable");
}

#[tokio::test]
async fn a_descriptor_fetch_against_an_unreachable_cluster_reports_the_same() {
    // `descriptor()` is the only `async` member of the trait and the only thing
    // `resolve()` awaits. It must fail like any other call rather than hanging:
    // startup never blocks on cluster reachability (invariant I6).
    let client = RemoteClusterClient::connect_lazy("http://127.0.0.1:1").expect("a valid endpoint");

    let err = client
        .descriptor("orders")
        .await
        .expect_err("nothing is listening there");
    assert!(
        err.is_retryable(),
        "expected a retryable failure, got: {err}"
    );
}
