//! REST handlers: one catalog-service call each, `$select` projection on the
//! serialized list responses, and the `ETag` / `If-Match` concurrency
//! headers of DESIGN §3.3 (DESIGN §3.2 REST API). Paths and bodies come in
//! through the toolkit extractors, so a malformed one is a canonical Problem.

use std::sync::Arc;

use axum::extract::{Extension, OriginalUri};
use axum::http::{HeaderMap, HeaderName, StatusCode, Uri, header};
use toolkit::api::canonical_prelude::*;
use toolkit::api::rest::extract::{Json as JsonRequest, Path};
use toolkit::api::select::page_to_projected_json;
use toolkit_odata::Page;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::dto::{
    CreateModelRequestDto, CreateProviderRequestDto, ModelDto, ModelEntryDto, ProviderEntryDto,
    UpdateModelRequestDto, UpdateProviderRequestDto,
};
use super::parse;
use crate::domain::error::DomainError;
use crate::domain::precondition::version_tag;
use crate::domain::service::Service;

type Svc = Extension<Arc<Service>>;

/// A page of DTOs with `$select` applied to every item.
fn projected<T: serde::Serialize, U: Into<T>>(
    page: Page<U>,
    query: &OData,
) -> Json<Page<serde_json::Value>> {
    let dto_page = Page {
        items: page.items.into_iter().map(Into::into).collect::<Vec<T>>(),
        page_info: page.page_info,
    };
    Json(page_to_projected_json(&dto_page, query.selected_fields()))
}

/// The strong `ETag` of a record: its bare row version, quoted.
fn etag(version: i64) -> (HeaderName, String) {
    (header::ETAG, version_tag(version))
}

/// 200 with the entry and its `ETag`.
fn ok_with_etag<T: serde::Serialize>(version: i64, dto: T) -> impl IntoResponse {
    (StatusCode::OK, [etag(version)], Json(dto))
}

/// 201 with `Location`, the entry, and its `ETag`.
fn created_with_etag<T: serde::Serialize>(
    version: i64,
    dto: T,
    uri: &Uri,
    id: &str,
) -> impl IntoResponse + use<T> {
    let location = [uri.path().trim_end_matches('/'), id].join("/");
    (
        StatusCode::CREATED,
        [(header::LOCATION, location), etag(version)],
        Json(dto),
    )
}

// ==================== Resolution ====================

/// `GET /model-registry/v1/available-models/{canonical_id}`: the full
/// `ModelV1`, as the SDK returns it.
pub async fn resolve_model(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Svc,
    Path(canonical_id): Path<String>,
) -> ApiResult<JsonBody<ModelDto>> {
    let model = svc.resolve_model(&ctx, &canonical_id).await?;
    Ok(Json(ModelDto::from(model)))
}

/// `GET /model-registry/v1/available-models`
pub async fn list_available_models(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Svc,
    OData(query): OData,
) -> ApiResult<JsonBody<Page<serde_json::Value>>> {
    let page = svc.list_available_models(&ctx, &query).await?;
    Ok(projected::<ModelDto, _>(page, &OData(query)))
}

// ==================== Providers ====================

/// `POST /model-registry/v1/providers`
pub async fn create_provider(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Svc,
    OriginalUri(uri): OriginalUri,
    JsonRequest(dto): JsonRequest<CreateProviderRequestDto>,
) -> ApiResult<impl IntoResponse> {
    let req = parse::create_provider(dto).map_err(DomainError::for_provider)?;
    let entry = svc.create_provider(&ctx, req).await?;
    let id = entry.provider.id.to_string();
    let version = entry.provider.version;
    Ok(created_with_etag(
        version,
        ProviderEntryDto::from(entry),
        &uri,
        &id,
    ))
}

/// `GET /model-registry/v1/providers/{id}`
pub async fn get_provider(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Svc,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let entry = svc.get_provider(&ctx, id).await?;
    Ok(ok_with_etag(
        entry.provider.version,
        ProviderEntryDto::from(entry),
    ))
}

