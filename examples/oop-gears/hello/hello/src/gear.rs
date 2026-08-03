//! Hello gear definition.
//!
//! A minimal REST-only gear with no dependencies. Exposes a single public,
//! anonymous `GET /hello/v1/ping` route.

use anyhow::Result;
use async_trait::async_trait;
use axum::Router;

use toolkit::api::OpenApiRegistry;
use toolkit::context::GearCtx;
use toolkit::contracts::RestApiCapability;

use crate::api::rest::routes;

/// Minimal example gear exposing one public REST route.
#[toolkit::gear(
    name = "hello",
    capabilities = [rest]
)]
pub struct Hello;

impl Default for Hello {
    fn default() -> Self {
        Self
    }
}

#[async_trait]
impl toolkit::Gear for Hello {
    async fn init(&self, _ctx: &GearCtx) -> Result<()> {
        // Nothing to initialize: no dependencies, no state.
        Ok(())
    }
}

impl RestApiCapability for Hello {
    fn register_rest(
        &self,
        _ctx: &GearCtx,
        router: Router,
        openapi: &dyn OpenApiRegistry,
    ) -> Result<Router> {
        tracing::info!("Registering hello REST routes");
        let router = routes::register_routes(router, openapi)?;
        tracing::info!("hello REST routes registered");
        Ok(router)
    }
}
