use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use token_issuer_sdk::TokenIssuerError;

use super::*;

#[derive(Default)]
struct MockMinter {
    calls: AtomicUsize,
}

impl MockMinter {
    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    #[allow(clippy::unused_async)] // shaped like the real async mint closure
    async fn mint(&self, jwt: &str, obo_exp: i64) -> Result<(String, i64), TokenIssuerError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok((jwt.to_owned(), obo_exp))
    }
}

fn key(jti: u128, scopes_hash: u8) -> OboCacheKey {
    OboCacheKey::new(Uuid::from_u128(jti), [scopes_hash; 32])
}

#[tokio::test]
async fn same_key_returns_same_token_minting_once() {
    let minter = Arc::new(MockMinter::default());
    let cache = OboCache::new();
    let k = key(1, 0x11);
    // cap_exp far in the future, obo_exp future.
    let m1 = Arc::clone(&minter);
    let a = cache
        .get_or_mint(&k, 10_000, 1_000, || async move {
            m1.mint("OBO-A", 1_060).await
        })
        .await
        .unwrap();
    let m2 = Arc::clone(&minter);
    let b = cache
        .get_or_mint(&k, 10_000, 1_000, || async move {
            m2.mint("OBO-B", 1_060).await
        })
        .await
        .unwrap();
    assert_eq!(a, "OBO-A");
    assert_eq!(b, "OBO-A", "second call returns the cached token");
    assert_eq!(minter.calls(), 1, "mint invoked once");
}

#[tokio::test]
async fn different_scope_set_is_a_distinct_entry() {
    let minter = Arc::new(MockMinter::default());
    let cache = OboCache::new();
    let m1 = Arc::clone(&minter);
    let a = cache
        .get_or_mint(&key(1, 0x11), 10_000, 1_000, || async move {
            m1.mint("OBO-A", 1_060).await
        })
        .await
        .unwrap();
    // Same jti, different scope hash → separate entry, fresh mint.
    let m2 = Arc::clone(&minter);
    let d = cache
        .get_or_mint(&key(1, 0x22), 10_000, 1_000, || async move {
            m2.mint("OBO-C", 1_060).await
        })
        .await
        .unwrap();
    assert_eq!(a, "OBO-A");
    assert_eq!(d, "OBO-C");
    assert_ne!(a, d);
    assert_eq!(minter.calls(), 2);
}

#[tokio::test]
async fn expired_entry_is_removed_by_bounded_cleanup() {
    let cache = OboCache::new();
    let expired_key = key(1, 0x11);
    cache
        .get_or_mint(&expired_key, 110, 100, || async {
            Ok::<_, TokenIssuerError>(("expired".to_owned(), 105))
        })
        .await
        .unwrap();

    let other_key = key(2, 0x22);
    cache
        .get_or_mint(&other_key, 300, 111, || async {
            Ok::<_, TokenIssuerError>(("other".to_owned(), 200))
        })
        .await
        .unwrap();

    let state = cache.state.read().await;
    assert!(!state.slots.contains_key(&expired_key));
    assert!(state.slots.contains_key(&other_key));
}

#[tokio::test]
async fn stale_deadline_record_preserves_live_acceptance_window_entry() {
    let cache = OboCache::new();
    let live_key = key(1, 0x11);
    cache
        .get_or_mint(&live_key, 300, 100, || async {
            Ok::<_, TokenIssuerError>(("live".to_owned(), 250))
        })
        .await
        .unwrap();
    cache
        .state
        .write()
        .await
        .deadlines
        .entry(100)
        .or_default()
        .push(DeadlineRecord {
            key: live_key.clone(),
            expected_deadline: Some(100),
        });

    let other_key = key(2, 0x22);
    cache
        .get_or_mint(&other_key, 300, 150, || async {
            Ok::<_, TokenIssuerError>(("other".to_owned(), 250))
        })
        .await
        .unwrap();

    assert!(cache.state.read().await.slots.contains_key(&live_key));
    let jwt = cache
        .get_or_mint(&live_key, 300, 150, || async {
            Ok::<_, TokenIssuerError>(("unexpected".to_owned(), 250))
        })
        .await
        .unwrap();
    assert_eq!(jwt, "live");
}

