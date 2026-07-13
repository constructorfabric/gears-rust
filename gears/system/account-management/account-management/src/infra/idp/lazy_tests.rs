//! Tests for [`LazyIdpProvider`].
//!
//! Covers the four contract-defining cases of the wrapper, mirroring
//! the test shape used by `authn-resolver`'s `Service`:
//!
//!   * happy path: vendor + scope match, forwards to the resolved
//!     plugin, second call hits the cached `gts_id` (no re-resolution).
//!   * vendor mismatch: `choose_plugin_instance` returns nothing.
//!     `required = true` surfaces `CleanFailure` (HTTP 503 at AM's
//!     boundary); `required = false` delegates to `NoopIdpProvider`
//!     (HTTP 501 `UnsupportedOperation`).
//!   * priority tiebreak within the same vendor (lower wins),
//!     proven by elimination: only the winner's scope is registered,
//!     success means the wrapper picked the winner.
//!   * catalogue advertises but scope missing: wrapper does NOT
//!     cache the partial-failure, so a subsequent call after the
//!     plugin registers (e.g. an init-order race recovers) resolves
//!     correctly. Critical for the "catalogue settles after AM init"
//!     timing the wrapper exists to handle.

use std::sync::Arc;
use toolkit_gts::gts_id;

use account_management_sdk::{IdpPluginClient, IdpPluginSpecV1, IdpProvisionTenantRequest};
use gts::GtsTypeId;
use toolkit::ClientHub;
use toolkit::client_hub::ClientScope;
use toolkit::gts::PluginV1;
use toolkit_security::SecurityContext;
use types_registry_sdk::testing::{MockTypesRegistryClient, make_test_instance};
use uuid::Uuid;

use super::{LazyIdpProvider, NoopIdpProvider};

/// Build a `(instance_id, GtsInstance)` pair for a synthetic `IdP`
/// plugin advertisement.
fn build_instance(
    instance_segment: &str,
    vendor: &str,
    priority: i16,
) -> (String, types_registry_sdk::GtsInstance) {
    let (id, payload) =
        PluginV1::<IdpPluginSpecV1>::build_registration(instance_segment, vendor, priority)
            .expect("build_registration must succeed for the marker spec");
    let gts_id = id.as_ref().to_owned();
    let instance = make_test_instance(&gts_id, payload);
    (gts_id, instance)
}

/// Synthetic `IdpProvisionTenantRequest` -- content is irrelevant,
/// the wrapper just forwards / refuses based on resolution outcome.
/// Uses the same `cf.core.am.customer.v1~` synthetic tenant type
/// `noop_tests` and the rest of the AM unit-test suite use, so the
/// test stays within the upstream-shared GTS namespace (no
/// deployment-specific identifiers leak into `gears-rust`).
fn request() -> IdpProvisionTenantRequest {
    let tenant_type = GtsTypeId::new(gts_id!("cf.core.am.tenant_type.v1~cf.core.am.customer.v1~"));
    IdpProvisionTenantRequest::new(
        Uuid::from_u128(0xC11D),
        Uuid::from_u128(0xBEEF),
        "child",
        tenant_type,
    )
}

fn ctx() -> SecurityContext {
    SecurityContext::anonymous()
}

#[tokio::test]
async fn resolves_and_forwards_when_vendor_matches() {
    let (gts_id, instance) = build_instance("cf.builtin.lazy_idp_test.plugin.v1", "cf", 100);
    let registry = Arc::new(MockTypesRegistryClient::new().with_instances([instance]));
    let hub = Arc::new(ClientHub::new());
    let plugin: Arc<dyn IdpPluginClient> = Arc::new(NoopIdpProvider);
    hub.register_scoped::<dyn IdpPluginClient>(ClientScope::gts_id(&gts_id), plugin);

    let lazy = LazyIdpProvider::new(Arc::clone(&hub), registry.clone(), "cf".into(), true);

    // First call resolves and forwards. NoopIdpProvider returns
    // `UnsupportedOperation` from the trait default — that's the
    // marker that the wrapper successfully forwarded (rather than
    // surfacing the wrapper's own "not resolved" CleanFailure shape).
    let err = lazy
        .provision_tenant(&ctx(), &request())
        .await
        .expect_err("NoopIdpProvider returns UnsupportedOperation");
    assert_eq!(err.as_metric_label(), "unsupported_operation");

    // Cache pin: registry should have been queried exactly once (the
    // `GtsPluginSelector` caches the gts_id on Ok).
    let queries_before = registry.list_instance_calls();
    drop(lazy.provision_tenant(&ctx(), &request()).await);
    assert_eq!(
        registry.list_instance_calls(),
        queries_before,
        "second call MUST hit the cached gts_id; no additional list_instances"
    );
}

