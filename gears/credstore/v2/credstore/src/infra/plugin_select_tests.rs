//! Plugin selection over the platform catalogue.
//!
//! Each registered plugin is a distinct `FakePlugin`, so the tests tell which
//! one the selector chose by pointer identity.

use std::sync::Arc;
use std::time::Duration;

use credstore_sdk::{CredStorePluginClientV2, CredStorePluginSpecV2};
use toolkit::ClientHub;
use toolkit::client_hub::ClientScope;
use toolkit::gts::PluginV1;
use toolkit_canonical_errors::CanonicalError;
use types_registry_sdk::PlatformTypesRegistryApi;
use types_registry_sdk::testing_platform::{Call, Fault, MockTypesRegistry};

use super::{DISCOVERY_BUDGET, GtsCredStorePluginSelector};
use crate::domain::error::DomainError;
use crate::domain::ports::plugin::PluginSelector;
use crate::domain::secret::test_support::FakePlugin;
use crate::infra::stalled_registry::StalledRegistry;

/// Publishes `segment` under `vendor`/`priority` in `registry`, and returns its id.
fn publish(registry: &MockTypesRegistry, segment: &str, vendor: &str, priority: i16) -> String {
    let (id, content) =
        PluginV1::<CredStorePluginSpecV2>::build_registration(segment, vendor, priority)
            .expect("a valid registration");
    registry.seed(id.as_ref(), content);
    id.as_ref().to_owned()
}

/// Binds a running plugin under the scope of `id` and returns it.
fn run(hub: &ClientHub, id: &str) -> Arc<FakePlugin> {
    let plugin = FakePlugin::new();
    hub.register_scoped::<dyn CredStorePluginClientV2>(
        ClientScope::gts_id(id),
        Arc::clone(&plugin) as Arc<dyn CredStorePluginClientV2>,
    );
    plugin
}

fn hub_with(registry: &Arc<MockTypesRegistry>) -> Arc<ClientHub> {
    let hub = Arc::new(ClientHub::new());
    registry.install(&hub);
    hub
}

fn selector(hub: Arc<ClientHub>) -> GtsCredStorePluginSelector {
    GtsCredStorePluginSelector::new(hub, "cf".to_owned())
}

/// Whether `selected` is the very plugin `expected`.
fn is(selected: &Arc<dyn CredStorePluginClientV2>, expected: &Arc<FakePlugin>) -> bool {
    std::ptr::addr_eq(Arc::as_ptr(selected), Arc::as_ptr(expected))
}

#[tokio::test]
async fn the_lowest_priority_running_plugin_of_the_vendor_is_selected() {
    let registry = Arc::new(MockTypesRegistry::new());
    let hub = hub_with(&registry);
    let low = publish(&registry, "cf.core._.low.v1", "cf", 10);
    let high = publish(&registry, "cf.core._.high.v1", "cf", 50);
    let other = publish(&registry, "cf.core._.other.v1", "acme", 0);
    let low = run(&hub, &low);
    run(&hub, &high);
    run(&hub, &other);

    let chosen = selector(hub).resolve().await.expect("a plugin is selected");
    assert!(is(&chosen, &low));
}

/// The catalogue is durable: an instance a previous boot published stays listed
/// after its plugin is no longer linked. A better-ranked stale instance must
/// not win.
#[tokio::test]
async fn an_instance_without_a_running_plugin_is_never_selected() {
    let registry = Arc::new(MockTypesRegistry::new());
    let hub = hub_with(&registry);
    publish(&registry, "cf.core._.stale.v1", "cf", 0);
    let live = publish(&registry, "cf.core._.live.v1", "cf", 100);
    let live = run(&hub, &live);

    let chosen = selector(hub).resolve().await.expect("a plugin is selected");
    assert!(is(&chosen, &live));
}

