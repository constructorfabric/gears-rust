#![cfg(feature = "integration")]
// The doubles, the fixture and the polls are plain helpers, which
// `allow-expect-in-tests` does not reach; a failed expectation there is the
// test failing, as in the sibling PostgreSQL suites.
#![allow(clippy::expect_used, clippy::unwrap_used)]
//! Two replicas of the gear over one `PostgreSQL`, joined by a delivering
//! broker: the Level 2 rows *Two-replica convergence* and *Dropped broadcast*
//! of DESIGN-activation §7.
//!
//! Each replica is a whole `SettingsService`: `init` over its own `ClientHub`,
//! then the managed lifecycle, so the end-of-request publish, the replica's
//! consumer and its cache are the production ones. What the test hosts is the
//! platform around them: the Event Broker (an in-memory `EventBrokerApi` that
//! fans every published event out to each joined group's open stream, with a
//! join barrier and a drop switch), the Types Registry (accepts what is
//! registered, serves the value type), the Tenant Resolver (a root with one
//! child) and the Credential Store the gear's init resolves.
//!
//! Docker required: without it the suite skips, unless
//! `SETTINGS_SERVICE_PG_REQUIRE_DOCKER=1` turns the skip into a failure, as CI
//! sets it. Run via `make test-integration LANE=settings-service-pg` or:
//!
//! ```sh
//! cargo nextest run -p cf-gears-settings-service --features integration --test pg_replica_coherence_test
//! ```

// @cpt-dod:cpt-cf-settings-service-dod-replica-coherence-tests:p1

mod support;

use std::time::{Duration, Instant};

use event_broker_sdk::Event;
use serde_json::json;
use settings_service_sdk::SettingKey;
use settings_service_sdk::models::ScopeClass;

use support::platform::{CHILD, Platform, ROOT, Replica, pg_fixture, platform};

/// How long a poll waits for a broadcast to converge a replica: far below the
/// default 30 s `cache_ttl_seconds`, so only the broadcast can explain a pass.
const CONVERGENCE_DEADLINE: Duration = Duration::from_secs(5);

fn setting(name: &str) -> SettingKey {
    SettingKey::contributed("cf", "demo", "coherence", name, std::num::NonZeroU32::MIN)
        .expect("a valid key")
}

/// The tenant and keys a `cache_invalidate` event names.
fn named(event: &Event) -> (String, Vec<String>) {
    let data = event.data.as_ref().expect("the event carries a body");
    let tenant = data["tenant"].as_str().expect("a tenant").to_owned();
    let keys = data["changed_keys"]
        .as_array()
        .expect("a key list")
        .iter()
        .map(|k| k.as_str().expect("a key").to_owned())
        .collect();
    (tenant, keys)
}

