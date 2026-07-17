//! Composite fallback + provenance tests with in-memory fake sources.

use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

use async_trait::async_trait;
use bss_ledger_sdk::{CurrencyPair, ProviderRate, RateProviderError, RateProviderV1};
use chrono::DateTime;
use toolkit_security::SecurityContext;

use super::CompositeRateProvider;

struct Fake {
    id: &'static str,
    ok: bool,
}

#[async_trait]
impl RateProviderV1 for Fake {
    fn provider_id(&self) -> &str {
        self.id
    }
    async fn fetch_latest(
        &self,
        _ctx: &SecurityContext,
        _pairs: &[CurrencyPair],
        _request_id: &str,
    ) -> Result<Vec<ProviderRate>, RateProviderError> {
        if self.ok {
            Ok(vec![ProviderRate {
                base: "EUR".to_owned(),
                quote: "USD".to_owned(),
                rate_micro: 1_000_000,
                as_of: DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
            }])
        } else {
            Err(RateProviderError::Unreachable("down".to_owned()))
        }
    }
}

#[tokio::test]
async fn falls_back_and_reports_serving_source() {
    let composite = CompositeRateProvider::new(vec![
        Arc::new(Fake { id: "ecb", ok: false }),
        Arc::new(Fake {
            id: "bank-x",
            ok: true,
        }),
    ]);
    let ctx = SecurityContext::anonymous();
    let rates = composite.fetch_latest(&ctx, &[], "req").await.unwrap();
    assert_eq!(rates.len(), 1);
    assert_eq!(composite.provider_id(), "bank-x");
}

#[tokio::test]
async fn all_fail_returns_last_error_and_never_none() {
    let composite = CompositeRateProvider::new(vec![
        Arc::new(Fake { id: "ecb", ok: false }),
        Arc::new(Fake {
            id: "bank-x",
            ok: false,
        }),
    ]);
    let ctx = SecurityContext::anonymous();
    assert!(composite.fetch_latest(&ctx, &[], "req").await.is_err());
    // Provenance defaults to the primary; NEVER "none" (the ledger's sentinel).
    assert_eq!(composite.provider_id(), "ecb");
}

#[test]
fn empty_sources_reports_the_sentinel_instead_of_panicking() {
    // `CompositeRateProvider::new` debug_asserts non-emptiness, so it can't be
    // used here to build the violating instance under a test/debug profile.
    // Construct directly (this test module is a child of `composite`, so the
    // private fields are visible) to simulate the release-mode case the
    // debug_assert doesn't check for: `provider_id()` must degrade to the
    // ledger's "none" sentinel rather than index out of bounds.
    let composite = CompositeRateProvider {
        sources: Vec::new(),
        last_served: AtomicUsize::new(0),
    };
    assert_eq!(composite.provider_id(), "none");
}
