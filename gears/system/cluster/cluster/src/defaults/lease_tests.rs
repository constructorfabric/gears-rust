//! Tests for the store-owned lease algebra (§5.8.1) — the properties item `L1`
//! exists to establish.
//!
//! The load-bearing ones are the cross-handle tests: they drive the lease through
//! **two independently constructed backends over one cache**, which is the
//! in-process stand-in for two cluster replicas over one backing store. If a lease
//! operation needed anything the acquiring handle remembered, these are the tests
//! that would fail (invariant I7).

use std::sync::Arc;
use std::time::Duration;

use super::{Acquisition, CacheLeaseStore};
use crate::defaults::test_cache::MemoryCache;
use cluster_sdk::cache::ClusterCacheBackend;
use cluster_sdk::cache::types::{PutRequest, Ttl};
use cluster_sdk::error::ClusterError;
use cluster_sdk::lease::{LeaseRecord, LeaseToken};

const KEY: &str = "lock/ledger";
const NAME: &str = "ledger";
const TTL: Duration = Duration::from_secs(30);

fn store(cache: &Arc<MemoryCache>) -> CacheLeaseStore {
    CacheLeaseStore::new(Arc::clone(cache) as Arc<dyn ClusterCacheBackend>)
}

/// A store with an explicit fence-retention window, for the `L3` tests that turn
/// the window into the variable.
fn store_retaining(cache: &Arc<MemoryCache>, retention: Duration) -> CacheLeaseStore {
    CacheLeaseStore::with_retention(Arc::clone(cache) as Arc<dyn ClusterCacheBackend>, retention)
}

async fn acquired(store: &CacheLeaseStore, owner: &str, ttl: Duration) -> LeaseToken {
    match store.try_acquire(KEY, NAME, owner, ttl).await {
        Ok(Acquisition::Acquired(token)) => token,
        Ok(Acquisition::Contended { .. }) => panic!("expected to acquire, but contended"),
        Err(err) => panic!("expected to acquire, got {err:?}"),
    }
}

async fn record_at(cache: &Arc<MemoryCache>, key: &str) -> LeaseRecord {
    let Ok(Some(entry)) = cache.get(key).await else {
        panic!("expected a record at {key}");
    };
    LeaseRecord::decode(&entry.value).expect("the record must be decodable")
}

// ---------------------------------------------------------------------------
// Acquisition and the fence
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_fresh_acquisition_writes_owner_deadline_and_fence() {
    let cache = MemoryCache::linearizable();
    let leases = store(&cache);
    let token = acquired(&leases, "owner-a", TTL).await;
    assert_eq!(token.name, NAME, "the token names the lease, not the key");
    assert_eq!(token.owner, "owner-a");
    assert_eq!(token.fence, 1, "the first acquisition of a name is fence 1");

    let record = record_at(&cache, KEY).await;
    assert_eq!(record.owner, "owner-a");
    assert_eq!(record.fence, 1);
    assert!(
        leases.is_live(&record),
        "a just-acquired lease must not be lapsed"
    );
}

#[tokio::test]
async fn a_live_lease_contends_and_reports_when_it_lapses() {
    let cache = MemoryCache::linearizable();
    let leases = store(&cache);
    let _held = acquired(&leases, "owner-a", TTL).await;
    let Ok(Acquisition::Contended { lapse_in }) =
        leases.try_acquire(KEY, NAME, "owner-b", TTL).await
    else {
        panic!("a live lease must contend");
    };
    let lapse_in = lapse_in.expect("a readable live record must report its remaining lifetime");
    assert!(
        lapse_in <= TTL && lapse_in > TTL.saturating_sub(Duration::from_secs(1)),
        "the reported lapse must be about the full TTL, got {lapse_in:?}"
    );
}

#[tokio::test]
async fn the_same_owner_contends_with_its_own_live_lease() {
    // Re-entrant acquisition has always contended, and store-owned leases keep it
    // that way: the predicate is over liveness, not over who is asking.
    let cache = MemoryCache::linearizable();
    let leases = store(&cache);
    let _held = acquired(&leases, "owner-a", TTL).await;
    assert!(matches!(
        leases.try_acquire(KEY, NAME, "owner-a", TTL).await,
        Ok(Acquisition::Contended { .. })
    ));
}

