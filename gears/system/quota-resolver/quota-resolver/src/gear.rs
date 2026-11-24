//! Quota resolver gear.

use std::sync::Arc;

use async_trait::async_trait;
use quota_resolver_sdk::{PlatformQuotaResolverApi, QuotaResolverApi};
use toolkit::api::OpenApiRegistry;
use toolkit::context::GearCtx;
use toolkit::{ClientHub, Gear, RestApiCapability};
use toolkit_contract::policy::PolicyStack;
use tracing::info;

use crate::config::QuotaResolverConfig;
use crate::domain::{QuotaResolverLocalClient, Service};
use crate::infra::metrics;

/// Quota Resolver gear.
///
/// Provides the platform and the tenant contract, each locally and over REST.
/// The platform routes are internal; only the tenant `quota-state` route is
/// exposed. The SDK's GTS types reach types-registry through the gear's GTS
/// publication. The gear always wires both contracts `Local`: a remote
/// `clients.provides` entry for either is a boot error
/// ([`refuse_remote_wiring`]).
#[toolkit::gear(
    name = "quota-resolver",
    deps = [types_registry],
    capabilities = [rest],
    gts_publication(crates = [quota_resolver_sdk]),
)]
#[toolkit::provides(
    contract = quota_resolver_sdk::PlatformQuotaResolverApi,
    local = Self::build_platform,
    transports = [local, rest],
)]
#[toolkit::provides(
    contract = quota_resolver_sdk::QuotaResolverApi,
    local = Self::build_tenant,
    transports = [local, rest],
)]
#[derive(Default)]
pub(crate) struct QuotaResolver;

impl QuotaResolver {
    /// Both contracts share one service, so they share the plugin selection and
    /// the mapping memo.
    fn build_client(ctx: &GearCtx) -> anyhow::Result<Arc<QuotaResolverLocalClient>> {
        let cfg: QuotaResolverConfig = ctx.config_or_default()?;
        cfg.validate()?;
        info!(vendor = %cfg.vendor, "wiring quota-resolver local client");
        let svc = Arc::new(Service::new(
            ctx.client_hub(),
            cfg.vendor,
            cfg.principal_subject_types,
            metrics::build_default_adapter(),
        ));
        Ok(Arc::new(QuotaResolverLocalClient::new(svc)))
    }

    /// The client `init` holds in the hub while it wires the two contracts: the
    /// `provides` factories receive a context, not the gear.
    fn held(ctx: &GearCtx) -> anyhow::Result<Arc<QuotaResolverLocalClient>> {
        ctx.client_hub()
            .get::<QuotaResolverLocalClient>()
            .map_err(|_| anyhow::anyhow!("quota-resolver wires its APIs only from init"))
    }

    /// `provides` factory: the platform API.
    fn build_platform(
        ctx: &GearCtx,
        _policies: Arc<PolicyStack>,
    ) -> anyhow::Result<Arc<dyn PlatformQuotaResolverApi>> {
        Ok(Self::held(ctx)?)
    }

    /// `provides` factory: the tenant API over the same client.
    fn build_tenant(
        ctx: &GearCtx,
        _policies: Arc<PolicyStack>,
    ) -> anyhow::Result<Arc<dyn QuotaResolverApi>> {
        Ok(Self::held(ctx)?)
    }
}

#[async_trait]
impl Gear for QuotaResolver {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        refuse_remote_wiring(ctx.config_provider(), ctx.gear_name())?;
        held_while(&ctx.client_hub(), Self::build_client(ctx)?, async {
            self.wire_platform_quota_resolver_api(ctx).await?;
            self.wire_quota_resolver_api(ctx).await
        })
        .await
    }
}

impl RestApiCapability for QuotaResolver {
    fn register_rest(
        &self,
        ctx: &GearCtx,
        router: axum::Router,
        openapi: &dyn OpenApiRegistry,
    ) -> anyhow::Result<axum::Router> {
        let hub = ctx.client_hub();
        let router = quota_resolver_sdk::rest::register_platform_quota_resolver_api_rest_routes(
            router,
            openapi,
            hub.get::<dyn PlatformQuotaResolverApi>()?,
        );
        Ok(quota_resolver_sdk::rest::register_quota_state_route(
            router,
            openapi,
            hub.get::<dyn QuotaResolverApi>()?,
        ))
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

const PROVIDED_CONTRACTS: [&str; 2] = ["platform_quota_resolver_api", "quota_resolver_api"];

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
                "gears.{gear}.clients.provides.{key}: quota-resolver serves this contract itself; \
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
