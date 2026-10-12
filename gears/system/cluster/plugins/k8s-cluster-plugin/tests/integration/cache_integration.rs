//! Layer 3 — cache integration scenarios (docs/TESTING.md §4.2). These mirror the
//! conformance cache scenarios with assertions on the actual `ClusterCacheEntry`
//! objects and on the request counts, which conformance cannot see.

#![cfg(feature = "integration")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::large_futures,
    reason = "integration tests: a setup failure IS the test failure"
)]

use crate::common;

use std::time::Duration;

use cluster_sdk::cache::{CacheConsistency, CacheEvent, CacheWatchEvent, PutRequest, Ttl};
use cluster_sdk::error::ClusterError;
use k8s_cluster_plugin::K8sCachePlugin;
use kube::ResourceExt;
use serde_json::json;

/// The wire labels/annotations, asserted at the server verbatim so a future
/// re-encoding is a test failure rather than a silent change (DESIGN §2.6).
const LABEL_MANAGED_BY: &str = "cluster.cf-gears.io/managed-by";
const MANAGED_BY_VALUE: &str = "cf-gears-cluster";
const LABEL_PRIMITIVE: &str = "cluster.cf-gears.io/primitive";
const ANNOTATION_NAME: &str = "cluster.cf-gears.io/name";

/// `K8S-CACHE-001`: `put` + `get` round-trip, verified through the API and against
/// the real `ClusterCacheEntry` object (value, version, labels, name annotation).
#[tokio::test]
async fn k8s_cache_001_put_get_round_trip() {
    let ns = common::fresh_namespace("cache-001").await;
    let client = ns.client.clone();
    let handle = K8sCachePlugin::builder(ns.cache_config_with(json!({})))
        .with_client(client)
        .build_and_start()
        .await
        .expect("cache starts");
    let cache = handle.cache();

    cache
        .put(PutRequest {
            key: "shard/7",
            value: b"holder-broker-7",
            ttl: Ttl::Indefinite,
        })
        .await
        .expect("put");

    let entry = cache.get("shard/7").await.expect("get").expect("present");
    assert_eq!(entry.value, b"holder-broker-7");
    assert_eq!(entry.version, 1);

    // The real object carries our bytes, version 1, the labels, and the *original*
    // unmapped key in the name annotation.
    let objects = ns.list_cache_entries().await;
    assert_eq!(objects.len(), 1, "exactly one cache object");
    let obj = &objects[0];
    assert_eq!(obj.spec.version, 1);
    let labels = obj.labels();
    assert_eq!(
        labels.get(LABEL_MANAGED_BY).map(String::as_str),
        Some(MANAGED_BY_VALUE)
    );
    assert_eq!(
        labels.get(LABEL_PRIMITIVE).map(String::as_str),
        Some("cache")
    );
    assert_eq!(
        obj.annotations().get(ANNOTATION_NAME).map(String::as_str),
        Some("shard/7"),
        "K8S-CACHE-001: the name annotation round-trips the original unmapped key"
    );

    handle.stop().await;
}

/// `K8S-CACHE-002`: version is ours, monotonic, and resets on delete-and-recreate;
/// `resourceVersion` moves by amounts unrelated to it.
#[tokio::test]
async fn k8s_cache_002_version_is_ours_monotonic_and_reset_on_recreate() {
    let ns = common::fresh_namespace("cache-002").await;
    let client = ns.client.clone();
    let handle = K8sCachePlugin::builder(ns.cache_config_with(json!({})))
        .with_client(client)
        .build_and_start()
        .await
        .expect("cache starts");
    let cache = handle.cache();

    let put = |v: &'static [u8]| {
        let cache = cache.clone();
        async move {
            cache
                .put(PutRequest {
                    key: "k",
                    value: v,
                    ttl: Ttl::Indefinite,
                })
                .await
                .expect("put");
        }
    };

    put(b"v1").await;
    assert_eq!(
        cache.get("k").await.expect("get").expect("present").version,
        1
    );
    let rv1 = ns.list_cache_entries().await[0]
        .resource_version()
        .expect("rv");
    put(b"v2").await;
    assert_eq!(
        cache.get("k").await.expect("get").expect("present").version,
        2
    );
    put(b"v3").await;
    assert_eq!(
        cache.get("k").await.expect("get").expect("present").version,
        3
    );
    let rv3 = ns.list_cache_entries().await[0]
        .resource_version()
        .expect("rv");
    assert_ne!(
        rv1, rv3,
        "resourceVersion moves independently of our version"
    );

    // Delete and recreate: version returns to 1.
    assert!(cache.delete("k").await.expect("delete"));
    put(b"again").await;
    assert_eq!(
        cache.get("k").await.expect("get").expect("present").version,
        1,
        "K8S-CACHE-002: version resets to 1 on delete-and-recreate"
    );

    handle.stop().await;
}

