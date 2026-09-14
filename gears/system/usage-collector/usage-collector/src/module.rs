//! `usage-collector` module.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use anyhow::Context;
use async_trait::async_trait;
use authz_resolver_sdk::{AuthZResolverApi, PolicyEnforcer};
use tokio_util::sync::CancellationToken;
use toolkit::api::OpenApiRegistry;
use toolkit::{Gear, GearCtx, RestApiCapability};
use tracing::info;
use usage_collector_sdk::UsageCollectorClientV1;

/// Interval between `uc_usage_types` gauge refreshes. Matches the platform
/// inventory-gauge cadence (rbac) and the OTLP export interval.
const USAGE_TYPES_GAUGE_REFRESH_INTERVAL: Duration = Duration::from_mins(1);

use crate::api::rest::routes as rest_routes;
use crate::config::UsageCollectorConfig;
use crate::domain::ports::metrics::UsageCollectorMetrics;
use crate::domain::{Service, UsageCollectorLocalClient};

/// Usage Collector gateway module.
///
/// This module:
/// 1. Reads the `[usage-collector]` configuration once at `init` (vendor
///    binding only — the usage-type catalog is plugin-owned per ADR-0012).
/// 2. Resolves the PDP (`authz-resolver`) hard dependency and builds a
///    [`PolicyEnforcer`].
/// 3. Constructs the domain [`Service`] (embedded `GtsPluginSelector` for
///    lazy storage-plugin resolution; PDP enforcer is passed in at
///    construction).
/// 4. Registers `Arc<dyn UsageCollectorClientV1>` in `ClientHub` for in-process
///    consumers.
///
/// Per ADR-0012 the durable `usage_type_catalog` and `usage_records` rows
/// both live in the bound storage plugin's backend, so the gateway hosts no
/// database of its own and declares no `db` capability.
///
/// The `UsageCollectorPluginSpecV1` schema itself reaches `types-registry`
/// automatically via the `toolkit-gts` link-time inventory — no per-init
/// registration is needed.
#[toolkit::gear(
    name = "usage-collector",
    deps = [types_registry, authz_resolver],
    capabilities = [rest, stateful],
    lifecycle(entry = "serve", stop_timeout = "30s")
)]
#[derive(Default)]
pub struct UsageCollectorModule {
    service: OnceLock<Arc<Service>>,
}

#[async_trait]
impl Gear for UsageCollectorModule {
    #[tracing::instrument(skip_all, fields(vendor))]
    // @cpt-flow:cpt-cf-usage-collector-flow-foundation-plugin-host-binding:p1
    // @cpt-dod:cpt-cf-usage-collector-dod-foundation-constraint-nfr-thresholds:p2
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        // 1. Read-once: `[usage_collector].vendor` is read exactly once here;
        //    changing the binding requires a module restart (no runtime
        //    config-change channel).
        // @cpt-begin:cpt-cf-usage-collector-flow-foundation-plugin-host-binding:p1:inst-binding-config-read-once
        let cfg: UsageCollectorConfig = ctx.config_or_default()?;
        // @cpt-end:cpt-cf-usage-collector-flow-foundation-plugin-host-binding:p1:inst-binding-config-read-once
        cfg.validate()?;
        tracing::Span::current().record("vendor", cfg.vendor.as_str());
        info!(vendor = %cfg.vendor);

        // 2. PEP boundary — resolve the PDP (`authz-resolver`) hard dependency
        //    from ClientHub. The collector fails init if no resolver client is
        //    registered; it never serves a permissive or local authorization
        //    decision per
        //    `cpt-cf-usage-collector-principle-pdp-centric-authorization`.
        // @cpt-dod:cpt-cf-usage-collector-dod-foundation-adr-pdp-centric-authorization:p2
        // @cpt-dod:cpt-cf-usage-collector-dod-foundation-principle-fail-closed:p2
        let authz: Arc<dyn AuthZResolverApi> = ctx
            .client_hub()
            .get::<dyn AuthZResolverApi>()
            .with_context(|| format!("{} requires an authz-resolver client", Self::MODULE_NAME))?;
        let enforcer = PolicyEnforcer::new(authz);
        info!(module = Self::MODULE_NAME, "authz-resolver wired");

        // 2b. Observability substrate — declare the operational instruments on
        //     a scoped `Meter` from ToolKit's global `SdkMeterProvider` (OTLP
        //     push; no gear-local exporter or `/metrics` scrape endpoint) per
        //     `cpt-cf-usage-collector-principle-otlp-push-emission`. The
        //     `authz-resolver` client is bound above, so the PDP-readiness
        //     gauge is a constant `1` post-bootstrap (structural binding fact).
        // @cpt-begin:cpt-cf-usage-collector-flow-foundation-plugin-host-binding:p1:inst-binding-meter-bootstrap
        let metrics = crate::infra::metrics::build_default_adapter(cfg.metrics.effective_prefix());
        // @cpt-begin:cpt-cf-usage-collector-algo-foundation-pdp-authorize:p2:inst-algo-pdp-ready-gauge
        metrics.set_pdp_ready(true);
        // @cpt-end:cpt-cf-usage-collector-algo-foundation-pdp-authorize:p2:inst-algo-pdp-ready-gauge
        // @cpt-end:cpt-cf-usage-collector-flow-foundation-plugin-host-binding:p1:inst-binding-meter-bootstrap

        // 3. Construct the plugin-routing domain service (embeds
        //    `GtsPluginSelector`; no types-registry query at init —
        //    storage-plugin resolution is lazy). All durable catalog rows
        //    live in the bound plugin per ADR-0012; the service routes
        //    catalog SPI calls through
        //    `ClientHub::try_get_scoped::<dyn UsageCollectorPluginV1>`.
        let hub = ctx.client_hub();
        let svc = Service::new_with_metrics(hub, cfg.vendor, enforcer, metrics);

