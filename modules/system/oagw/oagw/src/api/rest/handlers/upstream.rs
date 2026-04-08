use axum::Json;
use axum::extract::{Extension, Path, Query};
use axum::response::IntoResponse;
use http::StatusCode;
use modkit::api::problem::Problem;
use modkit_security::SecurityContext;

use crate::api::rest::dto::{CreateUpstreamRequest, UpdateUpstreamRequest, UpstreamResponse};
use crate::api::rest::error::domain_error_to_problem;
use crate::api::rest::extractors::{PaginationQuery, parse_gts_id};
use crate::domain::gts_helpers as gts;
use crate::domain::model::Upstream;
use crate::module::AppState;

fn to_response(u: Upstream) -> UpstreamResponse {
    UpstreamResponse {
        id: gts::format_upstream_gts(u.id),
        tenant_id: u.tenant_id,
        alias: u.alias,
        server: u.server.into(),
        protocol: u.protocol,
        enabled: u.enabled,
        auth: u.auth.map(Into::into),
        headers: u.headers.map(Into::into),
        plugins: u.plugins.map(Into::into),
        rate_limit: u.rate_limit.map(Into::into),
        tags: u.tags,
    }
}

pub async fn create_upstream(
    Extension(state): Extension<AppState>,
    Extension(ctx): Extension<SecurityContext>,
    Json(req): Json<CreateUpstreamRequest>,
) -> Result<impl IntoResponse, Problem> {
    let upstream = state
        .cp
        .create_upstream(&ctx, req.into())
        .await
        .map_err(|e| domain_error_to_problem(e, "/oagw/v1/upstreams"))?;
    // Defensive no-op: new IDs have no cache entry, but keeps CRUD handlers uniform.
    state.backend_selector.invalidate(upstream.id);
    Ok((StatusCode::CREATED, Json(to_response(upstream))))
}

pub async fn get_upstream(
    Extension(state): Extension<AppState>,
    Extension(ctx): Extension<SecurityContext>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, Problem> {
    let instance = format!("/oagw/v1/upstreams/{id}");
    let uuid = parse_gts_id(&id, &instance)?;
    let upstream = state
        .cp
        .get_upstream(&ctx, uuid)
        .await
        .map_err(|e| domain_error_to_problem(e, &instance))?;
    Ok(Json(to_response(upstream)))
}

pub async fn list_upstreams(
    Extension(state): Extension<AppState>,
    Extension(ctx): Extension<SecurityContext>,
    Query(pagination): Query<PaginationQuery>,
) -> Result<impl IntoResponse, Problem> {
    let query = pagination.to_list_query();
    let upstreams = state
        .cp
        .list_upstreams(&ctx, &query)
        .await
        .map_err(|e| domain_error_to_problem(e, "/oagw/v1/upstreams"))?;
    let response: Vec<UpstreamResponse> = upstreams.into_iter().map(to_response).collect();
    Ok(Json(response))
}

pub async fn update_upstream(
    Extension(state): Extension<AppState>,
    Extension(ctx): Extension<SecurityContext>,
    Path(id): Path<String>,
    Json(req): Json<UpdateUpstreamRequest>,
) -> Result<impl IntoResponse, Problem> {
    let instance = format!("/oagw/v1/upstreams/{id}");
    let uuid = parse_gts_id(&id, &instance)?;
    let upstream = state
        .cp
        .update_upstream(&ctx, uuid, req.into())
        .await
        .map_err(|e| domain_error_to_problem(e, &instance))?;
    state.backend_selector.invalidate(upstream.id);
    Ok(Json(to_response(upstream)))
}

pub async fn delete_upstream(
    Extension(state): Extension<AppState>,
    Extension(ctx): Extension<SecurityContext>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, Problem> {
    let instance = format!("/oagw/v1/upstreams/{id}");
    let uuid = parse_gts_id(&id, &instance)?;
    state
        .cp
        .delete_upstream(&ctx, uuid)
        .await
        .map_err(|e| domain_error_to_problem(e, &instance))?;
    state.backend_selector.invalidate(uuid);
    state.dp.remove_rate_limit_key(&format!("upstream:{uuid}"));
    Ok(StatusCode::NO_CONTENT)
}
