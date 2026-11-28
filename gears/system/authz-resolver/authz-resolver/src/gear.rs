//! `AuthZ` resolver gear.

use std::sync::Arc;

use async_trait::async_trait;
use authz_resolver_sdk::AuthZResolverApi;
use toolkit::api::{OpenApiRegistry, RestRouteOptions};
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
/// otherwise. This gear publishes the SDK's `AuthZResolverPluginSpecV1` schema before
/// application `init`. Plugin discovery is lazy: it happens on the first evaluation,
/// after every local plugin has bound its admitted instance in `init`.
///
/// `AuthZResolverApi` is exposed as a `#[toolkit::contract]`: `provides`
/// auto-wires the local impl into `ClientHub`, and the `rest` capability hosts
/// the contract's REST projection (`/authz-resolver/v1/evaluate`, an internal
/// platform-plane authenticated route that is not edge-exposed) so out-of-process
/// PEPs can reach the PDP over HTTP via directory resolution.
#[toolkit::consumes(contract = types_registry_sdk::PlatformTypesRegistryApi, from = "types-registry")]
#[toolkit::gear(
    name = "authz-resolver",
    capabilities = [rest],
    gts_publication(crates = [authz_resolver_sdk]),
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
        cfg.validate()?;
        info!(vendor = %cfg.vendor, "wiring authz-resolver local client");
        let svc =
            Arc::new(Service::new(ctx.client_hub(), cfg.vendor).with_batch_config(&cfg.batch)?);
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
        // The batch route gets its own request body limit (`413` above it),
        // tighter than the gateway-wide default; `/evaluate` is unchanged.
        let cfg: AuthZResolverConfig = ctx.config_or_default()?;
        cfg.validate()?;
        let options =
            RestRouteOptions::default().body_limit("evaluate_batch", cfg.batch.max_body_bytes);
        Ok(
            authz_resolver_sdk::rest::register_auth_z_resolver_api_rest_routes_with(
                router, openapi, service, &options,
            )?,
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

    /// A gear section `{ "config": <config> }`, as the runtime provides it.
    struct JsonConfig(serde_json::Value);
    impl ConfigProvider for JsonConfig {
        fn get_gear_config(&self, _gear_name: &str) -> Option<&serde_json::Value> {
            Some(&self.0)
        }
    }

    fn ctx_with(config: &serde_json::Value) -> GearCtx {
        GearCtx::new(
            "authz-resolver",
            Uuid::nil(),
            Arc::new(JsonConfig(serde_json::json!({ "config": config }))),
            Arc::new(ClientHub::default()),
            CancellationToken::new(),
        )
    }

    fn batch_of(items: usize) -> authz_resolver_sdk::EvaluationBatchRequest {
        serde_json::from_value(serde_json::json!({
            "evaluations": vec![serde_json::json!({}); items]
        }))
        .unwrap()
    }

    /// An out-of-range batch bound fails startup instead of being clamped.
    #[tokio::test]
    async fn an_out_of_range_batch_config_fails_init() {
        let ctx = ctx_with(&serde_json::json!({ "batch": { "max_items": 0 } }));
        let err = AuthZResolver
            .init(&ctx)
            .await
            .expect_err("max_items = 0 is out of range");
        assert!(format!("{err:#}").contains("max_items"), "{err:#}");
    }

    /// The configured item limit reaches the wired client: an oversized batch
    /// is rejected (`400`) before any plugin lookup.
    #[tokio::test]
    async fn the_configured_item_limit_is_enforced_by_the_wired_client() {
        let ctx = ctx_with(&serde_json::json!({ "batch": { "max_items": 1 } }));
        AuthZResolver.init(&ctx).await.unwrap();
        let client = ctx.client_hub().get::<dyn AuthZResolverApi>().unwrap();
        let err = client
            .evaluate_batch(
                toolkit_security::PlatformSecurityContext::outbound_marker(),
                batch_of(2),
            )
            .await
            .unwrap_err();
        assert!(
            matches!(
                err,
                toolkit_canonical_errors::CanonicalError::InvalidArgument { .. }
            ),
            "{err:?}"
        );
    }

    /// The configured body limit reaches the batch route (`413`) and leaves
    /// `/evaluate` on the default limit.
    #[tokio::test]
    async fn the_configured_body_limit_reaches_the_batch_route_only() {
        use tower::ServiceExt as _;

        let ctx = ctx_with(&serde_json::json!({ "batch": { "max_body_bytes": 64 } }));
        AuthZResolver.init(&ctx).await.unwrap();
        let openapi: &dyn OpenApiRegistry = &OpenApiRegistryImpl::new();
        let router =
            AuthZResolver
                .register_rest(&ctx, axum::Router::new(), openapi)
                .unwrap()
                .layer(axum::middleware::from_fn(
                    |mut req: axum::http::Request<axum::body::Body>,
                     next: axum::middleware::Next| async move {
                        req.extensions_mut()
                            .insert(toolkit_security::PlatformSecurityContext::outbound_marker());
                        next.run(req).await
                    },
                ));
        let post = |path: &str, body: String| {
            axum::http::Request::post(path)
                .header("content-type", "application/json")
                .body(axum::body::Body::from(body))
                .unwrap()
        };
        let oversized = serde_json::to_string(&batch_of(50)).unwrap();
        assert!(oversized.len() > 64);

        let batch = router
            .clone()
            .oneshot(post("/authz-resolver/v1/evaluations", oversized.clone()))
            .await
            .unwrap();
        assert_eq!(batch.status(), axum::http::StatusCode::PAYLOAD_TOO_LARGE);

        // The same size on `/evaluate` gets past the body limit (and fails
        // decoding as an evaluation request instead).
        let single = router
            .oneshot(post("/authz-resolver/v1/evaluate", oversized))
            .await
            .unwrap();
        assert_ne!(single.status(), axum::http::StatusCode::PAYLOAD_TOO_LARGE);
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

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "gts_publication_tests.rs"]
mod gts_publication_tests;
