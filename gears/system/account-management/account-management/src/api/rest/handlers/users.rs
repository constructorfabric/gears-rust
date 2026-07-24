//! REST handlers for tenant-scoped user ops. `IdP` is the authoritative
//! source of truth — AM holds no local user table. PEP gate runs inside
//! `UserService`; handlers forward `SecurityContext` + body. The `OData`
//! lowering seam ([`lower_odata_to_list_users_query`]) runs the only
//! boundary validation at the handler layer (mapped to
//! `DomainError::Validation` → HTTP 400).
//! `DomainError → CanonicalError` via the `From` impl in
//! `crate::infra::sdk_error_mapping`.

use std::sync::Arc;

use axum::Extension;
use axum::extract::Path;
use axum::response::IntoResponse;
use tracing::field::Empty;
use uuid::Uuid;

use account_management_sdk::IdpUserFilterField;
use account_management_sdk::IdpUserPagination;
use account_management_sdk::ListUsersQuery as SdkListUsersQuery;
use toolkit::api::canonical_prelude::*;
use toolkit::api::odata::OData;
use toolkit_odata::ODataQuery;
use toolkit_odata::filter::convert_expr_to_filter_node;
use toolkit_security::SecurityContext;

use crate::api::rest::dto::{UserCreateRequestDto, UserDto, UserUpdateRequestDto};
use crate::domain::error::DomainError;
use crate::domain::user::service::UserService;

/// `GET /account-management/v1/tenants/{tenant_id}/users`
///
/// Accepts the standard `OData` query surface (`$filter`, `$orderby`,
/// `limit`, `cursor`). The authoritative existence-check shape is
/// `$filter=id eq <uuid>`; the empty page on that filter is the
/// canonical "absent" signal (no 404) per FEATURE §5.5 `DoD`.
///
/// # Errors
///
/// Surfaces a canonical `Problem` envelope. Notable codes:
/// `validation` (400 — malformed `$filter` / unknown filter field /
/// unsupported op / type mismatch; `limit` rejected by the SDK
/// pagination constructor; cursor encoding failure; tenant not in
/// `Active` status), `cross_tenant_denied` (403), tenant `not_found`
/// (404), `idp_unavailable` (503), `idp_unsupported_operation` (501).
#[tracing::instrument(
    skip(svc, ctx, query),
    fields(tenant_id = %tenant_id, request_id = Empty)
)]
pub async fn list_users(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<UserService>>,
    Path(tenant_id): Path<Uuid>,
    OData(query): OData,
) -> ApiResult<Json<toolkit_odata::Page<UserDto>>> {
    let list_query = lower_odata_to_list_users_query(query, svc.max_listing_top())?;
    let page = svc.list_users(&ctx, tenant_id, list_query).await?;
    Ok(Json(page.map_items(UserDto::from_idp_user)))
}