#[tokio::test]
async fn surfaces_clean_failure_when_required_and_vendor_misses() {
    // Catalogue advertises a `cf` plugin, but we asked for a
    // different vendor. `choose_plugin_instance` returns nothing;
    // wrapper with `required = true` surfaces `CleanFailure`
    // (HTTP 503 at the AM boundary, retryable). Vendor name is a
    // deployment-shaped string -- the test uses `alt-vendor` so the
    // upstream test never references any specific downstream IdP
    // plugin's vendor identifier.
    let (_, instance) = build_instance("cf.builtin.lazy_idp_test.plugin.v1", "cf", 100);
    let registry = Arc::new(MockTypesRegistryClient::new().with_instances([instance]));
    let hub = Arc::new(ClientHub::new());

    let lazy = LazyIdpProvider::new(hub, registry, "alt-vendor".into(), true);

    let err = lazy
        .provision_tenant(&ctx(), &request())
        .await
        .expect_err("required + vendor miss MUST error");
    assert_eq!(
        err.as_metric_label(),
        "clean_failure",
        "required + unresolvable must map to CleanFailure (HTTP 503), got: {err:?}"
    );
    assert!(
        err.detail().contains("alt-vendor"),
        "error detail should mention the configured vendor: {}",
        err.detail()
    );
}

#[tokio::test]
async fn falls_back_to_noop_when_not_required_and_vendor_misses() {
    // Same setup, but `required = false` — wrapper delegates to
    // NoopIdpProvider which surfaces UnsupportedOperation
    // (HTTP 501, permanent, distinct from the 503 retryable shape).
    let (_, instance) = build_instance("cf.builtin.lazy_idp_test.plugin.v1", "cf", 100);
    let registry = Arc::new(MockTypesRegistryClient::new().with_instances([instance]));
    let hub = Arc::new(ClientHub::new());

    let lazy = LazyIdpProvider::new(hub, registry, "alt-vendor".into(), false);

    let err = lazy
        .provision_tenant(&ctx(), &request())
        .await
        .expect_err("noop fallback still returns Unsupported");
    assert_eq!(
        err.as_metric_label(),
        "unsupported_operation",
        "non-required fallback must map through NoopIdpProvider -> UnsupportedOperation"
    );
}

#[tokio::test]
async fn priority_tiebreak_within_same_vendor_lower_wins() {
    // Two cf-vendor instances. ONLY the winner (priority=50) has its
    // scope registered in the hub. Wrapper selects winner via
    // priority tiebreak → forwards successfully. If it had picked
    // the loser (priority=100) the scoped get would miss and the
    // wrapper would surface CleanFailure.
    let (winner_id, winner_inst) = build_instance("cf.builtin.lazy_first.plugin.v1", "cf", 50);
    let (_loser_id, loser_inst) = build_instance("cf.builtin.lazy_second.plugin.v1", "cf", 100);
    let registry =
        Arc::new(MockTypesRegistryClient::new().with_instances([winner_inst, loser_inst]));
    let hub = Arc::new(ClientHub::new());
    let plugin: Arc<dyn IdpPluginClient> = Arc::new(NoopIdpProvider);
    hub.register_scoped::<dyn IdpPluginClient>(ClientScope::gts_id(&winner_id), plugin);

    let lazy = LazyIdpProvider::new(hub, registry, "cf".into(), true);
    let err = lazy
        .provision_tenant(&ctx(), &request())
        .await
        .expect_err("forwarded to NoopIdpProvider winner");
    assert_eq!(
        err.as_metric_label(),
        "unsupported_operation",
        "winner forwarded → trait default UnsupportedOperation; got {err:?}"
    );
}

#[tokio::test]
async fn does_not_cache_resolution_failure_so_late_registration_recovers() {
    // Init-order race scenario: AM `Gear::init` constructs the
    // wrapper before the plugin's scoped registration appears. The
    // first call resolves the catalogue (success — instance is
    // there) but the scoped `try_get_scoped` returns None. The
    // wrapper MUST NOT cache the partial-failure: when the plugin
    // later registers, the next call must resolve correctly.
    //
    // Pin: register scope AFTER first call, second call succeeds.
    let (gts_id, instance) = build_instance("cf.builtin.lazy_late.plugin.v1", "cf", 100);
    let registry = Arc::new(MockTypesRegistryClient::new().with_instances([instance]));
    let hub = Arc::new(ClientHub::new());
    let lazy = LazyIdpProvider::new(Arc::clone(&hub), registry, "cf".into(), true);

    // First call: catalogue resolves but no scope, so CleanFailure.
    let err = lazy
        .provision_tenant(&ctx(), &request())
        .await
        .expect_err("no scoped registration yet -> CleanFailure");
    assert_eq!(err.as_metric_label(), "clean_failure");

    // Plugin registers late.
    let plugin: Arc<dyn IdpPluginClient> = Arc::new(NoopIdpProvider);
    hub.register_scoped::<dyn IdpPluginClient>(ClientScope::gts_id(&gts_id), plugin);

    // Second call: forwards (UnsupportedOperation from Noop default).
    let err2 = lazy
        .provision_tenant(&ctx(), &request())
        .await
        .expect_err("forwarded after late registration");
    assert_eq!(
        err2.as_metric_label(),
        "unsupported_operation",
        "wrapper MUST retry resolution after partial failure recovered"
    );
}
