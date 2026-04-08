// @cpt-dod:cpt-cf-resource-group-dod-entity-hier-rest-handlers:p1
use std::sync::Arc;

use axum::Extension;
use axum::extract::{Path, Query};
use axum::http::Uri;
use axum::response::IntoResponse;
use tracing::field::Empty;

use modkit::api::odata::OData;
use modkit::api::prelude::*;

use super::{
    CreateGroupDto, GroupDto, GroupWithDepthDto, PatchGroupDto, SecurityContext, UpdateGroupDto,
    info,
};
use crate::module::ConcreteGroupService;

/// Query parameters for delete endpoint.
#[derive(Debug, serde::Deserialize)]
pub struct DeleteGroupQuery {
    #[serde(default)]
    pub force: Option<bool>,
}

/// List resource groups with optional `OData` filtering and pagination.
#[tracing::instrument(
    skip(svc, ctx, query),
    fields(request_id = Empty)
)]
pub async fn list_groups(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteGroupService>>,
    OData(query): OData,
) -> ApiResult<Json<modkit_odata::Page<GroupDto>>> {
    info!("Listing resource groups");

    let page = svc.list_groups(&ctx, &query).await?;
    let dto_page = page.map_items(GroupDto::from);

    Ok(Json(dto_page))
}

/// Create a new resource group.
#[tracing::instrument(
    skip(svc, req_body, ctx, uri),
    fields(
        group.name = %req_body.name,
        request_id = Empty,
    )
)]
pub async fn create_group(
    uri: Uri,
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteGroupService>>,
    Json(req_body): Json<CreateGroupDto>,
) -> ApiResult<impl IntoResponse> {
    info!(
        name = %req_body.name,
        "Creating new resource group"
    );

    // Derive tenant_id from SecurityContext
    let tenant_id = ctx.subject_tenant_id();

    let group = svc.create_group(&ctx, req_body.into(), tenant_id).await?;
    let id_str = group.id.to_string();
    let dto = GroupDto::from(group);

    Ok(created_json(dto, &uri, &id_str).into_response())
}

/// Get a resource group by ID.
#[tracing::instrument(
    skip(svc, ctx),
    fields(
        group.id = %group_id,
        request_id = Empty,
    )
)]
pub async fn get_group(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteGroupService>>,
    Path(group_id): Path<uuid::Uuid>,
) -> ApiResult<Json<GroupDto>> {
    info!(
        group_id = %group_id,
        "Getting resource group"
    );

    let group = svc.get_group(&ctx, group_id).await?;
    Ok(Json(GroupDto::from(group)))
}

/// Update a resource group (full replacement via PUT).
#[tracing::instrument(
    skip(svc, req_body, ctx),
    fields(
        group.id = %group_id,
        request_id = Empty,
    )
)]
pub async fn update_group(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteGroupService>>,
    Path(group_id): Path<uuid::Uuid>,
    Json(req_body): Json<UpdateGroupDto>,
) -> ApiResult<Json<GroupDto>> {
    info!(
        group_id = %group_id,
        "Updating resource group"
    );

    let group = svc.update_group(&ctx, group_id, req_body.into()).await?;
    Ok(Json(GroupDto::from(group)))
}

/// Patch a resource group (partial update via PATCH).
#[tracing::instrument(
    skip(svc, req_body, ctx),
    fields(
        group.id = %group_id,
        request_id = Empty,
    )
)]
pub async fn patch_group(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteGroupService>>,
    Path(group_id): Path<uuid::Uuid>,
    Json(req_body): Json<PatchGroupDto>,
) -> ApiResult<Json<GroupDto>> {
    info!(
        group_id = %group_id,
        "Patching resource group"
    );

    let group = svc.patch_group(&ctx, group_id, req_body.into()).await?;
    Ok(Json(GroupDto::from(group)))
}

/// Delete a resource group.
#[tracing::instrument(
    skip(svc, ctx, params),
    fields(
        group.id = %group_id,
        request_id = Empty,
    )
)]
pub async fn delete_group(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteGroupService>>,
    Path(group_id): Path<uuid::Uuid>,
    Query(params): Query<DeleteGroupQuery>,
) -> ApiResult<impl IntoResponse> {
    let force = params.force.unwrap_or(false);
    info!(
        group_id = %group_id,
        force = force,
        "Deleting resource group"
    );

    svc.delete_group(&ctx, group_id, force).await?;
    Ok(no_content().into_response())
}

/// List hierarchy for a resource group.
#[tracing::instrument(
    skip(svc, ctx, query),
    fields(
        group.id = %group_id,
        request_id = Empty,
    )
)]
pub async fn list_group_hierarchy(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteGroupService>>,
    Path(group_id): Path<uuid::Uuid>,
    OData(query): OData,
) -> ApiResult<Json<modkit_odata::Page<GroupWithDepthDto>>> {
    info!(
        group_id = %group_id,
        "Listing group hierarchy"
    );

    let page = svc.list_group_hierarchy(&ctx, group_id, &query).await?;
    let dto_page = page.map_items(GroupWithDepthDto::from);

    Ok(Json(dto_page))
}