/// A vendor with no registered running plugin is a retryable 503, and the
/// selection is retried once one runs.
#[tokio::test]
async fn no_running_plugin_is_unavailable_and_is_retried_once_one_runs() {
    let registry = Arc::new(MockTypesRegistry::new());
    let hub = hub_with(&registry);
    let id = publish(&registry, "cf.core._.late.v1", "cf", 0);

    let selector = selector(Arc::clone(&hub));
    let err = selector.resolve().await.err().expect("no plugin runs yet");
    assert!(
        matches!(&err, DomainError::ServiceUnavailable { detail, .. }
            if detail.contains("no credstore plugin found for vendor 'cf'")),
        "got: {err:?}"
    );

    let late = run(&hub, &id);
    let chosen = selector.resolve().await.expect("the plugin runs now");
    assert!(is(&chosen, &late));
}

/// Every page is read: a selection over the first page alone would miss the
/// winner. A failed page fails the selection, which is not cached.
#[tokio::test]
async fn a_failed_page_fails_the_selection_and_is_not_cached() {
    let registry = Arc::new(MockTypesRegistry::new());
    let hub = hub_with(&registry);
    for n in 0..60 {
        let id = publish(&registry, &format!("cf.core._.a{n:02}.v1"), "cf", 100);
        run(&hub, &id);
    }
    let winner = publish(&registry, "cf.core._.zz.v1", "cf", 1);
    let winner = run(&hub, &winner);
    registry.inject(
        Fault::on(Call::List)
            .nth(2)
            .fail(CanonicalError::service_unavailable().create()),
    );

    let selector = selector(hub);
    let err = selector.resolve().await.err().expect("the page fails");
    assert!(
        matches!(err, DomainError::ServiceUnavailable { .. }),
        "got: {err:?}"
    );
    let chosen = selector.resolve().await.expect("the retry succeeds");
    assert!(is(&chosen, &winner));
    let lists = registry.calls(Call::List);
    selector.resolve().await.expect("served from the cache");
    assert_eq!(registry.calls(Call::List), lists, "a selection is cached");
}

#[tokio::test]
async fn without_the_platform_api_the_selection_fails_closed() {
    let hub = Arc::new(ClientHub::new());
    let id = PluginV1::<CredStorePluginSpecV2>::build_registration("cf.core._.orphan.v1", "cf", 0)
        .expect("a valid registration")
        .0;
    run(&hub, id.as_ref());

    let err = selector(hub)
        .resolve()
        .await
        .err()
        .expect("no registry to list");
    assert!(
        matches!(err, DomainError::ServiceUnavailable { .. }),
        "got: {err:?}"
    );
}

/// The selector applies its discovery budget and maps expiry to a 503. A zero
/// budget against a registry that never answers expires at once, with no wait.
#[tokio::test]
async fn a_stalled_registry_fails_the_selection_when_the_discovery_budget_expires() {
    let hub = Arc::new(ClientHub::new());
    hub.register::<dyn PlatformTypesRegistryApi>(Arc::new(StalledRegistry));

    let err = selector(hub)
        .with_discovery_budget(Duration::ZERO)
        .resolve()
        .await
        .err()
        .expect("the budget expires");

    assert!(
        matches!(err, DomainError::ServiceUnavailable { .. }),
        "got: {err:?}"
    );
}

#[test]
fn a_new_selector_uses_the_default_discovery_budget() {
    assert_eq!(
        selector(Arc::new(ClientHub::new())).discovery_budget,
        DISCOVERY_BUDGET
    );
}

/// Only an availability failure of the registry is an outage (503); any other
/// failure it answers is internal (500).
#[tokio::test]
async fn registry_failures_are_outages_only_when_retryable() {
    for (error, outage) in [
        (CanonicalError::service_unavailable().create(), true),
        (CanonicalError::internal("corrupt page").create(), false),
    ] {
        let registry = Arc::new(MockTypesRegistry::new());
        let hub = hub_with(&registry);
        let id = publish(&registry, "cf.core._.any.v1", "cf", 0);
        run(&hub, &id);
        registry.inject(Fault::on(Call::List).fail(error.clone()));

        let err = selector(hub)
            .resolve()
            .await
            .err()
            .expect("the listing fails");
        if outage {
            assert!(
                matches!(err, DomainError::ServiceUnavailable { .. }),
                "{error:?} -> {err:?}"
            );
        } else {
            assert!(
                matches!(err, DomainError::Internal { .. }),
                "{error:?} -> {err:?}"
            );
        }
    }
}
