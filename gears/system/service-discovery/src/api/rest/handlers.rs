use axum::Extension;
use std::sync::Arc;
use toolkit::api::canonical_prelude::*;

use super::dto::GearDto;
use crate::domain::service::GearsService;

/// List all registered gears with their capabilities, instances, and deployment mode.
///
/// # Errors
///
/// Returns `ApiError` if the response cannot be constructed.
pub async fn list_gears(
    Extension(svc): Extension<Arc<GearsService>>,
) -> ApiResult<Json<Vec<GearDto>>> {
    let gears: Vec<GearDto> = svc.list_gears().iter().map(GearDto::from).collect();
    Ok(Json(gears))
}