        let svc = Arc::new(svc);
        self.service
            .set(svc.clone())
            .map_err(|_| anyhow::anyhow!("{} module already initialized", Self::MODULE_NAME))?;

        // 4. Register local client in ClientHub for in-process consumers.
        let api: Arc<dyn UsageCollectorClientV1> = Arc::new(UsageCollectorLocalClient::new(svc));
        ctx.client_hub().register::<dyn UsageCollectorClientV1>(api);

        Ok(())
    }
}

impl UsageCollectorModule {
    /// Background lifecycle entry (wired via `lifecycle(entry = "serve")` on the
    /// gear macro): periodically refresh the `uc_usage_types` gauge to the true
    /// catalog count. Runs until the runtime cancels `cancel` at shutdown.
    ///
    /// The refresh is best-effort and bounded inside
    /// [`Service::refresh_usage_types_gauge`]; an unbound plugin (lazy binding)
    /// or a failed read simply leaves the gauge at its prior value and retries
    /// next tick — so the gauge is populated within one interval of plugin
    /// readiness, with no dependence on a create/delete having occurred.
    // @cpt-begin:cpt-cf-usage-collector-flow-foundation-plugin-host-binding:p1:inst-serve-usage-types-gauge-refresh
    pub(crate) async fn serve(&self, cancel: CancellationToken) -> anyhow::Result<()> {
        let svc =
            self.service.get().cloned().ok_or_else(|| {
                anyhow::anyhow!("{}: serve invoked before init", Self::MODULE_NAME)
            })?;
        info!(
            module = Self::MODULE_NAME,
            interval_secs = USAGE_TYPES_GAUGE_REFRESH_INTERVAL.as_secs(),
            "uc_usage_types gauge refresh loop started"
        );
        Self::run_refresh_loop(&svc, &cancel).await;
        info!(
            module = Self::MODULE_NAME,
            "uc_usage_types gauge refresh loop stopping (cancelled)"
        );
        Ok(())
    }

    /// The periodic refresh loop: on each interval tick, refresh the gauge
    /// (raced against `cancel`), and return as soon as `cancel` fires — whether
    /// idle between ticks or mid-refresh.
    async fn run_refresh_loop(svc: &Service, cancel: &CancellationToken) {
        let mut ticker = tokio::time::interval(USAGE_TYPES_GAUGE_REFRESH_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                biased;
                () = cancel.cancelled() => break,
                _ = ticker.tick() => {
                    if Self::refresh_until_cancelled(svc, cancel).await {
                        break;
                    }
                }
            }
        }
    }

    /// Run one gauge refresh, abandoning it the instant `cancel` fires.
    ///
    /// The refresh must be raced against the token — awaiting it bare would not
    /// poll cancellation, and its first (cold) call resolves the storage plugin
    /// via `types-registry`, so a hung/slow resolve would defer shutdown until
    /// the runtime's `stop_timeout` force-aborts. Returns `true` when
    /// cancellation interrupted the refresh (the caller should stop the loop).
    async fn refresh_until_cancelled(svc: &Service, cancel: &CancellationToken) -> bool {
        tokio::select! {
            biased;
            () = cancel.cancelled() => true,
            () = svc.refresh_usage_types_gauge() => false,
        }
    }
    // @cpt-end:cpt-cf-usage-collector-flow-foundation-plugin-host-binding:p1:inst-serve-usage-types-gauge-refresh
}

impl RestApiCapability for UsageCollectorModule {
    /// Mount the FOUNDATION REST surface onto the runtime router.
    ///
    /// Wires the shared substrate execution shape — gateway-resolved
    /// `SecurityContext` acceptance with fail-closed `AuthN`-delegation
    /// rejection, the canonical RFC-9457 `Problem` envelope (via the
    /// host-crate `UsageCollectorError` → `CanonicalError` lift), W3C
    /// trace-context correlation propagation — and registers the four
    /// foundation catalog routes per DESIGN §3.5
    /// (`POST/GET /usage-collector/v1/usage-types`,
    /// `GET/DELETE /usage-collector/v1/usage-types/{gts_id}`). No
    /// module-local health / liveness / readiness / metrics endpoint is
    /// exposed — those are owned by the `ToolKit` host above the module
    /// boundary.
    ///
    /// The runtime calls `register_rest` AFTER `init` per the toolkit
    /// lifecycle contract, so the `OnceLock` read below is infallible in
    /// practice; the `ok_or_else` guard turns a misordered runtime into a
    /// precise bootstrap failure rather than a panic.
    fn register_rest(
        &self,
        _ctx: &GearCtx,
        router: axum::Router,
        openapi: &dyn OpenApiRegistry,
    ) -> anyhow::Result<axum::Router> {
        info!(module = Self::MODULE_NAME, "registering REST surface");

        // The domain Service (plugin binding + PDP/tenant) must be wired
        // before the foundation REST routes mount: every catalog handler
        // dispatches through this service. Fail closed if init did not
        // complete. The in-process SDK surface (via `ClientHub`) wraps the
        // same `Arc<Service>` in `UsageCollectorLocalClient`, so REST and
        // SDK consumers share a single PDP-gated dispatch path through the
        // service per `cpt-cf-usage-collector-component-usage-type-catalog`.
        let service = self.service.get().cloned().ok_or_else(|| {
            anyhow::anyhow!("{} module Service not initialized", Self::MODULE_NAME)
        })?;

        let router = rest_routes::register_routes(router, openapi, service);

        info!(module = Self::MODULE_NAME, "REST surface registered");
        Ok(router)
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "module_tests.rs"]
mod module_tests;
