//! Consumer gear definition and wiring.
//!
//! `#[toolkit::consumes]` emits — behind this crate's `directory-rest-client`
//! feature — a `ConsumerRegistration` that the runtime's proxy-wiring phase
//! replays at startup: if the `api-contracts` provider already registered a
//! local `PaymentApi` in the `ClientHub`, that impl wins; otherwise a
//! directory-resolving REST client is wired under `dyn PaymentApi`. This gear's
//! own code never branches on the binding mode — it just resolves the contract
//! from the hub (see [`crate::domain::ChargeProxyService`]).

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use axum::Router;

use toolkit::api::OpenApiRegistry;
use toolkit::{Gear, GearCtx, RestApiCapability};

use crate::domain::ChargeProxyService;
use crate::rest;

/// Consumer gear — *needs* `PaymentApi`, provided by the `api-contracts` gear.
///
/// - `#[toolkit::gear]` registers it under the name `api-contracts-consumer`
///   with a REST capability.
/// - `#[toolkit::consumes]` declares the cross-gear dependency (resolved lazily,
///   tolerant of a not-yet-ready or remote provider — no topo-sort dep).
#[toolkit::gear(name = "api-contracts-consumer", capabilities = [rest])]
#[toolkit::consumes(contract = api_contracts_sdk::PaymentApi, from = "api-contracts")]
#[derive(Default)]
pub struct ApiContractsConsumer;

#[async_trait]
impl Gear for ApiContractsConsumer {
    async fn init(&self, _ctx: &GearCtx) -> Result<()> {
        // Lazy consumption: nothing is resolved at init. `PaymentApi` is pulled
        // from the ClientHub per request, so startup does not block on the
        // provider being ready (eventual readiness).
        tracing::info!("api-contracts-consumer initialized");
        Ok(())
    }
}

impl RestApiCapability for ApiContractsConsumer {
    fn register_rest(
        &self,
        ctx: &GearCtx,
        router: Router,
        openapi: &dyn OpenApiRegistry,
    ) -> Result<Router> {
        // The domain service holds the hub and resolves `PaymentApi` per call.
        let service = Arc::new(ChargeProxyService::new(ctx.client_hub()));
        Ok(rest::register_routes(router, openapi, service))
    }
}
