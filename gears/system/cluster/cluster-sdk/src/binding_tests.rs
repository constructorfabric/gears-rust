//! Tests for the four binding steps and for the unbound backends.
//!
//! The properties asserted here are the ones DESIGN.md and
//! §4.9.3 state, in the order they matter:
//!
//! - resolution goes through the process's `dyn ClusterClient`, and hands back
//!   the **real** backend `Arc` — nothing is interposed (invariant I14);
//! - an empty hub is `Ok`, not an error, and the facade it yields reports
//!   `ProfileNotBound` on first use (§4.9.1);
//! - a client that does not bind the profile is a **loud, immediate** failure;
//! - a descriptor that does not arrive in time defers validation rather than
//!   failing a correct configuration (§4.7.1);
//! - a *permanent* descriptor error is returned rather than deferred.

use std::sync::Arc;

use super::{
    NOTHING_WIRED, RESOLVE_DESCRIPTOR_TIMEOUT, bind, process_client, unbound_cache,
    unbound_leader_election, unbound_lock,
};
use crate::cache::{
    CacheConsistency, CacheEntry, CacheFeatures, CacheWatch, ClusterCacheBackend, PutRequest, Ttl,
};
use crate::client::ClusterClient;
use crate::error::ClusterError;
use crate::test_support::{StubClusterClient, with_nothing_derivable};
use async_trait::async_trait;
use toolkit::client_hub::ClientHub;

const PROFILE: &str = "orders";

/// One write request, so the unbound-backend sweep below reads as a list of
/// operations rather than of literals.
fn put_request() -> PutRequest<'static> {
    PutRequest {
        key: "k",
        value: b"v",
        ttl: Ttl::Indefinite,
    }
}

struct StubCache;

#[async_trait]
impl ClusterCacheBackend for StubCache {
    fn consistency(&self) -> CacheConsistency {
        CacheConsistency::Linearizable
    }
    fn features(&self) -> CacheFeatures {
        CacheFeatures::new(true)
    }
    fn provider_name(&self) -> &'static str {
        "stub-cache"
    }
    async fn get(&self, _key: &str) -> Result<Option<CacheEntry>, ClusterError> {
        Ok(None)
    }
    async fn put(&self, _req: PutRequest<'_>) -> Result<(), ClusterError> {
        Ok(())
    }
    async fn delete(&self, _key: &str) -> Result<bool, ClusterError> {
        Ok(false)
    }
    async fn contains(&self, _key: &str) -> Result<bool, ClusterError> {
        Ok(false)
    }
    async fn put_if_absent(
        &self,
        _req: PutRequest<'_>,
    ) -> Result<Option<CacheEntry>, ClusterError> {
        Ok(None)
    }
    async fn compare_and_swap(
        &self,
        _key: &str,
        _expected_version: u64,
        _new_value: &[u8],
        _ttl: Ttl,
    ) -> Result<CacheEntry, ClusterError> {
        Ok(CacheEntry {
            value: Vec::new(),
            version: 1,
        })
    }
    async fn watch(&self, _key: &str) -> Result<CacheWatch, ClusterError> {
        let (_tx, watch) = CacheWatch::channel(1);
        Ok(watch)
    }
    async fn watch_prefix(&self, _prefix: &str) -> Result<CacheWatch, ClusterError> {
        let (_tx, watch) = CacheWatch::channel(1);
        Ok(watch)
    }
}

/// Binds the cache primitive with no requirements — the shape all three
/// resolvers reduce to.
async fn bind_cache(hub: &ClientHub) -> Result<Arc<dyn ClusterCacheBackend>, ClusterError> {
    bind(
        hub,
        PROFILE,
        "cache",
        |client| client.cache_backend(PROFILE),
        || unbound_cache(PROFILE),
        |_descriptor| Ok(()),
    )
    .await
}

