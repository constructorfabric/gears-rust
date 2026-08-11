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