/// `GET /model-registry/v1/providers`
pub async fn list_providers(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Svc,
    OData(query): OData,
) -> ApiResult<JsonBody<Page<serde_json::Value>>> {
    let page = svc.list_providers(&ctx, &query).await?;
    Ok(projected::<ProviderEntryDto, _>(page, &OData(query)))
}

/// `PATCH /model-registry/v1/providers/{id}`; requires `If-Match`.
pub async fn update_provider(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Svc,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    JsonRequest(body): JsonRequest<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    let (precondition, req) = parse::if_match(&headers, true)
        .and_then(|precondition| {
            parse::check_update_body(
                &body,
                parse::PROVIDER_IMMUTABLE_FIELDS,
                parse::PROVIDER_NULLABLE_FIELDS,
            )?;
            let dto: UpdateProviderRequestDto = parse::body(body)?;
            Ok((precondition, parse::update_provider(dto)?))
        })
        .map_err(DomainError::for_provider)?;
    let entry = svc.update_provider_if(&ctx, id, req, precondition).await?;
    Ok(ok_with_etag(
        entry.provider.version,
        ProviderEntryDto::from(entry),
    ))
}

/// `DELETE /model-registry/v1/providers/{id}`; optional `If-Match`.
pub async fn delete_provider(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Svc,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let precondition = parse::if_match(&headers, false).map_err(DomainError::for_provider)?;
    svc.delete_provider_if(&ctx, id, precondition).await?;
    Ok(no_content())
}

// ==================== Models ====================

/// `POST /model-registry/v1/models`
pub async fn create_model(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Svc,
    OriginalUri(uri): OriginalUri,
    JsonRequest(dto): JsonRequest<CreateModelRequestDto>,
) -> ApiResult<impl IntoResponse> {
    let req = parse::create_model(dto)?;
    let entry = svc.create_model(&ctx, req).await?;
    let id = entry.model.id.to_string();
    let version = entry.model.version;
    Ok(created_with_etag(
        version,
        ModelEntryDto::from(entry),
        &uri,
        &id,
    ))
}

/// `GET /model-registry/v1/models/{id}`
pub async fn get_model(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Svc,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let entry = svc.get_model(&ctx, id).await?;
    Ok(ok_with_etag(
        entry.model.version,
        ModelEntryDto::from(entry),
    ))
}

/// `GET /model-registry/v1/models`
pub async fn list_models(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Svc,
    OData(query): OData,
) -> ApiResult<JsonBody<Page<serde_json::Value>>> {
    let page = svc.list_models(&ctx, &query).await?;
    Ok(projected::<ModelEntryDto, _>(page, &OData(query)))
}

/// `PATCH /model-registry/v1/models/{id}`; requires `If-Match`.
pub async fn update_model(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Svc,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    JsonRequest(body): JsonRequest<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    let precondition = parse::if_match(&headers, true)?;
    parse::check_update_body(
        &body,
        parse::MODEL_IMMUTABLE_FIELDS,
        parse::MODEL_NULLABLE_FIELDS,
    )?;
    let dto: UpdateModelRequestDto = parse::body(body)?;
    let entry = svc
        .update_model_if(&ctx, id, parse::update_model(dto)?, precondition)
        .await?;
    Ok(ok_with_etag(
        entry.model.version,
        ModelEntryDto::from(entry),
    ))
}

/// `DELETE /model-registry/v1/models/{id}`; optional `If-Match`.
pub async fn delete_model(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Svc,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let precondition = parse::if_match(&headers, false)?;
    svc.delete_model_if(&ctx, id, precondition).await?;
    Ok(no_content())
}

#[cfg(test)]
mod tests {
    use super::etag;

    #[test]
    fn etag_is_the_quoted_version() {
        let (name, value) = etag(3);
        assert_eq!(name, axum::http::header::ETAG);
        assert_eq!(value, "\"3\"");
    }
}