/// `K8S-CACHE-003`: a byte-identical `put` still bumps the version (and the object's
/// `resourceVersion`), the no-op-`PUT` trap.
#[tokio::test]
async fn k8s_cache_003_byte_identical_put_still_bumps_version() {
    let ns = common::fresh_namespace("cache-003").await;
    let client = ns.client.clone();
    let handle = K8sCachePlugin::builder(ns.cache_config_with(json!({})))
        .with_client(client)
        .build_and_start()
        .await
        .expect("cache starts");
    let cache = handle.cache();

    cache
        .put(PutRequest {
            key: "k",
            value: b"same",
            ttl: Ttl::Indefinite,
        })
        .await
        .expect("put");
    let rv1 = ns.list_cache_entries().await[0]
        .resource_version()
        .expect("rv");

    cache
        .put(PutRequest {
            key: "k",
            value: b"same",
            ttl: Ttl::Indefinite,
        })
        .await
        .expect("put again, identical value");

    let entry = cache.get("k").await.expect("get").expect("present");
    assert_eq!(
        entry.version, 2,
        "K8S-CACHE-003: identical value still bumps version 1 -> 2"
    );
    let rv2 = ns.list_cache_entries().await[0]
        .resource_version()
        .expect("rv");
    assert_ne!(rv1, rv2, "the object was genuinely rewritten");

    handle.stop().await;
}

/// `K8S-CACHE-004`/`005`: 20 concurrent CAS writers from the same expected version —
/// exactly one wins, 19 get `CasConflict` with a populated `current`; and a
/// conflicted CAS writes nothing (version advanced by exactly 1).
#[tokio::test]
async fn k8s_cache_004_cas_under_concurrent_writers_and_writes_nothing_on_conflict() {
    let ns = common::fresh_namespace("cache-004").await;
    let client = ns.client.clone();
    let handle = K8sCachePlugin::builder(ns.cache_config_with(json!({})))
        .with_client(client)
        .build_and_start()
        .await
        .expect("cache starts");
    let cache = handle.cache();

    let created = cache
        .put_if_absent(PutRequest {
            key: "k",
            value: b"base",
            ttl: Ttl::Indefinite,
        })
        .await
        .expect("put_if_absent")
        .expect("absent -> created");
    assert_eq!(created.version, 1);

    let mut tasks = Vec::new();
    for i in 0..20u8 {
        let cache = cache.clone();
        tasks.push(tokio::spawn(async move {
            cache
                .compare_and_swap("k", created.version, &[i], Ttl::Indefinite)
                .await
        }));
    }
    let mut wins = 0;
    let mut conflicts = 0;
    for t in tasks {
        match t.await.expect("task") {
            Ok(_) => wins += 1,
            Err(ClusterError::CasConflict { current, .. }) => {
                conflicts += 1;
                assert!(
                    current.is_some(),
                    "K8S-CACHE-004: CasConflict carries current"
                );
            }
            Err(other) => panic!("unexpected CAS error: {other:?}"),
        }
    }
    assert_eq!(wins, 1, "K8S-CACHE-004: exactly one CAS winner");
    assert_eq!(conflicts, 19, "K8S-CACHE-004: nineteen conflicts");

    // K8S-CACHE-005: the conflicted writers wrote nothing; version advanced by 1.
    assert_eq!(
        cache.get("k").await.expect("get").expect("present").version,
        2,
        "K8S-CACHE-005: exactly one successful write past the base"
    );

    handle.stop().await;
}

