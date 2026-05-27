// Created: 2026-04-16 by Constructor Tech
// Updated: 2026-04-28 by Constructor Tech
// @cpt-dod:cpt-cf-resource-group-dod-type-mgmt-rest-handlers:p1
use std::sync::Arc;

use axum::Extension;
use axum::extract::Path;
use axum::http::Uri;
use axum::response::IntoResponse;
use tracing::field::Empty;

use modkit::api::canonical_prelude::*;

use super::{CreateTypeDto, SecurityContext, TypeDto, UpdateTypeDto, info};
use crate::module::ConcreteTypeService;

/// List GTS types with optional `OData` filtering and pagination.
#[tracing::instrument(
    skip(svc, ctx, query),
    fields(request_id = Empty)
)]
pub async fn list_types(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteTypeService>>,
    OData(query): OData,
) -> ApiResult<Json<modkit_odata::Page<TypeDto>>> {
    info!("Listing GTS types");

    let page = svc.list_types(&ctx, &query).await?;
    let dto_page = page.map_items(TypeDto::from);

    Ok(Json(dto_page))
}

/// Create a new GTS type definition.
#[tracing::instrument(
    skip(svc, req_body, ctx, uri),
    fields(
        type.code = %req_body.code,
        request_id = Empty,
    )
)]
pub async fn create_type(
    uri: Uri,
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteTypeService>>,
    Json(req_body): Json<CreateTypeDto>,
) -> ApiResult<impl IntoResponse> {
    // @cpt-begin:cpt-cf-resource-group-flow-type-mgmt-create-type:p1:inst-create-type-1
    // Actor sends POST /api/types-registry/v1/types with type definition payload
    info!(
        code = %req_body.code,
        "Creating new GTS type"
    );

    let code = req_body.code.clone();
    // @cpt-end:cpt-cf-resource-group-flow-type-mgmt-create-type:p1:inst-create-type-1
    let rg_type = svc.create_type(&ctx, req_body.into()).await?;
    let dto = TypeDto::from(rg_type);

    Ok(created_json(dto, &uri, &code).into_response())
}

/// Get a GTS type definition by code.
#[tracing::instrument(
    skip(svc, ctx),
    fields(
        type.code = %code,
        request_id = Empty,
    )
)]
pub async fn get_type(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteTypeService>>,
    Path(code): Path<String>,
) -> ApiResult<Json<TypeDto>> {
    info!(
        code = %code,
        "Getting GTS type"
    );

    let rg_type = svc.get_type(&ctx, &code).await?;
    Ok(Json(TypeDto::from(rg_type)))
}

/// Update a GTS type definition (full replacement).
#[tracing::instrument(
    skip(svc, req_body, ctx),
    fields(
        type.code = %code,
        request_id = Empty,
    )
)]
pub async fn update_type(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteTypeService>>,
    Path(code): Path<String>,
    Json(req_body): Json<UpdateTypeDto>,
) -> ApiResult<Json<TypeDto>> {
    // @cpt-begin:cpt-cf-resource-group-flow-type-mgmt-update-type:p1:inst-update-type-1
    // Actor sends PUT /api/types-registry/v1/types/{code} with updated definition
    info!(
        code = %code,
        "Updating GTS type"
    );

    let rg_type = svc.update_type(&ctx, &code, req_body.into()).await?;
    // @cpt-end:cpt-cf-resource-group-flow-type-mgmt-update-type:p1:inst-update-type-1
    Ok(Json(TypeDto::from(rg_type)))
}

/// Delete a GTS type definition.
#[tracing::instrument(
    skip(svc, ctx),
    fields(
        type.code = %code,
        request_id = Empty,
    )
)]
pub async fn delete_type(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteTypeService>>,
    Path(code): Path<String>,
) -> ApiResult<impl IntoResponse> {
    info!(
        code = %code,
        "Deleting GTS type"
    );

    svc.delete_type(&ctx, &code).await?;
    Ok(no_content().into_response())
}