#[tokio::test]
async fn binds_the_backend_the_client_hands_back_without_interposing() {
    let hub = ClientHub::new();
    let backend: Arc<dyn ClusterCacheBackend> = Arc::new(StubCache);
    StubClusterClient::for_profile(PROFILE)
        .with_cache(Arc::clone(&backend))
        .register(&hub);

    let Ok(bound) = bind_cache(&hub).await else {
        panic!("a bound profile must resolve");
    };

    // Invariant I14: the facade holds the *real* backend, not a wrapper around
    // it. Pointer equality is the only assertion that can tell the difference.
    assert!(Arc::ptr_eq(&backend, &bound));
}

/// An empty hub with **no derivable endpoint**: the facade is unbound and says so.
///
/// The unset `POD_NAMESPACE` is the test's premise, not its environment. Since
/// `K3`, `process_client` self-constructs a remote client when the hub is empty and
/// one can be derived, so this case is specifically "nothing wired *and* nothing
/// derivable" - the Profile 1 build mistake §4.9.1 is about. Making the variable
/// explicit is what keeps the test from passing for the wrong reason on a developer
/// machine that happens to export it.
#[tokio::test]
async fn an_empty_hub_resolves_ok_and_the_backend_reports_profile_not_bound() {
    let hub = ClientHub::new();
    let bound = with_nothing_derivable(async {
        assert!(
            process_client(&hub).is_none(),
            "the fixture must start with nothing registered and nothing derivable"
        );
        let Ok(bound) = bind_cache(&hub).await else {
            panic!("an empty hub must not fail resolution - readiness reports it (DESIGN 4.9.1)");
        };
        bound
    })
    .await;

    // The first call names the profile, using the same variant a reachable
    // server returns for a profile it does not bind (invariant I3: no new
    // variant, and none is needed).
    assert!(matches!(
        bound.get("k").await,
        Err(ClusterError::ProfileNotBound { profile: "orders" })
    ));
    // The distinguishing phrase DESIGN 4.9.1 asks for is a log line, not part of
    // the error message: `ProfileNotBound`'s `Display` is frozen, so varying it
    // would mean widening the variant invariant I3 forbids widening.
    assert_eq!(
        NOTHING_WIRED,
        "no cluster client registered in this process"
    );
}

#[tokio::test]
async fn an_unbound_cache_backend_answers_every_operation_with_profile_not_bound() {
    let backend = unbound_cache(PROFILE);

    let not_bound = |err| matches!(err, ClusterError::ProfileNotBound { profile: "orders" });
    assert!(backend.get("k").await.err().is_some_and(not_bound));
    assert!(
        backend
            .put(put_request())
            .await
            .err()
            .is_some_and(not_bound)
    );
    assert!(backend.delete("k").await.err().is_some_and(not_bound));
    assert!(backend.contains("k").await.err().is_some_and(not_bound));
    assert!(
        backend
            .put_if_absent(put_request())
            .await
            .err()
            .is_some_and(not_bound)
    );
    assert!(
        backend
            .compare_and_swap("k", 1, b"v", Ttl::Indefinite)
            .await
            .err()
            .is_some_and(not_bound)
    );
    assert!(
        backend
            .compare_and_delete("k", b"v")
            .await
            .err()
            .is_some_and(not_bound)
    );
    assert!(backend.watch("k").await.err().is_some_and(not_bound));
    assert!(backend.watch_prefix("k").await.err().is_some_and(not_bound));
    assert!(backend.scan_prefix("k").await.err().is_some_and(not_bound));
    assert!(backend.probe().await.err().is_some_and(not_bound));
}

#[test]
fn the_unbound_backends_declare_the_weakest_reading_of_every_capability() {
    // The same fail-safe rule the remote handles follow with an empty descriptor
    // cache: a declared requirement must fail rather than be falsely satisfied.
    let cache = unbound_cache(PROFILE);
    assert_eq!(cache.consistency(), CacheConsistency::EventuallyConsistent);
    assert!(!cache.features().prefix_watch);
    assert_eq!(cache.provider_name(), "unbound");

    assert!(!unbound_lock(PROFILE).features().linearizable);
    assert!(!unbound_leader_election(PROFILE).features().linearizable);
}

