use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

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
    async fn mint(&self, exp: i64) -> Result<(String, i64), TokenIssuerError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(("fixed.jwt.token".to_owned(), exp))
    }
}

fn sample_key() -> CacheKey {
    CacheKey {
        sub: Uuid::from_u128(1),
        subject_tenant: Uuid::from_u128(2),
        context_tenant: Uuid::from_u128(3),
        context_project_id: None,
        aud: "aud".to_owned(),
        scopes_hash: [7u8; 32],
        operation: None,
        resource_type: None,
    }
}

#[tokio::test]
async fn get_or_mint_reuses_within_floor() {
    let minter = Arc::new(MockMinter::default());
    let cache = CapCache::new(150);
    let key = sample_key();
    // exp = now + 300, floor = 150 → remaining 300 > 150 → reuse on 2nd call.
    let m1 = Arc::clone(&minter);
    let (_, o1) = cache
        .get_or_mint(&key, 1_000, || async move { m1.mint(1_300).await })
        .await
        .unwrap();
    assert_eq!(o1, CacheOutcome::Miss);
    let m2 = Arc::clone(&minter);
    let (_, o2) = cache
        .get_or_mint(&key, 1_000, || async move { m2.mint(1_300).await })
        .await
        .unwrap();
    assert_eq!(o2, CacheOutcome::Hit);
    assert_eq!(minter.calls(), 1, "second call must be a cache hit");
}

#[tokio::test]
async fn expired_entry_is_removed_by_bounded_cleanup() {
    let cache = CapCache::new(0);
    let expired_key = sample_key();
    cache
        .get_or_mint(&expired_key, 100, || async {
            Ok(("expired.jwt".to_owned(), 110))
        })
        .await
        .unwrap();

    let mut other_key = sample_key();
    other_key.sub = Uuid::from_u128(99);
    cache
        .get_or_mint(&other_key, 111, || async {
            Ok(("other.jwt".to_owned(), 200))
        })
        .await
        .unwrap();

    let state = cache.state.read().await;
    assert!(!state.slots.contains_key(&expired_key));
    assert!(state.slots.contains_key(&other_key));
}

