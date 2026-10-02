use std::sync::Arc;

use axum::Extension;
use toolkit::api::canonical_prelude::*;
use toolkit::api::rest::extract::Path;
use toolkit_security::SecurityContext;

use crate::api::rest::dto::{ModelDto, ModelListDto};
use crate::gear::AppServices;

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