/// `K8S-CACHE-006`: TTL expiry is deadline-armed — a 50ms TTL produces an `Expired`
/// watch event well within a second and the object is gone, with no sweep interval
/// configured.
#[tokio::test]
async fn k8s_cache_006_ttl_expiry_is_deadline_armed() {
    let ns = common::fresh_namespace("cache-006").await;
    let client = ns.client.clone();
    let handle = K8sCachePlugin::builder(ns.cache_config_with(json!({})))
        .with_client(client)
        .build_and_start()
        .await
        .expect("cache starts");
    let cache = handle.cache();

    // With the watcher on (the default), both watch dimensions are declared — the
    // `CacheFeatures::new(true)` branch, complementing K8S-CACHE-017's watchless case.
    assert!(
        cache.features().watch() && cache.features().prefix_watch(),
        "K8S-CACHE-006: cache_watch:true declares exact and prefix watch"
    );

    let mut watch = cache.watch("k").await.expect("watch");
    cache
        .put(PutRequest {
            key: "k",
            value: b"v",
            ttl: Ttl::Of(Duration::from_millis(50)),
        })
        .await
        .expect("put with a 50ms ttl");

    // Drain the put's own Changed, then the deadline-armed sweep's Expired.
    let mut saw_expired = false;
    for _ in 0..16 {
        match tokio::time::timeout(Duration::from_secs(2), watch.recv()).await {
            Ok(Some(CacheWatchEvent::Event(CacheEvent::Expired { key }))) if key == "k" => {
                saw_expired = true;
                break;
            }
            Ok(Some(_)) => {}
            other => panic!("K8S-CACHE-006: watch ended before Expired: {other:?}"),
        }
    }
    assert!(
        saw_expired,
        "K8S-CACHE-006: a 50ms TTL emits Expired promptly"
    );
    assert!(
        cache.get("k").await.expect("get").is_none(),
        "reads as absent"
    );
    let gone = common::wait_until(
        Duration::from_secs(3),
        Duration::from_millis(50),
        || async { ns.list_cache_entries().await.is_empty() },
    )
    .await;
    assert!(gone, "K8S-CACHE-006: the object is reclaimed");

    handle.stop().await;
}

/// `K8S-CACHE-007`: read-path expiry is enforced independent of the sweeper — with
/// the watcher off and a long sweep interval, an entry past its `expiresAt` reads as
/// absent while the object still exists in the API server.
#[tokio::test]
async fn k8s_cache_007_read_path_expiry_is_independent_of_the_sweeper() {
    let ns = common::fresh_namespace("cache-007").await;
    let client = ns.client.clone();
    // No watcher, and a sweep interval far longer than the test: nothing reclaims
    // the object, so a `None` read can only come from read-path enforcement.
    let handle = K8sCachePlugin::builder(
        ns.cache_config_with(json!({ "cache_watch": false, "cache_sweep_interval_ms": 3_600_000 })),
    )
    .with_client(client)
    .build_and_start()
    .await
    .expect("cache starts");
    let cache = handle.cache();

    cache
        .put(PutRequest {
            key: "k",
            value: b"v",
            ttl: Ttl::Of(Duration::from_millis(50)),
        })
        .await
        .expect("put");
    tokio::time::sleep(Duration::from_millis(150)).await;

    assert!(
        cache.get("k").await.expect("get").is_none(),
        "K8S-CACHE-007: get is None past expiry"
    );
    assert!(
        !cache.contains("k").await.expect("contains"),
        "K8S-CACHE-007: contains is false"
    );
    assert_eq!(
        ns.list_cache_entries().await.len(),
        1,
        "K8S-CACHE-007: the object still exists - only the read enforced expiry"
    );

    handle.stop().await;
}

/// `K8S-CACHE-006` (fixed-interval half): with `cache_watch: false`, the documented
/// fallback sweeper reclaims an expired entry on `cache_sweep_interval_ms` instead of
/// the deadline-armed path — proportionally slower, but it does happen (DESIGN §6.2).
/// The complement of `K8S-CACHE-007`, which uses a long interval to prove nothing
/// reclaims within the window; here a short interval proves the sweep lands.
#[tokio::test]
async fn k8s_cache_006_fixed_interval_fallback_reclaims() {
    let ns = common::fresh_namespace("cache-006b").await;
    let client = ns.client.clone();
    // Watcher off, so reclamation runs on the fixed-interval fallback; a short
    // interval keeps the test quick. The sweeper sleeps one interval before its first
    // pass (sleep-first ordering), so the object is present immediately after the put.
    let handle = K8sCachePlugin::builder(
        ns.cache_config_with(json!({ "cache_watch": false, "cache_sweep_interval_ms": 500 })),
    )
    .with_client(client)
    .build_and_start()
    .await
    .expect("cache starts");
    let cache = handle.cache();

    cache
        .put(PutRequest {
            key: "k",
            value: b"v",
            ttl: Ttl::Of(Duration::from_millis(50)),
        })
        .await
        .expect("put with a 50ms ttl");

    // The object outlives its TTL until the fallback sweep runs, then is reclaimed.
    let gone = common::wait_until(
        Duration::from_secs(5),
        Duration::from_millis(100),
        || async { ns.list_cache_entries().await.is_empty() },
    )
    .await;
    assert!(
        gone,
        "K8S-CACHE-006: the fixed-interval fallback sweeper reclaims the expired object"
    );

    handle.stop().await;
}