#[tokio::test]
async fn busy_earliest_batch_does_not_starve_later_due_record() {
    let cache = OboCache::new();
    let mut held_slots = Vec::new();
    let later_key = {
        let mut state = cache.state.write().await;
        for id in 0..CLEANUP_BATCH_SIZE {
            let scopes_hash =
                u8::try_from(id).expect("cleanup batch index must fit in a u8 scope hash");
            let busy_key = key(1_000 + id as u128, scopes_hash);
            let slot = Arc::new(Mutex::new(Some(Cached {
                jwt: format!("busy-{id}"),
                obo_exp: 1,
                cap_valid_until: 1,
            })));
            held_slots.push(Arc::clone(&slot));
            state.slots.insert(busy_key.clone(), slot);
            state.deadlines.entry(1).or_default().push(DeadlineRecord {
                key: busy_key,
                expected_deadline: Some(1),
            });
        }
        let later_key = key(9_999, 0xee);
        state.slots.insert(
            later_key.clone(),
            Arc::new(Mutex::new(Some(Cached {
                jwt: "later".to_owned(),
                obo_exp: 2,
                cap_valid_until: 2,
            }))),
        );
        state.deadlines.entry(2).or_default().push(DeadlineRecord {
            key: later_key.clone(),
            expected_deadline: Some(2),
        });
        later_key
    };

    let mut state = cache.state.write().await;
    OboCache::cleanup_expired(&mut state, 10);
    assert!(state.slots.contains_key(&later_key));
    OboCache::cleanup_expired(&mut state, 10);
    assert!(!state.slots.contains_key(&later_key));
    assert_eq!(held_slots.len(), CLEANUP_BATCH_SIZE);
}

#[tokio::test]
async fn hit_only_traffic_eventually_runs_bounded_cleanup() {
    let cache = OboCache::new();
    let expired_key = key(1, 0x11);
    cache
        .get_or_mint(&expired_key, 10, 0, || async {
            Ok::<_, TokenIssuerError>(("expired".to_owned(), 10))
        })
        .await
        .unwrap();
    let live_key = key(2, 0x22);
    cache
        .get_or_mint(&live_key, 1_000, 0, || async {
            Ok::<_, TokenIssuerError>(("live".to_owned(), 1_000))
        })
        .await
        .unwrap();

    for _ in 0..CLEANUP_INTERVAL {
        let jwt = cache
            .get_or_mint(&live_key, 1_000, 20, || async {
                Ok::<_, TokenIssuerError>(("unexpected".to_owned(), 2_000))
            })
            .await
            .unwrap();
        assert_eq!(jwt, "live");
    }

    let state = cache.state.read().await;
    assert!(!state.slots.contains_key(&expired_key));
    assert!(state.slots.contains_key(&live_key));
}

#[tokio::test]
async fn mint_error_leaves_indexed_pending_slot_that_cleanup_reclaims() {
    let cache = OboCache::new();
    let failed_key = key(1, 0x11);
    let result = cache
        .get_or_mint(&failed_key, 500, 100, || async {
            Err::<(String, i64), _>("mint failed")
        })
        .await;
    assert_eq!(result, Err("mint failed"));

    {
        let state = cache.state.read().await;
        assert!(state.slots.contains_key(&failed_key));
        assert!(
            state
                .deadlines
                .values()
                .flatten()
                .any(|record| { record.key == failed_key && record.expected_deadline.is_none() })
        );
    }

    let other_key = key(2, 0x22);
    cache
        .get_or_mint(&other_key, 500, 101, || async {
            Ok::<_, &'static str>(("other".to_owned(), 400))
        })
        .await
        .unwrap();
    assert!(!cache.state.read().await.slots.contains_key(&failed_key));
}

