//! License resolver gear.

use std::sync::Arc;

use async_trait::async_trait;
use license_resolver_sdk::{LicenseResolverApi, PlatformLicenseResolverApi};
use toolkit::api::OpenApiRegistry;
use toolkit::context::GearCtx;
use toolkit::{ClientHub, Gear, RestApiCapability};
use toolkit_contract::policy::PolicyStack;
use tracing::info;

use crate::config::LicenseResolverConfig;
use crate::domain::{ContractValidator, LicenseResolverLocalClient, PrincipalMapper, Service};
use crate::infra::{GtsContractRegistry, metrics};

/// License Resolver gear.
///
/// The licensing base types and the `LicenseResolverPluginSpecV1` schema reach
/// `types-registry` on their own via the `toolkit-gts` link-time inventory — no
/// per-init registration is needed here.
///
/// Both public contracts are `#[toolkit::contract]`s served by one
/// [`LicenseResolverLocalClient`]: `provides` wires it into `ClientHub` for each,
/// and the `rest` capability hosts their REST projections
/// (`/license-resolver/v1/platform/is-licensed`, internal, and
/// `/license-resolver/v1/is-licensed`, exposed to any authenticated principal).
/// The gear always wires them `Local`: a remote `clients.provides` entry for
/// either is a boot error ([`refuse_remote_wiring`]).
#[toolkit::gear(
    name = "license-resolver",
    deps = [types_registry],
    capabilities = [rest]
)]
#[toolkit::provides(
    contract = license_resolver_sdk::PlatformLicenseResolverApi,
    local = Self::build_platform,
    transports = [local, rest],
)]
#[toolkit::provides(
    contract = license_resolver_sdk::LicenseResolverApi,
    local = Self::build_tenant,
    transports = [local, rest],
)]
#[derive(Default)]
pub(crate) struct LicenseResolver;

impl LicenseResolver {
    fn build_client(ctx: &GearCtx) -> anyhow::Result<Arc<LicenseResolverLocalClient>> {
        let cfg: LicenseResolverConfig = ctx.config_or_default()?;
        info!(vendor = %cfg.vendor, "wiring license-resolver local client");

        let hub = ctx.client_hub();
        let registry = Arc::new(GtsContractRegistry::new(hub.clone()));
        let svc = Arc::new(Service::new(
            hub,
            cfg.vendor.clone(),
            ContractValidator::new(registry.clone()),
            PrincipalMapper::new(cfg.principal_subject_types.clone(), registry),
            metrics::build_default_adapter(&cfg.vendor),
        ));
        Ok(Arc::new(LicenseResolverLocalClient::new(svc)))
    }

    /// The client `init` holds in the hub while it wires the two contracts: the
    /// `provides` factories receive a context, not the gear.
    fn held(ctx: &GearCtx) -> anyhow::Result<Arc<LicenseResolverLocalClient>> {
        ctx.client_hub()
            .get::<LicenseResolverLocalClient>()
            .map_err(|_| anyhow::anyhow!("license-resolver wires its APIs only from init"))
    }

    /// `provides` factory: the platform API.
    fn build_platform(
        ctx: &GearCtx,
        _policies: Arc<PolicyStack>,
    ) -> anyhow::Result<Arc<dyn PlatformLicenseResolverApi>> {
        Ok(Self::held(ctx)?)
    }

    /// `provides` factory: the tenant API over the same client.
    fn build_tenant(
        ctx: &GearCtx,
        _policies: Arc<PolicyStack>,
    ) -> anyhow::Result<Arc<dyn LicenseResolverApi>> {
        Ok(Self::held(ctx)?)
    }
}

#[async_trait]
impl Gear for LicenseResolver {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        refuse_remote_wiring(ctx.config_provider(), ctx.gear_name())?;
        held_while(&ctx.client_hub(), Self::build_client(ctx)?, async {
            self.wire_platform_license_resolver_api(ctx).await?;
            self.wire_license_resolver_api(ctx).await
        })
        .await
    }
}

impl RestApiCapability for LicenseResolver {
    fn register_rest(
        &self,
        ctx: &GearCtx,
        router: axum::Router,
        openapi: &dyn OpenApiRegistry,
    ) -> anyhow::Result<axum::Router> {
        let hub = ctx.client_hub();
        let router = license_resolver_sdk::rest::register_platform_license_resolver_api_rest_routes(
            router,
            openapi,
            hub.get::<dyn PlatformLicenseResolverApi>()?,
        );
        Ok(
            license_resolver_sdk::rest::register_license_resolver_api_rest_routes(
                router,
                openapi,
                hub.get::<dyn LicenseResolverApi>()?,
            ),
        )
    }
}

/// Register `value` in `hub` for the duration of `wire` only, and return what `wire`
/// returns: the entry is gone afterwards, whether wiring succeeded, failed, panicked or
/// was dropped unfinished.
async fn held_while<T: Send + Sync + 'static>(
    hub: &ClientHub,
    value: Arc<T>,
    wire: impl Future<Output = anyhow::Result<()>>,
) -> anyhow::Result<()> {
    /// Removes the held `T` from the hub when dropped.
    struct Held<'a, T: Send + Sync + 'static>(&'a ClientHub, std::marker::PhantomData<T>);

    impl<T: Send + Sync + 'static> Drop for Held<'_, T> {
        fn drop(&mut self) {
            self.0.remove::<T>();
        }
    }

    hub.register::<T>(value);
    let _held = Held::<T>(hub, std::marker::PhantomData);
    wire.await
}

const PROVIDED_CONTRACTS: [&str; 2] = ["platform_license_resolver_api", "license_resolver_api"];

/// Refuse a `clients.provides` entry that binds either contract remotely: the
/// gear serves both itself, and a remote binding would point it at itself.
fn refuse_remote_wiring(
    cfg: &dyn toolkit::config::ConfigProvider,
    gear: &str,
) -> anyhow::Result<()> {
    for key in PROVIDED_CONTRACTS {
        if !matches!(
            toolkit::clients_config::provided_wiring(cfg, gear, key)?,
            toolkit::clients_config::ClientWiring::Local
        ) {
            anyhow::bail!(
                "gears.{gear}.clients.provides.{key}: license-resolver serves this contract itself; \
                 remove the entry or set transport `local`"
            );
        }
    }
    Ok(())
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "gear_tests.rs"]
mod gear_tests;
