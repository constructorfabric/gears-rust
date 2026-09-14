//! REST handlers for tenant-scoped service accounts (machine
//! identities). The `IdP` is the authoritative source of truth — AM holds
//! no account table and no credential store. The PEP gate and the
//! tenant guard both run inside
//! [`ServiceAccountService`]; handlers only parse the path / body, call
//! the service, and shape the response. `DomainError → CanonicalError`
//! goes through the `From` impl in `crate::infra::sdk_error_mapping`.
//!
//! # Credential responses
//!
//! Create and rotate-secret are the only two responses in AM that carry
//! a secret, and each carries it exactly once. Both set
//! `Cache-Control: no-store` so no proxy, CDN, or browser cache retains
//! the body — centralised in [`no_store_json`] so the rule cannot be
//! forgotten on one of them.

use std::sync::Arc;

use axum::Extension;
use axum::extract::Path;
use axum::http::header;
use axum::response::IntoResponse;
use percent_encoding::{AsciiSet, CONTROLS, utf8_percent_encode};
use tracing::field::Empty;
use uuid::Uuid;

/// RFC 3986 path-segment encode set: everything except the unreserved
/// characters (`A-Z a-z 0-9 - . _ ~`).
///
/// Mirrors the set `toolkit_contract::runtime::http` uses to build path
/// segments; duplicated because that one is private to the contract runtime
/// and this crate does not depend on it.
const PATH_SEGMENT: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'%')
    .add(b'&')
    .add(b'\'')
    .add(b'(')
    .add(b')')
    .add(b'*')
    .add(b'+')
    .add(b',')
    .add(b'/')
    .add(b':')
    .add(b';')
    .add(b'<')
    .add(b'=')
    .add(b'>')
    .add(b'?')
    .add(b'@')
    .add(b'[')
    .add(b'\\')
    .add(b']')
    .add(b'^')
    .add(b'`')
    .add(b'{')
    .add(b'|')
    .add(b'}');

use toolkit::api::canonical_prelude::*;
use toolkit_security::SecurityContext;

use crate::api::rest::dto::{
    ServiceAccountCreateRequestDto, ServiceAccountCredentialsDto, ServiceAccountDto,
    ServiceAccountListDto,
};
use crate::domain::service_account::service::ServiceAccountService;

/// Build a JSON response carrying credentials with
/// `Cache-Control: no-store`. Used by rotate-secret directly; create
/// builds its response inline because it additionally needs a `Location`
/// header, but applies the same header.
// @cpt-begin:cpt-cf-account-management-dod-service-accounts-one-time-secret:p1:inst-dod-sa-one-time-secret-no-store
fn no_store_json(
    status: StatusCode,
    dto: ServiceAccountCredentialsDto,
) -> impl IntoResponse + use<> {
    (status, [(header::CACHE_CONTROL, "no-store")], Json(dto))
}
// @cpt-end:cpt-cf-account-management-dod-service-accounts-one-time-secret:p1:inst-dod-sa-one-time-secret-no-store

