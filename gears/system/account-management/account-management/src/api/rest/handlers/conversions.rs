//! REST handlers for tenant conversions — own (child-side) and inbound
//! (parent-side) symmetric URL families. PEP gate runs inside
//! `ConversionService`; handlers build the URL-bound `ConversionCaller`
//! (`child(tenant_id)` / `parent(parent_id)`) and forward.
//! `DomainError → CanonicalError` via the `From` impl in
//! `crate::infra::sdk_error_mapping`.

use std::collections::HashMap;
use std::sync::Arc;

use axum::Extension;
use axum::extract::{Path, Query};
use axum::http::Uri;
use axum::response::IntoResponse;
use tracing::Span;
use tracing::field::{Empty, display};
use uuid::Uuid;

use toolkit::api::canonical_prelude::*;
use toolkit::api::odata::OData;
use toolkit_security::SecurityContext;

use crate::api::rest::dto::{
    ChildConversionRequestDto, ConversionPatchDto, ConversionPatchStatusDto,
    OwnConversionRequestDto, RequestChildConversionDto, RequestOwnConversionDto,
};
use crate::api::rest::handlers::common::{clamp_listing_top, reject_non_odata_params};
use crate::domain::conversion::model::ConversionRequest;
use crate::domain::conversion::service::{ConversionCaller, ConversionService};
use crate::domain::error::DomainError;

/// Concrete [`ConversionService`] type the conversion handlers are wired against.
pub(crate) type ConcreteConversionService = ConversionService;

// =====================================================================
// Child-side (own) endpoints — /tenants/{tenant_id}/conversions*
// =====================================================================

/// `POST /account-management/v1/tenants/{tenant_id}/conversions`
///
/// Returns HTTP 201 with the post-insert projection and a `Location`
/// header pointing at `GET /tenants/{tenant_id}/conversions/{request_id}`.
///
/// # Errors
///
/// Surfaces a canonical `Problem` envelope. Notable codes:
/// `validation` (400 — `target_mode` not inverse of current mode;
/// `comment` empty / oversize; tenant not in `Active` status;
/// `root_tenant_cannot_convert`), `already_exists` (409 —
/// `pending_exists` carrying the existing `request_id` per the
/// at-most-one-pending invariant), `cross_tenant_denied` (403),
/// tenant `not_found` (404), `internal` (500),
/// `service_unavailable` (503 — PDP / DB transport failure).
#[tracing::instrument(
    skip(svc, ctx, body),
    fields(tenant_id = %tenant_id, request_id = Empty)
)]
pub async fn request_own_conversion(
    uri: Uri,
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteConversionService>>,
    Path(tenant_id): Path<Uuid>,
    Json(body): Json<RequestOwnConversionDto>,
) -> ApiResult<impl IntoResponse> {
    let input = body.into_service_input(ConversionCaller::child(tenant_id));
    let row = svc.request_conversion(&ctx, input).await?;
    Span::current().record("request_id", display(row.id));
    let id_str = row.id.to_string();
    let dto = OwnConversionRequestDto::from_conversion(row);
    Ok(created_json(dto, &uri, &id_str).into_response())
}

/// `GET /account-management/v1/tenants/{tenant_id}/conversions`
///
/// # Errors
///
/// Surfaces a canonical `Problem` envelope. Notable codes:
/// `validation` (400 — malformed `$filter` / `$orderby`;
/// unrecognized non-`OData` query parameter — see
/// [`reject_non_odata_params`]), `cross_tenant_denied` (403), tenant
/// `not_found` (404), `service_unavailable` (503 — PDP / DB transport
/// failure).
// `Query<HashMap<String, String>>` is the canonical Axum form for
// scanning unmodelled query keys; generic-hasher generalisation has no
// pay-off here because Axum's `serde_urlencoded` extractor always
// produces the default hasher. Allow the lint at the handler-signature
// level rather than rewriting the extractor type for no reason.
#[allow(clippy::implicit_hasher)]
#[tracing::instrument(
    skip(svc, ctx, query, extras),
    fields(tenant_id = %tenant_id, request_id = Empty)
)]
pub async fn list_own_conversions(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteConversionService>>,
    Path(tenant_id): Path<Uuid>,
    Query(extras): Query<HashMap<String, String>>,
    OData(query): OData,
) -> ApiResult<Json<toolkit_odata::Page<OwnConversionRequestDto>>> {
    reject_non_odata_params(&extras)?;
    let query = clamp_listing_top(query, svc.max_listing_top());
    let page = svc.list_own_for_tenant(&ctx, tenant_id, &query).await?;
    Ok(Json(
        page.map_items(OwnConversionRequestDto::from_conversion),
    ))
}

