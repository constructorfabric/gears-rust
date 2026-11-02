//! Plugin selection over the platform catalogue.
//!
//! Each fake plugin refuses every token with its own name, so the error says which
//! plugin the service selected.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use authn_resolver_sdk::{
    AuthNResolverError, AuthNResolverPluginClient, AuthNResolverPluginSpecV1, AuthenticationResult,
    ClientCredentialsRequest,
};
use toolkit::ClientHub;
use toolkit::client_hub::ClientScope;
use toolkit::gts::PluginV1;
use toolkit_canonical_errors::CanonicalError;
use types_registry_sdk::testing_platform::{Call, Fault, MockTypesRegistry};

use super::{DISCOVERY_BUDGET, Service};
use crate::domain::DomainError;

/// An error scope for the failures the fake registry answers with.
#[toolkit_canonical_errors::resource_error(toolkit_gts::gts_id!("cf.core.types_registry.entity.v1~"))]
struct Catalogue;

/// Every availability category the selection treats as a retryable registry outage.
fn outages() -> [CanonicalError; 4] {
    [
        CanonicalError::service_unavailable().create(),
        Catalogue::deadline_exceeded("listing timed out").create(),
        Catalogue::resource_exhausted("rate limited")
            .with_quota_violation("listing", "rate limit")
            .create(),
        Catalogue::aborted("listing aborted")
            .with_reason("LISTING_ABORTED")
            .create(),
    ]
}

/// Failures the registry answers that are not outages.
fn non_outages() -> [CanonicalError; 2] {
    [
        Catalogue::permission_denied()
            .with_reason("PLATFORM_CREDENTIAL_REFUSED")
            .create(),
        CanonicalError::internal("corrupt page").create(),
    ]
}

