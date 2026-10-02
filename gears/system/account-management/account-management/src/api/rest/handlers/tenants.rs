//! REST handlers for tenants. PEP gate lives on `TenantService`;
//! handlers forward `SecurityContext` + body only. `DomainError →
//! CanonicalError` via the `From` impl in
//! `crate::infra::sdk_error_mapping`.

use std::collections::HashMap;
use std::sync::Arc;

use axum::Extension;
use axum::extract::{Path, Query};
use axum::http::Uri;
use axum::response::IntoResponse;
use tracing::field::Empty;
use uuid::Uuid;

use toolkit::api::canonical_prelude::*;
use toolkit::api::odata::OData;
use toolkit_security::SecurityContext;

use crate::api::rest::dto::{TenantCreateRequestDto, TenantDto, TenantUpdateRequestDto};
use crate::api::rest::handlers::common::{
    bind_cursor_to_children_mode, clamp_listing_top, parse_recursive_flag,
};
use crate::domain::tenant::service::TenantService;
use crate::infra::storage::repo_impl::TenantRepoImpl;

/// Concrete alias — pins the generic `TenantService<R>` so axum's
/// `Extension` type stays unambiguous at the handler boundary.
pub(crate) type ConcreteTenantService = TenantService<TenantRepoImpl>;

/// `POST /account-management/v1/tenants`
///
/// Returns HTTP 201 with the post-create projection (`status=active`)
/// and a `Location` header at `GET /tenants/{tenant_id}`.
///
/// # Errors
///
/// Surfaces a canonical `Problem` envelope. Notable codes:
/// `validation` (400 — parent not active, invalid `tenant_type` chain,
/// GTS-schema rejection on the name), `failed_precondition` (400 —
/// incompatible parent/child `tenant_type` pairing
/// [`type_not_allowed`], hierarchy depth strict-limit exceeded
/// [`tenant_depth_exceeded`]), `cross_tenant_denied` (403), parent
/// tenant `not_found` (404), `already_exists` (409 — server-allocated
/// child UUID collision; astronomically rare but the unique-violation
/// arm exists), `aborted` (409 — serialization-conflict retry budget
/// exhausted on parallel create under the same parent),
/// `idp_unsupported_operation` (501 — `IdP` plugin does not implement
/// tenant provisioning), `idp_unavailable` / `service_unavailable`
/// (503 — `IdP` / types-registry transport failure).
#[tracing::instrument(
    skip(svc, ctx, body),
    fields(parent_id = %body.parent_id, request_id = Empty)
)]
pub async fn create_tenant(
    uri: Uri,
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteTenantService>>,
    Json(body): Json<TenantCreateRequestDto>,
) -> ApiResult<impl IntoResponse> {
    let request = body.into_sdk_create_request();
    let tenant = svc.create_tenant(&ctx, request).await?;
    let id_str = tenant.id.to_string();
    let dto = TenantDto::from_sdk_tenant(tenant);
    Ok(created_json(dto, &uri, &id_str).into_response())
}

/// `GET /account-management/v1/tenants/{tenant_id}`
///
/// Soft-deleted (`status=deleted`) tenants stay visible here, unlike
/// the children listing which hides them by default. AM-internal
/// `provisioning` rows are never surfaced.
///
/// # Errors
///
/// Surfaces a canonical `Problem` envelope. Notable codes:
/// `cross_tenant_denied` (403), `not_found` (404), `internal` (500 —
/// AM-internal `provisioning` row bypassed the SDK-visibility
/// filter; should never happen), `service_unavailable` (503 — PDP /
/// DB transport failure).
#[tracing::instrument(
    skip(svc, ctx),
    fields(tenant_id = %tenant_id, request_id = Empty)
)]
pub async fn get_tenant(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteTenantService>>,
    Path(tenant_id): Path<Uuid>,
) -> ApiResult<Json<TenantDto>> {
    let tenant = svc.get_tenant(&ctx, tenant_id).await?;
    Ok(Json(TenantDto::from_sdk_tenant(tenant)))
}

