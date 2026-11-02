use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tenant_resolver_sdk::{
    GetAncestorsOptions, GetAncestorsResponse, GetDescendantsOptions, GetDescendantsResponse,
    GetTenantsOptions, IsAncestorOptions, TenantId, TenantInfo, TenantResolverError,
    TenantResolverPluginClient, TenantResolverPluginSpecV1,
};
use toolkit::client_hub::{ClientHub, ClientScope};
use toolkit::gts::PluginV1;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;
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

/// A plugin backend that is only ever selected, never called.
struct Backend;

#[async_trait]
impl TenantResolverPluginClient for Backend {
    async fn get_tenant(
        &self,
        _: &SecurityContext,
        _: TenantId,
    ) -> Result<TenantInfo, TenantResolverError> {
        Err(TenantResolverError::Unauthorized)
    }
    async fn get_root_tenant(
        &self,
        _: &SecurityContext,
    ) -> Result<TenantInfo, TenantResolverError> {
        Err(TenantResolverError::Unauthorized)
    }
    async fn get_tenants(
        &self,
        _: &SecurityContext,
        _: &[TenantId],
        _: &GetTenantsOptions,
    ) -> Result<Vec<TenantInfo>, TenantResolverError> {
        Err(TenantResolverError::Unauthorized)
    }
    async fn get_ancestors(
        &self,
        _: &SecurityContext,
        _: TenantId,
        _: &GetAncestorsOptions,
    ) -> Result<GetAncestorsResponse, TenantResolverError> {
        Err(TenantResolverError::Unauthorized)
    }
    async fn get_descendants(
        &self,
        _: &SecurityContext,
        _: TenantId,
        _: &GetDescendantsOptions,
    ) -> Result<GetDescendantsResponse, TenantResolverError> {
        Err(TenantResolverError::Unauthorized)
    }
    async fn is_ancestor(
        &self,
        _: &SecurityContext,
        _: TenantId,
        _: TenantId,
        _: &IsAncestorOptions,
    ) -> Result<bool, TenantResolverError> {
        Err(TenantResolverError::Unauthorized)
    }
}

struct Host {
    hub: Arc<ClientHub>,
    registry: Arc<MockTypesRegistry>,
}

impl Host {
    fn new() -> Self {
        let hub = Arc::new(ClientHub::new());
        let registry = Arc::new(MockTypesRegistry::new());
        registry.install(&hub);
        Self { hub, registry }
    }

    /// A stored instance whose plugin did not start in this process.
    fn stored_plugin(&self, segment: &str, vendor: &str, priority: i16) -> String {
        let (id, content) =
            PluginV1::<TenantResolverPluginSpecV1>::build_registration(segment, vendor, priority)
                .expect("plugin instance serializes");
        self.registry.seed(id.as_ref(), content);
        id.as_ref().to_owned()
    }

    /// A stored instance whose plugin registered its scoped backend.
    fn running_plugin(&self, segment: &str, vendor: &str, priority: i16) -> String {
        let id = self.stored_plugin(segment, vendor, priority);
        let backend: Arc<dyn TenantResolverPluginClient> = Arc::new(Backend);
        self.hub
            .register_scoped::<dyn TenantResolverPluginClient>(ClientScope::gts_id(&id), backend);
        id
    }

    fn service(&self) -> Service {
        Service::new(Arc::clone(&self.hub), "acme".to_owned())
    }
}

#[tokio::test]
async fn selects_the_vendor_winner_from_a_later_discovery_page() {
    let host = Host::new();
    // More than one default page (50) of other vendors' instances, sorted before the winner.
    for n in 0..60 {
        host.running_plugin(&format!("cf.test.a{n:02}.plugin.v1"), "other", 1);
    }
    let winner = host.running_plugin("cf.test.zz.plugin.v1", "acme", 10);

    let selected = host
        .service()
        .resolve_plugin()
        .await
        .expect("plugin resolves");

    assert_eq!(selected, winner);
    assert!(
        host.registry.calls(Call::List) >= 2,
        "discovery followed the cursor"
    );
}

#[tokio::test]
async fn keeps_priority_selection_among_the_vendor_instances() {
    let host = Host::new();
    host.running_plugin("cf.test.low.plugin.v1", "acme", 50);
    let preferred = host.running_plugin("cf.test.high.plugin.v1", "acme", 10);
    host.running_plugin("cf.test.foreign.plugin.v1", "other", 0);

    let selected = host
        .service()
        .resolve_plugin()
        .await
        .expect("plugin resolves");

    assert_eq!(selected, preferred);
}