#[tokio::test]
async fn cancellation_while_minting_leaves_reclaimable_pending_slot() {
    let cache = Arc::new(OboCache::new());
    let cancelled_key = key(1, 0x11);
    let entered = Arc::new(tokio::sync::Notify::new());
    let task = {
        let cache = Arc::clone(&cache);
        let key = cancelled_key.clone();
        let entered = Arc::clone(&entered);
        tokio::spawn(async move {
            cache
                .get_or_mint(&key, 500, 100, || async move {
                    entered.notify_one();
                    std::future::pending::<Result<(String, i64), TokenIssuerError>>().await
                })
                .await
        })
    };
    entered.notified().await;
    task.abort();
    assert!(
        task.await
            .expect_err("aborted task must return a JoinError")
            .is_cancelled(),
        "aborted task JoinError must report cancellation"
    );

    {
        let state = cache.state.read().await;
        assert!(
            state.deadlines.values().flatten().any(|record| {
                record.key == cancelled_key && record.expected_deadline.is_none()
            })
        );
    }
    let other_key = key(2, 0x22);
    cache
        .get_or_mint(&other_key, 500, 101, || async {
            Ok::<_, TokenIssuerError>(("other".to_owned(), 400))
        })
        .await
        .unwrap();
    assert!(!cache.state.read().await.slots.contains_key(&cancelled_key));
}

#[tokio::test]
async fn cancellation_waiting_for_final_state_lock_does_not_publish() {
    let cache = Arc::new(OboCache::new());
    let cancelled_key = key(1, 0x11);
    let mint_entered = Arc::new(tokio::sync::Notify::new());
    let finish_mint = Arc::new(tokio::sync::Notify::new());
    let task = {
        let cache = Arc::clone(&cache);
        let key = cancelled_key.clone();
        let mint_entered = Arc::clone(&mint_entered);
        let finish_mint = Arc::clone(&finish_mint);
        tokio::spawn(async move {
            cache
                .get_or_mint(&key, 500, 100, || async move {
                    mint_entered.notify_one();
                    finish_mint.notified().await;
                    Ok::<_, TokenIssuerError>(("must-not-publish".to_owned(), 400))
                })
                .await
        })
    };
    mint_entered.notified().await;
    let state = cache.state.write().await;
    finish_mint.notify_one();
    tokio::task::yield_now().await;
    task.abort();
    assert!(
        task.await
            .expect_err("aborted task must return a JoinError")
            .is_cancelled(),
        "aborted task JoinError must report cancellation"
    );

    let slot = state.slots.get(&cancelled_key).unwrap();
    let inner = slot.try_lock().unwrap();
    assert!(inner.is_none());
    assert!(
        state
            .deadlines
            .values()
            .flatten()
            .any(|record| { record.key == cancelled_key && record.expected_deadline.is_none() })
    );
}

#[tokio::test]
async fn concurrent_same_key_callers_mint_once_and_share_result() {
    let cache = Arc::new(OboCache::new());
    let calls = Arc::new(AtomicUsize::new(0));
    let mint_entered = Arc::new(tokio::sync::Notify::new());
    let finish_mint = Arc::new(tokio::sync::Notify::new());
    let shared_key = key(1, 0x11);

    let first = {
        let cache = Arc::clone(&cache);
        let calls = Arc::clone(&calls);
        let mint_entered = Arc::clone(&mint_entered);
        let finish_mint = Arc::clone(&finish_mint);
        let key = shared_key.clone();
        tokio::spawn(async move {
            cache
                .get_or_mint(&key, 500, 100, || async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    mint_entered.notify_one();
                    finish_mint.notified().await;
                    Ok::<_, TokenIssuerError>(("shared".to_owned(), 400))
                })
                .await
        })
    };
    mint_entered.notified().await;
    let second = {
        let cache = Arc::clone(&cache);
        let calls = Arc::clone(&calls);
        let key = shared_key.clone();
        tokio::spawn(async move {
            cache
                .get_or_mint(&key, 500, 100, || async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok::<_, TokenIssuerError>(("unexpected".to_owned(), 400))
                })
                .await
        })
    };
    finish_mint.notify_one();

    let first_result = first.await.unwrap().unwrap();
    let second_result = second.await.unwrap().unwrap();
    assert_eq!(first_result, "shared");
    assert_eq!(second_result, "shared");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn expired_obo_is_reminted_in_place() {
    let minter = Arc::new(MockMinter::default());
    let cache = OboCache::new();
    let k = key(1, 0x11);
    // First mint at now=1000 with obo_exp=1060 (cap_exp 10_000).
    let m1 = Arc::clone(&minter);
    let a = cache
        .get_or_mint(&k, 10_000, 1_000, || async move {
            m1.mint("OBO-A", 1_060).await
        })
        .await
        .unwrap();
    // Later, now=2000 > obo_exp 1060 but < cap_exp 10_000 → re-mint replaces.
    let m2 = Arc::clone(&minter);
    let b = cache
        .get_or_mint(&k, 10_000, 2_000, || async move {
            m2.mint("OBO-B", 2_060).await
        })
        .await
        .unwrap();
    assert_eq!(a, "OBO-A");
    assert_eq!(b, "OBO-B", "expired OBO is re-minted");
    assert_eq!(minter.calls(), 2);

    // The replacement is now cached and reused while fresh.
    let m3 = Arc::clone(&minter);
    let c = cache
        .get_or_mint(&k, 10_000, 2_010, || async move {
            m3.mint("OBO-D", 2_060).await
        })
        .await
        .unwrap();
    assert_eq!(c, "OBO-B");
    assert_eq!(minter.calls(), 2, "fresh replacement is reused");
}

