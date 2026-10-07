//! Layer 3 — distributed-lock integration scenarios (docs/TESTING.md §4.4),
//! asserting on the actual `Lease` objects (holder token, ttl-ms annotation,
//! clear-not-delete on release) and cross-instance behaviour.

#![cfg(feature = "integration")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::large_futures,
    reason = "integration tests: a setup failure IS the test failure"
)]

use crate::common;

use std::sync::Arc;
use std::time::Duration;

use cluster_sdk::error::ClusterError;
use k8s_cluster_plugin::K8sLockPlugin;
use k8s_openapi::api::coordination::v1::Lease;
use kube::ResourceExt;
use serde_json::json;

const ANNOTATION_NAME: &str = "cluster.cf-gears.io/name";
const ANNOTATION_TTL_MS: &str = "cluster.cf-gears.io/ttl-ms";
const LABEL_PRIMITIVE: &str = "cluster.cf-gears.io/primitive";

/// The single lock `Lease` whose name annotation is `lock_name`, if present.
async fn lease_for(ns: &common::NamespaceGuard, lock_name: &str) -> Option<Lease> {
    ns.list_leases()
        .await
        .into_iter()
        .find(|l| l.annotations().get(ANNOTATION_NAME).map(String::as_str) == Some(lock_name))
}

fn holder_of(lease: &Lease) -> Option<String> {
    lease.spec.as_ref().and_then(|s| s.holder_identity.clone())
}

/// `K8S-LOCK-001`: `try_lock` acquires and `release` frees. The Lease holder is a
/// `<owner>#<fence>` token with the ttl-ms annotation; a second `try_lock` from
/// the same instance contends; after `release` the object still exists with a null
/// holder and is immediately re-acquirable (clear-not-delete).
#[tokio::test]
async fn k8s_lock_001_try_lock_acquires_and_release_frees() {
    let ns = common::fresh_namespace("lock-001").await;
    let handle = K8sLockPlugin::builder(ns.lock_config_with(json!({})))
        .with_client(ns.client.clone())
        .build_and_start()
        .await
        .expect("lock starts");
    let lock = handle.lock();

    let guard = lock
        .try_lock("res", Duration::from_secs(30))
        .await
        .expect("acquire");

    let lease = lease_for(&ns, "res").await.expect("lease exists");
    let holder = holder_of(&lease).expect("holder");
    assert!(
        holder.contains('#'),
        "K8S-LOCK-001: holder is an <owner>#<fence> token: {holder}"
    );
    assert!(
        lease.annotations().contains_key(ANNOTATION_TTL_MS),
        "K8S-LOCK-001: the ttl-ms annotation is present"
    );
    assert_eq!(
        lease.labels().get(LABEL_PRIMITIVE).map(String::as_str),
        Some("lock")
    );

    let contended = lock.try_lock("res", Duration::from_secs(30)).await;
    assert!(
        matches!(contended, Err(ClusterError::LockContended { .. })),
        "K8S-LOCK-001: a held lock contends even from the same instance, got {contended:?}"
    );

    guard.release().await.expect("release");
    let after = lease_for(&ns, "res")
        .await
        .expect("K8S-LOCK-001: the object persists after release");
    assert!(
        holder_of(&after).is_none(),
        "K8S-LOCK-001: release clears the holder, not the object"
    );
    lock.try_lock("res", Duration::from_secs(30))
        .await
        .expect("K8S-LOCK-001: immediately re-acquirable after release");

    handle.stop().await;
}

/// `K8S-LOCK-002`: a blocked `lock` returns `LockTimeout` (not `Provider`, not
/// `LockContended`) after the budget elapses, and leaves nothing behind.
#[tokio::test]
async fn k8s_lock_002_lock_times_out() {
    let ns = common::fresh_namespace("lock-002").await;
    let handle = K8sLockPlugin::builder(ns.lock_config_with(json!({})))
        .with_client(ns.client.clone())
        .build_and_start()
        .await
        .expect("lock starts");
    let lock = handle.lock();

    let held = lock
        .try_lock("res", Duration::from_secs(30))
        .await
        .expect("holder acquires");
    let outcome = lock
        .lock("res", Duration::from_secs(30), Duration::from_millis(500))
        .await;
    assert!(
        matches!(outcome, Err(ClusterError::LockTimeout { .. })),
        "K8S-LOCK-002: a blocked lock times out, got {outcome:?}"
    );

    held.release().await.expect("release");
    lock.try_lock("res", Duration::from_secs(30))
        .await
        .expect("K8S-LOCK-002: acquirable the moment the holder releases");

    handle.stop().await;
}