#[tokio::test(start_paused = true)]
async fn acquiring_an_expired_lease_strictly_increases_the_fence() {
    // `L1` exit criterion: acquisition of an expired lease strictly increases
    // `fence`. This is the property that makes a steal safe.
    let cache = MemoryCache::linearizable();
    let leases = store(&cache);
    let first = acquired(&leases, "owner-a", Duration::from_secs(5)).await;
    assert_eq!(first.fence, 1);

    tokio::time::advance(Duration::from_secs(6)).await;
    let second = acquired(&leases, "owner-b", Duration::from_secs(5)).await;
    assert!(
        second.fence > first.fence,
        "a stolen lease must fence its predecessor: {} !> {}",
        second.fence,
        first.fence
    );
    assert_eq!(second.fence, 2);

    tokio::time::advance(Duration::from_secs(6)).await;
    let third = acquired(&leases, "owner-c", Duration::from_secs(5)).await;
    assert_eq!(third.fence, 3, "the counter keeps climbing across owners");
}

#[tokio::test(start_paused = true)]
async fn the_fence_survives_re_acquisition_by_the_same_owner() {
    // The regression the fence-in-the-value rule exists to prevent: `CacheEntry`
    // versions reset to 1 on a delete-then-insert, so a same-owner re-acquisition
    // would otherwise hand the old token a matching predicate again (§5.8.1).
    let cache = MemoryCache::linearizable();
    let leases = store(&cache);
    let stale = acquired(&leases, "owner-a", Duration::from_secs(5)).await;
    tokio::time::advance(Duration::from_secs(6)).await;
    let fresh = acquired(&leases, "owner-a", Duration::from_secs(5)).await;

    assert!(fresh.fence > stale.fence);
    assert!(
        matches!(
            leases.renew(KEY, &stale, Duration::from_secs(5)).await,
            Err(ClusterError::LockExpired { .. })
        ),
        "the superseded token must not renew the lease that replaced it"
    );
}

#[tokio::test(start_paused = true)]
async fn the_record_outlives_the_lease_it_fenced() {
    let cache = MemoryCache::linearizable();
    let leases = store(&cache);
    let _lapsed = acquired(&leases, "owner-a", Duration::from_secs(5)).await;
    tokio::time::advance(Duration::from_mins(1)).await;

    let record = record_at(&cache, KEY).await;
    assert!(
        !leases.is_live(&record),
        "the lease has lapsed on the store's clock"
    );
    assert_eq!(
        record.fence, 1,
        "yet the record - and so the fence - is still there, which is what \
         `fence_retention` buys"
    );
}

/// `L3`'s exit criterion, stated as a test: a lease that lapses, is left long
/// enough that a TTL sweep has certainly run over it, and is then re-acquired **by
/// the same owner** gets a strictly greater fence.
///
/// The same-owner part is the whole point. A different owner is fenced by the
/// owner column alone; it is the *same* one that a reset counter would hand a
/// matching predicate, because its stale token carries the identity that survives.
///
/// The sweeper here is real (`MemoryCache` runs one on a 25 ms interval) and time
/// is paused, so the minute advanced below is dozens of sweeps, not an assumption
/// that none ran.
#[tokio::test(start_paused = true)]
async fn the_fence_climbs_across_a_lapse_a_sweep_and_the_same_owner() {
    let cache = MemoryCache::linearizable();
    let leases = store(&cache);

    let stale = acquired(&leases, "owner-a", Duration::from_secs(5)).await;
    assert_eq!(stale.fence, 1);

    tokio::time::advance(Duration::from_mins(1)).await;

    let fresh = acquired(&leases, "owner-a", Duration::from_secs(5)).await;
    assert!(
        fresh.fence > stale.fence,
        "the same owner re-acquiring after a lapse must be fenced against its own \
         stale token: {} !> {}",
        fresh.fence,
        stale.fence
    );

    // And the stale token is inert against the lease it used to be: same name,
    // same owner, older fence.
    let err = leases
        .renew(KEY, &stale, Duration::from_secs(5))
        .await
        .expect_err("a stale token must not renew the lease that replaced it");
    assert!(matches!(err, ClusterError::LockExpired { .. }), "{err:?}");
}

