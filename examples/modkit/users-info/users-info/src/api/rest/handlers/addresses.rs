use axum::Extension;
use axum::extract::Path;
use axum::response::IntoResponse;
use tracing::field::Empty;
use uuid::Uuid;

use super::{
    AddressDto, ApiResult, Json, JsonBody, PutAddressReq, SecurityContext, info, no_content,
};
use crate::module::ConcreteAppServices;

/// Get address for a specific user
#[tracing::instrument(
    skip(svc, ctx),
    fields(
        user.id = %user_id,
        request_id = Empty,
        requester.id = %ctx.subject_id()
    )
)]
pub async fn get_user_address(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<std::sync::Arc<ConcreteAppServices>>,
    Path(user_id): Path<Uuid>,
) -> ApiResult<JsonBody<AddressDto>> {
    info!(
        user_id = %user_id,
        requester_id = %ctx.subject_id(),
        "Getting user address"
    );

    let address = svc.addresses.get_user_address(&ctx, user_id).await?;

    let address =
        address.ok_or_else(|| crate::domain::error::DomainError::not_found("Address", user_id))?;

    Ok(Json(AddressDto::from(address)))
}

/// Upsert address for a specific user (PUT = create or replace)
#[tracing::instrument(
    skip(svc, req_body, ctx),
    fields(
        user.id = %user_id,
        request_id = Empty,
        updater.id = %ctx.subject_id()
    )
)]
pub async fn put_user_address(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<std::sync::Arc<ConcreteAppServices>>,
    Path(user_id): Path<Uuid>,
    Json(req_body): Json<PutAddressReq>,
) -> ApiResult<impl IntoResponse> {
    info!(
        user_id = %user_id,
        updater_id = %ctx.subject_id(),
        "Upserting user address"
    );

    let new_address = req_body.into_new_address(user_id);
    let address = svc
        .addresses
        .put_user_address(&ctx, user_id, new_address)
        .await?;

    Ok(Json(AddressDto::from(address)).into_response())
}

/// Delete address for a specific user
#[tracing::instrument(
    skip(svc, ctx),
    fields(
        user.id = %user_id,
        request_id = Empty,
        deleter.id = %ctx.subject_id()
    )
)]
pub async fn delete_user_address(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<std::sync::Arc<ConcreteAppServices>>,
    Path(user_id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    info!(
        user_id = %user_id,
        deleter_id = %ctx.subject_id(),
        "Deleting user address"
    );

    svc.addresses.delete_user_address(&ctx, user_id).await?;
    Ok(no_content().into_response())
}
