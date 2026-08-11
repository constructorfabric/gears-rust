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