/// The negative control, and the reason the window is a window rather than a
/// constant `true`: shorten it below the elapsed time and the record really is
/// reaped, the counter really does restart at 1, and the stale token really does
/// match again.
///
/// This is the pre-`L3` behaviour reproduced on demand — which is what makes the
/// test above a statement about the retention window rather than about the
/// `MemoryCache` sweeper happening not to run.
#[tokio::test(start_paused = true)]
async fn a_window_shorter_than_the_lapse_lets_the_fence_reset() {
    let cache = MemoryCache::linearizable();
    let leases = store_retaining(&cache, Duration::from_secs(5));

    let stale = acquired(&leases, "owner-a", Duration::from_secs(5)).await;
    assert_eq!(stale.fence, 1);

    // Past the lease (5s) *and* past its retention (5s more), so the physical TTL
    // has expired and the sweeper has had dozens of chances at it.
    tokio::time::advance(Duration::from_mins(1)).await;
    assert!(
        cache
            .get(KEY)
            .await
            .expect("the cache read must succeed")
            .is_none(),
        "the record must be physically gone once its window has also passed"
    );

    let fresh = acquired(&leases, "owner-a", Duration::from_secs(5)).await;
    assert_eq!(
        fresh.fence, 1,
        "with no record left there is no counter to carry, so acquisition starts over"
    );
    // The consequence, stated rather than implied: the stale token now matches.
    leases
        .renew(KEY, &stale, Duration::from_secs(5))
        .await
        .expect("this is the defect the window exists to close");
}

/// A lease taken for longer than the window meant to outlive it warns, once,
/// naming both durations - §5.8.1's "shorter than the longest lease TTL" check,
/// made where a lease TTL actually exists.
#[tokio::test(start_paused = true)]
#[tracing_test::traced_test]
async fn a_ttl_at_or_over_the_window_warns_once() {
    let cache = MemoryCache::linearizable();
    let leases = store_retaining(&cache, Duration::from_secs(10));

    let _first = acquired(&leases, "owner-a", Duration::from_secs(30)).await;
    assert!(
        logs_contain("lease TTL is at least the fence retention window"),
        "the first over-long acquisition must warn"
    );

    // A hot lock acquires at rate; the warning must not.
    tokio::time::advance(Duration::from_mins(1)).await;
    let _second = acquired(&leases, "owner-a", Duration::from_secs(30)).await;
    logs_assert(|lines: &[&str]| {
        let warned = lines
            .iter()
            .filter(|line| line.contains("lease TTL is at least the fence retention window"))
            .count();
        if warned == 1 {
            Ok(())
        } else {
            Err(format!("expected exactly one warning, saw {warned}"))
        }
    });
}

// ---------------------------------------------------------------------------
// Renew: the predicate is a property of the record
// ---------------------------------------------------------------------------

#[tokio::test]
async fn renew_extends_the_deadline_of_a_live_lease() {
    let cache = MemoryCache::linearizable();
    let leases = store(&cache);
    let token = acquired(&leases, "owner-a", Duration::from_secs(5)).await;
    let before = record_at(&cache, KEY).await.deadline_ms;
    assert!(
        leases
            .renew(KEY, &token, Duration::from_mins(1))
            .await
            .is_ok()
    );
    let after = record_at(&cache, KEY).await;
    assert!(
        after.deadline_ms > before,
        "renewing to a longer TTL must push the deadline out"
    );
    assert_eq!(after.fence, token.fence, "renewal never moves the fence");
    assert_eq!(after.owner, token.owner);
}

#[tokio::test]
async fn renew_rejects_a_foreign_owner() {
    let cache = MemoryCache::linearizable();
    let leases = store(&cache);
    let token = acquired(&leases, "owner-a", TTL).await;
    let impostor = LeaseToken::new(&token.name, "owner-b", token.fence);
    assert!(matches!(
        leases.renew(KEY, &impostor, TTL).await,
        Err(ClusterError::LockExpired { name }) if name == NAME
    ));
}

#[tokio::test]
async fn renew_rejects_a_stale_fence() {
    let cache = MemoryCache::linearizable();
    let leases = store(&cache);
    let token = acquired(&leases, "owner-a", TTL).await;
    let stale = LeaseToken::new(&token.name, &token.owner, token.fence - 1);
    assert!(matches!(
        leases.renew(KEY, &stale, TTL).await,
        Err(ClusterError::LockExpired { .. })
    ));
}

