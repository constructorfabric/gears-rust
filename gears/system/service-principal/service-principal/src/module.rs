//! `toolkit` gear wiring for the service-principal REST facade: resolve the PDP
//! at init, hold the `ClientHub` for lazy SPI resolution, and mount the REST
//! surface. No DB, no background lifecycle — a thin authorizing facade.

use std::sync::{Arc, OnceLock};

use anyhow::anyhow;
use async_trait::async_trait;
use authz_resolver_sdk::{AuthZResolverClient, pep::PolicyEnforcer};
use toolkit::api::OpenApiRegistry;
use toolkit::{Gear, GearCtx, RestApiCapability};
use tracing::info;

use crate::domain::service::Service;

#[toolkit::gear(
    name = "service-principal",
    capabilities = [rest],
    deps = [types_registry, authz_resolver],
)]
pub struct ServicePrincipalGear {
    service: OnceLock<Arc<Service>>,
}

impl Default for ServicePrincipalGear {
    fn default() -> Self {
        Self {
            service: OnceLock::new(),
        }
    }
}

#[async_trait]
impl Gear for ServicePrincipalGear {
    #[tracing::instrument(skip_all, fields(module = "service-principal"))]
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        // PEP boundary: resolve the PDP (authz-resolver) hard dependency. No
        // `TenantHierarchy` capability is advertised, so the PDP pre-expands a
        // subtree grant into a flat allowed-tenant list — this DB-less gear needs
        // no local tenant-closure to authorize a descendant tenant.
        let authz = ctx
            .client_hub()
            .get::<dyn AuthZResolverClient>()
            .map_err(|e| anyhow!("service-principal requires an authz-resolver client: {e}"))?;
        let enforcer = PolicyEnforcer::new(authz);

        // The SPI provider (an IdP adapter) is intentionally NOT a gear dep — it is
        // resolved lazily per request from the ClientHub so adapters stay pluggable.
        let svc = Arc::new(Service::new(enforcer, ctx.client_hub()));
        self.service
            .set(svc)
            .map_err(|_| anyhow!("{} module already initialized", Self::MODULE_NAME))?;

        info!("service-principal module initialized");
        Ok(())
    }
}

impl RestApiCapability for ServicePrincipalGear {
    fn register_rest(
        &self,
        _ctx: &GearCtx,
        router: axum::Router,
        openapi: &dyn OpenApiRegistry,
    ) -> anyhow::Result<axum::Router> {
        let svc = self
            .service
            .get()
            .cloned()
            .ok_or_else(|| anyhow!("service-principal Service not initialized"))?;
        Ok(crate::api::rest::register_routes(router, openapi, svc))
    }
}
