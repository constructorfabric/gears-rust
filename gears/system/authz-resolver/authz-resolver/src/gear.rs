//! `AuthZ` resolver gear.

use std::sync::Arc;

use async_trait::async_trait;
use authz_resolver_sdk::AuthZResolverApi;
use toolkit::api::OpenApiRegistry;
use toolkit::context::GearCtx;
use toolkit::{Gear, RestApiCapability};
use toolkit_contract::policy::PolicyStack;
use tracing::info;

use crate::config::AuthZResolverConfig;
use crate::domain::{AuthZResolverLocalClient, Service};

/// `AuthZ` Resolver gear.
///
/// This gear:
/// 1. Discovers plugin instances via types-registry
/// 2. Routes requests to the selected plugin based on vendor configuration
///
/// The registry is consumed, not linked: `PlatformTypesRegistryApi` is the registry's own
/// local client where the registry runs in this process, and its resolving REST client
/// otherwise. The `AuthZResolverPluginSpecV1` schema reaches the registry through the
/// `toolkit-gts` inventory it seeds from. Plugin discovery is lazy: it happens on the
/// first evaluation, never in `init`, so no registry call waits on this gear's phase.
///
/// `AuthZResolverApi` is exposed as a `#[toolkit::contract]`: `provides`
/// auto-wires the local impl into `ClientHub`, and the `rest` capability hosts
/// the contract's REST projection (`/authz-resolver/v1/evaluate`, an internal
/// tenant-plane authenticated route that is not edge-exposed) so out-of-process
/// PEPs can reach the PDP over HTTP via directory resolution.
#[toolkit::gear(
    name = "authz-resolver",
    capabilities = [rest],
    gts(crates = [authz_resolver_sdk]),
)]
#[toolkit::provides(
    contract = authz_resolver_sdk::AuthZResolverApi,
    local = Self::build_local,
    transports = [local, rest],
)]
#[derive(Default)]
pub(crate) struct AuthZResolver;

impl AuthZResolver {
    /// Local factory invoked by `#[toolkit::provides]` when wiring resolves to
    /// `ClientWiring::Local` (the in-process default for the provider itself).
    ///
    /// Builds the domain [`Service`] from the gear's config + `ClientHub` and
    /// wraps it in the object-safe [`AuthZResolverLocalClient`].
    fn build_local(
        ctx: &GearCtx,
        _policies: Arc<PolicyStack>,
    ) -> anyhow::Result<Arc<dyn AuthZResolverApi>> {
        let cfg: AuthZResolverConfig = ctx.config_or_default()?;
        info!(vendor = %cfg.vendor, "wiring authz-resolver local client");
        let svc = Arc::new(Service::new(ctx.client_hub(), cfg.vendor));
        Ok(Arc::new(AuthZResolverLocalClient::new(svc)))
    }
}

#[async_trait]
impl Gear for AuthZResolver {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        // `#[toolkit::provides]`-generated wiring: validates the contract IR,
        // reads wiring config, and registers `Arc<dyn AuthZResolverApi>` in
        // the ClientHub (local impl by default).
        self.wire_auth_z_resolver_api(ctx).await?;
        Ok(())
    }
}

