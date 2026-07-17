//! Tests for the lazy FX rate-provider plugin discovery
//! ([`resolve_rate_provider`](super::resolve_rate_provider)): it resolves the
//! composite plugin the external gear registers, and falls back to the fail-safe
//! `UnconfiguredRateProviderV1` (`provider_id() == "none"`, so the sync job stays
//! silent, no alarm) whenever the registry is unavailable or no matching plugin
//! is registered.

use std::sync::Arc;

use async_trait::async_trait;
use bss_ledger_sdk::{
    CurrencyPair, ProviderRate, RateProviderError, RateProviderPluginSpecV1, RateProviderV1,
};
use toolkit::client_hub::{ClientHub, ClientScope};
use toolkit::gts::PluginV1;
use toolkit_security::SecurityContext;
use types_registry_sdk::testing::{make_test_instance, MockTypesRegistryClient};
use types_registry_sdk::TypesRegistryClient;

use super::resolve_rate_provider;

/// A stand-in composite whose `provider_id` is a real (non-`"none"`) id.
struct FakeComposite;

#[async_trait]
impl RateProviderV1 for FakeComposite {
    #[allow(
        clippy::unnecessary_literal_bound,
        reason = "trait signature is `-> &str` tied to &self; this fake returns a literal"
    )]
    fn provider_id(&self) -> &str {
        "ecb"
    }
    async fn fetch_latest(
        &self,
        _ctx: &SecurityContext,
        _pairs: &[CurrencyPair],
        _request_id: &str,
    ) -> Result<Vec<ProviderRate>, RateProviderError> {
        Ok(vec![])
    }
}

/// A hub advertising one composite rate-provider plugin (under `vendor`) + a
/// scoped `FakeComposite` bound to its instance id.
fn hub_with_composite(vendor: &str) -> Arc<ClientHub> {
    let hub = Arc::new(ClientHub::default());
    let (id, json) = PluginV1::<RateProviderPluginSpecV1>::build_registration(
        "cf.bss.rate_provider_composite.plugin.v1",
        vendor,
        100,
    )
    .unwrap();
    let id_str = id.as_ref().to_owned();
    let registry: Arc<dyn TypesRegistryClient> =
        Arc::new(MockTypesRegistryClient::new().with_instances([make_test_instance(&id_str, json)]));
    hub.register::<dyn TypesRegistryClient>(registry);
    let composite: Arc<dyn RateProviderV1> = Arc::new(FakeComposite);
    hub.register_scoped::<dyn RateProviderV1>(ClientScope::gts_id(&id_str), composite);
    hub
}

#[tokio::test]
async fn resolves_registered_composite() {
    let hub = hub_with_composite("cf.bss");
    let provider = resolve_rate_provider(&hub, "cf.bss").await;
    assert_eq!(provider.provider_id(), "ecb");
}

#[tokio::test]
async fn falls_back_to_unconfigured_when_no_registry() {
    // No `TypesRegistryClient` registered at all.
    let hub = ClientHub::default();
    let provider = resolve_rate_provider(&hub, "cf.bss").await;
    assert_eq!(provider.provider_id(), "none");
}

#[tokio::test]
async fn falls_back_to_unconfigured_on_vendor_mismatch() {
    let hub = hub_with_composite("cf.bss");
    let provider = resolve_rate_provider(&hub, "some-other-vendor").await;
    assert_eq!(provider.provider_id(), "none");
}

#[tokio::test]
async fn falls_back_to_unconfigured_when_no_instances() {
    let hub = Arc::new(ClientHub::default());
    let registry: Arc<dyn TypesRegistryClient> = Arc::new(MockTypesRegistryClient::new());
    hub.register::<dyn TypesRegistryClient>(registry);
    let provider = resolve_rate_provider(&hub, "cf.bss").await;
    assert_eq!(provider.provider_id(), "none");
}
