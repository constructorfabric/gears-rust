//! REST handlers for the Types Registry module.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Extension, Path, Query};
use modkit::api::prelude::*;
use modkit::api::problem::Problem;
use types_registry_sdk::RegisterSummary;

use super::dto::{
    GtsEntityDto, ListEntitiesQuery, ListEntitiesResponse, RegisterEntitiesRequest,
    RegisterEntitiesResponse, RegisterResultDto, RegisterSummaryDto,
};
use crate::domain::error::DomainError;
use crate::domain::service::TypesRegistryService;

/// POST /api/v1/types-registry/entities
///
/// Register GTS entities in batch.
/// REST API always validates entities, regardless of ready state.
/// However, REST API is blocked until service is ready.
pub async fn register_entities(
    Extension(service): Extension<Arc<TypesRegistryService>>,
    Json(req): Json<RegisterEntitiesRequest>,
) -> ApiResult<(StatusCode, Json<RegisterEntitiesResponse>)> {
    if !service.is_ready() {
        return Err(DomainError::NotInReadyMode.into());
    }

    let results = service.register_validated(req.entities);

    let summary = RegisterSummary::from_results(&results);
    let result_dtos: Vec<RegisterResultDto> = results.into_iter().map(Into::into).collect();

    let response = RegisterEntitiesResponse {
        summary: RegisterSummaryDto::from(summary),
        results: result_dtos,
    };

    Ok((StatusCode::OK, Json(response)))
}

/// GET /api/v1/types-registry/entities
///
/// List GTS entities with optional filtering.
pub async fn list_entities(
    Extension(service): Extension<Arc<TypesRegistryService>>,
    Query(query): Query<ListEntitiesQuery>,
) -> ApiResult<Json<ListEntitiesResponse>> {
    if !service.is_ready() {
        return Err(DomainError::NotInReadyMode.into());
    }

    let list_query = query.to_list_query();

    let entities = service.list(&list_query).map_err(Problem::from)?;

    let entity_dtos: Vec<GtsEntityDto> = entities.into_iter().map(Into::into).collect();
    let count = entity_dtos.len();

    Ok(Json(ListEntitiesResponse {
        entities: entity_dtos,
        count,
    }))
}

/// GET /api/v1/types-registry/entities/{gts_id}
///
/// Get a single GTS entity by its identifier.
pub async fn get_entity(
    Extension(service): Extension<Arc<TypesRegistryService>>,
    Path(gts_id): Path<String>,
) -> ApiResult<Json<GtsEntityDto>> {
    if !service.is_ready() {
        return Err(DomainError::NotInReadyMode.into());
    }

    let entity = service.get(&gts_id).map_err(Problem::from)?;

    Ok(Json(entity.into()))
}
#[cfg(test)]
#[path = "handlers_tests.rs"]
mod tests;