/// `K8S-LOCK-003`: a blocked `lock` wakes on an explicit release, far inside the
/// holder's TTL — a wake at ~one TTL would mean the watch notification was missed.
#[tokio::test]
async fn k8s_lock_003_lock_wakes_on_release() {
    let ns = common::fresh_namespace("lock-003").await;
    let handle = K8sLockPlugin::builder(ns.lock_config_with(json!({})))
        .with_client(ns.client.clone())
        .build_and_start()
        .await
        .expect("lock starts");
    let lock = handle.lock();

    let guard = lock
        .try_lock("res", Duration::from_secs(30))
        .await
        .expect("holder acquires");

    let waiter_lock = Arc::clone(&lock);
    let waiter = tokio::spawn(async move {
        let start = tokio::time::Instant::now();
        let result = waiter_lock
            .lock("res", Duration::from_secs(30), Duration::from_secs(20))
            .await;
        (start.elapsed(), result)
    });

    // Let the waiter genuinely block, then release.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        !waiter.is_finished(),
        "K8S-LOCK-003: the waiter must be blocked before the release"
    );
    guard.release().await.expect("release");

    let (elapsed, result) = waiter.await.expect("waiter task");
    assert!(
        result.is_ok(),
        "K8S-LOCK-003: the waiter acquires after release, got {result:?}"
    );
    assert!(
        elapsed < Duration::from_secs(3),
        "K8S-LOCK-003: woke on the release notification (~{elapsed:?}), not a TTL lapse"
    );

    handle.stop().await;
}

/// `K8S-LOCK-004`: an expired lease is reclaimed with no cooperation — A holds a 2s
/// lock and never renews; B acquires once A's `Observed` deadline passes, and A's
/// subsequent `renew` reports `LockExpired`.
#[tokio::test]
async fn k8s_lock_004_expired_lease_is_reclaimed() {
    let ns = common::fresh_namespace("lock-004").await;
    let handle = K8sLockPlugin::builder(ns.lock_config_with(json!({})))
        .with_client(ns.client.clone())
        .build_and_start()
        .await
        .expect("lock starts");
    let lock = handle.lock();

    let a_guard = lock
        .try_lock("res", Duration::from_secs(2))
        .await
        .expect("A acquires");

    // Once A's claim lapses, a fresh acquire succeeds by overwriting it.
    let reclaimed = common::wait_until(Duration::from_secs(6), Duration::from_millis(100), || {
        let lock = Arc::clone(&lock);
        async move { lock.try_lock("res", Duration::from_secs(30)).await.is_ok() }
    })
    .await;
    assert!(
        reclaimed,
        "K8S-LOCK-004: B reclaims the lapsed lock with no cooperation from A"
    );

    let renew = a_guard.renew(Duration::from_secs(2)).await;
    assert!(
        matches!(renew, Err(ClusterError::LockExpired { .. })),
        "K8S-LOCK-004: A's renew on a reclaimed lock reports LockExpired, got {renew:?}"
    );

    handle.stop().await;
}

/// `K8S-LOCK-005`: `renew` extends the lease — `renewTime` advances and both
/// `leaseDurationSeconds` and the ttl-ms annotation reflect the new TTL.
#[tokio::test]
async fn k8s_lock_005_renew_extends_the_lease() {
    let ns = common::fresh_namespace("lock-005").await;
    let handle = K8sLockPlugin::builder(ns.lock_config_with(json!({})))
        .with_client(ns.client.clone())
        .build_and_start()
        .await
        .expect("lock starts");
    let lock = handle.lock();

    let guard = lock
        .try_lock("res", Duration::from_secs(5))
        .await
        .expect("acquire");
    let before = lease_for(&ns, "res").await.expect("lease");
    let renew0 = before.spec.as_ref().unwrap().renew_time.clone();

    tokio::time::sleep(Duration::from_millis(50)).await;
    guard.renew(Duration::from_secs(20)).await.expect("renew");

    let after = lease_for(&ns, "res").await.expect("lease");
    let spec = after.spec.as_ref().unwrap();
    assert_ne!(renew0, spec.renew_time, "K8S-LOCK-005: renewTime advanced");
    assert_eq!(
        spec.lease_duration_seconds,
        Some(20),
        "K8S-LOCK-005: leaseDurationSeconds is the new TTL"
    );
    assert_eq!(
        after
            .annotations()
            .get(ANNOTATION_TTL_MS)
            .map(String::as_str),
        Some("20000"),
        "K8S-LOCK-005: the ttl-ms annotation reflects the new TTL"
    );

    guard.release().await.expect("release");
    handle.stop().await;
}