struct Named(&'static str);

#[async_trait]
impl AuthNResolverPluginClient for Named {
    async fn authenticate(&self, _token: &str) -> Result<AuthenticationResult, AuthNResolverError> {
        Err(AuthNResolverError::Unauthorized(self.0.to_owned()))
    }

    async fn exchange_client_credentials(
        &self,
        _request: &ClientCredentialsRequest,
    ) -> Result<AuthenticationResult, AuthNResolverError> {
        Err(AuthNResolverError::TokenAcquisitionFailed(
            self.0.to_owned(),
        ))
    }
}

/// Publishes `segment` under `vendor`/`priority` in `registry`, and returns its id.
fn publish(registry: &MockTypesRegistry, segment: &str, vendor: &str, priority: i16) -> String {
    let (id, content) =
        PluginV1::<AuthNResolverPluginSpecV1>::build_registration(segment, vendor, priority)
            .expect("a valid registration");
    registry.seed(id.as_ref(), content);
    id.as_ref().to_owned()
}

/// Binds a running plugin named `name` under the scope of `id`.
fn run(hub: &ClientHub, id: &str, name: &'static str) {
    hub.register_scoped::<dyn AuthNResolverPluginClient>(
        ClientScope::gts_id(id),
        Arc::new(Named(name)),
    );
}

fn hub_with(registry: &Arc<MockTypesRegistry>) -> Arc<ClientHub> {
    let hub = Arc::new(ClientHub::new());
    registry.install(&hub);
    hub
}

/// The plugin `service` selected, by the name it refused the token with.
async fn selected(service: &Service) -> Result<String, DomainError> {
    match service.authenticate("token").await {
        Err(DomainError::Unauthorized(name)) => Ok(name),
        Err(other) => Err(other),
        Ok(_) => panic!("a fake plugin never authenticates"),
    }
}

#[tokio::test]
async fn the_lowest_priority_running_plugin_of_the_vendor_is_selected() {
    let registry = Arc::new(MockTypesRegistry::new());
    let hub = hub_with(&registry);
    let low = publish(&registry, "cf.builtin.low.plugin.v1", "cf", 10);
    let high = publish(&registry, "cf.builtin.high.plugin.v1", "cf", 50);
    let other = publish(&registry, "cf.builtin.other.plugin.v1", "acme", 0);
    run(&hub, &low, "low");
    run(&hub, &high, "high");
    run(&hub, &other, "other");

    let service = Service::new(hub, "cf".to_owned());
    assert_eq!(selected(&service).await.unwrap(), "low");
}

/// The catalogue is durable: an instance a previous boot published stays listed after
/// its plugin is no longer linked. A better-ranked stale instance must not win.
#[tokio::test]
async fn an_instance_without_a_running_plugin_is_never_selected() {
    let registry = Arc::new(MockTypesRegistry::new());
    let hub = hub_with(&registry);
    publish(&registry, "cf.builtin.stale.plugin.v1", "cf", 0);
    let oidc = publish(&registry, "cf.builtin.oidc.plugin.v1", "cf", 100);
    run(&hub, &oidc, "oidc");

    let service = Service::new(hub, "cf".to_owned());
    assert_eq!(selected(&service).await.unwrap(), "oidc");
}

#[tokio::test]
async fn no_running_plugin_is_not_found_and_is_retried_once_one_runs() {
    let registry = Arc::new(MockTypesRegistry::new());
    let hub = hub_with(&registry);
    let id = publish(&registry, "cf.builtin.late.plugin.v1", "cf", 0);

    let service = Service::new(Arc::clone(&hub), "cf".to_owned());
    assert!(matches!(
        selected(&service).await,
        Err(DomainError::PluginNotFound { .. })
    ));

    run(&hub, &id, "late");
    assert_eq!(selected(&service).await.unwrap(), "late");
}

/// Every page is read: a selection over the first page alone would miss the winner.
#[tokio::test]
async fn a_failed_page_fails_the_selection_and_is_not_cached() {
    let registry = Arc::new(MockTypesRegistry::new());
    let hub = hub_with(&registry);
    for n in 0..60 {
        let id = publish(
            &registry,
            &format!("cf.builtin.a{n:02}.plugin.v1"),
            "cf",
            100,
        );
        run(&hub, &id, "loser");
    }
    let winner = publish(&registry, "cf.builtin.zz.plugin.v1", "cf", 1);
    run(&hub, &winner, "winner");
    registry.inject(
        Fault::on(Call::List)
            .nth(2)
            .fail(CanonicalError::service_unavailable().create()),
    );

    let service = Service::new(hub, "cf".to_owned());
    assert!(matches!(
        selected(&service).await,
        Err(DomainError::TypesRegistryUnavailable(_))
    ));
    assert_eq!(selected(&service).await.unwrap(), "winner");
    let lists = registry.calls(Call::List);
    assert_eq!(selected(&service).await.unwrap(), "winner");
    assert_eq!(registry.calls(Call::List), lists, "a selection is cached");
}

#[tokio::test]
async fn without_the_platform_api_the_selection_fails_closed() {
    let hub = Arc::new(ClientHub::new());
    let id = PluginV1::<AuthNResolverPluginSpecV1>::build_registration(
        "cf.builtin.orphan.plugin.v1",
        "cf",
        0,
    )
    .unwrap()
    .0;
    run(&hub, id.as_ref(), "orphan");

    let service = Service::new(hub, "cf".to_owned());
    assert!(matches!(
        selected(&service).await,
        Err(DomainError::TypesRegistryUnavailable(_))
    ));
}

/// Discovery timeout bounds queued calls, caches no failure, and permits a later retry.
#[tokio::test(start_paused = true)]
async fn a_stalled_registry_fails_the_selection_at_the_discovery_budget() {
    let registry = Arc::new(MockTypesRegistry::new());
    let hub = hub_with(&registry);
    let id = publish(&registry, "cf.builtin.late.plugin.v1", "cf", 0);
    run(&hub, &id, "late");
    registry.inject(
        Fault::on(Call::List)
            .first(2)
            .delay(Duration::from_secs(3600)),
    );
    let service = Arc::new(Service::new(hub, "cf".to_owned()));

    let first = tokio::spawn({
        let service = Arc::clone(&service);
        async move { selected(&service).await }
    });
    // The first call now holds the selector, stalled in the listing.
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(10)).await;
    let queued_at = tokio::time::Instant::now();
    let queued = selected(&service).await;
    assert_eq!(queued_at.elapsed(), DISCOVERY_BUDGET, "the wait counts");
    for result in [first.await.unwrap(), queued] {
        assert!(
            matches!(result, Err(DomainError::TypesRegistryUnavailable(_))),
            "{result:?}"
        );
    }
    assert_eq!(registry.calls(Call::List), 2);
    assert_eq!(selected(&service).await.unwrap(), "late");
}

/// Only an availability failure of the registry is an outage, retryable at the contract;
/// anything else it answers stays internal.
#[tokio::test]
async fn registry_failures_are_outages_only_when_retryable() {
    for (error, outage) in outages()
        .into_iter()
        .map(|error| (error, true))
        .chain(non_outages().into_iter().map(|error| (error, false)))
    {
        let registry = Arc::new(MockTypesRegistry::new());
        let hub = hub_with(&registry);
        let id = publish(&registry, "cf.builtin.any.plugin.v1", "cf", 0);
        run(&hub, &id, "any");
        registry.inject(Fault::on(Call::List).fail(error.clone()));

        let result = selected(&Service::new(hub, "cf".to_owned())).await;
        let summary = format!("{error:?} -> {result:?}");
        let domain_outage = matches!(result, Err(DomainError::TypesRegistryUnavailable(_)));
        let domain_internal = matches!(result, Err(DomainError::Internal(_)));
        let contract = result.err().map(AuthNResolverError::from);
        if outage {
            assert!(domain_outage, "{summary}");
            assert!(
                matches!(contract, Some(AuthNResolverError::ServiceUnavailable(_))),
                "{summary} -> {contract:?}"
            );
        } else {
            assert!(domain_internal, "{summary}");
            assert!(
                matches!(contract, Some(AuthNResolverError::Internal(_))),
                "{summary} -> {contract:?}"
            );
        }
    }
}
