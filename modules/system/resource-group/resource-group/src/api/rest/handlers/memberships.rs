// @cpt-begin:cpt-cf-resource-group-dod-membership-rest-handlers:p1:inst-full
// @cpt-dod:cpt-cf-resource-group-dod-membership-rest-handlers:p1

use std::sync::Arc;

use axum::Extension;
use axum::extract::Path;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use tracing::field::Empty;

use modkit::api::odata::OData;
use modkit::api::prelude::*;

use super::{MembershipDto, SecurityContext, info};
use crate::module::ConcreteMembershipService;

/// Path parameters for membership add/remove endpoints.
#[derive(Debug, serde::Deserialize)]
pub struct MembershipPathParams {
    pub group_id: uuid::Uuid,
    pub resource_type: String,
    pub resource_id: String,
}

/// List memberships with optional `OData` filtering and pagination.
#[tracing::instrument(
    skip(svc, ctx, query),
    fields(request_id = Empty)
)]
pub async fn list_memberships(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteMembershipService>>,
    OData(query): OData,
) -> ApiResult<Json<modkit_odata::Page<MembershipDto>>> {
    info!("Listing memberships");

    let page = svc.list_memberships(&ctx, &query).await?;
    let dto_page = page.map_items(MembershipDto::from);

    Ok(Json(dto_page))
}

/// Add a membership link between a group and a resource.
#[tracing::instrument(
    skip(svc, ctx),
    fields(
        membership.group_id = %params.group_id,
        membership.resource_type = %params.resource_type,
        membership.resource_id = %params.resource_id,
        request_id = Empty,
    )
)]
// @cpt-begin:cpt-cf-resource-group-flow-membership-add:p1:inst-add-memb-1
pub async fn add_membership(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteMembershipService>>,
    Path(params): Path<MembershipPathParams>,
) -> ApiResult<impl IntoResponse> {
    info!(
        group_id = %params.group_id,
        resource_type = %params.resource_type,
        resource_id = %params.resource_id,
        "Adding membership"
    );

    let membership = svc
        .add_membership(
            &ctx,
            params.group_id,
            &params.resource_type,
            &params.resource_id,
        )
        .await?;
    let dto = MembershipDto::from(membership);

    Ok((StatusCode::CREATED, Json(dto)).into_response())
}
// @cpt-end:cpt-cf-resource-group-flow-membership-add:p1:inst-add-memb-1

/// Remove a membership link.
#[tracing::instrument(
    skip(svc, ctx),
    fields(
        membership.group_id = %params.group_id,
        membership.resource_type = %params.resource_type,
        membership.resource_id = %params.resource_id,
        request_id = Empty,
    )
)]
pub async fn remove_membership(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteMembershipService>>,
    Path(params): Path<MembershipPathParams>,
) -> ApiResult<impl IntoResponse> {
    info!(
        group_id = %params.group_id,
        resource_type = %params.resource_type,
        resource_id = %params.resource_id,
        "Removing membership"
    );

    svc.remove_membership(
        &ctx,
        params.group_id,
        &params.resource_type,
        &params.resource_id,
    )
    .await?;
    Ok(no_content().into_response())
}
// @cpt-end:cpt-cf-resource-group-dod-membership-rest-handlers:p1:inst-full