/// `K8S-LOCK-006`: `renew` and `release` are token-fenced — A's lease lapses and B
/// acquires the same name; A's `renew` is `LockExpired`, A's `release` is `Ok(())`
/// that issues no write and leaves B's holder intact.
#[tokio::test]
async fn k8s_lock_006_renew_and_release_are_token_fenced() {
    let ns = common::fresh_namespace("lock-006").await;
    let handle = K8sLockPlugin::builder(ns.lock_config_with(json!({})))
        .with_client(ns.client.clone())
        .build_and_start()
        .await
        .expect("lock starts");
    let lock = handle.lock();

    let a_guard = lock
        .try_lock("res", Duration::from_secs(2))
        .await
        .expect("A acquires");

    // Wait out A's claim, then B takes it.
    let b_guard = loop {
        tokio::time::sleep(Duration::from_millis(150)).await;
        if let Ok(g) = lock.try_lock("res", Duration::from_secs(30)).await {
            break g;
        }
    };
    let b_holder = holder_of(&lease_for(&ns, "res").await.unwrap()).unwrap();

    // A's renew is fenced out; A's release writes nothing and leaves B intact.
    assert!(
        matches!(
            a_guard.renew(Duration::from_secs(2)).await,
            Err(ClusterError::LockExpired { .. })
        ),
        "K8S-LOCK-006: A's renew is fenced"
    );
    let rv_before = lease_for(&ns, "res").await.unwrap().resource_version();
    a_guard
        .release()
        .await
        .expect("K8S-LOCK-006: A's release is Ok(()) even fenced out");
    let after = lease_for(&ns, "res").await.expect("lease still present");
    assert_eq!(
        holder_of(&after),
        Some(b_holder),
        "K8S-LOCK-006: B's holder is intact"
    );
    assert_eq!(
        after.resource_version(),
        rv_before,
        "K8S-LOCK-006: A's fenced release issued no write"
    );

    b_guard.release().await.expect("release");
    handle.stop().await;
}

/// `K8S-LOCK-007`: 20 concurrent local acquirers, at most one holder — exactly one
/// succeeds, 19 get `LockContended`.
#[tokio::test]
async fn k8s_lock_007_concurrent_local_acquirers() {
    let ns = common::fresh_namespace("lock-007").await;
    let handle = K8sLockPlugin::builder(ns.lock_config_with(json!({})))
        .with_client(ns.client.clone())
        .build_and_start()
        .await
        .expect("lock starts");
    let lock = handle.lock();

    let mut tasks = Vec::new();
    for _ in 0..20 {
        let lock = Arc::clone(&lock);
        tasks.push(tokio::spawn(async move {
            lock.try_lock("res", Duration::from_secs(30)).await
        }));
    }
    let mut wins = 0;
    let mut contended = 0;
    let mut guards = Vec::new();
    for t in tasks {
        match t.await.expect("task") {
            Ok(g) => {
                wins += 1;
                guards.push(g);
            }
            Err(ClusterError::LockContended { .. }) => contended += 1,
            Err(other) => panic!("K8S-LOCK-007: unexpected error {other:?}"),
        }
    }
    assert_eq!(wins, 1, "K8S-LOCK-007: exactly one local acquirer wins");
    assert_eq!(contended, 19, "K8S-LOCK-007: nineteen contend");

    handle.stop().await;
}

