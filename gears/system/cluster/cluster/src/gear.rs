// Created: 2026-06-16 by Constructor Tech
//! The `cluster` gear — the `RunnableCapability` that owns the [`ClusterHandle`]
//! across its lifecycle (DESIGN §3.7, as amended: the wiring library and the host
//! gear are the same crate, matching the platform's one-gear-per-domain layout).
//!
//! `init` captures the hub and parses [`ClusterConfig`]; `start` assembles the
//! provider registry, calls [`ClusterWiring::from_config`], and takes ownership of
//! the resulting [`ClusterHandle`]; `stop` runs [`ClusterHandle::stop`] under the
//! framework's shutdown deadline. The builder/handle and config types remain `pub`
//! library surface (see crate root) so consumers may embed the wiring directly.

use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use anyhow::anyhow;
use async_trait::async_trait;
use tokio_util::sync::CancellationToken;
use toolkit::client_hub::ClientHub;
use toolkit::contracts::RunnableCapability;
use toolkit::{Gear, GearCtx};

use crate::config::ClusterConfig;
use crate::provider::ProviderRegistry;
use crate::wiring::{ClusterHandle, ClusterWiring};

#[toolkit::gear(name = "cluster", capabilities = [stateful])]
struct ClusterGear {
    /// Captured in `init` so `start` (which gets no `GearCtx`) can register
    /// backends into it.
    hub: OnceLock<Arc<ClientHub>>,
    /// Parsed operator config, captured in `init` and consumed in `start`.
    config: OnceLock<ClusterConfig>,
    /// The running wiring, owned from `start` to `stop`.
    handle: Mutex<Option<ClusterHandle>>,
}

impl Default for ClusterGear {
    fn default() -> Self {
        Self {
            hub: OnceLock::new(),
            config: OnceLock::new(),
            handle: Mutex::new(None),
        }
    }
}

impl ClusterGear {
    /// Assembles the provider registry from the backend plugins linked into this
    /// build. Today only the in-process standalone provider; future plugins add a
    /// `with_cache_provider` line here.
    fn provider_registry() -> ProviderRegistry {
        ProviderRegistry::new()
            .with_cache_provider(Arc::new(standalone_cluster_plugin::StandaloneCacheProvider))
    }
}

#[async_trait]
impl Gear for ClusterGear {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        let config: ClusterConfig = ctx.config_or_default()?;
        self.hub
            .set(ctx.client_hub())
            .map_err(|_| anyhow!("{} already initialized", Self::MODULE_NAME))?;
        self.config
            .set(config)
            .map_err(|_| anyhow!("{} already initialized", Self::MODULE_NAME))?;
        Ok(())
    }
}

#[async_trait]
impl RunnableCapability for ClusterGear {
    async fn start(&self, _cancel: CancellationToken) -> anyhow::Result<()> {
        let hub = self.hub.get().ok_or_else(|| {
            anyhow!(
                "{}: hub not set — init must run before start",
                Self::MODULE_NAME
            )
        })?;
        let config = self.config.get().ok_or_else(|| {
            anyhow!(
                "{}: config not set — init must run before start",
                Self::MODULE_NAME
            )
        })?;

        // Backends (and their background tasks) come up here and are registered
        // under `cluster:{profile}`; the handle owns each plugin's shutdown hook.
        let handle =
            ClusterWiring::from_config(Arc::clone(hub), config, &Self::provider_registry())?;
        *self.handle.lock().unwrap_or_else(PoisonError::into_inner) = Some(handle);
        Ok(())
    }

    async fn stop(&self, deadline: CancellationToken) -> anyhow::Result<()> {
        // Take the handle out before awaiting so the lock isn't held across the
        // shutdown await.
        let handle = self
            .handle
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(handle) = handle {
            tokio::select! {
                () = handle.stop() => {}           // graceful: deregister + compose plugin stops
                () = deadline.cancelled() => {}    // framework deadline elapsed
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "gear_tests.rs"]
mod gear_tests;