#[tokio::test]
async fn stale_deadline_record_does_not_remove_live_replacement() {
    let cache = CapCache::new(0);
    let live_key = sample_key();
    cache
        .get_or_mint(&live_key, 100, || async {
            Ok(("live.jwt".to_owned(), 300))
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

    let mut other_key = sample_key();
    other_key.sub = Uuid::from_u128(99);
    cache
        .get_or_mint(&other_key, 150, || async {
            Ok(("other.jwt".to_owned(), 300))
        })
        .await
        .unwrap();

    assert!(cache.state.read().await.slots.contains_key(&live_key));
    let (jwt, outcome) = cache
        .get_or_mint(&live_key, 150, || async {
            Ok(("unexpected.jwt".to_owned(), 400))
        })
        .await
        .unwrap();
    assert_eq!(jwt, "live.jwt");
    assert_eq!(outcome, CacheOutcome::Hit);
}

#[tokio::test]
async fn busy_earliest_batch_does_not_starve_later_due_record() {
    let cache = CapCache::new(0);
    let mut held_slots = Vec::new();
    let later_key = {
        let mut state = cache.state.write().await;
        for id in 0..CLEANUP_BATCH_SIZE {
            let mut busy_key = sample_key();
            busy_key.sub = Uuid::from_u128(1_000 + id as u128);
            let slot = Arc::new(Mutex::new(Some(Cached {
                jwt: format!("busy-{id}"),
                exp: 1,
            })));
            held_slots.push(Arc::clone(&slot));
            state.slots.insert(busy_key.clone(), slot);
            state.deadlines.entry(1).or_default().push(DeadlineRecord {
                key: busy_key,
                expected_deadline: Some(1),
            });
        }
        let mut later_key = sample_key();
        later_key.sub = Uuid::from_u128(9_999);
        state.slots.insert(
            later_key.clone(),
            Arc::new(Mutex::new(Some(Cached {
                jwt: "later".to_owned(),
                exp: 2,
            }))),
        );
        state.deadlines.entry(2).or_default().push(DeadlineRecord {
            key: later_key.clone(),
            expected_deadline: Some(2),
        });
        later_key
    };

    let mut state = cache.state.write().await;
    CapCache::cleanup_expired(&mut state, 10);
    assert!(state.slots.contains_key(&later_key));
    CapCache::cleanup_expired(&mut state, 10);
    assert!(!state.slots.contains_key(&later_key));
    assert_eq!(held_slots.len(), CLEANUP_BATCH_SIZE);
}

#[tokio::test]
async fn hit_only_traffic_eventually_runs_bounded_cleanup() {
    let cache = CapCache::new(0);
    let expired_key = sample_key();
    cache
        .get_or_mint(&expired_key, 0, || async { Ok(("expired".to_owned(), 10)) })
        .await
        .unwrap();
    let mut live_key = sample_key();
    live_key.sub = Uuid::from_u128(77);
    cache
        .get_or_mint(&live_key, 0, || async { Ok(("live".to_owned(), 1_000)) })
        .await
        .unwrap();

    for _ in 0..CLEANUP_INTERVAL {
        let (_, outcome) = cache
            .get_or_mint(&live_key, 20, || async {
                Ok(("unexpected".to_owned(), 2_000))
            })
            .await
            .unwrap();
        assert_eq!(outcome, CacheOutcome::Hit);
    }

    let state = cache.state.read().await;
    assert!(!state.slots.contains_key(&expired_key));
    assert!(state.slots.contains_key(&live_key));
}

#[tokio::test]
async fn mint_error_leaves_indexed_pending_slot_that_cleanup_reclaims() {
    let cache = CapCache::new(0);
    let failed_key = sample_key();
    let result = cache
        .get_or_mint(&failed_key, 100, || async {
            Err(TokenIssuerError::Internal("mint failed".to_owned()))
        })
        .await;
    assert!(result.is_err());

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

    let mut other_key = sample_key();
    other_key.sub = Uuid::from_u128(88);
    cache
        .get_or_mint(&other_key, 101, || async { Ok(("other".to_owned(), 500)) })
        .await
        .unwrap();
    assert!(!cache.state.read().await.slots.contains_key(&failed_key));
}

#[tokio::test]
async fn cancellation_while_minting_leaves_reclaimable_pending_slot() {
    let cache = Arc::new(CapCache::new(0));
    let cancelled_key = sample_key();
    let entered = Arc::new(tokio::sync::Notify::new());
    let task = {
        let cache = Arc::clone(&cache);
        let key = cancelled_key.clone();
        let entered = Arc::clone(&entered);
        tokio::spawn(async move {
            cache
                .get_or_mint(&key, 100, || async move {
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
    let mut other_key = sample_key();
    other_key.sub = Uuid::from_u128(89);
    cache
        .get_or_mint(&other_key, 101, || async { Ok(("other".to_owned(), 500)) })
        .await
        .unwrap();
    assert!(!cache.state.read().await.slots.contains_key(&cancelled_key));
}

#[tokio::test]
async fn cancellation_waiting_for_final_state_lock_does_not_publish() {
    let cache = Arc::new(CapCache::new(0));
    let cancelled_key = sample_key();
    let mint_entered = Arc::new(tokio::sync::Notify::new());
    let finish_mint = Arc::new(tokio::sync::Notify::new());
    let task = {
        let cache = Arc::clone(&cache);
        let key = cancelled_key.clone();
        let mint_entered = Arc::clone(&mint_entered);
        let finish_mint = Arc::clone(&finish_mint);
        tokio::spawn(async move {
            cache
                .get_or_mint(&key, 100, || async move {
                    mint_entered.notify_one();
                    finish_mint.notified().await;
                    Ok(("must-not-publish".to_owned(), 500))
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
    let cache = Arc::new(CapCache::new(0));
    let calls = Arc::new(AtomicUsize::new(0));
    let mint_entered = Arc::new(tokio::sync::Notify::new());
    let finish_mint = Arc::new(tokio::sync::Notify::new());
    let key = sample_key();

    let first = {
        let cache = Arc::clone(&cache);
        let calls = Arc::clone(&calls);
        let mint_entered = Arc::clone(&mint_entered);
        let finish_mint = Arc::clone(&finish_mint);
        let key = key.clone();
        tokio::spawn(async move {
            cache
                .get_or_mint(&key, 100, || async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    mint_entered.notify_one();
                    finish_mint.notified().await;
                    Ok(("shared".to_owned(), 500))
                })
                .await
        })
    };
    mint_entered.notified().await;
    let second = {
        let cache = Arc::clone(&cache);
        let calls = Arc::clone(&calls);
        let key = key.clone();
        tokio::spawn(async move {
            cache
                .get_or_mint(&key, 100, || async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok(("unexpected".to_owned(), 500))
                })
                .await
        })
    };
    finish_mint.notify_one();

    let first_result = first.await.unwrap().unwrap();
    let second_result = second.await.unwrap().unwrap();
    assert_eq!(first_result.0, "shared");
    assert_eq!(second_result.0, "shared");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn get_or_mint_resigns_when_stale() {
    let minter = Arc::new(MockMinter::default());
    let cache = CapCache::new(150);
    let key = sample_key();
    // First mint at now=1000 with exp=1100 (remaining 100 <= floor 150).
    let m1 = Arc::clone(&minter);
    let _ = cache
        .get_or_mint(&key, 1_000, || async move { m1.mint(1_100).await })
        .await
        .unwrap();
    // Second call: cached remaining 100 <= floor → re-mint.
    let m2 = Arc::clone(&minter);
    let (_, o2) = cache
        .get_or_mint(&key, 1_000, || async move { m2.mint(1_400).await })
        .await
        .unwrap();
    assert_eq!(o2, CacheOutcome::Miss);
    assert_eq!(minter.calls(), 2, "stale token must be re-minted");
}