/// `K8S-LOCK-008`: two independent instances cannot hold the same lock — A acquires,
/// B contends, exactly one Lease exists with A's holder, and B acquires once A
/// releases.
#[tokio::test]
async fn k8s_lock_008_two_instances_cannot_both_hold() {
    let ns = common::fresh_namespace("lock-008").await;
    let a = K8sLockPlugin::builder(ns.lock_config_with(json!({})))
        .with_client(ns.client.clone())
        .build_and_start()
        .await
        .expect("A starts");
    let b = K8sLockPlugin::builder(ns.lock_config_with(json!({})))
        .with_client(ns.client.clone())
        .build_and_start()
        .await
        .expect("B starts");

    let a_guard = a
        .lock()
        .try_lock("res", Duration::from_secs(30))
        .await
        .expect("A acquires");
    let contended = b.lock().try_lock("res", Duration::from_secs(30)).await;
    assert!(
        matches!(contended, Err(ClusterError::LockContended { .. })),
        "K8S-LOCK-008: B cannot take A's lock, got {contended:?}"
    );
    assert_eq!(ns.list_leases().await.len(), 1, "K8S-LOCK-008: one Lease");

    a_guard.release().await.expect("A releases");
    let b_guard = common::wait_until(Duration::from_secs(5), Duration::from_millis(50), || {
        let b = b.lock();
        async move { b.try_lock("res", Duration::from_secs(30)).await.is_ok() }
    })
    .await;
    assert!(b_guard, "K8S-LOCK-008: B acquires once A releases");

    a.stop().await;
    b.stop().await;
}

/// `K8S-LOCK-009`: sub-second TTLs are honoured at millisecond precision — a 300ms
/// lock is re-acquirable ~300ms later (not 1s), while `leaseDurationSeconds` reads 1.
#[tokio::test]
async fn k8s_lock_009_sub_second_ttls_at_ms_precision() {
    let ns = common::fresh_namespace("lock-009").await;
    let handle = K8sLockPlugin::builder(ns.lock_config_with(json!({})))
        .with_client(ns.client.clone())
        .build_and_start()
        .await
        .expect("lock starts");
    let lock = handle.lock();

    let guard = lock
        .try_lock("res", Duration::from_millis(300))
        .await
        .expect("acquire");
    std::mem::forget(guard); // never released or renewed: only the TTL can free it.

    let lease = lease_for(&ns, "res").await.expect("lease");
    assert_eq!(
        lease.spec.as_ref().unwrap().lease_duration_seconds,
        Some(1),
        "K8S-LOCK-009: leaseDurationSeconds rounds up to 1 while the real TTL is sub-second"
    );

    let start = tokio::time::Instant::now();
    let reacquired = common::wait_until(
        Duration::from_millis(900),
        Duration::from_millis(20),
        || {
            let lock = Arc::clone(&lock);
            async move { lock.try_lock("res", Duration::from_secs(30)).await.is_ok() }
        },
    )
    .await;
    assert!(
        reacquired,
        "K8S-LOCK-009: re-acquirable well under a second"
    );
    assert!(
        start.elapsed() < Duration::from_millis(900),
        "K8S-LOCK-009: re-acquired at ~300ms, not the 1s the object's duration would imply"
    );

    handle.stop().await;
}

/// `K8S-LOCK-011`: `lock()` after `stop()` answers `Shutdown` immediately, rather
/// than retrying a torn-down backend for its whole budget.
#[tokio::test]
async fn k8s_lock_011_lock_after_stop_is_shutdown() {
    let ns = common::fresh_namespace("lock-011").await;
    let handle = K8sLockPlugin::builder(ns.lock_config_with(json!({})))
        .with_client(ns.client.clone())
        .build_and_start()
        .await
        .expect("lock starts");
    let lock = handle.lock();
    handle.stop().await;

    let start = tokio::time::Instant::now();
    let outcome = lock
        .lock("res", Duration::from_secs(30), Duration::from_secs(30))
        .await;
    assert!(
        matches!(outcome, Err(ClusterError::Shutdown)),
        "K8S-LOCK-011: lock after stop is Shutdown, got {outcome:?}"
    );
    assert!(
        start.elapsed() < Duration::from_secs(1),
        "K8S-LOCK-011: answered immediately"
    );

    let try_outcome = lock.try_lock("res", Duration::from_secs(30)).await;
    assert!(
        matches!(try_outcome, Err(ClusterError::Shutdown)),
        "try_lock after stop is Shutdown too"
    );
}