/// `K8S-CACHE-017`: with `cache_watch: false` the backend runs no watcher task, so it
/// serves no watch feed. It must declare that honestly — `features().watch` and
/// `prefix_watch` both `false` — and refuse `watch`/`watch_prefix` with the *canonical*
/// `Unsupported` feature strings. `CasBasedLeaderElectionBackend::elect` matches
/// `Unsupported { feature: "watch" }` exactly to degrade to its renewal timer; any other
/// shape (a live-but-silent channel, a non-canonical string) kills leader election in
/// the `cache: k8s` + `cache_watch: false` configuration (finding #4 / cluster #4778).
#[tokio::test]
async fn k8s_cache_017_watchless_mode_declares_and_refuses_watch() {
    let ns = common::fresh_namespace("cache-017").await;
    let client = ns.client.clone();
    let handle = K8sCachePlugin::builder(
        ns.cache_config_with(json!({ "cache_watch": false, "cache_sweep_interval_ms": 3_600_000 })),
    )
    .with_client(client)
    .build_and_start()
    .await
    .expect("cache starts");
    let cache = handle.cache();

    let features = cache.features();
    assert!(
        !features.watch(),
        "K8S-CACHE-017: cache_watch:false declares no exact watch"
    );
    assert!(
        !features.prefix_watch(),
        "K8S-CACHE-017: cache_watch:false declares no prefix watch"
    );
    assert!(
        matches!(
            cache.watch("k").await,
            Err(ClusterError::Unsupported { feature: "watch" })
        ),
        "K8S-CACHE-017: watch refuses with the canonical `watch` feature"
    );
    assert!(
        matches!(
            cache.watch_prefix("p").await,
            Err(ClusterError::Unsupported {
                feature: "prefix_watch"
            })
        ),
        "K8S-CACHE-017: watch_prefix refuses with the canonical `prefix_watch` feature"
    );

    handle.stop().await;
}

/// `K8S-CACHE-008`: an `Indefinite` entry is never swept and has no `expiresAt`.
#[tokio::test]
async fn k8s_cache_008_indefinite_is_never_swept() {
    let ns = common::fresh_namespace("cache-008").await;
    let client = ns.client.clone();
    let handle = K8sCachePlugin::builder(ns.cache_config_with(json!({})))
        .with_client(client)
        .build_and_start()
        .await
        .expect("cache starts");
    let cache = handle.cache();

    cache
        .put(PutRequest {
            key: "k",
            value: b"v",
            ttl: Ttl::Indefinite,
        })
        .await
        .expect("put");
    let obj = &ns.list_cache_entries().await[0];
    assert!(obj.spec.expires_at.is_none(), "K8S-CACHE-008: no expiresAt");

    // Still present after many multiples of any TTL in the suite.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        cache.get("k").await.expect("get").is_some(),
        "K8S-CACHE-008: still present"
    );
    assert_eq!(ns.list_cache_entries().await.len(), 1);

    handle.stop().await;
}

/// `K8S-CACHE-009`: `compare_and_delete` is atomic and value-guarded — a match
/// deletes; a mismatch is `Ok(false)` and leaves the entry intact.
#[tokio::test]
async fn k8s_cache_009_compare_and_delete_is_value_guarded() {
    let ns = common::fresh_namespace("cache-009").await;
    let client = ns.client.clone();
    let handle = K8sCachePlugin::builder(ns.cache_config_with(json!({})))
        .with_client(client)
        .build_and_start()
        .await
        .expect("cache starts");
    let cache = handle.cache();

    cache
        .put(PutRequest {
            key: "k",
            value: b"real",
            ttl: Ttl::Indefinite,
        })
        .await
        .expect("put");

    // Mismatch -> Ok(false), object intact.
    assert!(
        !cache
            .compare_and_delete("k", b"wrong")
            .await
            .expect("cad mismatch"),
        "K8S-CACHE-009: a value mismatch does not delete"
    );
    assert!(
        cache.get("k").await.expect("get").is_some(),
        "still present after mismatch"
    );

    // Match -> deleted.
    assert!(
        cache
            .compare_and_delete("k", b"real")
            .await
            .expect("cad match"),
        "K8S-CACHE-009: a value match deletes"
    );
    assert!(
        cache.get("k").await.expect("get").is_none(),
        "gone after match"
    );

    handle.stop().await;
}

