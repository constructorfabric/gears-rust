//! Secure gear definition.
//!
//! A REST-only gear exposing a single exposed, authenticated
//! `GET /secure/v1/whoami` route.

use anyhow::Result;
use async_trait::async_trait;
use axum::Router;

use toolkit::api::OpenApiRegistry;
use toolkit::context::GearCtx;
use toolkit::contracts::RestApiCapability;

use crate::api::rest::routes;

/// Example gear exposing one authenticated REST route.
#[toolkit::gear(
    name = "secure",
    capabilities = [rest]
)]
pub struct Secure;

impl Default for Secure {
    fn default() -> Self {
        Self
    }
}

#[async_trait]
impl toolkit::Gear for Secure {
    async fn init(&self, _ctx: &GearCtx) -> Result<()> {
        // Nothing to initialize: no dependencies, no state. The tenant-plane
        // authenticator is wired at the binary/bootstrap layer, not here.
        Ok(())
    }
}

impl RestApiCapability for Secure {
    fn register_rest(
        &self,
        _ctx: &GearCtx,
        router: Router,
        openapi: &dyn OpenApiRegistry,
    ) -> Result<Router> {
        tracing::info!("Registering secure REST routes");
        let router = routes::register_routes(router, openapi)?;
        tracing::info!("secure REST routes registered");
        Ok(router)
    }
}
