use axum::Json;
use axum::extract::{Extension, Path, Query};
use axum::response::IntoResponse;
use http::StatusCode;
use modkit_canonical_errors::Problem;
use modkit_security::SecurityContext;

use crate::api::rest::dto::{CreateRouteRequest, RouteResponse, UpdateRouteRequest};
use crate::api::rest::error::domain_error_to_problem;
use crate::api::rest::extractors::parse_gts_id;
use crate::domain::gts_helpers as gts;
use crate::domain::model::Route;
use crate::module::AppState;

fn to_response(r: Route) -> RouteResponse {
    RouteResponse {
        id: gts::format_route_gts(r.id),
        tenant_id: r.tenant_id,
        upstream_id: gts::format_upstream_gts(r.upstream_id),
        match_rules: r.match_rules.into(),
        plugins: r.plugins.map(Into::into),
        rate_limit: r.rate_limit.map(Into::into),
        cors: r.cors.map(Into::into),
        tags: r.tags,
        priority: r.priority,
        enabled: r.enabled,
    }
}

pub async fn create_route(
    Extension(state): Extension<AppState>,
    Extension(ctx): Extension<SecurityContext>,
    Json(req): Json<CreateRouteRequest>,
) -> Result<impl IntoResponse, Problem> {
    let instance = "/oagw/v1/routes";
    let upstream_uuid = parse_gts_id(&req.upstream_id, gts::UPSTREAM_SCHEMA, instance)?;
    let route = state
        .cp
        .create_route(&ctx, (upstream_uuid, req).into())
        .await
        .map_err(|e| domain_error_to_problem(e, instance))?;
    Ok((StatusCode::CREATED, Json(to_response(route))))
}

pub async fn get_route(
    Extension(state): Extension<AppState>,
    Extension(ctx): Extension<SecurityContext>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, Problem> {
    let instance = format!("/oagw/v1/routes/{id}");
    let uuid = parse_gts_id(&id, gts::ROUTE_SCHEMA, &instance)?;
    let route = state
        .cp
        .get_route(&ctx, uuid)
        .await
        .map_err(|e| domain_error_to_problem(e, &instance))?;
    Ok(Json(to_response(route)))
}

/// Query parameters for `GET /oagw/v1/routes`.
#[derive(Debug, serde::Deserialize)]
pub struct ListRoutesQuery {
    /// Optional upstream GTS identifier to filter routes by upstream.
    #[serde(default)]
    pub upstream_id: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: u32,
    #[serde(default)]
    pub offset: u32,
}

fn default_limit() -> u32 {
    50
}

pub async fn list_routes(
    Extension(state): Extension<AppState>,
    Extension(ctx): Extension<SecurityContext>,
    Query(params): Query<ListRoutesQuery>,
) -> Result<impl IntoResponse, Problem> {
    let instance = "/oagw/v1/routes";
    let upstream_uuid = params
        .upstream_id
        .as_deref()
        .map(
            #[allow(clippy::result_large_err)]
            |id| parse_gts_id(id, gts::UPSTREAM_SCHEMA, instance),
        )
        .transpose()?;
    let query = crate::domain::model::ListQuery {
        top: params.limit.min(100),
        skip: params.offset,
    };
    let routes = state
        .cp
        .list_routes(&ctx, upstream_uuid, &query)
        .await
        .map_err(|e| domain_error_to_problem(e, instance))?;
    let response: Vec<RouteResponse> = routes.into_iter().map(to_response).collect();
    Ok(Json(response))
}

pub async fn update_route(
    Extension(state): Extension<AppState>,
    Extension(ctx): Extension<SecurityContext>,
    Path(id): Path<String>,
    Json(req): Json<UpdateRouteRequest>,
) -> Result<impl IntoResponse, Problem> {
    let instance = format!("/oagw/v1/routes/{id}");
    let uuid = parse_gts_id(&id, gts::ROUTE_SCHEMA, &instance)?;
    // Snapshot old rate_limit before update so we can detect changes and
    // clean up stale rate-limit keys (avoids accumulating orphaned buckets).
    let old_rate_limit = state
        .cp
        .get_route(&ctx, uuid)
        .await
        .map_err(|e| domain_error_to_problem(e, &instance))?
        .rate_limit;
    let route = state
        .cp
        .update_route(&ctx, uuid, req.into())
        .await
        .map_err(|e| domain_error_to_problem(e, &instance))?;
    if route.rate_limit != old_rate_limit {
        state.dp.remove_rate_limit_keys_for_route(uuid);
    }
    Ok(Json(to_response(route)))
}

pub async fn delete_route(
    Extension(state): Extension<AppState>,
    Extension(ctx): Extension<SecurityContext>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, Problem> {
    let instance = format!("/oagw/v1/routes/{id}");
    let uuid = parse_gts_id(&id, gts::ROUTE_SCHEMA, &instance)?;
    state
        .cp
        .delete_route(&ctx, uuid)
        .await
        .map_err(|e| domain_error_to_problem(e, &instance))?;
    state.dp.remove_rate_limit_keys_for_route(uuid);
    Ok(StatusCode::NO_CONTENT)
}