/// `K8S-CACHE-010`: `put_if_absent` on a live entry returns `None` and does not
/// overwrite. The `CREATE` `409`s, then a read distinguishes a *live* entry (present
/// → `None`) from an expired-but-unswept one (logically absent → reclaim), so the
/// live path is one mutating request plus one read — asserted via the request counter.
#[tokio::test]
async fn k8s_cache_010_put_if_absent_on_a_live_key_returns_none_without_overwrite() {
    let ns = common::fresh_namespace("cache-010").await;
    let (client, counts) = common::counted_client().await;
    let handle = K8sCachePlugin::builder(ns.cache_config_with(json!({})))
        .with_client(client)
        .build_and_start()
        .await
        .expect("cache starts");
    let cache = handle.cache();

    cache
        .put(PutRequest {
            key: "k",
            value: b"live",
            ttl: Ttl::Indefinite,
        })
        .await
        .expect("seed");

    let reads_before = counts.reads();
    let mutating_before = counts.mutating();
    let absent = cache
        .put_if_absent(PutRequest {
            key: "k",
            value: b"other",
            ttl: Ttl::Indefinite,
        })
        .await
        .expect("put_if_absent on a live entry");
    assert!(
        absent.is_none(),
        "K8S-CACHE-010: returns None on a live entry"
    );

    assert_eq!(
        counts.mutating() - mutating_before,
        1,
        "K8S-CACHE-010: exactly one mutating request (the CREATE that 409'd)"
    );
    assert_eq!(
        counts.reads() - reads_before,
        1,
        "K8S-CACHE-010: one read to confirm the 409'd entry is live (not expired)"
    );
    assert_eq!(
        cache.get("k").await.expect("get").expect("present").value,
        b"live",
        "K8S-CACHE-010: the live value is untouched"
    );

    handle.stop().await;
}

/// `K8S-CACHE-016`: `put_if_absent` on an *expired-but-unswept* entry reclaims it and
/// claims the key — the core of the CRITICAL fix (finding 5). With the watcher off and a
/// 1h sweep interval the object provably lingers past its TTL (as in K8S-CACHE-007), so
/// the only thing that can turn it into `Some` is `put_if_absent`'s own
/// `409 → read → expired → guarded-delete → re-create` path. Under the pre-fix code it
/// returned `None` (reported present), stalling a CAS-based leader-election failover.
#[tokio::test]
async fn k8s_cache_016_put_if_absent_reclaims_an_expired_entry() {
    let ns = common::fresh_namespace("cache-016").await;
    let client = ns.client.clone();
    let handle = K8sCachePlugin::builder(
        ns.cache_config_with(json!({ "cache_watch": false, "cache_sweep_interval_ms": 3_600_000 })),
    )
    .with_client(client)
    .build_and_start()
    .await
    .expect("cache starts");
    let cache = handle.cache();

    // Seed a short-TTL entry and let it lapse; nothing reclaims it within the window.
    cache
        .put(PutRequest {
            key: "lock",
            value: b"holder-a",
            ttl: Ttl::Of(Duration::from_millis(50)),
        })
        .await
        .expect("seed a short-ttl entry");
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(
        ns.list_cache_entries().await.len(),
        1,
        "K8S-CACHE-016: the expired object still physically exists (unswept)"
    );
    assert!(
        cache.get("lock").await.expect("get").is_none(),
        "K8S-CACHE-016: the read path already treats it as absent"
    );

    // `put_if_absent` must agree it is absent: reclaim the expired object and claim.
    let claimed = cache
        .put_if_absent(PutRequest {
            key: "lock",
            value: b"holder-b",
            ttl: Ttl::Indefinite,
        })
        .await
        .expect("put_if_absent on an expired entry");
    let entry =
        claimed.expect("K8S-CACHE-016: an expired entry is reclaimed, not reported present");
    assert_eq!(
        entry.value, b"holder-b",
        "K8S-CACHE-016: the new holder's value"
    );
    assert_eq!(
        entry.version, 1,
        "K8S-CACHE-016: the reclaim re-creates at version 1"
    );

    // Exactly one object, carrying the new value — reclaimed in place, not forked.
    assert_eq!(
        ns.list_cache_entries().await.len(),
        1,
        "K8S-CACHE-016: still exactly one object after the reclaim"
    );
    assert_eq!(
        cache
            .get("lock")
            .await
            .expect("get")
            .expect("present")
            .value,
        b"holder-b",
        "K8S-CACHE-016: the live value is the new holder's"
    );

    handle.stop().await;
}

