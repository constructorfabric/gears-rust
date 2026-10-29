//! The gate routes (S3-11, S3-12): `POST /bss-orders/v1/orders/preview` and
//! `POST /bss-orders/v1/orders/{orderId}/submit`.
//!
//! Development delivery only (M2-1): the gear merges [`router`] only when it is built with the
//! `gate-dev-stubs` feature **and** configured with `gate.dev_stubs = true`; every other build
//! keeps the production surface of [`super::DELIVERED_OPERATIONS`], where this path answers the
//! router's 404. The route is authenticated, needs no license, binds the gateway's
//! identity-keyed caller zone and reads the optional delegation proof header like every Orders
//! operation. Boundary order: `X-Delegation-Proof-Ref` (`request-invalid`, D-202), then the body
//! (`request-invalid`), then the shared [`PreviewService`] (authorization, the per-caller
//! limiter, the run). The body is `snake_case` (D-206) except the `tcvWithheld` annotation,
//! whose wire name is the registered camelCase one.
//!
//! Submit (M3b, D-234) is mounted under the same development-delivery condition by
//! [`submit_router`], beside Preview and outside the census. It is an existing-order write and
//! keeps the authoring routes' boundary order exactly ([`super::capture`]): the per-(caller,
//! order) edge limiter, then the required `If-Match` (428), `Idempotency-Key`,
//! `X-Delegation-Proof-Ref`, the [`SubmitBody`] (`request-invalid`), and only then the shared
//! [`SubmitService`] (its per-caller limiter, the key's recovery probe, the authorized read, the
//! gate run, the commercial attempt). The settled outcome is rendered exactly as stored, as
//! capture renders it, so a same-key replay is byte-identical and carries
//! `Idempotency-Replayed: true`.
use std::sync::Arc;

use axum::body::Bytes;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Router};
use bss_orders_sdk::OrdersError;
use bss_orders_sdk::catalog::Reason;
use bss_orders_sdk::preview::PreviewBasket;
use tokio_util::sync::CancellationToken;
use toolkit::api::OpenApiRegistry;
use toolkit::api::operation_builder::OperationBuilder;
use toolkit::api::rest::extract::Path;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::dto::{OrdersPreviewBasket, OrdersPreviewResult, OrdersTransitionResult, SubmitBody};
use super::error::{ApiError, ApiResult};
use super::throttle::{PerOrderLimiter, caller_write_throttling};
use crate::authz::Caller;
use crate::infra::diagnostics::serde_category;
use crate::infra::gate::preview::{PreviewError, PreviewService};
use crate::infra::gate::submit::{SubmitError, SubmitService};

const TAG: &str = "Orders";

/// `request-invalid` for a body the wire refuses; the debug record names the operation, the
/// refused member where it is static and the serde category, never the serde message (it quotes
/// the input).
fn refused_body(
    operation: &'static str,
    member: Option<&'static str>,
    category: Option<&'static str>,
) -> OrdersError {
    tracing::debug!(
        target: "orders.rest.gate",
        operation,
        member,
        category,
        "request body refused request-invalid"
    );
    OrdersError::Refused(Reason::RequestInvalid)
}

/// The `preview` body: an object decoded as [`OrdersPreviewBasket`] and converted by the SDK's
/// own scalar parsers.
///
/// # Errors
/// `request-invalid` for any body the wire refuses.
#[doc(hidden)]
pub fn preview_body(body: &[u8]) -> Result<PreviewBasket, OrdersError> {
    let basket: OrdersPreviewBasket = serde_json::from_slice(body)
        .map_err(|error| refused_body("preview", None, Some(serde_category(&error))))?;
    PreviewBasket::try_from(basket).map_err(
        |bss_orders_sdk::authoring::AuthoringInvalid(member)| {
            refused_body("preview", Some(member), None)
        },
    )
}

/// The `submit` body: exactly `{ "expected_draft_revision": N }` with `N` a nonnegative signed
/// bigint; every other member is refused.
///
/// # Errors
/// `request-invalid` for any body the wire refuses.
#[doc(hidden)]
pub fn submit_body(body: &[u8]) -> Result<SubmitBody, OrdersError> {
    serde_json::from_slice(body)
        .map_err(|error| refused_body("submit", None, Some(serde_category(&error))))
}

impl From<PreviewError> for ApiError {
    fn from(error: PreviewError) -> Self {
        match error {
            PreviewError::Orders(error) => error.into(),
            PreviewError::Throttled(rejection) => rejection.into(),
        }
    }
}

impl From<SubmitError> for ApiError {
    fn from(error: SubmitError) -> Self {
        match error {
            SubmitError::Orders(error) => error.into(),
            SubmitError::Throttled(rejection) => rejection.into(),
        }
    }
}