/// `GET /account-management/v1/tenants/{tenant_id}/conversions/{request_id}`
///
/// A `request_id` whose stored `tenant_id` does NOT match the URL
/// collapses to `not_found` so callers cannot probe row existence
/// through the error code.
///
/// # Errors
///
/// Surfaces a canonical `Problem` envelope. Notable codes:
/// `cross_tenant_denied` (403), `not_found` (404),
/// `service_unavailable` (503 — PDP / DB transport failure).
#[tracing::instrument(
    skip(svc, ctx),
    fields(tenant_id = %tenant_id, request_id = %request_id)
)]
pub async fn get_own_conversion(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteConversionService>>,
    Path((tenant_id, request_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<OwnConversionRequestDto>> {
    let row = svc.get_own_for_tenant(&ctx, tenant_id, request_id).await?;
    Ok(Json(OwnConversionRequestDto::from_conversion(row)))
}

/// `PATCH /account-management/v1/tenants/{tenant_id}/conversions/{request_id}`
///
/// Drive a `pending` row to a terminal status (`approved`, `cancelled`,
/// `rejected`). `approve` / `reject` are counterparty-only; `cancel`
/// is initiator-only — enforced by the service.
///
/// # Errors
///
/// Surfaces a canonical `Problem` envelope. Notable codes:
/// `validation` (400 — `comment` empty / oversize),
/// `failed_precondition` (400 — `invalid_actor_for_transition`,
/// `already_resolved`, type re-evaluation rejected on approve),
/// `cross_tenant_denied` (403), `not_found` (404),
/// `aborted` (409 — serialization-conflict retry budget exhausted on
/// approve's apply TX), `internal` (500 — `apply_conversion_approval`
/// invariant failure), `service_unavailable` (503 — PDP / DB / types-
/// registry transport failure).
#[tracing::instrument(
    skip(svc, ctx, body),
    fields(tenant_id = %tenant_id, request_id = %request_id)
)]
pub async fn patch_own_conversion(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteConversionService>>,
    Path((tenant_id, request_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<ConversionPatchDto>,
) -> ApiResult<Json<OwnConversionRequestDto>> {
    let caller = ConversionCaller::child(tenant_id);
    let row = dispatch_patch(&svc, &ctx, request_id, caller, body).await?;
    Ok(Json(OwnConversionRequestDto::from_conversion(row)))
}

// =====================================================================
// Parent-side (inbound) endpoints — /tenants/{tenant_id}/child-conversions*
// =====================================================================

/// `POST /account-management/v1/tenants/{tenant_id}/child-conversions`
///
/// URL binds the parent; body's `child_tenant_id` binds the child. A
/// misrouted call (child whose `parent_id` is not the URL-bound parent)
/// surfaces as `not_found`. Returns HTTP 201 with the cross-barrier
/// minimal projection.
///
/// # Errors
///
/// Surfaces a canonical `Problem` envelope. Notable codes:
/// `validation` (400 — `target_mode` not inverse of current mode;
/// `comment` empty / oversize; child not in `Active` status;
/// `root_tenant_cannot_convert`), `already_exists` (409 —
/// `pending_exists`), `cross_tenant_denied` (403), child / parent
/// `not_found` (404), `internal` (500), `service_unavailable` (503).
#[tracing::instrument(
    skip(svc, ctx, body),
    fields(parent_id = %parent_id, request_id = Empty)
)]
pub async fn request_child_conversion(
    uri: Uri,
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteConversionService>>,
    Path(parent_id): Path<Uuid>,
    Json(body): Json<RequestChildConversionDto>,
) -> ApiResult<impl IntoResponse> {
    let input = body.into_service_input(ConversionCaller::parent(parent_id));
    let row = svc.request_conversion(&ctx, input).await?;
    Span::current().record("request_id", display(row.id));
    let id_str = row.id.to_string();
    // Project for the live `child_tenant_name`, matching the GET / list
    // surfaces — the row's stamped name may be stale if the child was
    // renamed between request and response.
    let projection = svc.project_for_parent_view(row).await;
    let dto = ChildConversionRequestDto::from_parent_projection(projection);
    Ok(created_json(dto, &uri, &id_str).into_response())
}

/// `GET /account-management/v1/tenants/{tenant_id}/child-conversions`
///
/// Parent-side inbound listing — cross-barrier minimal projection.
/// Dual-consent flows live under the parent's URL authority, so the
/// service runs `respect_barriers = false` and surfaces rows even when
/// the closure barrier would otherwise hide a self-managed child.
///
/// # Errors
///
/// Surfaces a canonical `Problem` envelope. Notable codes:
/// `validation` (400 — malformed `$filter` / `$orderby`;
/// unrecognized non-`OData` query parameter — see
/// [`reject_non_odata_params`]), `cross_tenant_denied` (403), parent
/// `not_found` (404), `service_unavailable` (503).
// See `list_own_conversions` for the rationale on this allow.
#[allow(clippy::implicit_hasher)]
#[tracing::instrument(
    skip(svc, ctx, query, extras),
    fields(parent_id = %parent_id, request_id = Empty)
)]
pub async fn list_child_conversions(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteConversionService>>,
    Path(parent_id): Path<Uuid>,
    Query(extras): Query<HashMap<String, String>>,
    OData(query): OData,
) -> ApiResult<Json<toolkit_odata::Page<ChildConversionRequestDto>>> {
    reject_non_odata_params(&extras)?;
    let query = clamp_listing_top(query, svc.max_listing_top());
    let page = svc.list_inbound_for_parent(&ctx, parent_id, &query).await?;
    Ok(Json(page.map_items(
        ChildConversionRequestDto::from_parent_projection,
    )))
}