#[tokio::test]
async fn reuses_entry_within_cap_skew_window() {
    // Regression (design review #11): callers pass `cap_valid_until = cap exp +
    // clock_skew_secs` (Gate 1's acceptance horizon), so an entry stays reusable
    // at a `now` that is past the bare cap exp but still within the skew window —
    // exactly where Gate 1 still accepts the cap. Here bare exp would be 1_000 and
    // the horizon (exp + 30 s skew) is 1_030; a retry at now=1_020 must reuse.
    let minter = Arc::new(MockMinter::default());
    let cache = OboCache::new();
    let k = key(1, 0x11);
    let m1 = Arc::clone(&minter);
    let a = cache
        .get_or_mint(
            &k,
            1_030,
            1_005,
            || async move { m1.mint("OBO-A", 1_100).await },
        )
        .await
        .unwrap();
    // now=1_020: past bare exp (1_000) but within the horizon (1_030) → reuse.
    let m2 = Arc::clone(&minter);
    let b = cache
        .get_or_mint(
            &k,
            1_030,
            1_020,
            || async move { m2.mint("OBO-B", 1_100).await },
        )
        .await
        .unwrap();
    assert_eq!(a, "OBO-A");
    assert_eq!(
        b, "OBO-A",
        "within the skew horizon the cached token is reused"
    );
    assert_eq!(minter.calls(), 1, "no fresh mint inside the skew window");
    // now=1_031: past the horizon → evicted, fresh mint.
    let m3 = Arc::clone(&minter);
    let c = cache
        .get_or_mint(
            &k,
            1_030,
            1_031,
            || async move { m3.mint("OBO-C", 1_100).await },
        )
        .await
        .unwrap();
    assert_eq!(c, "OBO-C", "past the horizon a fresh OBO is minted");
    assert_eq!(minter.calls(), 2);
}

#[tokio::test]
async fn entry_past_cap_exp_is_evicted_and_reminted() {
    let minter = Arc::new(MockMinter::default());
    let cache = OboCache::new();
    let k = key(1, 0x11);
    let m1 = Arc::clone(&minter);
    let _ = cache
        .get_or_mint(
            &k,
            1_500,
            1_000,
            || async move { m1.mint("OBO-A", 1_060).await },
        )
        .await
        .unwrap();
    // now past cap_exp → entry dead, re-mint even though a value exists.
    let m2 = Arc::clone(&minter);
    let b = cache
        .get_or_mint(
            &k,
            9_000,
            1_600,
            || async move { m2.mint("OBO-B", 1_660).await },
        )
        .await
        .unwrap();
    assert_eq!(b, "OBO-B");
    assert_eq!(minter.calls(), 2);
}