/// `K8S-LOCK-012`: `stop()` leaves held claims to lapse, and says so — three held
/// Leases are still present with our holder after stop, and become acquirable once
/// the deadline passes (no best-effort remote cleanup on shutdown).
#[tokio::test]
async fn k8s_lock_012_stop_leaves_claims_to_lapse() {
    let ns = common::fresh_namespace("lock-012").await;
    let handle = K8sLockPlugin::builder(ns.lock_config_with(json!({})))
        .with_client(ns.client.clone())
        .build_and_start()
        .await
        .expect("lock starts");
    let lock = handle.lock();

    let mut guards = Vec::new();
    for name in ["a", "b", "c"] {
        guards.push(
            lock.try_lock(name, Duration::from_secs(2))
                .await
                .expect("acquire"),
        );
    }
    std::mem::forget(guards); // simulate a crash: no voluntary release.

    handle.stop().await;

    // The three Leases persist with our holder immediately after stop.
    let leases = ns.list_leases().await;
    assert_eq!(
        leases.len(),
        3,
        "K8S-LOCK-012: held Leases are left behind, not cleaned up"
    );
    assert!(
        leases
            .iter()
            .all(|l| holder_of(l).is_some_and(|h| h.contains('#'))),
        "K8S-LOCK-012: each still carries our holder token"
    );

    // Once the deadline passes, the names are acquirable by a fresh instance.
    let fresh = K8sLockPlugin::builder(ns.lock_config_with(json!({})))
        .with_client(ns.client.clone())
        .build_and_start()
        .await
        .expect("fresh instance starts");
    let acquired = common::wait_until(Duration::from_secs(5), Duration::from_millis(100), || {
        let lock = fresh.lock();
        async move { lock.try_lock("a", Duration::from_secs(30)).await.is_ok() }
    })
    .await;
    assert!(
        acquired,
        "K8S-LOCK-012: the lapsed claim becomes acquirable once its deadline passes"
    );

    fresh.stop().await;
}

/// `K8S-LOCK-013`: released lock objects are reaped, held ones are not — with
/// `lock_object_retention_ms` lowered, a released lock's empty Lease is deleted by
/// the reaper while a held lock's Lease of the same age is untouched.
#[tokio::test]
async fn k8s_lock_013_released_objects_are_reaped_held_are_not() {
    let ns = common::fresh_namespace("lock-013").await;
    let handle = K8sLockPlugin::builder(ns.lock_config_with(json!({
        "reaper_interval_ms": 500,
        "lock_object_retention_ms": 500
    })))
    .with_client(ns.client.clone())
    .build_and_start()
    .await
    .expect("lock starts");
    let lock = handle.lock();

    // "released" is acquired then released (empty holder, subject to reaping);
    // "held" stays held (never reaped regardless of age).
    let released = lock
        .try_lock("released", Duration::from_secs(30))
        .await
        .expect("acquire");
    released.release().await.expect("release");
    let _held = lock
        .try_lock("held", Duration::from_secs(100))
        .await
        .expect("acquire");

    let reaped = common::wait_until(
        Duration::from_secs(8),
        Duration::from_millis(200),
        || async { lease_for(&ns, "released").await.is_none() },
    )
    .await;
    assert!(
        reaped,
        "K8S-LOCK-013: the released (empty) Lease is reaped past retention"
    );
    assert!(
        lease_for(&ns, "held").await.is_some(),
        "K8S-LOCK-013: a held Lease of the same age is never reaped"
    );

    handle.stop().await;
}

/// `K8S-LOCK-016` (mirrors redis `RD-LOCK-016`): the store-owned-leases token path.
/// `acquire` hands back a `LeaseToken` (not a guard), whose Lease holder is the
/// `<owner>#<fence>` string; a second `acquire` contends against the live lease;
/// `renew` extends it; `release` clears the holder (not the object) and the name is
/// immediately re-acquirable.
#[tokio::test]
async fn k8s_lock_016_acquire_renew_contend_release_token_path() {
    let ns = common::fresh_namespace("lock-016").await;
    let handle = K8sLockPlugin::builder(ns.lock_config_with(json!({})))
        .with_client(ns.client.clone())
        .build_and_start()
        .await
        .expect("lock starts");
    let lock = handle.lock();

    let token = lock
        .acquire("res", "owner-a", Duration::from_secs(30))
        .await
        .expect("acquire");
    assert_eq!(token.name, "res");
    assert_eq!(token.owner, "owner-a");
    let want = format!("owner-a#{}", token.fence);
    let lease = lease_for(&ns, "res").await.expect("lease exists");
    assert_eq!(
        holder_of(&lease).as_deref(),
        Some(want.as_str()),
        "K8S-LOCK-016: the Lease holder is <owner>#<fence>"
    );

    let contended = lock
        .acquire("res", "owner-b", Duration::from_secs(30))
        .await;
    assert!(
        matches!(contended, Err(ClusterError::LockContended { .. })),
        "K8S-LOCK-016: a live lease contends any acquirer, got {contended:?}"
    );

    lock.renew(&token, Duration::from_secs(30))
        .await
        .expect("K8S-LOCK-016: renew holds the lease");
    assert_eq!(
        lease_for(&ns, "res")
            .await
            .and_then(|l| holder_of(&l))
            .as_deref(),
        Some(want.as_str()),
        "K8S-LOCK-016: renew keeps our holder"
    );

    lock.release(&token)
        .await
        .expect("K8S-LOCK-016: release frees the lease");
    let after = lease_for(&ns, "res")
        .await
        .expect("K8S-LOCK-016: the object persists after release");
    assert!(
        holder_of(&after).is_none(),
        "K8S-LOCK-016: release clears the holder, not the object"
    );
    lock.acquire("res", "owner-c", Duration::from_secs(30))
        .await
        .expect("K8S-LOCK-016: immediately re-acquirable after release");

    handle.stop().await;
}

