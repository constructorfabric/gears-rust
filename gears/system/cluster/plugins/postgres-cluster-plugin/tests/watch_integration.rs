//! Layer 3 — cache watch integration scenarios (docs/TESTING.md §4.4,
//! `PG-WATCH-001..007`).
//!
//! Every scenario here constructs the cache through the real
//! `PostgresClusterPlugin::build_and_start` path deliberately — `put`/`delete`
//! never call the watch registry directly, only `NOTIFY`; delivery happens
//! through the plugin's own dedicated LISTEN task started by
//! `build_and_start` (DESIGN.md §4.3, `cache/watch.rs`'s module doc). A
//! bare `PostgresCache` (if constructed directly, bypassing the plugin)
//! would never receive a single watch event.

#![cfg(feature = "integration")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration tests: a setup failure IS the test failure"
)]

mod common;

use std::time::Duration;

use cluster_sdk::cache::{PutRequest, Ttl};
use cluster_sdk::error::ClusterError;
use cluster_sdk::{CacheEvent, CacheWatchEvent};
use postgres_cluster_plugin::PostgresClusterPlugin;
use serde_json::json;

/// `PG-WATCH-001`: `watch(key)` receives `Changed` on `put`.
#[tokio::test]
async fn pg_watch_001_receives_changed_on_put() {
    let (_container, config) = common::start_postgres().await;
    let handle = PostgresClusterPlugin::builder(config)
        .build_and_start()
        .await
        .unwrap();
    let cache = handle.cache();

    let mut watch = cache.watch("w1").await.expect("watch succeeds");
    cache
        .put(PutRequest {
            key: "w1",
            value: b"v",
            ttl: Ttl::Indefinite,
        })
        .await
        .expect("put succeeds");

    let event = tokio::time::timeout(Duration::from_secs(3), watch.recv())
        .await
        .expect("event arrives within 3s");
    assert!(
        matches!(event, Some(CacheWatchEvent::Event(CacheEvent::Changed { ref key })) if key == "w1"),
        "PG-WATCH-001: expected Changed{{key: \"w1\"}}, got {event:?}"
    );

    handle.stop().await;
}

/// `PG-WATCH-002`: `watch(key)` receives `Deleted` on `delete`.
#[tokio::test]
async fn pg_watch_002_receives_deleted_on_delete() {
    let (_container, config) = common::start_postgres().await;
    let handle = PostgresClusterPlugin::builder(config)
        .build_and_start()
        .await
        .unwrap();
    let cache = handle.cache();

    // The watch is registered *before* the setup `put`, and that put's own
    // `Changed` event is explicitly drained first — registering a watch
    // after an unrelated write to the same key risks catching that write's
    // still-in-flight NOTIFY instead of (or ahead of) the delete's, since
    // NOTIFY delivery is asynchronous relative to the writing transaction's
    // commit (see `PG-CACHE-004`'s identical race and fix).
    let mut watch = cache.watch("w2").await.expect("watch succeeds");
    cache
        .put(PutRequest {
            key: "w2",
            value: b"v",
            ttl: Ttl::Indefinite,
        })
        .await
        .expect("put succeeds");
    let changed = tokio::time::timeout(Duration::from_secs(3), watch.recv())
        .await
        .expect("the setup put's own Changed event arrives");
    assert!(
        matches!(changed, Some(CacheWatchEvent::Event(CacheEvent::Changed { ref key })) if key == "w2"),
        "PG-WATCH-002 setup: expected the put's own Changed event first, got {changed:?}"
    );

    cache.delete("w2").await.expect("delete succeeds");

    let event = tokio::time::timeout(Duration::from_secs(3), watch.recv())
        .await
        .expect("event arrives within 3s");
    assert!(
        matches!(event, Some(CacheWatchEvent::Event(CacheEvent::Deleted { ref key })) if key == "w2"),
        "PG-WATCH-002: expected Deleted{{key: \"w2\"}}, got {event:?}"
    );

    handle.stop().await;
}

/// `PG-WATCH-003`: the TTL reaper deleting an expired key emits `Expired`.
#[tokio::test]
async fn pg_watch_003_receives_expired_on_reaper_sweep() {
    let (_container, config) =
        common::start_postgres_with(json!({ "cache_reaper_interval_ms": 100 })).await;
    let handle = PostgresClusterPlugin::builder(config)
        .build_and_start()
        .await
        .unwrap();
    let cache = handle.cache();

    let mut watch = cache.watch("w3").await.expect("watch succeeds");
    cache
        .put(PutRequest {
            key: "w3",
            value: b"v",
            ttl: Ttl::Of(Duration::from_millis(50)),
        })
        .await
        .expect("put succeeds");

    // Drain the put's own `Changed` event (NOTIFY self-delivery, DESIGN.md
    // §4.3) before waiting for the reaper's `Expired` — otherwise this races
    // the first event on the same watch (see `PG-CACHE-004`'s identical fix).
    let changed = tokio::time::timeout(Duration::from_secs(2), watch.recv())
        .await
        .expect("the put's own Changed event arrives before the timeout");
    assert!(
        matches!(changed, Some(CacheWatchEvent::Event(CacheEvent::Changed { ref key })) if key == "w3"),
        "PG-WATCH-003 setup: expected the put's own Changed event first, got {changed:?}"
    );

    let event = tokio::time::timeout(Duration::from_secs(2), watch.recv())
        .await
        .expect("event arrives before the timeout");
    assert!(
        matches!(event, Some(CacheWatchEvent::Event(CacheEvent::Expired { ref key })) if key == "w3"),
        "PG-WATCH-003: expected Expired{{key: \"w3\"}}, got {event:?}"
    );

    handle.stop().await;
}

