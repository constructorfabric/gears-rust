//! Gear definition and wiring for api-contracts.
//!
//! Demonstrates `#[toolkit::provides]` auto-wiring: the producer gear
//! declares the contract + a local factory; the macro emits the
//! `wire_payment_api` method that handles IR validation, config-driven
//! transport selection (local / REST / gRPC), and `ClientHub` registration.

use std::sync::{Arc, OnceLock};

use api_contracts_sdk::{PaymentApi, PaymentApiV2};
use async_trait::async_trait;
use toolkit::api::OpenApiRegistry;
use toolkit::{Gear, GearCtx, RestApiCapability};
use toolkit_contract::policy::PolicyStack;

use crate::api::rest::routes;
use crate::client::local::{PaymentApiV2LocalClient, PaymentLocalClient};
use crate::domain::service::PaymentDomainService;

/// The one domain store shared by **both** contract versions.
///
/// `#[toolkit::provides]` calls each `local` factory as an associated fn
/// (`fn(&GearCtx, Arc<PolicyStack>)`) with no `&self`, so the shared instance
/// cannot live in a struct field. Without sharing, v1 and v2 would each get
/// their own store and a payment charged through one version would 404 on the
/// other — a major version changes the wire contract, not the business data.
fn domain_service() -> Arc<PaymentDomainService> {
    static DOMAIN: OnceLock<Arc<PaymentDomainService>> = OnceLock::new();
    Arc::clone(DOMAIN.get_or_init(|| Arc::new(PaymentDomainService::new())))
}

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
/// Two `#[toolkit::provides]` attributes stack: the gear provides both major
/// versions of the payment contract side by side (ADR-0007). Because the trait
/// names differ, each gets its own `wire_*` method and its own wiring config
/// key (`client_wiring.payment_api` / `client_wiring.payment_api_v2`) with no
/// collision.
#[toolkit::gear(name = "api-contracts", capabilities = [rest])]
#[toolkit::provides(
    contract   = api_contracts_sdk::PaymentApi,
    local      = Self::build_local,
    transports = [local, rest],
)]
#[toolkit::provides(
    contract   = api_contracts_sdk::PaymentApiV2,
    local      = Self::build_local_v2,
    transports = [local, rest],
)]
#[derive(Default)]
pub struct ApiContracts;

impl ApiContracts {
    /// Factory invoked by `#[toolkit::provides]` when wiring resolves to
    /// `ClientWiring::Local`. Signature matches the macro's contract:
    /// `fn(&GearCtx, Arc<PolicyStack>) -> anyhow::Result<Arc<dyn Contract>>`.
    // `#[toolkit::provides]` applies `?` to this factory's return value
    // (`#local_path(ctx, __policies)?`), so `Result` is a required part of
    // the macro's calling convention, not incidental — this example's local
    // path just happens to always succeed.
    #[allow(clippy::unnecessary_wraps)]
    fn build_local(
        _ctx: &GearCtx,
        policies: Arc<PolicyStack>,
    ) -> anyhow::Result<Arc<dyn PaymentApi>> {
        Ok(Arc::new(PaymentLocalClient::new(
            domain_service(),
            policies,
        )))
    }

    /// v2 counterpart of [`Self::build_local`], backed by the same domain
    /// store so both versions see the same payments.
    #[allow(clippy::unnecessary_wraps)]
    fn build_local_v2(
        _ctx: &GearCtx,
        policies: Arc<PolicyStack>,
    ) -> anyhow::Result<Arc<dyn PaymentApiV2>> {
        Ok(Arc::new(PaymentApiV2LocalClient::new(
            domain_service(),
            policies,
        )))
    }
}

#[async_trait]
impl Gear for ApiContracts {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        self.wire_payment_api(ctx).await?;
        self.wire_payment_api_v2(ctx).await?;
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
        let service_v2 = ctx.client_hub().get::<dyn PaymentApiV2>()?;

        // Three sources compose on one router: the macro-generated v1 routes
        // plus the manual SSE route (`list_payments`, `#[server_manual]`) from
        // `register_routes`, and the macro-generated v2 routes mounted under
        // `/v2` (ADR-0007 parallel versions). Generated registration is
        // additive both across versions and alongside hand-written
        // `OperationBuilder` calls.
        let router = routes::register_routes(router, openapi, service);
        Ok(routes::register_v2_routes(router, openapi, service_v2))
    }
}