/// `PATCH /account-management/v1/tenants/{tenant_id}`
///
/// Only `name` is mutable here. Lifecycle transitions go through
/// `/suspend`, `/unsuspend`, and `DELETE`.
///
/// # Errors
///
/// Surfaces a canonical `Problem` envelope. Notable codes:
/// `validation` (400 — empty patch; name violates length bounds /
/// GTS schema; unknown field on the body — including `status`,
/// `parent_id`, `tenant_type`, `self_managed`), `failed_precondition`
/// (400 — PATCH attempted on a `deleted` or `provisioning` row,
/// neither of which is mutable), `cross_tenant_denied` (403),
/// `not_found` (404), `aborted` (409 — serialization-conflict retry
/// budget exhausted by a concurrent PATCH / soft-delete on the same
/// row), `internal` (500 — AM-internal `provisioning` row bypassed
/// the SDK-visibility filter; should never happen in practice but
/// the service lifter can still surface it), `service_unavailable`
/// (503 — PDP / DB / types-registry transport failure).
#[tracing::instrument(
    skip(svc, ctx, body),
    fields(tenant_id = %tenant_id, request_id = Empty)
)]
pub async fn update_tenant(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteTenantService>>,
    Path(tenant_id): Path<Uuid>,
    Json(body): Json<TenantUpdateRequestDto>,
) -> ApiResult<Json<TenantDto>> {
    let patch = body.into_sdk_tenant_update();
    let tenant = svc.update_tenant(&ctx, tenant_id, patch).await?;
    Ok(Json(TenantDto::from_sdk_tenant(tenant)))
}

/// `DELETE /account-management/v1/tenants/{tenant_id}`
///
/// Soft-delete (flips `status=deleted`, arms the retention sweep).
/// Returns HTTP 204; callers that need the post-delete projection
/// re-read via `GET /tenants/{id}`.
///
/// # Errors
///
/// Surfaces a canonical `Problem` envelope. Notable codes:
/// `validation` (400 — `root_tenant_cannot_delete` per the
/// `From<DomainError>` mapping in
/// [`crate::infra::sdk_error_mapping`], which routes
/// `RootTenantCannotDelete` through `validation`),
/// `failed_precondition` (400 — `tenant_has_children`,
/// `tenant_has_resources`), `cross_tenant_denied` (403), `not_found`
/// (404), `aborted` (409 — serialization-conflict retry budget
/// exhausted by a concurrent PATCH / soft-delete on the same row),
/// `internal` (500 — AM-internal `provisioning` row bypassed the
/// SDK-visibility filter; the service lifter still defends against
/// it), `idp_unsupported_operation` (501 — `IdP` plugin does not
/// implement tenant deprovisioning), `idp_unavailable` /
/// `service_unavailable` (503 — `IdP` / PDP / DB / RG transport
/// failure).
#[tracing::instrument(
    skip(svc, ctx),
    fields(tenant_id = %tenant_id, request_id = Empty)
)]
pub async fn delete_tenant(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteTenantService>>,
    Path(tenant_id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    svc.delete_tenant(&ctx, tenant_id).await?;
    Ok(no_content().into_response())
}

/// `POST /account-management/v1/tenants/{tenant_id}/suspend`
///
/// Idempotent: a second call on an already-suspended row returns 200
/// with the same projection without bumping `updated_at`.
///
/// AIP-136 specifies the custom-method wire shape as
/// `POST /tenants/{tenant_id}:suspend`, but axum 0.8.x pins
/// `matchit = "=0.8.4"` which cannot split `{param}:suffix` in a
/// single segment. The sub-resource form below is the in-tree fallback
/// pattern (mirrors `gears/mini-chat/.../routes/turns.rs` —
/// `{request_id}/retry`); the cutover is tracked alongside axum's
/// matchit bump (PR <https://github.com/tokio-rs/axum/pull/3702>) and
/// the upstream routing issue
/// <https://github.com/tokio-rs/axum/issues/3140>.
///
/// # Errors
///
/// Surfaces a canonical `Problem` envelope. Notable codes:
/// `failed_precondition` (400 — target tenant is `deleted`),
/// `cross_tenant_denied` (403), `not_found` (404 — tenant missing or
/// AM-internal `provisioning`), `aborted` (409 — serialization-
/// conflict retry budget exhausted inside
/// [`TenantRepo::set_status`]'s SERIALIZABLE retry loop on a
/// concurrent PATCH / soft-delete on the same row),
/// `service_unavailable` (503 — PDP / DB transport failure).
#[tracing::instrument(
    skip(svc, ctx),
    fields(tenant_id = %tenant_id, request_id = Empty)
)]
pub async fn suspend_tenant(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteTenantService>>,
    Path(tenant_id): Path<Uuid>,
) -> ApiResult<Json<TenantDto>> {
    let tenant = svc.suspend_tenant(&ctx, tenant_id).await?;
    Ok(Json(TenantDto::from_sdk_tenant(tenant)))
}

