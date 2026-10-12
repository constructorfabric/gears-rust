//! Gear lifecycle. No database: a missing config block does not call `db_required`.

use std::sync::Arc;

use anyhow::Context as _;
use arc_swap::ArcSwapOption;
use async_trait::async_trait;
use axum::Router;
use toolkit::api::OpenApiRegistry;
use toolkit::config::ConfigError;
use toolkit::contracts::RestApiCapability;
use toolkit::{Gear, GearCtx};

use crate::api::ApiState;
use crate::config::ApprovalsConfig;

/// The approvals inbox.
#[toolkit::gear(name = "bss-approvals", capabilities = [rest])]
pub struct BssApprovalsGear {
    /// `None` until `init` sees a config object, and when the gear is not configured.
    runtime: ArcSwapOption<ApiState>,
}

impl Default for BssApprovalsGear {
    fn default() -> Self {
        Self {
            runtime: ArcSwapOption::from(None),
        }
    }
}

#[async_trait]
impl Gear for BssApprovalsGear {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        // `GearNotFound`: the gear is not in the file. `MissingConfigSection`: the gear is named
        // and has no config object, so there is no source list. Pricing continues from that arm
        // into `db_required`; this gear has no database, and it serves only when the object is
        // present. An invalid object still fails the boot.
        let cfg = match ctx.config::<ApprovalsConfig>() {
            Ok(cfg) => cfg,
            Err(ConfigError::GearNotFound { .. }) => {
                tracing::info!(
                    "bss-approvals: the gear is not in the config, so it does not serve"
                );
                return Ok(());
            }
            Err(ConfigError::MissingConfigSection { .. }) => {
                tracing::warn!(
                    "bss-approvals: the config names the gear and has no `sources` list, so it does not serve"
                );
                return Ok(());
            }
            Err(error) => return Err(error).context("bss-approvals: invalid config"),
        };
        let sources = crate::config::checked_sources(cfg.sources)?;
        self.runtime
            .store(Some(Arc::new(ApiState::new(sources, ctx.client_hub()))));
        Ok(())
    }
}

impl RestApiCapability for BssApprovalsGear {
    fn register_rest(
        &self,
        _ctx: &GearCtx,
        router: Router,
        openapi: &dyn OpenApiRegistry,
    ) -> anyhow::Result<Router> {
        let Some(state) = self.runtime.load_full() else {
            return Ok(router);
        };
        Ok(router.merge(crate::api::rest::router(state, openapi)))
    }
}

#[cfg(test)]
#[path = "gear_tests.rs"]
mod gear_tests;
