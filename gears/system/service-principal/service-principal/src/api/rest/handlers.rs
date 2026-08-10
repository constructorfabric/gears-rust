//! REST handlers for the service-principal gear. Thin: parse the path/body, call
//! the service, map the domain result to a canonical response. Errors propagate
//! via `?` through `From<DomainError> for CanonicalError`.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Extension, Path};
use axum::http::{StatusCode, header};
use axum::response::IntoResponse;
use service_principal_sdk::TenantId;
use toolkit::api::canonical_prelude::*;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::dto::{
    CreateServicePrincipalRequestDto, ListServicePrincipalsResponseDto,
    ServicePrincipalCredentialsDto,
};
use crate::domain::service::Service;

/// Build a JSON response carrying a `ServicePrincipalCredentialsDto` with
/// `Cache-Control: no-store`. Centralizes the "credential responses must never
/// be cached by any intermediary" rule for `rotate_secret` (`create` builds its
/// response inline since it additionally needs a `Location` header).
fn no_store_json(status: StatusCode, dto: ServicePrincipalCredentialsDto) -> impl IntoResponse {
    (status, [(header::CACHE_CONTROL, "no-store")], Json(dto))
}

/// `POST …/tenants/{tenant_id}/service-principals`
///
/// # Errors
/// Canonical `Problem` on invalid input (400), access denied (403), an ambiguous
/// upstream outcome that may have half-created the principal (409 — recover via
/// revoke + create), or upstream unavailable (503).
pub async fn create(
    Extension(svc): Extension<Arc<Service>>,
    Extension(ctx): Extension<SecurityContext>,
    Path(tenant_id): Path<Uuid>,
    uri: axum::http::Uri,
    Json(body): Json<CreateServicePrincipalRequestDto>,
) -> ApiResult<impl IntoResponse> {
    let creds = svc
        .create(&ctx, TenantId(tenant_id), body.name, body.scopes)
        .await?;
    // The created resource's canonical address is this same collection path with
    // the new `client_id` appended, e.g.
    // `/service-principal/v1/tenants/{tenant_id}/service-principals/{client_id}`
    // (mirrors `credstore`'s `create_secret`). A 201 without `Location` is
    // non-compliant REST house style, so this is built alongside (not instead
    // of) the `no-store` requirement below — the secret body must still never
    // be cached.
    let location = format!("{}/{}", uri.path().trim_end_matches('/'), creds.client_id);
    let dto = ServicePrincipalCredentialsDto::from(creds);
    Ok((
        StatusCode::CREATED,
        [
            (header::LOCATION, location),
            (header::CACHE_CONTROL, "no-store".to_owned()),
        ],
        Json(dto),
    )
        .into_response())
}

/// `GET …/tenants/{tenant_id}/service-principals`
///
/// # Errors
/// Canonical `Problem` on access denied (403) or upstream unavailable (503).
pub async fn list(
    Extension(svc): Extension<Arc<Service>>,
    Extension(ctx): Extension<SecurityContext>,
    Path(tenant_id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let items = svc.list(&ctx, TenantId(tenant_id)).await?;
    let dto = ListServicePrincipalsResponseDto {
        service_principals: items.into_iter().map(Into::into).collect(),
    };
    Ok((StatusCode::OK, Json(dto)).into_response())
}

/// `POST …/tenants/{tenant_id}/service-principals/{client_id}/rotate-secret`
///
/// # Errors
/// Canonical `Problem` on not found (404), access denied (403), an ambiguous
/// upstream outcome (409), or upstream unavailable (503).
pub async fn rotate_secret(
    Extension(svc): Extension<Arc<Service>>,
    Extension(ctx): Extension<SecurityContext>,
    Path((tenant_id, client_id)): Path<(Uuid, String)>,
) -> ApiResult<impl IntoResponse> {
    let creds = svc
        .rotate_secret(&ctx, TenantId(tenant_id), &client_id)
        .await?;
    let dto = ServicePrincipalCredentialsDto::from(creds);
    Ok(no_store_json(StatusCode::OK, dto).into_response())
}

/// `DELETE …/tenants/{tenant_id}/service-principals/{client_id}`
///
/// Idempotent: a missing principal returns `204`.
///
/// # Errors
/// Canonical `Problem` on access denied (403) or upstream unavailable (503).
pub async fn revoke(
    Extension(svc): Extension<Arc<Service>>,
    Extension(ctx): Extension<SecurityContext>,
    Path((tenant_id, client_id)): Path<(Uuid, String)>,
) -> ApiResult<impl IntoResponse> {
    svc.revoke(&ctx, TenantId(tenant_id), &client_id).await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}
