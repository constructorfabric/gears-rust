//! License resolver gear.

use std::sync::Arc;

use async_trait::async_trait;
use license_resolver_sdk::LicenseResolverApi;
use toolkit::api::OpenApiRegistry;
use toolkit::context::GearCtx;
use toolkit::contracts::SystemCapability;
use toolkit::{Gear, RestApiCapability};
use toolkit_contract::policy::PolicyStack;
use tracing::info;

use crate::config::LicenseResolverConfig;
use crate::domain::{ContractValidator, LicenseResolverLocalClient, Service};
use crate::infra::{GtsContractRegistry, metrics};

/// License Resolver gear.
///
/// The licensing base types and the `LicenseResolverPluginSpecV1` schema reach
/// `types-registry` on their own via the `toolkit-gts` link-time inventory — no
/// per-init registration is needed here.
///
/// `LicenseResolverApi` is a `#[toolkit::contract]`: `provides` wires the local
/// impl into `ClientHub`, and the `rest` capability hosts its REST projection
/// (`/license-resolver/v1/is-licensed`, internal, not edge-exposed).
#[toolkit::gear(
    name = "license-resolver",
    deps = [types_registry],
    capabilities = [system, rest]
)]
#[toolkit::provides(
    contract = license_resolver_sdk::LicenseResolverApi,
    local = Self::build_local,
    transports = [local, rest],
)]
#[derive(Default)]
pub(crate) struct LicenseResolver;

impl LicenseResolver {
    fn build_local(
        ctx: &GearCtx,
        _policies: Arc<PolicyStack>,
    ) -> anyhow::Result<Arc<dyn LicenseResolverApi>> {
        let cfg: LicenseResolverConfig = ctx.config_or_default()?;
        info!(vendor = %cfg.vendor, "wiring license-resolver local client");

        let hub = ctx.client_hub();
        let validator = ContractValidator::new(Arc::new(GtsContractRegistry::new(hub.clone())));
        let svc = Arc::new(Service::new(
            hub,
            cfg.vendor.clone(),
            validator,
            metrics::build_default_adapter(&cfg.vendor),
        ));
        Ok(Arc::new(LicenseResolverLocalClient::new(svc)))
    }
}

// Marked as `system` so that init() runs in the system-gear phase, making the
// client available in ClientHub before other system gears that gate on a license.
impl SystemCapability for LicenseResolver {}

#[async_trait]
impl Gear for LicenseResolver {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        self.wire_license_resolver_api(ctx).await?;
        Ok(())
    }
}

impl RestApiCapability for LicenseResolver {
    fn register_rest(
        &self,
        ctx: &GearCtx,
        router: axum::Router,
        openapi: &dyn OpenApiRegistry,
    ) -> anyhow::Result<axum::Router> {
        let service = ctx.client_hub().get::<dyn LicenseResolverApi>()?;
        Ok(
            license_resolver_sdk::rest::register_license_resolver_api_rest_routes(
                router, openapi, service,
            ),
        )
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "gear_tests.rs"]
mod gear_tests;