#[tokio::test(start_paused = true)]
async fn renew_rejects_a_lapsed_deadline() {
    let cache = MemoryCache::linearizable();
    let leases = store(&cache);
    let token = acquired(&leases, "owner-a", Duration::from_secs(5)).await;
    tokio::time::advance(Duration::from_secs(6)).await;
    // Nobody stole it — the record is still this owner's, at the same fence. The
    // deadline alone is what refuses the renewal.
    let record = record_at(&cache, KEY).await;
    assert!(record.matches(&token));
    assert!(matches!(
        leases.renew(KEY, &token, Duration::from_secs(5)).await,
        Err(ClusterError::LockExpired { .. })
    ));
}

#[tokio::test]
async fn renew_of_an_absent_lease_is_expired_not_a_silent_recreate() {
    let cache = MemoryCache::linearizable();
    let leases = store(&cache);
    let token = acquired(&leases, "owner-a", TTL).await;
    assert!(leases.release(KEY, &token).await.is_ok());
    assert!(matches!(
        leases.renew(KEY, &token, TTL).await,
        Err(ClusterError::LockExpired { .. })
    ));
    assert!(
        matches!(cache.get(KEY).await, Ok(None)),
        "a failed renew must not resurrect the record"
    );
}

// ---------------------------------------------------------------------------
// Release: idempotent by absence
// ---------------------------------------------------------------------------

#[tokio::test]
async fn release_of_an_absent_record_is_ok() {
    // `L1` exit criterion: `release` on an absent record returns `Ok`.
    let cache = MemoryCache::linearizable();
    let leases = store(&cache);
    let never_held = LeaseToken::new(NAME, "owner-a", 1);
    assert!(
        leases.release(KEY, &never_held).await.is_ok(),
        "releasing what is not there is success, not NotFound (DESIGN 6.10)"
    );
}

#[tokio::test]
async fn release_is_idempotent() {
    let cache = MemoryCache::linearizable();
    let leases = store(&cache);
    let token = acquired(&leases, "owner-a", TTL).await;
    assert!(leases.release(KEY, &token).await.is_ok());
    assert!(
        leases.release(KEY, &token).await.is_ok(),
        "a retried release must also succeed"
    );
}

#[tokio::test(start_paused = true)]
async fn release_leaves_a_successors_lease_untouched() {
    let cache = MemoryCache::linearizable();
    let leases = store(&cache);
    let stale = acquired(&leases, "owner-a", Duration::from_secs(5)).await;
    tokio::time::advance(Duration::from_secs(6)).await;
    let successor = acquired(&leases, "owner-b", Duration::from_secs(30)).await;

    // The fenced-out predecessor releases: a no-op `Ok` that must not touch the
    // lease that superseded it.
    assert!(leases.release(KEY, &stale).await.is_ok());
    let record = record_at(&cache, KEY).await;
    assert!(record.matches(&successor));
    assert!(leases.is_live(&record));
}

#[tokio::test(start_paused = true)]
async fn release_removes_a_lapsed_record_that_is_still_ours() {
    // Liveness is deliberately not part of release's predicate: a lapsed record
    // nobody stole is still this holder's to remove.
    let cache = MemoryCache::linearizable();
    let leases = store(&cache);
    let token = acquired(&leases, "owner-a", Duration::from_secs(5)).await;
    tokio::time::advance(Duration::from_secs(6)).await;
    assert!(leases.release(KEY, &token).await.is_ok());
    assert!(matches!(cache.get(KEY).await, Ok(None)));
}

// ---------------------------------------------------------------------------
// Cross-handle: the property the whole item exists for (I7)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_lease_is_renewable_through_a_handle_that_never_saw_the_acquire() {
    // `L1` exit criterion: the result of a lease operation is a property of the
    // record, asserted by renewing through a *different* backend handle than the
    // one that acquired. Two stores over one cache stand in for two replicas over
    // one backing store.
    let cache = MemoryCache::linearizable();
    let acquirer = store(&cache);
    let other_replica = store(&cache);

    let token = acquired(&acquirer, "owner-a", TTL).await;
    assert!(
        other_replica.renew(KEY, &token, TTL).await.is_ok(),
        "any replica must serve any renew - nothing about the lease lives in the \
         handle that issued it"
    );
    assert!(
        other_replica.release(KEY, &token).await.is_ok(),
        "and any replica must serve the release"
    );
    assert!(matches!(cache.get(KEY).await, Ok(None)));
}

