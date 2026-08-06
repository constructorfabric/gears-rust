//! `EventBrokerModule`: the `ModKit` gear that will wire Ingest/Delivery/
//! Dispatcher/Reaper per deployment mode (`DESIGN.md:2224`'s Deployment
//! Modes table; `docs/ADR/0007-service-decomposition.md`).
//!
//! `init` resolves and stores the configured [`DeploymentMode`], whose
//! `*_active()` predicates report which services/routes *should* exist for
//! that mode. Dispatcher forwarding routes register when
//! `dispatcher_active()` (`eb-dispatcher-routing`); ingest/delivery service
//! construction and handler bodies still land with #4346/#4347. `serve()`
//! self-registers with `ServiceDiscoveryV1` in `cluster_ingest`/
//! `cluster_delivery` mode (design.md D4/D5) and otherwise starts no
//! background work yet.

use std::net::SocketAddr;
use std::sync::{Arc, OnceLock};

use anyhow::Context;
use async_trait::async_trait;
use axum::Extension;
use cluster_sdk::{ServiceHandle, ServiceRegistration};
use tokio_util::sync::CancellationToken;
use toolkit::api::OpenApiRegistry;
use toolkit::client_hub::ClientHub;
use toolkit::{Gear, GearCtx, RestApiCapability};

use crate::api::rest::routes;
use crate::config::{DeploymentMode, EventBrokerConfig, RegistrationConfig};
use crate::domain::cluster::EventBrokerCluster;
use crate::infra::cluster::{AdvertiseAddressResolver, ConfigAdvertiseAddress};
use crate::infra::dispatcher::DispatcherState;

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
    client_hub: OnceLock<Arc<ClientHub>>,
    registration: OnceLock<RegistrationConfig>,
}

#[async_trait]
impl Gear for EventBrokerModule {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        let cfg: EventBrokerConfig = ctx.config()?;
        self.mode
            .set(cfg.mode)
            .map_err(|_| anyhow::anyhow!("{} module already initialized", Self::MODULE_NAME))?;
        self.client_hub
            .set(ctx.client_hub())
            .map_err(|_| anyhow::anyhow!("{} module already initialized", Self::MODULE_NAME))?;
        if matches!(
            cfg.mode,
            DeploymentMode::ClusterIngest | DeploymentMode::ClusterDelivery
        ) {
            let bound_addr: SocketAddr =
                cfg.registration.listen_addr.parse().with_context(|| {
                    format!(
                        "invalid registration.listen_addr '{}'",
                        cfg.registration.listen_addr
                    )
                })?;
            // Fail fast at startup rather than only once `serve()` actually
            // registers (design.md D5) - the resolved address itself is
            // discarded; `register_self()` recomputes it (a pure, cheap
            // string operation, not worth caching across the two calls).
            ConfigAdvertiseAddress {
                config: &cfg.registration,
            }
            .resolve(bound_addr)?;
        }
        self.registration
            .set(cfg.registration)
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
    /// Dispatcher forwarding routes register when `dispatcher_active()`
    /// (`eb-dispatcher-routing`). Ingest/delivery routes remain unregistered
    /// (their handler bodies land with #4346).
    ///
    /// Resolves `EventBrokerCluster` once and attaches it (plus a shared
    /// Pingora connector) via `Extension` after registration, matching the
    /// "attach service once after all routes are registered" convention
    /// (`docs/toolkit_unified_system/04_rest_operation_builder.md`).
    fn register_rest(
        &self,
        ctx: &GearCtx,
        router: axum::Router,
        openapi: &dyn OpenApiRegistry,
    ) -> anyhow::Result<axum::Router> {
        let mode = *self
            .mode
            .get()
            .ok_or_else(|| anyhow::anyhow!("init must run before register_rest()"))?;
        if !mode.dispatcher_active() {
            return Ok(router);
        }
        let router = routes::register_dispatcher_routes(router, openapi);
        let cluster = EventBrokerCluster::resolve(&ctx.client_hub())?;
        let state = Arc::new(DispatcherState::new(cluster));
        Ok(router.layer(Extension(state)))
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
    /// Self-registers with `ServiceDiscoveryV1` in `cluster_ingest`/
    /// `cluster_delivery` mode (design.md D4), holding the `ServiceHandle`
    /// for this function's lifetime so the instance lapses via TTL on
    /// shutdown. The `reaper` worker lands with #4346/#4347.
    pub(crate) async fn serve(&self, cancel: CancellationToken) -> anyhow::Result<()> {
        let mode = *self
            .mode
            .get()
            .ok_or_else(|| anyhow::anyhow!("init must run before serve()"))?;
        let service_name = match mode {
            DeploymentMode::ClusterIngest => Some("ingest"),
            DeploymentMode::ClusterDelivery => Some("delivery"),
            DeploymentMode::Standalone | DeploymentMode::ClusterDispatcher => None,
        };
        let _registration_handle = match service_name {
            Some(name) => Some(self.register_self(name).await?),
            None => None,
        };
        cancel.cancelled().await;
        Ok(())
    }

    /// Resolves `EventBrokerCluster`, resolves the advertise address
    /// (design.md D5), and registers this instance with `ServiceDiscoveryV1`
    /// under `service_name`.
    async fn register_self(&self, service_name: &str) -> anyhow::Result<ServiceHandle> {
        let hub = self
            .client_hub
            .get()
            .ok_or_else(|| anyhow::anyhow!("init must run before register_self()"))?;
        let registration = self
            .registration
            .get()
            .ok_or_else(|| anyhow::anyhow!("init must run before register_self()"))?;

        let bound_addr: SocketAddr = registration.listen_addr.parse().with_context(|| {
            format!(
                "invalid registration.listen_addr '{}'",
                registration.listen_addr
            )
        })?;
        let address = ConfigAdvertiseAddress {
            config: registration,
        }
        .resolve(bound_addr)?;

        let cluster = EventBrokerCluster::resolve(hub)?;
        let handle = cluster
            .service_discovery
            .register(ServiceRegistration {
                name: service_name.to_owned(),
                instance_id: None,
                address,
                metadata: std::collections::HashMap::new(),
            })
            .await?;
        tracing::info!(
            module = Self::MODULE_NAME,
            service_name,
            "registered with ServiceDiscoveryV1"
        );
        Ok(handle)
    }
}

#[cfg(test)]
#[path = "module_tests.rs"]
mod module_tests;