/// `PG-WATCH-004`: `watch_prefix` returns `Unsupported`; `features().prefix_watch == false`.
#[tokio::test]
async fn pg_watch_004_watch_prefix_unsupported() {
    let (_container, config) = common::start_postgres().await;
    let handle = PostgresClusterPlugin::builder(config)
        .build_and_start()
        .await
        .unwrap();
    let cache = handle.cache();

    assert!(
        !cache.features().prefix_watch,
        "PG-WATCH-004: prefix_watch must be false"
    );
    let result = cache.watch_prefix("svc/").await;
    assert!(
        matches!(
            result,
            Err(ClusterError::Unsupported {
                feature: "prefix_watch"
            })
        ),
        "PG-WATCH-004: expected Unsupported{{feature: \"prefix_watch\"}}, got {result:?}"
    );

    handle.stop().await;
}

/// `PG-WATCH-005`: an active watch receives terminal `Closed(Shutdown)`
/// before `stop()` returns.
#[tokio::test]
async fn pg_watch_005_closed_shutdown_before_stop_returns() {
    let (_container, config) = common::start_postgres().await;
    let handle = PostgresClusterPlugin::builder(config)
        .build_and_start()
        .await
        .unwrap();
    let cache = handle.cache();

    let mut watch = cache.watch("w5").await.expect("watch succeeds");
    handle.stop().await;

    let event = tokio::time::timeout(Duration::from_secs(3), watch.recv())
        .await
        .expect("event arrives")
        .expect("channel not closed without a terminal event");
    assert!(
        matches!(event, CacheWatchEvent::Closed(ClusterError::Shutdown)),
        "PG-WATCH-005: expected Closed(Shutdown), got {event:?}"
    );
}

/// `PG-WATCH-006`: a watcher on key `"a"` receives nothing when key `"b"` is
/// written.
#[tokio::test]
async fn pg_watch_006_no_events_for_different_key() {
    let (_container, config) = common::start_postgres().await;
    let handle = PostgresClusterPlugin::builder(config)
        .build_and_start()
        .await
        .unwrap();
    let cache = handle.cache();

    let mut watch = cache.watch("a").await.expect("watch succeeds");
    cache
        .put(PutRequest {
            key: "b",
            value: b"v",
            ttl: Ttl::Indefinite,
        })
        .await
        .expect("put succeeds");

    let outcome = tokio::time::timeout(Duration::from_secs(3), watch.recv()).await;
    assert!(
        outcome.is_err(),
        "PG-WATCH-006: watcher on \"a\" must receive nothing when \"b\" is written, got {outcome:?}"
    );

    handle.stop().await;
}

/// `PG-WATCH-007`: two watchers on the same key both receive the event; one
/// dropping does not affect the other.
#[tokio::test]
async fn pg_watch_007_multiple_watchers_same_key() {
    let (_container, config) = common::start_postgres().await;
    let handle = PostgresClusterPlugin::builder(config)
        .build_and_start()
        .await
        .unwrap();
    let cache = handle.cache();

    let mut watch_a = cache.watch("w7").await.expect("watch a succeeds");
    let mut watch_b = cache.watch("w7").await.expect("watch b succeeds");

    cache
        .put(PutRequest {
            key: "w7",
            value: b"v1",
            ttl: Ttl::Indefinite,
        })
        .await
        .expect("put succeeds");

    for watch in [&mut watch_a, &mut watch_b] {
        let event = tokio::time::timeout(Duration::from_secs(3), watch.recv())
            .await
            .expect("event arrives within 3s");
        assert!(
            matches!(event, Some(CacheWatchEvent::Event(CacheEvent::Changed { ref key })) if key == "w7"),
            "PG-WATCH-007: both watchers must receive Changed, got {event:?}"
        );
    }

    drop(watch_a);
    cache
        .put(PutRequest {
            key: "w7",
            value: b"v2",
            ttl: Ttl::Indefinite,
        })
        .await
        .expect("second put succeeds");
    let event = tokio::time::timeout(Duration::from_secs(3), watch_b.recv())
        .await
        .expect("survivor still receives events after the other watcher dropped");
    assert!(
        matches!(event, Some(CacheWatchEvent::Event(CacheEvent::Changed { ref key })) if key == "w7"),
        "PG-WATCH-007: the surviving watcher must be unaffected by the dropped one, got {event:?}"
    );

    handle.stop().await;
}