/// `POST /account-management/v1/tenants/{tenant_id}/unsuspend`
///
/// Reverse of [`suspend_tenant`]. Idempotent on already-active rows.
///
/// # Errors
///
/// See [`suspend_tenant`] — identical mapping.
#[tracing::instrument(
    skip(svc, ctx),
    fields(tenant_id = %tenant_id, request_id = Empty)
)]
pub async fn unsuspend_tenant(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteTenantService>>,
    Path(tenant_id): Path<Uuid>,
) -> ApiResult<Json<TenantDto>> {
    let tenant = svc.unsuspend_tenant(&ctx, tenant_id).await?;
    Ok(Json(TenantDto::from_sdk_tenant(tenant)))
}

/// `GET /account-management/v1/tenants/{tenant_id}/children`
///
/// Direct children by default; with `recursive=true` every descendant
/// visible to the caller, each item carrying `ancestors`. Soft-deleted
/// rows are hidden by default — callers opt in with
/// `?$filter=status eq 'deleted'`. AM-internal `provisioning` rows are never
/// surfaced. Effective sort is `(created_at ASC, id ASC)` for stable
/// cursor pagination across `created_at` ties; a cursor is bound to the
/// mode it was minted in (see `bind_cursor_to_children_mode`).
///
/// # Errors
///
/// Surfaces a canonical `Problem` envelope. Notable codes:
/// `validation` (400 — malformed `$filter` / `$orderby`, `recursive` not
/// `true`/`false`, a cursor replayed with a different `$filter` or mode, or
/// a fingerprint-less legacy cursor replayed in recursive mode),
/// `cross_tenant_denied` (403), parent tenant `not_found` (404),
/// `service_unavailable` (503 — PDP failure, or the DB unavailable for the
/// parent read), `internal` (500 — the page query itself failed: the
/// pagination helper does not keep the typed DB error, so availability
/// cannot be told apart there).
// `Query<HashMap<String, String>>` is the canonical Axum form for
// scanning unmodelled query keys (see `list_own_conversions`); the
// generic-hasher lint has no pay-off on Axum's default hasher.
#[allow(clippy::implicit_hasher)]
#[tracing::instrument(
    skip(svc, ctx, query, extras),
    fields(tenant_id = %tenant_id, request_id = Empty)
)]
pub async fn list_tenant_children(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteTenantService>>,
    Path(tenant_id): Path<Uuid>,
    Query(extras): Query<HashMap<String, String>>,
    OData(query): OData,
) -> ApiResult<Json<toolkit_odata::Page<TenantDto>>> {
    let recursive = parse_recursive_flag(&extras)?;
    let query = bind_cursor_to_children_mode(
        clamp_listing_top(query, svc.max_list_children_top()),
        recursive,
    )?;
    // @cpt-begin:cpt-cf-account-management-flow-tenant-hierarchy-management-list-children:p1:inst-flow-listch-ancestors
    if recursive {
        let page = svc.list_descendants(&ctx, tenant_id, &query).await?;
        return Ok(Json(page.map_items(TenantDto::from_sdk_node)));
    }
    // @cpt-end:cpt-cf-account-management-flow-tenant-hierarchy-management-list-children:p1:inst-flow-listch-ancestors
    let page = svc.list_children(&ctx, tenant_id, &query).await?;
    Ok(Json(page.map_items(TenantDto::from_sdk_tenant)))
}

#[cfg(test)]
#[path = "tenants_tests.rs"]
mod tests;