/// Lower the raw [`ODataQuery`] parsed by the `OData` extractor into
/// the SDK-side [`SdkListUsersQuery`]:
/// - `$filter`: typed validation against [`IdpUserFilterField`]
///   (unknown fields, type mismatches, unsupported ops surface as
///   `DomainError::Validation` → HTTP 400).
/// - `$orderby`: forwarded unchanged when non-empty; the service
///   injects the default order + `id ASC` tiebreaker when `None`.
/// - `limit`: clamped to `[1, max_top]`; defaults to
///   [`IdpUserPagination::DEFAULT_TOP`] (50) when omitted.
/// - `cursor`: encoded via [`toolkit_odata::CursorV1::encode`] and
///   forwarded as an opaque string; the `IdP` plugin decodes per its own
///   cursor format.
///
/// Detection of the `$filter=id eq <uuid>` existence-check shape lives
/// downstream in `UserService::list_users` (`extract_top_level_id_eq`).
/// Boundary code is intentionally shape-agnostic so any
/// `id eq <uuid>` expression — including ones nested inside `and` or
/// `or` — flows through the same typed lowering path.
pub(super) fn lower_odata_to_list_users_query(
    query: ODataQuery,
    max_top: u32,
) -> Result<SdkListUsersQuery, DomainError> {
    // Defensive floor: a misconfigured deployment that sets
    // `max_listing_top = 0` would otherwise panic at `clamp(1, 0)` and
    // surface every caller-supplied `limit` as a 500. Treat it as "no
    // operator cap configured" and let the SDK's MAX_TOP=200 take over.
    let max_top = max_top.max(1);
    let top = match query.limit {
        Some(n) => {
            // `ODataQuery::limit` is `u64`; clamp to `[1, max_top]` and
            // narrow to `u32` (the SDK pagination width). The `.max(1)`
            // floor stops a caller-supplied `0` from short-circuiting
            // into a false-negative empty page on plugins that honor
            // the literal value, and `u32::try_from` cannot fail after
            // the upper clamp because `max_top: u32`.
            let clamped = n.clamp(1, u64::from(max_top));
            u32::try_from(clamped).unwrap_or(max_top)
        }
        None => IdpUserPagination::DEFAULT_TOP.min(max_top),
    };
    // Capture the cursor's encoded order BEFORE consuming the cursor
    // into the opaque string the plugin expects. On continuation
    // requests the OData extractor rejects `cursor + $orderby` at
    // extraction time, so a caller who paginated under a non-default
    // order arrives here with `query.order` empty. Recovering the
    // order from `cursor.s` lets the next page reuse the same
    // effective order without forcing the caller to resend $orderby
    // — and without falling through to the default-order injection
    // downstream, which would cause an OrderMismatch at the plugin's
    // cursor-validation step.
    let cursor_order: Option<toolkit_odata::ODataOrderBy> = query
        .cursor
        .as_ref()
        .and_then(|c| toolkit_odata::ODataOrderBy::from_signed_tokens(&c.s).ok());

    let cursor: Option<String> = match query.cursor {
        Some(c) => Some(c.encode().map_err(|e| DomainError::Internal {
            diagnostic: format!("list_users: failed to encode cursor: {e}"),
            cause: None,
        })?),
        None => None,
    };
    let pagination =
        IdpUserPagination::new(top, cursor).map_err(|err| DomainError::Validation {
            detail: format!("list_users: invalid pagination: {err}"),
        })?;

    let filter = match query.filter {
        Some(boxed) => Some(
            convert_expr_to_filter_node::<IdpUserFilterField>(&boxed).map_err(|e| {
                DomainError::Validation {
                    detail: format!("list_users: invalid $filter: {e}"),
                }
            })?,
        ),
        None => None,
    };
    let order = if query.order.0.is_empty() {
        // Caller didn't pass $orderby. Prefer the cursor's encoded
        // order over the service-side default — this is what makes
        // continuation work for non-default orders.
        cursor_order
    } else {
        // Whitelist caller-supplied $orderby fields against
        // IdpUserFilterField — the OData extractor accepts arbitrary
        // field names at parse time (it's untyped per the framework's
        // design), so this is the seam that catches `$orderby=foo asc`
        // and surfaces it as 400 Validation. Without this gate, an
        // unknown field would silently no-op at the plugin layer.
        use toolkit_odata::filter::FilterField;
        let known: std::collections::HashSet<&'static str> = IdpUserFilterField::FIELDS
            .iter()
            .map(toolkit_odata::filter::FilterField::name)
            .collect();
        for key in &query.order.0 {
            if !known.contains(key.field.as_str()) {
                return Err(DomainError::Validation {
                    detail: format!(
                        "list_users: invalid $orderby: unknown field `{}` \
                         (allowed: {:?})",
                        key.field, known
                    ),
                });
            }
        }
        Some(query.order)
    };

    let mut q = SdkListUsersQuery::new(pagination);
    if let Some(f) = filter {
        q = q.with_filter(f);
    }
    if let Some(o) = order {
        q = q.with_order(o);
    }
    Ok(q)
}

#[cfg(test)]
#[path = "users_tests.rs"]
mod tests;

