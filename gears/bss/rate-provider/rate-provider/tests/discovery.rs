//! Integration: the core gear's `DiscoveringRateProvider` over a real
//! `ClientHub` + an in-memory types-registry mock.
//!
//! Exercises the Level-1 discovery the rework introduced (untested by the pure
//! unit tests): list source-plugin instances → filter by vendor → order by
//! `priority` → resolve scoped `RateProviderV1` → compose (ordered fallback +
//! provenance), plus the empty-set error and the `OnceCell` caching.
#![allow(
    clippy::unwrap_used,
    reason = "integration-test setup helpers: an unwrap here just fails the test"
)]

use std::sync::Arc;

use async_trait::async_trait;
use bss_ledger_sdk::{CurrencyPair, ProviderRate, RateProviderError, RateProviderV1};
use bss_rate_provider::discovery::DiscoveringRateProvider;
use bss_rate_provider_sdk::RateProviderSourcePluginSpecV1;
use chrono::DateTime;
use toolkit::client_hub::{ClientHub, ClientScope};
use toolkit::gts::PluginV1;
use toolkit_security::SecurityContext;
use types_registry_sdk::TypesRegistryClient;
use types_registry_sdk::models::GtsInstance;
use types_registry_sdk::testing::{MockTypesRegistryClient, make_test_instance};

/// A source whose `provider_id` is `id` and which succeeds or fails per `ok`.
struct FakeSource {
    id: String,
    ok: bool,
}

#[async_trait]
impl RateProviderV1 for FakeSource {
    fn provider_id(&self) -> &str {
        &self.id
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

/// One configured source for a test: `(name, vendor, priority, ok)`. `name` is
/// both the plugin's instance segment and the `provider_id` it reports.
type SourceSpec<'a> = (&'a str, &'a str, i16, bool);

/// Build a `ClientHub` with the given sources registered scoped + a mock
/// types-registry listing their `PluginV1` instances, and return the
/// `DiscoveringRateProvider` plus the mock (for call-count assertions).
fn setup(
    sources: &[SourceSpec<'_>],
    discover_vendor: &str,
) -> (DiscoveringRateProvider, Arc<MockTypesRegistryClient>) {
    let hub = Arc::new(ClientHub::default());
    let mut instances: Vec<GtsInstance> = Vec::new();
    for &(name, vendor, priority, ok) in sources {
        let segment = format!("cf.bss.rate_provider_{name}.plugin.v1");
        let (id, json) = PluginV1::<RateProviderSourcePluginSpecV1>::build_registration(
            &segment, vendor, priority,
        )
        .unwrap();
        let id_str = id.as_ref().to_owned();
        let source: Arc<dyn RateProviderV1> = Arc::new(FakeSource {
            id: name.to_owned(),
            ok,
        });
        hub.register_scoped::<dyn RateProviderV1>(ClientScope::gts_id(&id_str), source);
        instances.push(make_test_instance(&id_str, json));
    }
    let mock = Arc::new(MockTypesRegistryClient::new().with_instances(instances));
    hub.register::<dyn TypesRegistryClient>(mock.clone());
    let provider = DiscoveringRateProvider::new(
        hub,
        discover_vendor.to_owned(),
        "bss-rate-provider".to_owned(),
    );
    (provider, mock)
}

#[tokio::test]
async fn composes_in_priority_order_and_reports_provenance() {
    let (provider, _mock) = setup(
        &[("ecb", "cf.bss", 100, true), ("bank", "cf.bss", 200, true)],
        "cf.bss",
    );
    let ctx = SecurityContext::anonymous();
    let rates = provider.fetch_latest(&ctx, &[], "req").await.unwrap();
    assert_eq!(rates.len(), 1);
    // Lowest priority (100 = ecb) is tried first and succeeds → it serves.
    assert_eq!(provider.provider_id(), "ecb");
}

#[tokio::test]
async fn falls_back_to_next_priority_on_failure() {
    let (provider, _mock) = setup(
        &[("ecb", "cf.bss", 100, false), ("bank", "cf.bss", 200, true)],
        "cf.bss",
    );
    let ctx = SecurityContext::anonymous();
    let rates = provider.fetch_latest(&ctx, &[], "req").await.unwrap();
    assert_eq!(rates.len(), 1);
    // Primary (ecb) failed → the composite served the fallback (bank).
    assert_eq!(provider.provider_id(), "bank");
}

#[tokio::test]
async fn filters_out_other_vendor_sources() {
    // The rogue source has a LOWER priority (tried first if included) but a
    // different vendor, so it must be excluded and ecb must serve.
    let (provider, _mock) = setup(
        &[("ecb", "cf.bss", 100, true), ("rogue", "other", 10, true)],
        "cf.bss",
    );
    let ctx = SecurityContext::anonymous();
    provider.fetch_latest(&ctx, &[], "req").await.unwrap();
    assert_eq!(provider.provider_id(), "ecb");
}

#[tokio::test]
async fn errors_when_no_matching_vendor_source() {
    let (provider, _mock) = setup(&[("ecb", "other", 100, true)], "cf.bss");
    let ctx = SecurityContext::anonymous();
    assert!(provider.fetch_latest(&ctx, &[], "req").await.is_err());
}

#[tokio::test]
async fn discovery_is_cached_across_ticks() {
    let (provider, mock) = setup(&[("ecb", "cf.bss", 100, true)], "cf.bss");
    let ctx = SecurityContext::anonymous();
    provider.fetch_latest(&ctx, &[], "req").await.unwrap();
    provider.fetch_latest(&ctx, &[], "req").await.unwrap();
    // The composite is built once (OnceCell) — the registry is listed only once.
    assert_eq!(mock.list_instance_calls(), 1);
}

#[tokio::test]
async fn concurrent_first_fetches_discover_only_once() {
    let (provider, mock) = setup(&[("ecb", "cf.bss", 100, true)], "cf.bss");
    let ctx = SecurityContext::anonymous();

    // Two concurrent first-callers must be deduped by `OnceCell::get_or_try_init`
    // into a single discovery pass, not one per caller.
    let (a, b) = tokio::join!(
        provider.fetch_latest(&ctx, &[], "req-a"),
        provider.fetch_latest(&ctx, &[], "req-b")
    );
    a.unwrap();
    b.unwrap();
    assert_eq!(mock.list_instance_calls(), 1);
}
