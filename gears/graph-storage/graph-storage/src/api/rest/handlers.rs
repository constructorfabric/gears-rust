//! REST handlers. Thin by construction: they translate DTOs, read the
//! idempotency header, and delegate. Every admission bound and every
//! authorization decision lives in the domain services, so the `ClientHub` path
//! cannot be a weaker door into the same data.

use std::sync::Arc;

use axum::extract::{Path, Query};
use axum::http::HeaderMap;
use axum::{Extension, Json};
use graph_storage_sdk::models as m;
use serde::Deserialize;
use toolkit::api::odata::OData;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;

use crate::api::rest::dto;
use crate::domain::error::DomainError;
use crate::domain::service::GraphServices;

pub type ApiResult<T> = Result<T, CanonicalError>;

/// Idempotency travels in the platform header; the body field is the SDK's
/// mirror of it, and the header wins when both are present.
fn idempotency_key(headers: &HeaderMap, body: Option<String>) -> Option<String> {
    headers
        .get(toolkit_http::IDEMPOTENCY_KEY_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
        .or(body)
}

#[tracing::instrument(skip_all, fields(user.id = %ctx.subject_id()))]
pub async fn register_types(
    Extension(ctx): Extension<SecurityContext>,
    Extension(services): Extension<Arc<GraphServices>>,
    Json(request): Json<dto::GraphRegisterTypesRequest>,
) -> ApiResult<Json<Vec<dto::GraphTypeDto>>> {
    let batch: Vec<m::TypeRegistration> = request.types.into_iter().map(Into::into).collect();
    let records = services.register_types(&ctx, batch).await?;
    Ok(Json(records.into_iter().map(Into::into).collect()))
}

#[derive(Debug, Deserialize)]
pub struct ListTypesParams {
    /// `node`, `edge` or `attribute`.
    pub kind: Option<String>,
    /// GTS identifier pattern, resolved by the platform matcher. Spelled
    /// `pattern`, not `$filter`: it is not an `OData` filter over columns, and
    /// binding it as one would promise a filter surface the ontology has not
    /// got.
    pub pattern: Option<String>,
    pub limit: Option<u32>,
    /// Anything else the caller sent. Collected so it can be refused: a
    /// parameter silently ignored is a filter the caller believes is applied,
    /// which is the failure mode the projection's `OData` binding exists to
    /// prevent — the catalog owes callers the same.
    #[serde(flatten)]
    pub rest: std::collections::BTreeMap<String, String>,
}

#[tracing::instrument(skip_all, fields(user.id = %ctx.subject_id()))]
pub async fn list_types(
    Extension(ctx): Extension<SecurityContext>,
    Extension(services): Extension<Arc<GraphServices>>,
    Query(params): Query<ListTypesParams>,
) -> ApiResult<Json<dto::GraphTypeListDto>> {
    if let Some(unknown) = params.rest.keys().next() {
        return Err(DomainError::invalid(format!(
            "`{unknown}` is not an accepted query option; the type catalog takes \
             `kind`, `pattern` and `limit`"
        ))
        .into());
    }
    let kind = match params.kind.as_deref() {
        None => None,
        Some("node") => Some(m::TypeKind::Node),
        Some("edge") => Some(m::TypeKind::Edge),
        Some("attribute") => Some(m::TypeKind::Attribute),
        Some(other) => {
            return Err(DomainError::invalid(format!("unknown type kind `{other}`")).into());
        }
    };
    let page = services
        .list_types(
            &ctx,
            m::TypeQuery {
                kind,
                pattern: params.pattern,
                top: params.limit,
                cursor: None,
            },
        )
        .await?;
    Ok(Json(dto::GraphTypeListDto {
        items: page.items.into_iter().map(Into::into).collect(),
        next_cursor: page.next_cursor,
        revision: page.revision.into(),
    }))
}

#[tracing::instrument(skip_all, fields(user.id = %ctx.subject_id()))]
pub async fn get_type(
    Extension(ctx): Extension<SecurityContext>,
    Extension(services): Extension<Arc<GraphServices>>,
    Path(type_id): Path<String>,
) -> ApiResult<Json<dto::GraphTypeDto>> {
    let record = services.get_type(&ctx, &type_id).await?;
    Ok(Json(record.into()))
}

#[tracing::instrument(skip_all, fields(user.id = %ctx.subject_id()))]
pub async fn ingest(
    Extension(ctx): Extension<SecurityContext>,
    Extension(services): Extension<Arc<GraphServices>>,
    headers: HeaderMap,
    Json(request): Json<dto::GraphIngestRequest>,
) -> ApiResult<Json<dto::GraphIngestResultDto>> {
    let key = idempotency_key(&headers, request.idempotency_key);
    let ingest = m::IngestRequest {
        nodes: request.nodes.into_iter().map(Into::into).collect(),
        edges: request.edges.into_iter().map(Into::into).collect(),
        options: m::IngestOptions {
            create_phantoms: request.options.create_phantoms,
            report_per_item: request.options.report_per_item,
            embed: request.options.embed,
        },
        replace_scope: request.replace_scope.map(Into::into),
        idempotency_key: key,
    };
    let outcome = services.ingest(&ctx, ingest).await?;
    Ok(Json(outcome.into()))
}

#[derive(Debug, Deserialize)]
pub struct GetNodeParams {
    pub adjacency_limit: Option<u32>,
}

#[tracing::instrument(skip_all, fields(user.id = %ctx.subject_id()))]
pub async fn get_node(
    Extension(ctx): Extension<SecurityContext>,
    Extension(services): Extension<Arc<GraphServices>>,
    Path(node_key): Path<String>,
    Query(params): Query<GetNodeParams>,
) -> ApiResult<Json<dto::GraphNodeDto>> {
    let view = services
        .get_node(&ctx, &node_key, params.adjacency_limit)
        .await?;
    Ok(Json(view.into()))
}

/// Tabular projection.
///
/// The `OData` extractor is the platform binding: it parses and validates the
/// five accepted system query options — `cursor` included, as the documented
/// alias for `$skiptoken` — and refuses anything else, so an option a client
/// believes is applied can never be quietly dropped.
#[tracing::instrument(skip_all, fields(user.id = %ctx.subject_id()))]
pub async fn project_nodes(
    Extension(ctx): Extension<SecurityContext>,
    Extension(services): Extension<Arc<GraphServices>>,
    Query(params): Query<ProjectionTypeParams>,
    OData(query): OData,
) -> ApiResult<Json<toolkit_odata::Page<dto::GraphNodeRowDto>>> {
    let patterns: Vec<String> = params
        .type_pattern
        .map(|raw| raw.split(',').map(|p| p.trim().to_owned()).collect())
        .unwrap_or_default();
    let page = services.project_nodes(&ctx, &patterns, query).await?;
    Ok(Json(page.map_items(dto::GraphNodeRowDto::from)))
}

/// The type narrowing of a projection.
///
/// A plain parameter rather than an `OData` option: a GTS pattern is not a
/// filter expression over columns, and the interned type reference the rows
/// actually carry is not addressable in one either.
#[derive(Debug, Deserialize)]
pub struct ProjectionTypeParams {
    /// Comma-separated GTS identifier patterns.
    pub type_pattern: Option<String>,
}

#[tracing::instrument(skip_all, fields(user.id = %ctx.subject_id()))]
pub async fn delete_node(
    Extension(ctx): Extension<SecurityContext>,
    Extension(services): Extension<Arc<GraphServices>>,
    Path(node_key): Path<String>,
) -> ApiResult<Json<dto::GraphDeleteResultDto>> {
    let outcome = services.delete_node(&ctx, &node_key).await?;
    Ok(Json(outcome.into()))
}

#[tracing::instrument(skip_all, fields(user.id = %ctx.subject_id()))]
pub async fn delete_edge(
    Extension(ctx): Extension<SecurityContext>,
    Extension(services): Extension<Arc<GraphServices>>,
    Path(edge_key): Path<String>,
) -> ApiResult<Json<dto::GraphDeleteResultDto>> {
    let outcome = services.delete_edge(&ctx, &edge_key).await?;
    Ok(Json(outcome.into()))
}

#[tracing::instrument(skip_all, fields(user.id = %ctx.subject_id()))]
pub async fn search(
    Extension(ctx): Extension<SecurityContext>,
    Extension(services): Extension<Arc<GraphServices>>,
    Json(request): Json<dto::GraphSearchRequest>,
) -> ApiResult<Json<dto::GraphSearchResponseDto>> {
    let mode = match request.mode.as_str() {
        "lexical" => m::SearchMode::Lexical,
        "vector" => m::SearchMode::Vector,
        "hybrid" => m::SearchMode::Hybrid,
        other => {
            return Err(DomainError::invalid(format!(
                "unknown search mode `{other}`; expected lexical, vector or hybrid"
            ))
            .into());
        }
    };
    let arm_limit = request.arm_limit.unwrap_or(20);
    let response = services
        .search(
            &ctx,
            m::SearchRequest {
                mode,
                query: request.query,
                arm_limit,
                limit: request.limit.unwrap_or(arm_limit),
                type_patterns: request.type_patterns,
            },
        )
        .await?;
    Ok(Json(response.into()))
}

#[tracing::instrument(skip_all, fields(user.id = %ctx.subject_id()))]
pub async fn traverse(
    Extension(ctx): Extension<SecurityContext>,
    Extension(services): Extension<Arc<GraphServices>>,
    Json(request): Json<dto::GraphTraverseRequest>,
) -> ApiResult<Json<dto::GraphTraversalResponseDto>> {
    let response = services
        .traverse(
            &ctx,
            m::TraverseRequest {
                seeds: request.seeds,
                depth: request.depth,
                edge_type_patterns: request.edge_type_patterns,
                node_type_patterns: request.node_type_patterns,
                max_nodes: request.max_nodes,
            },
        )
        .await?;
    Ok(Json(response.into()))
}

#[tracing::instrument(skip_all, fields(user.id = %ctx.subject_id()))]
pub async fn neighborhood(
    Extension(ctx): Extension<SecurityContext>,
    Extension(services): Extension<Arc<GraphServices>>,
    Json(request): Json<dto::GraphNeighborhoodRequest>,
) -> ApiResult<Json<dto::GraphTraversalResponseDto>> {
    let response = services
        .neighborhood(
            &ctx,
            m::NeighborhoodRequest {
                root: request.root,
                depth: request.depth,
                node_budget: request.node_budget,
                include_phantoms: request.include_phantoms,
            },
        )
        .await?;
    Ok(Json(response.into()))
}

#[tracing::instrument(skip_all, fields(user.id = %ctx.subject_id()))]
pub async fn revision(
    Extension(ctx): Extension<SecurityContext>,
    Extension(services): Extension<Arc<GraphServices>>,
) -> ApiResult<Json<dto::GraphRevisionDto>> {
    let revision = services.revision(&ctx).await?;
    Ok(Json(revision.into()))
}