impl RestApiCapability for AuthZResolver {
    fn register_rest(
        &self,
        ctx: &GearCtx,
        router: axum::Router,
        openapi: &dyn OpenApiRegistry,
    ) -> anyhow::Result<axum::Router> {
        // Host the contract's REST projection (internal, non-edge-exposed
        // route; see the struct docs).
        let service = ctx.client_hub().get::<dyn AuthZResolverApi>()?;
        Ok(
            authz_resolver_sdk::rest::register_auth_z_resolver_api_rest_routes(
                router, openapi, service,
            ),
        )
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use tokio_util::sync::CancellationToken;
    use toolkit::api::{OpenApiRegistry, OpenApiRegistryImpl};
    use toolkit::client_hub::{ClientHub, ClientHubError};
    use toolkit::config::ConfigProvider;
    use uuid::Uuid;

    use super::*;

    /// Config provider with no gear sections: `config_or_default` falls back to
    /// `AuthZResolverConfig::default()`, and `#[toolkit::provides]` wiring
    /// defaults to `ClientWiring::Local`.
    struct EmptyConfig;
    impl ConfigProvider for EmptyConfig {
        fn get_gear_config(&self, _gear_name: &str) -> Option<&serde_json::Value> {
            None
        }
    }

    fn test_ctx() -> GearCtx {
        GearCtx::new(
            "authz-resolver",
            Uuid::nil(),
            Arc::new(EmptyConfig),
            Arc::new(ClientHub::default()),
            CancellationToken::new(),
        )
    }

    /// `init` runs the `provides`-generated wiring: with no wiring config it
    /// resolves to `Local`, invokes `build_local`, and registers
    /// `Arc<dyn AuthZResolverApi>` in the `ClientHub`. `register_rest` then
    /// resolves that client and mounts the REST projection. One test drives
    /// `build_local`, `init`, and `register_rest`.
    #[tokio::test]
    async fn init_wires_local_client_then_register_rest_succeeds() {
        let gear = AuthZResolver;
        let ctx = test_ctx();

        gear.init(&ctx)
            .await
            .expect("init should wire local client");

        // Local wiring must have registered the contract in the shared hub.
        ctx.client_hub()
            .get::<dyn AuthZResolverApi>()
            .expect("AuthZResolverApi must be registered after init");

        // The REST projection registers against the resolved client.
        let openapi: &dyn OpenApiRegistry = &OpenApiRegistryImpl::new();
        let _router = gear
            .register_rest(&ctx, axum::Router::new(), openapi)
            .expect("register_rest should mount the evaluate route");
    }

    /// Without a prior `init`, the contract is absent from the hub and
    /// `register_rest` fails fast rather than mounting a route with no backend.
    #[test]
    fn register_rest_errors_when_contract_unregistered() {
        let gear = AuthZResolver;
        let ctx = test_ctx();

        let openapi: &dyn OpenApiRegistry = &OpenApiRegistryImpl::new();
        let err = gear
            .register_rest(&ctx, axum::Router::new(), openapi)
            .expect_err("register_rest must fail when the client is unregistered");
        assert!(
            matches!(
                err.downcast_ref::<ClientHubError>(),
                Some(ClientHubError::NotFound { .. })
            ),
            "expected ClientHubError::NotFound, got: {err:?}"
        );
    }
}

/// The GTS publication `gts(...)` declares: exactly the SDK's declarations, under
/// this gear's name and version, independent of configuration.
#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[allow(clippy::expect_used)]
mod gts_publication_tests {
    use toolkit::config::ConfigProvider;
    use toolkit::gts_publication::GtsPublicationCapability as _;

    use super::*;

    struct NoConfig;

    impl ConfigProvider for NoConfig {
        fn get_gear_config(&self, _gear: &str) -> Option<&serde_json::Value> {
            None
        }
    }

    #[test]
    fn the_manifest_is_the_sdk_declarations() {
        let manifest = AuthZResolver
            .descriptor()
            .build_gts_manifest(&NoConfig)
            .expect("manifest");

        assert_eq!(manifest.publisher_name(), "authz-resolver");
        assert_eq!(manifest.publisher_version(), env!("CARGO_PKG_VERSION"));
        let ids: Vec<&str> = manifest
            .documents()
            .iter()
            .map(|(id, _)| id.as_str())
            .collect();
        let declared: Vec<String> = authz_resolver_sdk::gts_declarations()
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        assert_eq!(ids, declared);
        assert!(
            ids.contains(&authz_resolver_sdk::AuthZResolverPluginSpecV1::gts_type_id().as_ref()),
            "{ids:?}"
        );
    }
}