#[tokio::test]
async fn a_foreign_token_is_refused_by_every_handle_identically() {
    let cache = MemoryCache::linearizable();
    let acquirer = store(&cache);
    let other_replica = store(&cache);

    let token = acquired(&acquirer, "owner-a", TTL).await;
    let impostor = LeaseToken::new(&token.name, "owner-b", token.fence);
    let here = acquirer.renew(KEY, &impostor, TTL).await;
    let there = other_replica.renew(KEY, &impostor, TTL).await;
    assert!(matches!(here, Err(ClusterError::LockExpired { .. })));
    assert!(matches!(there, Err(ClusterError::LockExpired { .. })));
}

#[tokio::test(start_paused = true)]
async fn a_lease_stolen_through_one_handle_fences_the_other() {
    let cache = MemoryCache::linearizable();
    let first = store(&cache);
    let second = store(&cache);

    let stale = acquired(&first, "owner-a", Duration::from_secs(5)).await;
    tokio::time::advance(Duration::from_secs(6)).await;
    let stolen = acquired(&second, "owner-b", Duration::from_secs(30)).await;
    assert!(stolen.fence > stale.fence);

    // The original holder's renew fails and its release is a no-op `Ok`, neither
    // touching the new holder's lease (§7.6's fencing row).
    assert!(matches!(
        first.renew(KEY, &stale, Duration::from_secs(5)).await,
        Err(ClusterError::LockExpired { .. })
    ));
    assert!(first.release(KEY, &stale).await.is_ok());
    let record = record_at(&cache, KEY).await;
    assert!(record.matches(&stolen));
    assert!(
        second
            .renew(KEY, &stolen, Duration::from_secs(30))
            .await
            .is_ok()
    );
}

// ---------------------------------------------------------------------------
// Values cluster did not write
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_unreadable_value_is_treated_as_a_foreign_holder() {
    let cache = MemoryCache::linearizable();
    let leases = store(&cache);
    // A pre-lease holder marker, as the CAS lock wrote before this item.
    assert!(
        cache
            .put(PutRequest {
                key: KEY,
                value: b"3f2504e0-4f89-41d3-9a0c-0305e82c3301",
                ttl: Ttl::Of(TTL),
            })
            .await
            .is_ok()
    );
    assert!(
        matches!(
            leases.try_acquire(KEY, NAME, "owner-a", TTL).await,
            Ok(Acquisition::Contended { lapse_in: None })
        ),
        "a value we cannot read must contend - never be stolen or rewritten"
    );
    let Ok(Some(entry)) = cache.get(KEY).await else {
        panic!("the foreign value must survive");
    };
    assert_eq!(entry.value, b"3f2504e0-4f89-41d3-9a0c-0305e82c3301");
}

#[tokio::test]
async fn release_never_deletes_an_unreadable_value() {
    let cache = MemoryCache::linearizable();
    let leases = store(&cache);
    assert!(
        cache
            .put(PutRequest {
                key: KEY,
                value: b"not-a-lease",
                ttl: Ttl::Of(TTL),
            })
            .await
            .is_ok()
    );
    let token = LeaseToken::new(NAME, "owner-a", 1);
    assert!(leases.release(KEY, &token).await.is_ok());
    let Ok(Some(entry)) = cache.get(KEY).await else {
        panic!("the foreign value must survive a release");
    };
    assert_eq!(entry.value, b"not-a-lease");
}

#[tokio::test]
async fn read_reports_none_for_a_value_that_is_not_a_lease() {
    let cache = MemoryCache::linearizable();
    let leases = store(&cache);
    assert!(
        cache
            .put(PutRequest {
                key: KEY,
                value: b"not-a-lease",
                ttl: Ttl::Of(TTL),
            })
            .await
            .is_ok()
    );
    assert!(matches!(leases.read(KEY).await, Ok(None)));
}
