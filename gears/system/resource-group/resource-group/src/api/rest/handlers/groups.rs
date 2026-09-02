// Created: 2026-04-16 by Constructor Tech
// Updated: 2026-04-28 by Constructor Tech
// @cpt-dod:cpt-cf-resource-group-dod-entity-hier-rest-handlers:p1
use std::sync::Arc;

use axum::Extension;
use axum::extract::{Path, Query};
use axum::http::Uri;
use axum::response::IntoResponse;
use tracing::field::Empty;

use toolkit::api::canonical_prelude::*;

use super::{CreateGroupDto, GroupDto, GroupWithDepthDto, SecurityContext, UpdateGroupDto, info};
use crate::gear::ConcreteGroupService;

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
    // @cpt-begin:cpt-cf-resource-group-flow-integration-auth-jwt-request:p1:inst-jwt-1
    // Actor sends request to RG REST endpoint with JWT bearer token (any RG
    // REST endpoint terminates here once routed by axum).
    // @cpt-begin:cpt-cf-resource-group-flow-integration-auth-jwt-request:p1:inst-jwt-2
    // API Gateway: authenticate JWT via AuthNResolverClient → SecurityContext
    // {subject_id, subject_tenant_id} (extracted as Extension below).
    Extension(ctx): Extension<SecurityContext>,
    // @cpt-end:cpt-cf-resource-group-flow-integration-auth-jwt-request:p1:inst-jwt-2
    // @cpt-end:cpt-cf-resource-group-flow-integration-auth-jwt-request:p1:inst-jwt-1
    Extension(svc): Extension<Arc<ConcreteGroupService>>,
    OData(query): OData,
) -> ApiResult<Json<toolkit_odata::Page<GroupDto>>> {
    info!("Listing resource groups");

    // @cpt-begin:cpt-cf-resource-group-flow-integration-auth-jwt-request:p1:inst-jwt-3
    // @cpt-begin:cpt-cf-resource-group-flow-integration-auth-jwt-request:p1:inst-jwt-4
    // @cpt-begin:cpt-cf-resource-group-flow-integration-auth-jwt-request:p1:inst-jwt-5
    // @cpt-begin:cpt-cf-resource-group-flow-integration-auth-jwt-request:p1:inst-jwt-6
    // @cpt-begin:cpt-cf-resource-group-flow-integration-auth-jwt-request:p1:inst-jwt-7
    // @cpt-begin:cpt-cf-resource-group-flow-integration-auth-jwt-request:p1:inst-jwt-8
    // @cpt-begin:cpt-cf-resource-group-flow-integration-auth-jwt-request:p1:inst-jwt-9
    // RG Gateway delegates to GroupService.list_groups, which (jwt-3..9):
    //   3) calls PolicyEnforcer.access_scope(ctx, RG_GROUP_RESOURCE, action)
    //   4) PolicyEnforcer → AuthZResolverApi.evaluate(ctx, EvaluationRequest) -> Result<_, CanonicalError>
    //   5) AuthZ plugin internally calls ResourceGroupReadHierarchy.list_group_depth
    //      via ClientHub for tenant hierarchy resolution (in-process bypass)
    //   6) AuthZ plugin produces constraints (e.g., owner_tenant_id IN (...))
    //   7) PolicyEnforcer compiles constraints → AccessScope
    //   8) AccessScope is applied via SecureORM (WHERE tenant_id IN (...))
    //   9) RG Service executes query with SQL predicates and returns results
    let page = svc.list_groups(&ctx, &query).await?;
    // @cpt-end:cpt-cf-resource-group-flow-integration-auth-jwt-request:p1:inst-jwt-9
    // @cpt-end:cpt-cf-resource-group-flow-integration-auth-jwt-request:p1:inst-jwt-8
    // @cpt-end:cpt-cf-resource-group-flow-integration-auth-jwt-request:p1:inst-jwt-7
    // @cpt-end:cpt-cf-resource-group-flow-integration-auth-jwt-request:p1:inst-jwt-6
    // @cpt-end:cpt-cf-resource-group-flow-integration-auth-jwt-request:p1:inst-jwt-5
    // @cpt-end:cpt-cf-resource-group-flow-integration-auth-jwt-request:p1:inst-jwt-4
    // @cpt-end:cpt-cf-resource-group-flow-integration-auth-jwt-request:p1:inst-jwt-3
    let dto_page = page.map_items(GroupDto::from);

    // @cpt-begin:cpt-cf-resource-group-flow-integration-auth-jwt-request:p1:inst-jwt-10
    // RETURN response to actor
    Ok(Json(dto_page))
    // @cpt-end:cpt-cf-resource-group-flow-integration-auth-jwt-request:p1:inst-jwt-10
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
pub async fn get_group_descendants(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteGroupService>>,
    Path(group_id): Path<uuid::Uuid>,
    OData(query): OData,
) -> ApiResult<Json<toolkit_odata::Page<GroupWithDepthDto>>> {
    info!(group_id = %group_id, "Getting group descendants");
    let page = svc.get_group_descendants(&ctx, group_id, &query).await?;
    Ok(Json(page.map_items(GroupWithDepthDto::from)))
}

#[tracing::instrument(
    skip(svc, ctx, query),
    fields(
        group.id = %group_id,
        request_id = Empty,
    )
)]
pub async fn get_group_ancestors(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteGroupService>>,
    Path(group_id): Path<uuid::Uuid>,
    OData(query): OData,
) -> ApiResult<Json<toolkit_odata::Page<GroupWithDepthDto>>> {
    info!(group_id = %group_id, "Getting group ancestors");
    let page = svc.get_group_ancestors(&ctx, group_id, &query).await?;
    Ok(Json(page.map_items(GroupWithDepthDto::from)))
}
