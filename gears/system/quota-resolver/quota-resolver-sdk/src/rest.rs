//! REST projections of the two public contracts.
//!
//! The platform plane is internal and platform-authenticated; the tenant plane
//! exposes only `quota-state`. With `rest-client` the macro emits the clients,
//! with `rest-server` the route registration that the gear hosts. Every method is
//! retryable: reads are safe and every write carries an idempotency key.
//!
//! The tenant-plane `quota_state` is `#[server_manual]`: the macro cannot bind
//! a route to a throttling zone, so [`register_quota_state_route`] registers it
//! with the rate-limit zone [`QUOTA_STATE_RATE_LIMIT_ZONE`].

use toolkit::Page;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::{PlatformSecurityContext, SecurityContext};

use crate::api::{PlatformQuotaResolverApi, QuotaResolverApi};
use crate::models::{
    LeaseOutcome, PlatformAcquireLeaseRequest, PlatformCommitLeaseRequest, PlatformDebitRequest,
    PlatformQuotaStateRequest, PlatformReleaseLeaseRequest, PlatformReturnRequest, QuotaDecision,
    QuotaState, QuotaStateRequest,
};

/// HTTP projection of [`PlatformQuotaResolverApi`].
#[toolkit::rest_contract(base_path = "/quota-resolver/v1/platform")]
pub trait PlatformQuotaResolverApiRest: PlatformQuotaResolverApi {
    /// `POST /quota-resolver/v1/platform/debit`.
    #[post("/debit")]
    #[retryable]
    async fn debit(
        &self,
        ctx: PlatformSecurityContext,
        request: PlatformDebitRequest,
    ) -> Result<QuotaDecision, CanonicalError>;

    /// `POST /quota-resolver/v1/platform/acquire-lease`.
    #[post("/acquire-lease")]
    #[retryable]
    async fn acquire_lease(
        &self,
        ctx: PlatformSecurityContext,
        request: PlatformAcquireLeaseRequest,
    ) -> Result<LeaseOutcome, CanonicalError>;

    /// `POST /quota-resolver/v1/platform/commit-lease`.
    #[post("/commit-lease")]
    #[retryable]
    async fn commit_lease(
        &self,
        ctx: PlatformSecurityContext,
        request: PlatformCommitLeaseRequest,
    ) -> Result<(), CanonicalError>;

    /// `POST /quota-resolver/v1/platform/release-lease`.
    #[post("/release-lease")]
    #[retryable]
    async fn release_lease(
        &self,
        ctx: PlatformSecurityContext,
        request: PlatformReleaseLeaseRequest,
    ) -> Result<(), CanonicalError>;

    /// `POST /quota-resolver/v1/platform/return-amount`.
    #[post("/return-amount")]
    #[retryable]
    async fn return_amount(
        &self,
        ctx: PlatformSecurityContext,
        request: PlatformReturnRequest,
    ) -> Result<(), CanonicalError>;

    /// `POST /quota-resolver/v1/platform/quota-state`.
    #[post("/quota-state")]
    #[retryable]
    async fn quota_state(
        &self,
        ctx: PlatformSecurityContext,
        request: PlatformQuotaStateRequest,
    ) -> Result<Page<QuotaState>, CanonicalError>;
}

/// HTTP projection of [`QuotaResolverApi`].
#[toolkit::rest_contract(base_path = "/quota-resolver/v1", visibility = "exposed")]
pub trait QuotaResolverApiRest: QuotaResolverApi {
    /// `POST /quota-resolver/v1/quota-state`.
    #[post("/quota-state")]
    #[retryable]
    #[server_manual]
    async fn quota_state(
        &self,
        ctx: SecurityContext,
        request: QuotaStateRequest,
    ) -> Result<Page<QuotaState>, CanonicalError>;
}

/// The API gateway rate-limit zone of the tenant-plane `quota-state` route. It
/// must be declared in the gateway configuration.
pub const QUOTA_STATE_RATE_LIMIT_ZONE: &str = "rl_quota_resolver_quota_state";

/// Register the tenant-plane `POST /quota-resolver/v1/quota-state` route, bound
/// to [`QUOTA_STATE_RATE_LIMIT_ZONE`] after authentication so that the zone can
/// be keyed on the caller's identity.
#[cfg(feature = "rest-server")]
pub fn register_quota_state_route(
    router: axum::Router,
    openapi: &dyn toolkit::api::OpenApiRegistry,
    svc: std::sync::Arc<dyn QuotaResolverApi>,
) -> axum::Router {
    toolkit::api::OperationBuilder::post("/quota-resolver/v1/quota-state")
        .operation_id("quota_resolver_api_rest_quota_state")
        .authenticated()
        .no_license_required()
        .exposed()
        .with_throttling(toolkit::api::ThrottlingSpec {
            rate_limit_zone: Some(QUOTA_STATE_RATE_LIMIT_ZONE.to_owned()),
            in_flight_limit_zone: None,
            require_security_context: true,
            dry_run: false,
        })
        .json_request::<QuotaStateRequest>(openapi, "")
        .handler(
            move |axum::Extension(ctx): axum::Extension<SecurityContext>,
                  axum::Json(request): axum::Json<QuotaStateRequest>| {
                let svc = std::sync::Arc::clone(&svc);
                async move { svc.quota_state(ctx, request).await.map(axum::Json) }
            },
        )
        .json_response_with_schema::<Page<QuotaState>>(openapi, axum::http::StatusCode::OK, "")
        .standard_errors(openapi)
        .register(router, openapi)
}