/// `POST /account-management/v1/tenants/{tenant_id}/users`
///
/// Returns HTTP 201 with the `IdP`-projected user body.
///
/// # No `Location` header
///
/// AM intentionally does not surface a single-user GET. The per-user
/// entry path `/users/{user_id}` exists for `PATCH` (update) and
/// `DELETE` (deprovision) only — there is no `GET` on it. The canonical
/// read-back shape is the filtered listing
/// `GET /tenants/{tenant_id}/users?$filter=id eq <uuid>`. A `Location`
/// pointing at `/users/{user_id}` would advertise a readable resource
/// that does not exist and would 405 on a follow-up `GET`.
///
/// # Errors
///
/// Surfaces a canonical `Problem` envelope. Notable codes:
/// `validation` (400 — tenant not in `Active` status; empty /
/// whitespace-only username; oversized fields; GTS schema rejection;
/// provider-side validation), `cross_tenant_denied` (403), tenant
/// `not_found` (404), `idp_unavailable` (503),
/// `idp_unsupported_operation` (501).
#[tracing::instrument(
    skip(svc, ctx, body),
    fields(tenant_id = %tenant_id, request_id = Empty)
)]
pub async fn create_user(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<UserService>>,
    Path(tenant_id): Path<Uuid>,
    Json(body): Json<UserCreateRequestDto>,
) -> ApiResult<impl IntoResponse> {
    let payload = body.into_idp_new_user();
    let user = svc.create_user(&ctx, tenant_id, payload).await?;
    let dto = UserDto::from_idp_user(user);
    // 201 without a `Location` header: AM does not expose per-user
    // `GET /tenants/{tenant_id}/users/{user_id}`, so the canonical
    // `toolkit::api::response::created_json` helper (which stamps a
    // `Location` pointing at the new resource) would emit a header
    // that resolves to 404 on follow-up. The raw tuple is intentional;
    // do NOT swap to `created_json` without first landing the per-user
    // GET endpoint.
    Ok((axum::http::StatusCode::CREATED, Json(dto)))
}

/// `PATCH /account-management/v1/tenants/{tenant_id}/users/{user_id}`
///
/// JSON Merge Patch (RFC 7396) over the user's mutable attributes.
/// Returns HTTP 200 with the post-update `IdP`-projected user body.
/// Omitted fields are unchanged; an explicit `null` clears a nullable
/// profile field; `username` may be renamed but not cleared; `password`
/// sets a new credential (never echoed back).
///
/// # Errors
///
/// Surfaces a canonical `Problem` envelope. Notable codes:
/// `validation` (400 — empty patch; explicit `null` on `username`;
/// username all-whitespace / oversized; profile field oversized or GTS
/// schema rejection; rejected password; unknown field on the body;
/// provider-side validation; tenant not in `Active` status),
/// `cross_tenant_denied` (403), tenant or user `not_found` (404 — the
/// `IdP` reports the target user absent, which — unlike `DELETE` — is
/// NOT folded into success), `already_exists` (409 — a username rename
/// collided with an existing login), `idp_unavailable` (503),
/// `idp_unsupported_operation` (501).
#[tracing::instrument(
    skip(svc, ctx, body),
    fields(tenant_id = %tenant_id, user_id = %user_id, request_id = Empty)
)]
pub async fn update_user(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<UserService>>,
    Path((tenant_id, user_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<UserUpdateRequestDto>,
) -> ApiResult<Json<UserDto>> {
    let patch = body.into_idp_user_patch()?;
    let user = svc.update_user(&ctx, tenant_id, user_id, patch).await?;
    Ok(Json(UserDto::from_idp_user(user)))
}

/// `DELETE /account-management/v1/tenants/{tenant_id}/users/{user_id}`
///
/// Retry-safe: the plugin maps vendor "user does not exist" responses
/// to `Ok(())` per
/// `dod-idp-user-operations-contract-deprovision-idempotency`, so a
/// repeat DELETE also returns 204.
///
/// # Errors
///
/// Surfaces a canonical `Problem` envelope. Notable codes:
/// `validation` (400 — tenant not in `Active` status; `resolve_active_tenant`
/// rejects `Provisioning` / `Suspended` / `Deleted` per
/// `feature-idp-user-operations-contract` `DoD` line 292), `cross_tenant_denied`
/// (403), tenant `not_found` (404), `idp_unavailable` (503),
/// `idp_unsupported_operation` (501 — provider genuinely does not support
/// deprovisioning; this MUST NOT silently no-op per PRD §5.5).
#[tracing::instrument(
    skip(svc, ctx),
    fields(tenant_id = %tenant_id, user_id = %user_id, request_id = Empty)
)]
pub async fn delete_user(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<UserService>>,
    Path((tenant_id, user_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<impl IntoResponse> {
    svc.delete_user(&ctx, tenant_id, user_id).await?;
    Ok(no_content().into_response())
}
