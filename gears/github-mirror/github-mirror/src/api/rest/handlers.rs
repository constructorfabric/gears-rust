use std::sync::Arc;

use axum::{Json, extract::Extension};
use toolkit::api::canonical_prelude::*;

use crate::api::rest::routes::ConcreteService;

use super::dto::GithubMirrorHealthDto;

pub async fn health(
    Extension(svc): Extension<Arc<ConcreteService>>,
) -> ApiResult<JsonBody<GithubMirrorHealthDto>> {
    let status = svc.status();
    Ok(Json(status.into()))
}