async fn preview(
    Extension(service): Extension<Arc<PreviewService>>,
    Extension(ctx): Extension<SecurityContext>,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<Response> {
    let caller = super::caller(ctx, &headers)?;
    let basket = preview_body(&body)?;
    // The token is fresh and never cancelled: the dropped future is the cancellation (a client
    // that disconnects drops the handler, and with it the run). Capture passes no correlation id
    // either.
    let result = service
        .run(&caller, basket, None, &CancellationToken::new())
        .await?;
    let body = OrdersPreviewResult::try_from(result).map_err(|error| {
        tracing::error!(
            target: "orders.rest.gate",
            error = %error,
            "bss-orders: Preview result has no wire spelling"
        );
        OrdersError::Internal
    })?;
    Ok((StatusCode::OK, axum::Json(body)).into_response())
}

async fn submit(
    Extension(service): Extension<Arc<SubmitService>>,
    Extension(limiter): Extension<Arc<PerOrderLimiter>>,
    Extension(ctx): Extension<SecurityContext>,
    Path(order_id): Path<Uuid>,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<Response> {
    limiter.admit(ctx.subject_id(), order_id)?;
    let call = super::capture::write_meta(&headers)?;
    let request = submit_body(&body)?;
    let caller = Caller::from_meta(&ctx, call.delegation_proof_ref.as_ref())?;
    // A fresh token, never cancelled, as Preview: the dropped future is the cancellation.
    let settled = service
        .submit(
            &caller,
            order_id,
            request.expected_draft_revision,
            &call,
            &CancellationToken::new(),
        )
        .await?;
    Ok(super::capture::render(settled))
}

/// Register the Preview route and attach the shared service once (`ToolKit` REST rule).
pub fn router(service: Arc<PreviewService>, openapi: &dyn OpenApiRegistry) -> Router {
    let router = Router::new();
    let router = OperationBuilder::post("/bss-orders/v1/orders/preview")
        .operation_id("bss_orders.preview")
        .summary("Preview a basket")
        .description(
            "Runs the nonbinding gate assessment of a basket with the same axis-specific \
             authorization as purchase. Creates and mutates no order; the per-predicate outcomes \
             are kept as seven-day diagnostics under the returned assessment_id. Returns the \
             per-line results, the resolved total with producer-declared exclusions, the TCV \
             with its producer-declared tcv_basis (or tcvWithheld when a line omits \
             term or billing cycle), the indicative tax, and the expected fulfillment time with \
             per-line deferral. No approval verdict and no quote validity. Subscription-side \
             cardinality is advisory at order time: a gate or activation re-check pass is not \
             an admission guarantee. Subscriptions must enforce cardinality atomically at \
             activation; submit/activation is not production-ready until that contract is agreed \
             and delivered (D-180). Refusals: 400 \
             REQUEST_INVALID; 403/404 per the shared authorization contract; 429 when the \
             per-caller limit is exhausted; 503 when authorization, the store or the gate run is \
             unavailable.",
        )
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .with_throttling(caller_write_throttling())
        .json_request::<OrdersPreviewBasket>(openapi, "Basket")
        .param(super::delegation_proof_param())
        .handler(preview)
        .json_response_with_schema::<OrdersPreviewResult>(openapi, StatusCode::OK, "Assessment")
        .standard_errors(openapi)
        .error_429(openapi)
        .error_503(openapi)
        .register(router, openapi);
    router.layer(Extension(service))
}

/// Register the submit route (M3b, D-234) and attach the shared service and the edge limiter
/// once (`ToolKit` REST rule). Development delivery only: the gear merges it beside Preview when
/// the submit service exists, outside [`super::DELIVERED_OPERATIONS`].
pub fn submit_router(
    service: Arc<SubmitService>,
    limiter: Arc<PerOrderLimiter>,
    openapi: &dyn OpenApiRegistry,
) -> Router {
    let router = Router::new();
    let router = OperationBuilder::post("/bss-orders/v1/orders/{orderId}/submit")
        .operation_id("bss_orders.submit")
        .summary("Submit a draft order")
        .description(
            "Runs the binding gate assessment of the draft at expected_draft_revision, obtains \
             Pricing's acceptance receipt for every line and commits the submitted commercial \
             version with its pin, totals, audit row and event in one engine transaction. Returns \
             the frozen assessment_id; the settled outcome is replayed byte-identically under \
             the same key. Subscription-side cardinality is advisory at order time: a gate or \
             activation re-check pass is not an admission guarantee. Subscriptions must enforce \
             cardinality atomically at activation; submit/activation is not production-ready \
             until that contract is agreed and delivered (D-180). Workflow must handle later \
             overlap collisions through failure acknowledgement and compensation. Development \
             delivery only. Refusals: 400 REQUEST_INVALID and the registered gate refusals; \
             403/404 per the shared authorization contract; 400 NOT_ADMISSIBLE; 409 VERSION_CONFLICT, \
             IDEMPOTENCY_MISMATCH, STILL_PROCESSING, ORDER_IN_FLIGHT_FOR_KEY (gate or step 17); 412 for a stale If-Match; 428 without \
             If-Match; 429 when a per-caller or per-order limit is exhausted; 503 when \
             authorization, the store, the gate run or Pricing is unavailable.",
        )
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .path_param("orderId", "Order identity")
        .with_throttling(caller_write_throttling())
        .json_request::<SubmitBody>(openapi, "Expected draft revision")
        .param(super::capture::if_match())
        .param(super::capture::key_param())
        .param(super::delegation_proof_param())
        .handler(submit)
        .json_response_with_schema::<OrdersTransitionResult>(openapi, StatusCode::OK, "Committed")
        .response_header(super::capture::etag())
        .standard_errors(openapi)
        .problem_response(
            openapi,
            StatusCode::PRECONDITION_FAILED,
            super::capture::STALE_IF_MATCH,
        )
        .error_429(openapi)
        .error_503(openapi)
        .register(router, openapi);
    router.layer(Extension(service)).layer(Extension(limiter))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::rest::throttle::Rejection;

    /// A per-caller refusal reaches REST as the limiter's own 429 with `Retry-After`, while the
    /// SDK path keeps the canonical resource-exhausted error.
    /// In-crate: runs in the `orders-pg` CI lane with `gate-dev-stubs`.
    #[test]
    fn submits_per_caller_refusal_is_a_429_with_retry_after_and_the_sdk_error_otherwise() {
        let rejection = Rejection::PerCallerLimit {
            retry_after_seconds: 7,
        };
        let response = ApiError::from(SubmitError::Throttled(rejection)).into_response();
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(response.headers()["retry-after"], "7");
        let sdk = OrdersError::from(SubmitError::Throttled(rejection));
        assert!(matches!(sdk, OrdersError::Other { .. }), "{sdk:?}");
    }
}