/// `K8S-LOCK-017` (mirrors redis `RD-LOCK-017`): fencing across a re-claim. A
/// re-acquired name draws a fresh fence, so the previous holder's token is fenced
/// out — its `renew` reports `LockExpired` and its `release` is idempotent by
/// absence, leaving the successor's lease intact.
#[tokio::test]
async fn k8s_lock_017_stale_token_is_fenced_after_reclaim() {
    let ns = common::fresh_namespace("lock-017").await;
    let handle = K8sLockPlugin::builder(ns.lock_config_with(json!({})))
        .with_client(ns.client.clone())
        .build_and_start()
        .await
        .expect("lock starts");
    let lock = handle.lock();

    let ttl = Duration::from_secs(30);
    let a = lock
        .acquire("res", "owner-a", ttl)
        .await
        .expect("a acquires");
    lock.release(&a).await.expect("a releases");
    let b = lock
        .acquire("res", "owner-b", ttl)
        .await
        .expect("b re-acquires");

    let stale_renew = lock.renew(&a, ttl).await;
    assert!(
        matches!(stale_renew, Err(ClusterError::LockExpired { .. })),
        "K8S-LOCK-017: a fenced-out token cannot renew, got {stale_renew:?}"
    );
    lock.release(&a)
        .await
        .expect("K8S-LOCK-017: a stale release is idempotent by absence");
    let want_b = format!("owner-b#{}", b.fence);
    assert_eq!(
        lease_for(&ns, "res")
            .await
            .and_then(|l| holder_of(&l))
            .as_deref(),
        Some(want_b.as_str()),
        "K8S-LOCK-017: the stale release left the successor's lease untouched"
    );
    lock.renew(&b, ttl)
        .await
        .expect("K8S-LOCK-017: the live holder still renews its own lease");

    handle.stop().await;
}

/// `K8S-LOCK-018` (mirrors redis `RD-LOCK-018`): the blocking token acquire.
/// `acquire_waiting` times out while another owner holds the lease, then wins
/// promptly once it is released.
#[tokio::test]
async fn k8s_lock_018_acquire_waiting_blocks_then_wins() {
    let ns = common::fresh_namespace("lock-018").await;
    let handle = K8sLockPlugin::builder(ns.lock_config_with(json!({})))
        .with_client(ns.client.clone())
        .build_and_start()
        .await
        .expect("lock starts");
    let lock = handle.lock();

    let held = lock
        .acquire("res", "owner-a", Duration::from_secs(30))
        .await
        .expect("a holds");
    let timed = lock
        .acquire_waiting(
            "res",
            "owner-b",
            Duration::from_secs(30),
            Duration::from_millis(500),
        )
        .await;
    assert!(
        matches!(timed, Err(ClusterError::LockTimeout { .. })),
        "K8S-LOCK-018: a blocked acquire times out, got {timed:?}"
    );

    lock.release(&held).await.expect("release");
    let b = lock
        .acquire_waiting(
            "res",
            "owner-b",
            Duration::from_secs(30),
            Duration::from_secs(5),
        )
        .await
        .expect("K8S-LOCK-018: acquirable the moment the holder releases");
    assert_eq!(b.owner, "owner-b");

    handle.stop().await;
}