/// `GET /account-management/v1/tenants/{tenant_id}/child-conversions/{request_id}`
///
/// # Errors
///
/// Surfaces a canonical `Problem` envelope. Notable codes:
/// `cross_tenant_denied` (403), `not_found` (404),
/// `service_unavailable` (503).
#[tracing::instrument(
    skip(svc, ctx),
    fields(parent_id = %parent_id, request_id = %request_id)
)]
pub async fn get_child_conversion(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteConversionService>>,
    Path((parent_id, request_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<ChildConversionRequestDto>> {
    let projection = svc
        .get_inbound_for_parent(&ctx, parent_id, request_id)
        .await?;
    Ok(Json(ChildConversionRequestDto::from_parent_projection(
        projection,
    )))
}

/// `PATCH /account-management/v1/tenants/{tenant_id}/child-conversions/{request_id}`
///
/// Parent-side counterpart of [`patch_own_conversion`]. Response
/// carries the cross-barrier minimal projection.
///
/// # Errors
///
/// See [`patch_own_conversion`] — identical envelope.
#[tracing::instrument(
    skip(svc, ctx, body),
    fields(parent_id = %parent_id, request_id = %request_id)
)]
pub async fn patch_child_conversion(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteConversionService>>,
    Path((parent_id, request_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<ConversionPatchDto>,
) -> ApiResult<Json<ChildConversionRequestDto>> {
    let caller = ConversionCaller::parent(parent_id);
    let row = dispatch_patch(&svc, &ctx, request_id, caller, body).await?;
    // Live-name refresh — same rationale as `request_child_conversion`.
    let projection = svc.project_for_parent_view(row).await;
    Ok(Json(ChildConversionRequestDto::from_parent_projection(
        projection,
    )))
}

// =====================================================================
// PATCH dispatcher — shared by own + child patch handlers.
// =====================================================================

/// Route the PATCH body's `status` to the matching service method.
/// Shared between own / inbound PATCH handlers so any future
/// PATCH-status addition flows through one match instead of two.
pub(crate) async fn dispatch_patch(
    svc: &ConversionService,
    ctx: &SecurityContext,
    request_id: Uuid,
    caller: ConversionCaller,
    body: ConversionPatchDto,
) -> Result<ConversionRequest, DomainError> {
    match body.status {
        ConversionPatchStatusDto::Approved => {
            svc.approve(ctx, request_id, caller, body.comment).await
        }
        ConversionPatchStatusDto::Cancelled => {
            svc.cancel(ctx, request_id, caller, body.comment).await
        }
        ConversionPatchStatusDto::Rejected => {
            svc.reject(ctx, request_id, caller, body.comment).await
        }
    }
}

#[cfg(test)]
#[path = "conversions_tests.rs"]
mod tests;