/// Both replicas are down and each left and deleted its one group.
async fn assert_clean_stop(platform: &Platform, replicas: &[&Replica]) {
    for replica in replicas {
        replica.stop().await;
    }
    let (created, joined, left, deleted, held) = platform.broker.lifecycle();
    assert_eq!(created, replicas.len(), "one group per replica");
    assert_eq!(joined, replicas.len(), "one join per replica");
    assert_eq!(left, replicas.len(), "each replica left its subscription");
    assert_eq!(deleted, replicas.len(), "each replica deleted its group");
    assert_eq!(held, 0, "the broker holds no group after the stop");
    assert_eq!(platform.broker.open_streams(), 0);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// *Two-replica convergence*: after a write on A, B stops serving the old
/// value long before `cache_ttl_seconds` (the default 30 s) could have
/// expired it, for a `local` key at the written tenant and for a `cascading`
/// key at the descendant. Only the broadcast can explain the pass.
#[tokio::test]
async fn a_write_on_one_replica_converges_the_other_within_the_poll_deadline() {
    let Some(pg) = pg_fixture("replica").await else {
        return;
    };
    let platform = platform();
    let a = Replica::init(&pg, &platform, json!({})).await;
    let b = Replica::init(&pg, &platform, json!({})).await;

    let cascading = setting("cascading_flag");
    let local = setting("local_flag");
    a.declare(&cascading, ScopeClass::Cascading, "v0").await;
    a.declare(&local, ScopeClass::Local, "v0").await;

    // The warm read: B holds the previous value in its cache.
    assert_eq!(b.read(&cascading, CHILD).await, "v0");
    assert_eq!(b.read(&local, ROOT).await, "v0");

    a.serve().await;
    b.serve().await;
    platform.broker.await_joined(2).await;
    platform.broker.forget_published();

    a.write_at_root(&cascading, "v1").await;
    let converged = b
        .serves(&cascading, CHILD, "v1", CONVERGENCE_DEADLINE)
        .await
        .expect("B serves the cascading value at the descendant");
    a.write_at_root(&local, "v1").await;
    let converged_local = b
        .serves(&local, ROOT, "v1", CONVERGENCE_DEADLINE)
        .await
        .expect("B serves the local value at the written tenant");
    eprintln!(
        "convergence: cascading {} ms, local {} ms",
        converged.as_millis(),
        converged_local.as_millis()
    );

    // One broadcast per write, under the root tenant, naming the key.
    let broadcasts = platform.broker.cache_invalidations();
    assert_eq!(broadcasts.len(), 2, "one cache_invalidate per write");
    for (event, key) in broadcasts.iter().zip([&cascading, &local]) {
        assert_eq!(event.tenant_id, ROOT, "published under the root tenant");
        let (tenant, keys) = named(event);
        assert_eq!(tenant, ROOT.to_string());
        assert_eq!(keys, vec![key.as_str().to_owned()]);
        assert!(
            event.data.as_ref().unwrap().get("value").is_none(),
            "identifiers only"
        );
    }
    assert!(
        platform.broker.delivered() >= 4,
        "each broadcast reached both replicas"
    );

    assert_clean_stop(&platform, &[&a, &b]).await;
}

/// *Dropped broadcast*: the broker accepts the publish and delivers it to
/// nobody. B still serves the previous value before its TTL elapses and the
/// committed one after it, so `cache_ttl_seconds` alone bounds the staleness.
// @cpt-dod:cpt-cf-settings-service-dod-replica-coherence-ttl-backstop:p1
#[tokio::test]
async fn a_dropped_broadcast_leaves_the_peer_stale_for_at_most_the_ttl() {
    const TTL: Duration = Duration::from_secs(2);
    /// What the TTL may overshoot by before the new value must be served.
    const MARGIN: Duration = Duration::from_secs(2);

    let Some(pg) = pg_fixture("replica").await else {
        return;
    };
    let platform = platform();
    let a = Replica::init(&pg, &platform, json!({})).await;
    let b = Replica::init(
        &pg,
        &platform,
        json!({ "cache_ttl_seconds": TTL.as_secs() }),
    )
    .await;

    let key = setting("dropped_flag");
    let warmup = setting("dropped_warmup");
    a.declare(&key, ScopeClass::Cascading, "v0").await;
    a.declare(&warmup, ScopeClass::Cascading, "w0").await;

    a.serve().await;
    b.serve().await;
    platform.broker.await_joined(2).await;
    // A's write path is warmed by one throwaway write of another key, so the
    // first write the clock below times is not also A's first (connection,
    // statements, the publisher's first broker session). Its broadcast is
    // delivered to both replicas before the counters are reset and the drop
    // switch goes on.
    a.write_at_root(&warmup, "w1").await;
    tokio::time::timeout(CONVERGENCE_DEADLINE, async {
        while platform.broker.delivered() < 2 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the warm-up broadcast reached both replicas");
    platform.broker.forget_published();
    platform.broker.drop_publishes(true);

    // The warm read starts B's TTL clock; the write follows at once.
    assert_eq!(b.read(&key, CHILD).await, "v0");
    let warmed = Instant::now();
    a.write_at_root(&key, "v1").await;

    // Before the TTL: the old value, from B's cache.
    let seen_before = b.read(&key, CHILD).await;
    let elapsed = warmed.elapsed();
    assert!(
        elapsed < TTL,
        "the write and the read took {elapsed:?}, not under the {TTL:?} TTL: \
         the stale read proves nothing"
    );
    assert_eq!(
        seen_before, "v0",
        "B serves the previous value before the TTL"
    );

    // After the TTL plus the margin: the committed value, with no broadcast.
    b.serves(&key, CHILD, "v1", (TTL + MARGIN).saturating_sub(elapsed))
        .await
        .expect("B serves the new value once its TTL elapsed");
    let converged_after = warmed.elapsed();
    eprintln!(
        "dropped broadcast: converged {} ms after the warm read",
        converged_after.as_millis()
    );
    assert!(
        converged_after + Duration::from_millis(100) >= TTL,
        "converged after {converged_after:?}, before the {TTL:?} TTL could expire the entry: \
         something delivered the broadcast"
    );

    // The broker saw the publish and discarded it.
    assert_eq!(platform.broker.cache_invalidations().len(), 1);
    assert_eq!(platform.broker.delivered(), 0, "nothing was delivered");

    assert_clean_stop(&platform, &[&a, &b]).await;
}
