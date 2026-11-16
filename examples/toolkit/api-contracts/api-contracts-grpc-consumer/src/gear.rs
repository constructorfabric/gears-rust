//! Gear definition and wiring.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use axum::Router;

use toolkit::api::OpenApiRegistry;
use toolkit::{Gear, GearCtx, RestApiCapability};

use crate::domain::PaymentProxyService;
use crate::rest;

/// Calls `PaymentApi` of the `api-contracts` gear over gRPC.
///
/// No `grpc_endpoint` is configured in the example: the client resolves the
/// service through the directory, and follows the provider if it moves.
#[toolkit::gear(name = "api-contracts-grpc-consumer", capabilities = [rest])]
#[toolkit::consumes(
    contract = api_contracts_sdk::PaymentApi,
    from = "api-contracts",
    transport = grpc
)]
#[derive(Default)]
pub struct ApiContractsGrpcConsumer;

#[async_trait]
impl Gear for ApiContractsGrpcConsumer {
    async fn init(&self, _ctx: &GearCtx) -> Result<()> {
        // `PaymentApi` is taken from the hub per request, so startup does not
        // wait for the provider.
        tracing::info!("api-contracts-grpc-consumer initialized");
        Ok(())
    }
}

impl RestApiCapability for ApiContractsGrpcConsumer {
    fn register_rest(
        &self,
        ctx: &GearCtx,
        router: Router,
        openapi: &dyn OpenApiRegistry,
    ) -> Result<Router> {
        let service = Arc::new(PaymentProxyService::new(ctx.client_hub()));
        Ok(rest::register_routes(router, openapi, service))
    }
}
