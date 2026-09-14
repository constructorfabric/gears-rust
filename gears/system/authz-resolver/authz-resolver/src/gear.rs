//! `AuthZ` resolver gear.

use std::sync::Arc;

use async_trait::async_trait;
use authz_resolver_sdk::AuthZResolverApi;
use toolkit::api::OpenApiRegistry;
use toolkit::context::GearCtx;
use toolkit::contracts::SystemCapability;
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
/// The `AuthZResolverPluginSpecV1` schema itself reaches `types-registry`
/// automatically via the `toolkit-gts` link-time inventory — no per-init
/// registration is needed. Plugin discovery is lazy: happens on first API
/// call after types-registry is ready.
///
/// `AuthZResolverApi` is exposed as a `#[toolkit::contract]`: `provides`
/// auto-wires the local impl into `ClientHub`, and the `rest` capability hosts
/// the contract's REST projection (`/authz-resolver/v1/evaluate`, an internal
/// tenant-plane authenticated route that is not edge-exposed) so out-of-process
/// PEPs can reach the PDP over HTTP via directory resolution.
#[toolkit::gear(
    name = "authz-resolver",
    deps = [types_registry],
    capabilities = [system, rest]
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

// Marked as `system` so that init() runs in the system-gear phase.
// This ensures the AuthZResolver client is available in ClientHub before
// other system gears that depend on it.
impl SystemCapability for AuthZResolver {}

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
