//! `EventBrokerModule`: the `ModKit` gear that will wire Ingest/Delivery/
//! Dispatcher/Reaper per deployment mode (`DESIGN.md:2224`'s Deployment
//! Modes table; `docs/ADR/0007-service-decomposition.md`).
//!
//! This is structure-only scaffolding: `init` resolves and stores the
//! configured [`DeploymentMode`], whose `*_active()` predicates report
//! which services/routes *should* exist for that mode. No service is
//! constructed, no route is registered, and `serve()` starts no background
//! work yet - those predicates are the gate that #4345 (dispatcher
//! routing), #4346 (REST API), and #4347 (standalone runtime) will branch
//! real construction and registration on.

use std::sync::OnceLock;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;
use toolkit::api::OpenApiRegistry;
use toolkit::{Gear, GearCtx, RestApiCapability};

use crate::config::{DeploymentMode, EventBrokerConfig};

/// Expressed as predicates on [`DeploymentMode`] itself so the activation
/// set can never disagree with the mode it's derived from
/// (`docs/ADR/0007-service-decomposition.md` D6).
impl DeploymentMode {
    /// `DESIGN.md:2224`'s Deployment Modes table, column by column.
    #[must_use]
    pub fn ingest_active(self) -> bool {
        matches!(self, Self::Standalone | Self::ClusterIngest)
    }

    #[must_use]
    pub fn delivery_active(self) -> bool {
        matches!(self, Self::Standalone | Self::ClusterDelivery)
    }

    #[must_use]
    pub fn dispatcher_active(self) -> bool {
        matches!(self, Self::ClusterDispatcher)
    }

    /// The `reaper` worker runs in every mode except `cluster_dispatcher`
    /// (a stateless HTTP gateway has nothing for it to reap).
    #[must_use]
    pub fn reaper_active(self) -> bool {
        !matches!(self, Self::ClusterDispatcher)
    }
}

#[toolkit::gear(
    name = "event-broker",
    deps = [cluster],
    capabilities = [rest, stateful],
    lifecycle(entry = "serve", stop_timeout = "30s")
)]
#[derive(Default)]
pub struct EventBrokerModule {
    mode: OnceLock<DeploymentMode>,
}

#[async_trait]
impl Gear for EventBrokerModule {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        let cfg: EventBrokerConfig = ctx.config()?;
        self.mode
            .set(cfg.mode)
            .map_err(|_| anyhow::anyhow!("{} module already initialized", Self::MODULE_NAME))?;
        tracing::info!(
            module = Self::MODULE_NAME,
            mode = ?cfg.mode,
            "event-broker deployment-mode wiring resolved"
        );
        Ok(())
    }
}

impl RestApiCapability for EventBrokerModule {
    /// No routes are registered yet - handler bodies land with #4346.
    /// `mode()` and its `*_active()` predicates are already the gate future
    /// route registration will branch on.
    fn register_rest(
        &self,
        _ctx: &GearCtx,
        router: axum::Router,
        _openapi: &dyn OpenApiRegistry,
    ) -> anyhow::Result<axum::Router> {
        Ok(router)
    }
}

impl EventBrokerModule {
    /// Crate-internal introspection point for the per-mode gating tests
    /// (`docs/ADR/0007-service-decomposition.md` D6) - not public API.
    #[cfg(test)]
    pub(crate) fn mode(&self) -> DeploymentMode {
        *self.mode.get().expect("init must run before mode()")
    }

    /// Background lifecycle entry point (`lifecycle(entry = "serve")`).
    /// No background work is implemented yet - the `reaper` worker lands
    /// with #4346/#4347. This only honors the gear lifecycle contract by
    /// waiting for shutdown.
    pub(crate) async fn serve(&self, cancel: CancellationToken) -> anyhow::Result<()> {
        cancel.cancelled().await;
        Ok(())
    }
}

#[cfg(test)]
#[path = "module_tests.rs"]
mod module_tests;