/// `K8S-CACHE-011` (declaration half): `reads: cached` downgrades `consistency()`
/// to `EventuallyConsistent`; the default is `Linearizable`.
#[tokio::test]
async fn k8s_cache_011_cached_reads_downgrade_the_declaration() {
    let ns = common::fresh_namespace("cache-011").await;

    let quorum = K8sCachePlugin::builder(ns.cache_config_with(json!({})))
        .with_client(ns.client.clone())
        .build_and_start()
        .await
        .expect("quorum cache starts");
    assert_eq!(
        quorum.cache().consistency(),
        CacheConsistency::Linearizable,
        "K8S-CACHE-011: quorum reads are Linearizable"
    );
    quorum.stop().await;

    let cached = K8sCachePlugin::builder(ns.cache_config_with(json!({ "cache_reads": "cached" })))
        .with_client(ns.client.clone())
        .build_and_start()
        .await
        .expect("cached cache starts");
    assert_eq!(
        cached.cache().consistency(),
        CacheConsistency::EventuallyConsistent,
        "K8S-CACHE-011: cached reads are EventuallyConsistent"
    );
    cached.stop().await;
}

/// `K8S-CACHE-012`: an oversized value is refused locally, naming the limit, with
/// zero requests issued; one at the limit succeeds.
#[tokio::test]
async fn k8s_cache_012_oversized_values_are_refused_locally() {
    let ns = common::fresh_namespace("cache-012").await;
    let (client, counts) = common::counted_client().await;
    let handle = K8sCachePlugin::builder(ns.cache_config_with(json!({ "max_value_bytes": 1024 })))
        .with_client(client)
        .build_and_start()
        .await
        .expect("cache starts");
    let cache = handle.cache();

    let over = vec![0u8; 1025];
    let mutating_before = counts.mutating();
    let err = cache
        .put(PutRequest {
            key: "k",
            value: &over,
            ttl: Ttl::Indefinite,
        })
        .await
        .expect_err("oversized put is rejected");
    match err {
        ClusterError::InvalidConfig { reason } => {
            assert!(
                reason.contains("1024") || reason.contains("1025"),
                "names the size: {reason}"
            );
        }
        other => panic!("K8S-CACHE-012: expected InvalidConfig, got {other:?}"),
    }
    assert_eq!(
        counts.mutating() - mutating_before,
        0,
        "K8S-CACHE-012: rejected before any request"
    );

    let at_limit = vec![7u8; 1024];
    cache
        .put(PutRequest {
            key: "k",
            value: &at_limit,
            ttl: Ttl::Indefinite,
        })
        .await
        .expect("K8S-CACHE-012: a value at the limit succeeds");
    assert_eq!(
        cache.get("k").await.expect("get").expect("present").value,
        at_limit
    );

    handle.stop().await;
}

