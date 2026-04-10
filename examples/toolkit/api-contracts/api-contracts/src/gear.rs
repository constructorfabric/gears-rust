//! Gear definition and wiring for api-contracts.
//!
//! Demonstrates `#[toolkit::provides]` auto-wiring: the producer gear
//! declares the contract + a local factory; the macro emits the
//! `wire_payment_api` method that handles IR validation, config-driven
//! transport selection (local / REST / gRPC), and `ClientHub` registration.

use std::sync::Arc;

use api_contracts_sdk::PaymentApi;
use async_trait::async_trait;
use toolkit::api::OpenApiRegistry;
use toolkit::{Gear, GearCtx, RestApiCapability};
use toolkit_contract::policy::PolicyStack;

use crate::api::rest::routes;
use crate::client::local::PaymentLocalClient;
use crate::domain::service::PaymentDomainService;

/// Service hub demo gear — provides [`PaymentApi`].
///
/// Auto-wiring rules (declared via `#[toolkit::provides]`):
///
/// - `transports = [local, rest]` — match the default feature set
///   (`rest-client`). The `grpc-client` Cargo feature is opt-in and used
///   by the dedicated gRPC integration test, which constructs the gRPC
///   client manually.
/// - Default policies: `[TracingPolicy]` (applied inside the local client).
/// - Wiring config path:
///   `gears.api-contracts.config.client_wiring.payment_api`.
#[toolkit::gear(name = "api-contracts", capabilities = [rest])]
#[toolkit::provides(
    contract   = api_contracts_sdk::PaymentApi,
    local      = Self::build_local,
    transports = [local, rest],
)]
#[derive(Default)]
pub struct ApiContracts;

impl ApiContracts {
    /// Factory invoked by `#[toolkit::provides]` when wiring resolves to
    /// `ClientWiring::Local`. Signature matches the macro's contract:
    /// `fn(&GearCtx, Arc<PolicyStack>) -> anyhow::Result<Arc<dyn Contract>>`.
    fn build_local(
        _ctx: &GearCtx,
        policies: Arc<PolicyStack>,
    ) -> anyhow::Result<Arc<dyn PaymentApi>> {
        let domain_svc = Arc::new(PaymentDomainService::new());
        Ok(Arc::new(PaymentLocalClient::new(domain_svc, policies)))
    }
}

#[async_trait]
impl Gear for ApiContracts {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        self.wire_payment_api(ctx).await?;
        tracing::info!("api-contracts initialized");
        Ok(())
    }
}

impl RestApiCapability for ApiContracts {
    fn register_rest(
        &self,
        ctx: &GearCtx,
        router: axum::Router,
        openapi: &dyn OpenApiRegistry,
    ) -> anyhow::Result<axum::Router> {
        let service = ctx.client_hub().get::<dyn PaymentApi>()?;
        Ok(routes::register_routes(router, openapi, service))
    }
}
