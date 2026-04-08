use std::sync::Arc;

use axum::Extension;
use axum::extract::Path;
use modkit::api::prelude::*;
use modkit_security::SecurityContext;

use crate::api::rest::dto::{ModelDto, ModelListDto};
use crate::module::AppServices;

/// GET /mini-chat/v1/models
#[tracing::instrument(skip(svc, ctx))]
pub(crate) async fn list_models(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<AppServices>>,
) -> ApiResult<JsonBody<ModelListDto>> {
    let models = svc.models.list_models(&ctx).await?;
    let items = models.into_iter().map(ModelDto::from).collect();
    Ok(Json(ModelListDto { items }))
}

/// GET /mini-chat/v1/models/{id}
#[tracing::instrument(skip(svc, ctx), fields(model_id = %id))]
pub(crate) async fn get_model(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<AppServices>>,
    Path(id): Path<String>,
) -> ApiResult<JsonBody<ModelDto>> {
    let model = svc.models.get_model(&ctx, &id).await?;
    Ok(Json(ModelDto::from(model)))
}
