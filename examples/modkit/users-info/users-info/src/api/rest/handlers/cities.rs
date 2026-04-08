use axum::Extension;
use axum::extract::Path;
use axum::http::Uri;
use axum::response::IntoResponse;
use tracing::field::Empty;
use uuid::Uuid;

use modkit::api::odata::OData;

use super::{
    ApiResult, CityDto, CreateCityReq, Json, JsonBody, JsonPage, SecurityContext, UpdateCityReq,
    apply_select, created_json, info, no_content, page_to_projected_json,
};
use crate::module::ConcreteAppServices;

/// List cities with cursor-based pagination and optional field projection via $select
#[tracing::instrument(
    skip(svc, query, ctx),
    fields(
        limit = query.limit,
        request_id = Empty,
        user.id = %ctx.subject_id()
    )
)]
pub async fn list_cities(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<std::sync::Arc<ConcreteAppServices>>,
    OData(query): OData,
) -> ApiResult<JsonPage<serde_json::Value>> {
    info!(
        user_id = %ctx.subject_id(),
        "Listing cities with cursor pagination"
    );

    let page = svc.cities.list_cities_page(&ctx, &query).await?;
    let page = page.map_items(CityDto::from);

    Ok(Json(page_to_projected_json(&page, query.selected_fields())))
}

/// Get a specific city by ID with optional field projection via $select
#[tracing::instrument(
    skip(svc, ctx),
    fields(
        city.id = %id,
        request_id = Empty,
        requester.id = %ctx.subject_id()
    )
)]
pub async fn get_city(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<std::sync::Arc<ConcreteAppServices>>,
    Path(id): Path<Uuid>,
    OData(query): OData,
) -> ApiResult<JsonBody<serde_json::Value>> {
    info!(
        city_id = %id,
        requester_id = %ctx.subject_id(),
        "Getting city details"
    );

    let city = svc.cities.get_city(&ctx, id).await?;
    let city_dto = CityDto::from(city);

    let projected = apply_select(&city_dto, query.selected_fields());

    Ok(Json(projected))
}

/// Create a new city
#[tracing::instrument(
    skip(svc, req_body, ctx, uri),
    fields(
        city.name = %req_body.name,
        city.country = %req_body.country,
        city.tenant_id = %req_body.tenant_id,
        request_id = Empty,
        creator.id = %ctx.subject_id()
    )
)]
pub async fn create_city(
    uri: Uri,
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<std::sync::Arc<ConcreteAppServices>>,
    Json(req_body): Json<CreateCityReq>,
) -> ApiResult<impl IntoResponse> {
    info!(
        name = %req_body.name,
        country = %req_body.country,
        tenant_id = %req_body.tenant_id,
        creator_id = %ctx.subject_id(),
        "Creating new city"
    );

    let new_city = req_body.into();
    let city = svc.cities.create_city(&ctx, new_city).await?;
    let id_str = city.id.to_string();
    Ok(created_json(CityDto::from(city), &uri, &id_str).into_response())
}

/// Update an existing city
#[tracing::instrument(
    skip(svc, req_body, ctx),
    fields(
        city.id = %id,
        request_id = Empty,
        updater.id = %ctx.subject_id()
    )
)]
pub async fn update_city(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<std::sync::Arc<ConcreteAppServices>>,
    Path(id): Path<Uuid>,
    Json(req_body): Json<UpdateCityReq>,
) -> ApiResult<JsonBody<CityDto>> {
    info!(
        city_id = %id,
        updater_id = %ctx.subject_id(),
        "Updating city"
    );

    let patch = req_body.into();
    let city = svc.cities.update_city(&ctx, id, patch).await?;
    Ok(Json(CityDto::from(city)))
}

/// Delete a city by ID
#[tracing::instrument(
    skip(svc, ctx),
    fields(
        city.id = %id,
        request_id = Empty,
        deleter.id = %ctx.subject_id()
    )
)]
pub async fn delete_city(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<std::sync::Arc<ConcreteAppServices>>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    info!(
        city_id = %id,
        deleter_id = %ctx.subject_id(),
        "Deleting city"
    );

    svc.cities.delete_city(&ctx, id).await?;
    Ok(no_content().into_response())
}