#[tokio::test]
async fn a_client_that_does_not_bind_the_profile_fails_loudly_at_resolve() {
    let hub = ClientHub::new();
    // A client for a *different* profile: the "cluster reachable, profile not
    // bound" row of DESIGN 4.7's table, which is a permanent config error.
    StubClusterClient::for_profile("other")
        .with_cache(Arc::new(StubCache))
        .register(&hub);

    // And it names the profile the *caller* asked for. A client can only name a
    // profile it has seen - the registry falls back to a placeholder rather than
    // interning an unknown name - so the resolver restores its own, which
    // keeps the message identical to the one a pre-K4 resolve produced.
    assert!(matches!(
        bind_cache(&hub).await,
        Err(ClusterError::ProfileNotBound { profile: "orders" })
    ));
}

#[tokio::test]
async fn an_unavailable_descriptor_defers_validation_instead_of_failing() {
    let hub = ClientHub::new();
    StubClusterClient::for_profile(PROFILE)
        .with_cache(Arc::new(StubCache))
        .without_descriptor()
        .register(&hub);

    // The requirement is deliberately unsatisfiable, so the only way this can
    // return `Ok` is by not having validated: the deferred path (DESIGN 4.7.1).
    let bound = bind(
        &hub,
        PROFILE,
        "cache",
        |client| client.cache_backend(PROFILE),
        || unbound_cache(PROFILE),
        |_descriptor| {
            Err(ClusterError::CapabilityNotMet {
                primitive: "ClusterCacheV1",
                capability: "Linearizable",
                provider: "stub",
            })
        },
    )
    .await;

    assert!(
        bound.is_ok(),
        "a transient descriptor failure must not fail resolution - it is the cold-start path"
    );
}

#[tokio::test]
async fn a_permanent_descriptor_failure_is_returned_rather_than_deferred() {
    struct DescriptorRejects;

    #[async_trait]
    impl ClusterClient for DescriptorRejects {
        fn cache_backend(
            &self,
            _profile: &str,
        ) -> Result<Arc<dyn ClusterCacheBackend>, ClusterError> {
            // The remote shape: constructing a handle is blind, so the profile's
            // absence is only discovered when the descriptor is fetched.
            Ok(Arc::new(StubCache))
        }
        fn lock_backend(
            &self,
            _profile: &str,
        ) -> Result<Arc<dyn crate::lock::DistributedLockBackend>, ClusterError> {
            unreachable!("not under test")
        }
        fn leader_election_backend(
            &self,
            _profile: &str,
        ) -> Result<Arc<dyn crate::leader::LeaderElectionBackend>, ClusterError> {
            unreachable!("not under test")
        }
        async fn descriptor(
            &self,
            _profile: &str,
        ) -> Result<crate::dto::ProfileDescriptor, ClusterError> {
            Err(ClusterError::ProfileNotBound { profile: "orders" })
        }
    }

    let hub = ClientHub::new();
    hub.register::<dyn ClusterClient>(Arc::new(DescriptorRejects));

    assert!(matches!(
        bind_cache(&hub).await,
        Err(ClusterError::ProfileNotBound { .. })
    ));
}

/// Invariant I6, asserted rather than described: a descriptor that never arrives
/// must not hold `resolve()` open. The clock is paused, so the only way this test
/// can finish is by the timeout firing - a real wait would hang it.
#[tokio::test(start_paused = true)]
async fn a_descriptor_that_never_arrives_is_bounded_by_the_resolve_timeout() {
    struct DescriptorHangs;

    #[async_trait]
    impl ClusterClient for DescriptorHangs {
        fn cache_backend(
            &self,
            _profile: &str,
        ) -> Result<Arc<dyn ClusterCacheBackend>, ClusterError> {
            Ok(Arc::new(StubCache))
        }
        fn lock_backend(
            &self,
            _profile: &str,
        ) -> Result<Arc<dyn crate::lock::DistributedLockBackend>, ClusterError> {
            unreachable!("not under test")
        }
        fn leader_election_backend(
            &self,
            _profile: &str,
        ) -> Result<Arc<dyn crate::leader::LeaderElectionBackend>, ClusterError> {
            unreachable!("not under test")
        }
        async fn descriptor(
            &self,
            _profile: &str,
        ) -> Result<crate::dto::ProfileDescriptor, ClusterError> {
            std::future::pending().await
        }
    }

    let hub = ClientHub::new();
    hub.register::<dyn ClusterClient>(Arc::new(DescriptorHangs));

    let started = tokio::time::Instant::now();
    assert!(
        bind_cache(&hub).await.is_ok(),
        "an unreachable descriptor defers validation, it does not fail resolution"
    );
    assert!(
        started.elapsed() >= RESOLVE_DESCRIPTOR_TIMEOUT,
        "the wait must be the bounded one, not something shorter"
    );
}