/// `K8S-LOCK-019`: the guard path and the token path are one lease fenced one way.
/// A token-held lease contends a `try_lock` guard; a guard-held lease is not
/// releasable by a stale token; both fence on the same `<owner>#<fence>` value.
#[tokio::test]
async fn k8s_lock_019_guard_and_token_paths_share_one_lease() {
    let ns = common::fresh_namespace("lock-019").await;
    let handle = K8sLockPlugin::builder(ns.lock_config_with(json!({})))
        .with_client(ns.client.clone())
        .build_and_start()
        .await
        .expect("lock starts");
    let lock = handle.lock();

    let ttl = Duration::from_secs(30);
    // A token-held lease contends the guard path.
    let token = lock
        .acquire("res", "owner-a", ttl)
        .await
        .expect("token acquire");
    let contended = lock.try_lock("res", ttl).await;
    assert!(
        matches!(contended, Err(ClusterError::LockContended { .. })),
        "K8S-LOCK-019: a token-held lease contends try_lock, got {contended:?}"
    );

    // Released, the guard path takes the freed name; the stale token cannot release
    // the guard's lease (a different fence).
    lock.release(&token).await.expect("release token");
    let guard = lock
        .try_lock("res", ttl)
        .await
        .expect("K8S-LOCK-019: guard acquires the freed name");
    lock.release(&token)
        .await
        .expect("K8S-LOCK-019: a stale token release is idempotent, never the guard's");
    assert!(
        lease_for(&ns, "res")
            .await
            .and_then(|l| holder_of(&l))
            .is_some(),
        "K8S-LOCK-019: the guard still holds the lease after the stale release"
    );
    guard.release().await.expect("guard releases");

    handle.stop().await;
}

/// `K8S-LOCK-020`: pod-agnostic token portability — the property the store-owned-lease
/// half exists for (invariant I7). A `LeaseToken` minted by one backend instance is
/// renewed **and** released by a second, fully independent instance (a different
/// pod/replica that never saw the acquire), against the same namespace. Nothing
/// process-local crosses the boundary: the token's `owner`+`fence` alone reconstruct
/// the stored `holderIdentity` the second instance fences on. This is the k8s analogue
/// of the redis/postgres cross-replica guarantee, on the token path the cluster gear
/// serves every remote lock RPC through.
#[tokio::test]
async fn k8s_lock_020_a_token_renews_and_releases_from_another_instance() {
    let ns = common::fresh_namespace("lock-020").await;
    // Two independent lock backends over one namespace — two replicas of one deployment.
    let handle_a = K8sLockPlugin::builder(ns.lock_config_with(json!({})))
        .with_client(ns.client.clone())
        .build_and_start()
        .await
        .expect("instance A starts");
    let handle_b = K8sLockPlugin::builder(ns.lock_config_with(json!({})))
        .with_client(ns.client.clone())
        .build_and_start()
        .await
        .expect("instance B starts");
    let lock_a = handle_a.lock();
    let lock_b = handle_b.lock();

    // A mints the token; B never saw this call.
    let token = lock_a
        .acquire("res", "svc-a", Duration::from_secs(30))
        .await
        .expect("A acquires on the token path");

    // B renews the very lease A acquired, from the token alone (I7).
    lock_b
        .renew(&token, Duration::from_secs(30))
        .await
        .expect("K8S-LOCK-020: B renews A's lease via the token, having never seen the acquire");
    let want = format!("svc-a#{}", token.fence);
    assert_eq!(
        lease_for(&ns, "res")
            .await
            .and_then(|l| holder_of(&l))
            .as_deref(),
        Some(want.as_str()),
        "K8S-LOCK-020: the lease is still held under the same holder after B's renew"
    );
    // The lease is genuinely held cluster-wide: a fresh acquire on B still contends.
    assert!(
        matches!(
            lock_b
                .acquire("res", "svc-b", Duration::from_secs(30))
                .await,
            Err(ClusterError::LockContended { .. })
        ),
        "K8S-LOCK-020: the token-held lease contends acquisition from any instance"
    );

    // B releases the token; the name is then free for anyone.
    lock_b
        .release(&token)
        .await
        .expect("K8S-LOCK-020: B releases A's lease via the token");
    assert!(
        lease_for(&ns, "res")
            .await
            .and_then(|l| holder_of(&l))
            .is_none(),
        "K8S-LOCK-020: B's cross-instance release cleared the holder cluster-wide"
    );
    lock_a
        .acquire("res", "svc-c", Duration::from_secs(30))
        .await
        .expect("K8S-LOCK-020: re-acquirable after the cross-instance release");

    handle_a.stop().await;
    handle_b.stop().await;
}