/// `POST /account-management/v1/tenants/{tenant_id}/service-accounts`
///
/// # Errors
///
/// Surfaces a canonical `Problem` envelope. Notable codes:
/// `invalid_argument` (400 — AM boundary caps on `name` / `scopes`, or a
/// provider rejection such as a name already live in the tenant),
/// `cross_tenant_denied` (403), tenant `not_found` (404), `aborted` (409
/// with `reason = AMBIGUOUS_OUTCOME` — the provider may hold a
/// half-created account; reconcile via the listing rather than
/// retrying), `idp_unsupported_operation` (501), `idp_unavailable` (503).
#[tracing::instrument(
    skip(svc, ctx, body),
    fields(tenant_id = %tenant_id, request_id = Empty)
)]
pub async fn create_service_account(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ServiceAccountService>>,
    Path(tenant_id): Path<Uuid>,
    uri: axum::http::Uri,
    Json(body): Json<ServiceAccountCreateRequestDto>,
) -> ApiResult<impl IntoResponse> {
    let credentials = svc.create(&ctx, tenant_id, body.name, body.scopes).await?;
    // The created resource's canonical address is this same collection
    // path with the new `client_id` appended. A 201 without `Location`
    // is non-compliant REST house style, so this is built alongside (not
    // instead of) the `no-store` requirement — the secret in the body
    // must still never be cached.
    //
    // `client_id` is percent-encoded as a path segment, never concatenated
    // raw: the contract declares it opaque and adapter-chosen, and AM caps
    // only the submitted `name`'s length — charset is the adapter's to
    // judge. Raw, a `/` `?` or `#` would silently re-point the URL at a
    // different resource (or push the tail into a query or fragment), and
    // a space, control byte, or non-ASCII byte would make the header value
    // itself invalid. Encoding fixes both, and is a no-op for the
    // conventional `svc-<tenant_id>-<name>` shape, whose characters are all
    // unreserved.
    let location = format!(
        "{}/{}",
        uri.path().trim_end_matches('/'),
        utf8_percent_encode(&credentials.client_id, PATH_SEGMENT)
    );
    let dto = ServiceAccountCredentialsDto::from(credentials);
    // @cpt-begin:cpt-cf-account-management-dod-service-accounts-one-time-secret:p1:inst-dod-sa-one-time-secret-create-response
    Ok((
        StatusCode::CREATED,
        [
            (header::LOCATION, location),
            (header::CACHE_CONTROL, "no-store".to_owned()),
        ],
        Json(dto),
    )
        .into_response())
    // @cpt-end:cpt-cf-account-management-dod-service-accounts-one-time-secret:p1:inst-dod-sa-one-time-secret-create-response
}

/// `GET /account-management/v1/tenants/{tenant_id}/service-accounts`
///
/// Unpaginated by contract — see
/// [`ServiceAccountListDto`].
///
/// # Errors
///
/// Canonical `Problem`: `cross_tenant_denied` (403), tenant `not_found`
/// (404), `idp_unsupported_operation` (501), `idp_unavailable` (503).
#[tracing::instrument(
    skip(svc, ctx),
    fields(tenant_id = %tenant_id, request_id = Empty)
)]
pub async fn list_service_accounts(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ServiceAccountService>>,
    Path(tenant_id): Path<Uuid>,
) -> ApiResult<Json<ServiceAccountListDto>> {
    let items = svc.list(&ctx, tenant_id).await?;
    Ok(Json(ServiceAccountListDto {
        service_accounts: items.into_iter().map(ServiceAccountDto::from).collect(),
    }))
}

/// `POST /account-management/v1/tenants/{tenant_id}/service-accounts/{client_id}/rotate-secret`
///
/// # Errors
///
/// Canonical `Problem`: `cross_tenant_denied` (403), tenant or service
/// account `not_found` (404 — unlike revoke, an absent account is
/// not folded into success), `aborted` (409 `AMBIGUOUS_OUTCOME`),
/// `idp_unsupported_operation` (501), `idp_unavailable` (503).
#[tracing::instrument(
    skip(svc, ctx),
    fields(tenant_id = %tenant_id, request_id = Empty)
)]
pub async fn rotate_service_account_secret(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ServiceAccountService>>,
    Path((tenant_id, client_id)): Path<(Uuid, String)>,
) -> ApiResult<impl IntoResponse> {
    let credentials = svc.rotate_secret(&ctx, tenant_id, &client_id).await?;
    let dto = ServiceAccountCredentialsDto::from(credentials);
    Ok(no_store_json(StatusCode::OK, dto).into_response())
}

/// `DELETE /account-management/v1/tenants/{tenant_id}/service-accounts/{client_id}`
///
/// Idempotent: an account the provider reports absent still answers
/// `204`, so a retried DELETE is safe.
///
/// # Errors
///
/// Canonical `Problem`: `cross_tenant_denied` (403), tenant `not_found`
/// (404 — the *tenant*, not the account),
/// `idp_unsupported_operation` (501), `idp_unavailable` (503).
#[tracing::instrument(
    skip(svc, ctx),
    fields(tenant_id = %tenant_id, request_id = Empty)
)]
pub async fn revoke_service_account(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ServiceAccountService>>,
    Path((tenant_id, client_id)): Path<(Uuid, String)>,
) -> ApiResult<impl IntoResponse> {
    svc.revoke(&ctx, tenant_id, &client_id).await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}