/// `K8S-CACHE-014`: per-operation request volume, asserted as counts:
/// `get` is 1 request, `put_if_absent` is 1 on an absent key and 2 on a live key
/// (create(409) + a read to confirm liveness — see `K8S-CACHE-010`), `compare_and_swap`
/// is 2, `put` is 1 on create and 3 on overwrite (create(409) + read + guarded replace
/// — see the inline note below; DESIGN §6.1's table says 2, but the measured count is
/// the honest one), and `watch` is 0.
#[tokio::test]
async fn k8s_cache_014_request_volume_per_operation() {
    let ns = common::fresh_namespace("cache-014").await;
    let (client, counts) = common::counted_client().await;
    let handle = K8sCachePlugin::builder(ns.cache_config_with(json!({})))
        .with_client(client)
        .build_and_start()
        .await
        .expect("cache starts");
    let cache = handle.cache();

    let total = {
        let counts = counts.clone();
        move || counts.reads() + counts.mutating()
    };

    // put on create: 1 request (the CREATE succeeds).
    let before_create = total();
    cache
        .put(PutRequest {
            key: "k",
            value: b"v1",
            ttl: Ttl::Indefinite,
        })
        .await
        .expect("put");
    assert_eq!(
        total() - before_create,
        1,
        "K8S-CACHE-014: put-on-create is 1 request"
    );

    // put on overwrite: 3 requests - CREATE (409 AlreadyExists), then read + guarded
    // replace. DESIGN 6.1's table says 2, but a 409 AlreadyExists carries no
    // resourceVersion, so the guarded replace genuinely needs the intervening read;
    // the create-first strategy keeps the common create case at 1. The measured
    // count is the honest one.
    let before_overwrite = total();
    cache
        .put(PutRequest {
            key: "k",
            value: b"v2",
            ttl: Ttl::Indefinite,
        })
        .await
        .expect("put");
    assert_eq!(
        total() - before_overwrite,
        3,
        "K8S-CACHE-014: put-on-overwrite is create(409)+read+replace"
    );

    // get: 1.
    let before_get = total();
    cache.get("k").await.expect("get");
    assert_eq!(total() - before_get, 1, "K8S-CACHE-014: get is 1 request");

    // put_if_absent on a live key: 2 (CREATE 409s, then a read confirms the entry is
    // live rather than expired-but-unswept — see K8S-CACHE-010).
    let before_pia = total();
    let _pia = cache
        .put_if_absent(PutRequest {
            key: "k",
            value: b"x",
            ttl: Ttl::Indefinite,
        })
        .await;
    assert_eq!(
        total() - before_pia,
        2,
        "K8S-CACHE-014: put_if_absent on a live key is create(409)+read"
    );

    // compare_and_swap: 2 (read + guarded replace).
    let cur = cache.get("k").await.expect("get").expect("present");
    let before_cas = total();
    cache
        .compare_and_swap("k", cur.version, b"v3", Ttl::Indefinite)
        .await
        .expect("cas");
    assert_eq!(
        total() - before_cas,
        2,
        "K8S-CACHE-014: compare_and_swap is 2 requests"
    );

    // watch: 0 requests (fans out from the one shared watcher).
    let watch_before = counts.watches();
    let before_watch = total();
    let _w = cache.watch("k").await.expect("watch");
    assert_eq!(
        total() - before_watch,
        0,
        "K8S-CACHE-014: watch issues no per-subscriber request"
    );
    assert_eq!(
        counts.watches() - watch_before,
        0,
        "K8S-CACHE-014: no new watch connection"
    );

    handle.stop().await;
}

/// `K8S-CACHE-015`: `scan_prefix` filters by prefix, excludes expired-but-present
/// entries, and returns original (unmapped) keys.
#[tokio::test]
async fn k8s_cache_015_scan_prefix_filters_and_excludes_expired() {
    let ns = common::fresh_namespace("cache-015").await;
    let client = ns.client.clone();
    // Watcher off + long sweep so the short-TTL entry lingers as an object but is
    // excluded from the scan by read-path expiry.
    let handle = K8sCachePlugin::builder(
        ns.cache_config_with(json!({ "cache_watch": false, "cache_sweep_interval_ms": 3_600_000 })),
    )
    .with_client(client)
    .build_and_start()
    .await
    .expect("cache starts");
    let cache = handle.cache();

    for key in ["p/a", "p/b", "p/c"] {
        cache
            .put(PutRequest {
                key,
                value: b"v",
                ttl: Ttl::Indefinite,
            })
            .await
            .expect("put");
    }
    cache
        .put(PutRequest {
            key: "other/z",
            value: b"v",
            ttl: Ttl::Indefinite,
        })
        .await
        .expect("put");
    cache
        .put(PutRequest {
            key: "p/expired",
            value: b"v",
            ttl: Ttl::Of(Duration::from_millis(50)),
        })
        .await
        .expect("put");
    tokio::time::sleep(Duration::from_millis(150)).await;

    let mut keys = cache.scan_prefix("p/").await.expect("scan_prefix");
    keys.sort();
    assert_eq!(
        keys,
        vec!["p/a".to_owned(), "p/b".to_owned(), "p/c".to_owned()],
        "K8S-CACHE-015: original keys, prefix-filtered, expired excluded"
    );

    handle.stop().await;
}
