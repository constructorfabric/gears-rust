use std::sync::Arc;

use axum::{
    Json,
    extract::{Extension, Path},
};
use simple_user_settings_sdk::models::SimpleUserSettingsUpdate;
use toolkit::api::canonical_prelude::*;
use toolkit_security::SecurityContext;

use crate::domain::error::DomainError;

use crate::api::rest::routes::ConcreteService;

use super::dto::{
    NamedSettingDto, NamedSettingsListDto, PatchSimpleUserSettingsRequest, PutNamedSettingRequest,
    SimpleUserSettingsDto, UpdateSimpleUserSettingsRequest,
};

pub async fn get_settings(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
) -> ApiResult<JsonBody<SimpleUserSettingsDto>> {
    let settings = svc.get_settings(&ctx).await?;
    Ok(Json(settings.into()))
}

pub async fn update_settings(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Json(req): Json<UpdateSimpleUserSettingsRequest>,
) -> ApiResult<impl IntoResponse> {
    let update = SimpleUserSettingsUpdate {
        theme: req.theme,
        language: req.language,
    };
    let settings = svc.update_settings(&ctx, update).await?;
    let dto: SimpleUserSettingsDto = settings.into();
    Ok((StatusCode::OK, Json(dto)))
}

pub async fn patch_settings(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Json(req): Json<PatchSimpleUserSettingsRequest>,
) -> ApiResult<JsonBody<SimpleUserSettingsDto>> {
    let settings = svc.patch_settings(&ctx, req.into()).await?;
    Ok(Json(settings.into()))
}

pub async fn list_named_settings(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
) -> ApiResult<JsonBody<NamedSettingsListDto>> {
    let settings = svc.list_named_settings(&ctx).await?;
    Ok(Json(NamedSettingsListDto {
        settings: settings.into_iter().map(Into::into).collect(),
    }))
}

pub async fn get_named_setting(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Path(key): Path<String>,
) -> ApiResult<JsonBody<NamedSettingDto>> {
    let setting = svc
        .get_named_setting(&ctx, &key)
        .await?
        .ok_or(DomainError::NotFound)?;
    Ok(Json(setting.into()))
}

pub async fn put_named_setting(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Path(key): Path<String>,
    Json(req): Json<PutNamedSettingRequest>,
) -> ApiResult<JsonBody<NamedSettingDto>> {
    let setting = svc.put_named_setting(&ctx, &key, req.value).await?;
    Ok(Json(setting.into()))
}

/// `204` whether or not the key was set: the caller asked for it to be gone,
/// and it is.
pub async fn delete_named_setting(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Path(key): Path<String>,
) -> ApiResult<impl IntoResponse> {
    svc.delete_named_setting(&ctx, &key).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `204` whether or not anything was set, like the single-key delete.
pub async fn delete_all_named_settings(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
) -> ApiResult<impl IntoResponse> {
    svc.delete_all_named_settings(&ctx).await?;
    Ok(StatusCode::NO_CONTENT)
}
