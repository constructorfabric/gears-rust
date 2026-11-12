use axum::Json;
use axum::extract::{Extension, Path};
use axum::response::IntoResponse;
use http::StatusCode;
use toolkit_canonical_errors::builder::{NeedsFieldViolation, ResourceOptional};
use toolkit_canonical_errors::{Problem, ResourceErrorBuilder};
use toolkit_security::SecurityContext;

use crate::api::rest::dto::{CreateRouteRequest, RouteResponse, UpdateRouteRequest};
use crate::api::rest::error::{OagwRouteError, domain_error_to_problem};
use crate::api::rest::extractors::{ListQueryParams, parse_gts_id, parse_list_query};
use crate::domain::gts_helpers as gts;
use crate::domain::model::Route;
use crate::gear::AppState;

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
) -> Result<impl IntoResponse, Box<Problem>> {
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
) -> Result<impl IntoResponse, Box<Problem>> {
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
#[cfg_attr(test, derive(serde::Serialize, PartialEq))]
pub struct ListRoutesQuery {
    /// Optional upstream GTS identifier to filter routes by upstream.
    #[serde(default)]
    pub upstream_id: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: u32,
    #[serde(default)]
    pub offset: u32,
}

impl ListQueryParams for ListRoutesQuery {
    const KEYS: &'static [&'static str] = &["upstream_id", "limit", "offset"];

    fn invalid_argument() -> ResourceErrorBuilder<ResourceOptional, NeedsFieldViolation> {
        OagwRouteError::invalid_argument()
    }
}

fn default_limit() -> u32 {
    50
}

pub async fn list_routes(
    Extension(state): Extension<AppState>,
    Extension(ctx): Extension<SecurityContext>,
    uri: http::Uri,
) -> Result<impl IntoResponse, Box<Problem>> {
    let instance = "/oagw/v1/routes";
    let params: ListRoutesQuery = parse_list_query(&uri, instance)?;
    let upstream_uuid = params
        .upstream_id
        .as_deref()
        .map(|id| parse_gts_id(id, gts::UPSTREAM_SCHEMA, instance))
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
) -> Result<impl IntoResponse, Box<Problem>> {
    let instance = format!("/oagw/v1/routes/{id}");
    let uuid = parse_gts_id(&id, gts::ROUTE_SCHEMA, &instance)?;
    let route = state
        .cp
        .update_route(&ctx, uuid, req.into())
        .await
        .map_err(|e| domain_error_to_problem(e, &instance))?;
    Ok(Json(to_response(route)))
}

pub async fn delete_route(
    Extension(state): Extension<AppState>,
    Extension(ctx): Extension<SecurityContext>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, Box<Problem>> {
    let instance = format!("/oagw/v1/routes/{id}");
    let uuid = parse_gts_id(&id, gts::ROUTE_SCHEMA, &instance)?;
    state
        .cp
        .delete_route(&ctx, uuid)
        .await
        .map_err(|e| domain_error_to_problem(e, &instance))?;
    Ok(StatusCode::NO_CONTENT)
}
