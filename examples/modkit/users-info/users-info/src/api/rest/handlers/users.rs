use axum::Extension;
use axum::extract::Path;
use axum::http::Uri;
use axum::response::IntoResponse;
use tracing::field::Empty;
use uuid::Uuid;

use modkit::api::odata::OData;

use super::{
    ApiResult, Json, JsonBody, JsonPage, SecurityContext, UpdateUserReq, UserDto, UserFullDto,
    apply_select, created_json, info, no_content, page_to_projected_json,
};
use crate::api::rest::dto::CreateUserReq;
use crate::module::ConcreteAppServices;

/// List users with cursor-based pagination and optional field projection via $select
#[tracing::instrument(
    skip(svc, query, ctx),
    fields(
        limit = query.limit,
        request_id = Empty,
        user.id = %ctx.subject_id()
    )
)]
pub async fn list_users(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<std::sync::Arc<ConcreteAppServices>>,
    OData(query): OData,
) -> ApiResult<JsonPage<serde_json::Value>> {
    info!(
        user_id = %ctx.subject_id(),
        "Listing users with cursor pagination"
    );

    let page = svc.users.list_users_page(&ctx, &query).await?;
    let page = page.map_items(UserDto::from);

    Ok(Json(page_to_projected_json(&page, query.selected_fields())))
}

/// Get a specific user by ID with optional field projection via $select
#[tracing::instrument(
    skip(svc, ctx),
    fields(
        user.id = %id,
        request_id = Empty,
        requester.id = %ctx.subject_id()
    )
)]
pub async fn get_user(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<std::sync::Arc<ConcreteAppServices>>,
    Path(id): Path<Uuid>,
    OData(query): OData,
) -> ApiResult<JsonBody<serde_json::Value>> {
    info!(
        user_id = %id,
        requester_id = %ctx.subject_id(),
        "Getting user details with related entities"
    );

    let user_full = svc.users.get_user_full(&ctx, id).await?;
    let user_full_dto = UserFullDto::from(user_full);
    let projected = apply_select(&user_full_dto, query.selected_fields());
    Ok(Json(projected))
}

/// Create a new user
#[tracing::instrument(
    skip(svc, req_body, ctx, uri),
    fields(
        user.email = %req_body.email,
        user.display_name = %req_body.display_name,
        user.tenant_id = %req_body.tenant_id,
        request_id = Empty,
        creator.id = %ctx.subject_id()
    )
)]
pub async fn create_user(
    uri: Uri,
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<std::sync::Arc<ConcreteAppServices>>,
    Json(req_body): Json<CreateUserReq>,
) -> ApiResult<impl IntoResponse> {
    info!(
        email = %req_body.email,
        display_name = %req_body.display_name,
        tenant_id = %req_body.tenant_id,
        creator_id = %ctx.subject_id(),
        "Creating new user"
    );

    let CreateUserReq {
        id,
        tenant_id,
        email,
        display_name,
    } = req_body;

    let new_user = users_info_sdk::NewUser {
        id,
        tenant_id,
        email,
        display_name,
    };

    let user = svc.users.create_user(&ctx, new_user).await?;
    let id_str = user.id.to_string();
    Ok(created_json(UserDto::from(user), &uri, &id_str).into_response())
}

/// Update an existing user
#[tracing::instrument(
    skip(svc, req_body, ctx),
    fields(
        user.id = %id,
        request_id = Empty,
        updater.id = %ctx.subject_id()
    )
)]
pub async fn update_user(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<std::sync::Arc<ConcreteAppServices>>,
    Path(id): Path<Uuid>,
    Json(req_body): Json<UpdateUserReq>,
) -> ApiResult<JsonBody<UserDto>> {
    info!(
        user_id = %id,
        updater_id = %ctx.subject_id(),
        "Updating user"
    );

    let patch = req_body.into();
    let user = svc.users.update_user(&ctx, id, patch).await?;
    Ok(Json(UserDto::from(user)))
}

/// Delete a user by ID
#[tracing::instrument(
    skip(svc, ctx),
    fields(
        user.id = %id,
        request_id = Empty,
        deleter.id = %ctx.subject_id()
    )
)]
pub async fn delete_user(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<std::sync::Arc<ConcreteAppServices>>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    info!(
        user_id = %id,
        deleter_id = %ctx.subject_id(),
        "Deleting user"
    );

    svc.users.delete_user(&ctx, id).await?;
    Ok(no_content().into_response())
}