#[tokio::test]
async fn skips_a_stored_instance_whose_plugin_is_not_running_in_this_process() {
    let host = Host::new();
    // Published by an earlier boot with a winning priority, then disabled.
    host.stored_plugin("cf.test.disabled.plugin.v1", "acme", 0);
    let running = host.running_plugin("cf.test.running.plugin.v1", "acme", 100);

    let selected = host
        .service()
        .resolve_plugin()
        .await
        .expect("plugin resolves");

    assert_eq!(selected, running);
}

#[tokio::test]
async fn retries_discovery_once_a_stored_plugin_registers_its_backend() {
    let host = Host::new();
    let id = host.stored_plugin("cf.test.late.plugin.v1", "acme", 10);
    let service = host.service();

    // Through the selector, which caches only a successful selection.
    let error = service
        .selector
        .get_or_init(|| service.resolve_plugin())
        .await
        .expect_err("no running plugin yet");
    assert!(
        matches!(error, DomainError::PluginNotFound { .. }),
        "{error:?}"
    );

    let backend: Arc<dyn TenantResolverPluginClient> = Arc::new(Backend);
    host.hub
        .register_scoped::<dyn TenantResolverPluginClient>(ClientScope::gts_id(&id), backend);

    let selected = service
        .selector
        .get_or_init(|| service.resolve_plugin())
        .await
        .expect("plugin resolves");
    assert_eq!(selected.as_ref(), id);
}

#[tokio::test]
async fn a_failed_discovery_page_fails_resolution_instead_of_selecting_from_a_partial_listing() {
    let host = Host::new();
    for n in 0..60 {
        host.running_plugin(&format!("cf.test.a{n:02}.plugin.v1"), "acme", 100);
    }
    host.running_plugin("cf.test.zz.plugin.v1", "acme", 1);
    host.registry.inject(
        Fault::on(Call::List)
            .nth(2)
            .fail(CanonicalError::service_unavailable().create()),
    );
    let error = host
        .service()
        .resolve_plugin()
        .await
        .expect_err("a failed page must not select from the first page");

    assert!(
        matches!(error, DomainError::TypesRegistryUnavailable(_)),
        "{error:?}"
    );
}

#[tokio::test]
async fn no_instance_of_the_vendor_is_plugin_not_found() {
    let host = Host::new();
    host.running_plugin("cf.test.foreign.plugin.v1", "other", 0);

    let error = host
        .service()
        .resolve_plugin()
        .await
        .expect_err("no acme plugin");

    assert!(
        matches!(error, DomainError::PluginNotFound { .. }),
        "{error:?}"
    );
}

#[tokio::test]
async fn a_host_without_the_platform_registry_api_is_registry_unavailable() {
    let service = Service::new(Arc::new(ClientHub::new()), "acme".to_owned());

    let error = service.resolve_plugin().await.expect_err("no registry");

    assert!(
        matches!(error, DomainError::TypesRegistryUnavailable(_)),
        "{error:?}"
    );
}

/// Whether `service` selects a plugin through its bounded, cached path.
async fn selected(service: &Service) -> Result<(), DomainError> {
    service.get_plugin().await.map(|_| ())
}

/// Discovery timeout bounds queued calls, caches no failure, and permits a later retry.
#[tokio::test(start_paused = true)]
async fn a_stalled_registry_fails_the_selection_at_the_discovery_budget() {
    let host = Host::new();
    host.running_plugin("cf.test.late.plugin.v1", "acme", 0);
    host.registry.inject(
        Fault::on(Call::List)
            .first(2)
            .delay(Duration::from_secs(3600)),
    );
    let service = Arc::new(host.service());

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
    assert_eq!(host.registry.calls(Call::List), 2);
    selected(&service)
        .await
        .expect("the registry answers again");
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
        let host = Host::new();
        host.running_plugin("cf.test.any.plugin.v1", "acme", 0);
        host.registry
            .inject(Fault::on(Call::List).fail(error.clone()));

        let result = selected(&host.service()).await;
        let summary = format!("{error:?} -> {result:?}");
        let domain_outage = matches!(result, Err(DomainError::TypesRegistryUnavailable(_)));
        let domain_internal = matches!(result, Err(DomainError::Internal(_)));
        let contract = result.err().map(TenantResolverError::from);
        if outage {
            assert!(domain_outage, "{summary}");
            assert!(
                matches!(contract, Some(TenantResolverError::ServiceUnavailable(_))),
                "{summary} -> {contract:?}"
            );
        } else {
            assert!(domain_internal, "{summary}");
            assert!(
                matches!(contract, Some(TenantResolverError::Internal(_))),
                "{summary} -> {contract:?}"
            );
        }
    }
}