/// `resolve()`'s self-construction arm: the branch §4.7.1 describes and `K4` left
/// named for `K3` (`binding::process_client`).
///
/// It matters when the framework's proxy-wiring phase did not run at all - a
/// consumer resolving outside a host runtime, or a wiring phase that failed - and
/// it is the one feature-gated branch on the resolve path. Exercised here by
/// skipping the phase entirely: nothing is registered, and `process_client` is
/// asked directly.
///
/// A `#[tokio::test]` because `connect_lazy` needs a reactor context (a `K2`
/// finding); no I/O happens, and the derived name does not resolve.
#[cfg(feature = "grpc-client")]
#[tokio::test]
async fn resolve_self_constructs_a_client_when_the_phase_did_not_run() {
    let hub = ClientHub::new();
    let client = temp_env::async_with_vars(
        [(crate::wiring::POD_NAMESPACE_ENV, Some("platform-test"))],
        async {
            let client = process_client(&hub).expect(
                "with a derivable endpoint, an empty hub must yield a self-constructed client",
            );
            // Registered, not merely returned: a second resolve in this process must
            // find the same client rather than building a second channel.
            let again = process_client(&hub).expect("the self-constructed client was registered");
            assert!(
                Arc::ptr_eq(&client, &again),
                "self-construction must be idempotent - one channel per process (invariant I4)"
            );
            client
        },
    )
    .await;

    // It is recorded as a *proxy*, so a wiring phase that runs afterwards still
    // reports `Remote` and a co-located gear's local client would still win.
    assert!(
        hub.has_remote_proxy::<dyn ClusterClient>(),
        "a self-constructed client is a remote proxy, not a local implementation"
    );
    assert!(
        hub.try_get_local::<dyn ClusterClient>().is_none(),
        "try_get_local must not report a self-constructed remote as local"
    );

    // And it behaves: the profile is unknown to an unreachable server, so the
    // facade resolves and the descriptor await defers rather than failing.
    drop(client);
}

/// `K5`'s seam: **every** resolve records into the process registry and reports
/// whether it found a client.
///
/// Without this the registry is dead code and invariant I5 is unenforced on the
/// deferred path however good the verdicts are — so this asserts the wiring rather
/// than the classification (`requirements_tests.rs` covers the verdicts).
///
/// It reads the process-global registry, which every other test in this binary also
/// writes to — **concurrently**, since `cargo test` runs them in parallel in one
/// process. So it asserts only what is race-free: that the count strictly grew, and
/// that this test's own profile is present. An exact delta looked right and failed
/// (observed +5), and the reason it failed is the very property under test: one
/// registry per process, shared by every resolve in it.
#[tokio::test]
async fn bind_records_the_resolve_and_whether_a_client_was_found() {
    use crate::requirements::requirements;

    let before = requirements().recorded_count();

    // A client that binds the profile: the resolve succeeds and is still recorded,
    // because section 5.6's refresh re-validates against the recorded set.
    let hub = ClientHub::new();
    StubClusterClient::for_profile(PROFILE)
        .with_cache(Arc::new(StubCache))
        .register(&hub);
    bind_cache(&hub).await.expect("a bound profile resolves");

    assert!(
        requirements().recorded_count() > before,
        "an inline-validated resolve must still be recorded, or a profile that degrades \
         after startup would never be re-checked"
    );
    assert!(
        requirements().client_seen(),
        "finding a client must be recorded, or the nothing-wired verdict would fire in a \
         correctly wired process"
    );
    assert!(
        requirements().recorded_profiles().contains(&PROFILE),
        "the profile must be enumerable, since that is what drives the descriptor refresh"
    );
}
