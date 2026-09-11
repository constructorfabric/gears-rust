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

/// One migration step, or a named refusal.
///
/// The fields a step does not use must be *absent*, not ignored: a `drop`
/// carrying a `value` means the caller believes something will happen to it.
fn migration_step(step: &dto::GraphMigrationStepDto) -> Result<m::MigrationStep, DomainError> {
    let unexpected = |names: &[(&str, bool)]| -> Result<(), DomainError> {
        for (name, present) in names {
            if *present {
                return Err(DomainError::invalid(format!(
                    "a `{}` step takes no `{name}`",
                    step.op
                )));
            }
        }
        Ok(())
    };
    let required = |name: &str, value: Option<String>| {
        value.ok_or_else(|| DomainError::invalid(format!("a `{}` step needs `{name}`", step.op)))
    };
    match step.op.as_str() {
        "rename" => {
            unexpected(&[
                ("path", step.path.is_some()),
                ("value", step.value.is_some()),
            ])?;
            Ok(m::MigrationStep::Rename {
                from: required("from", step.from.clone())?,
                to: required("to", step.to.clone())?,
            })
        }
        "default" => {
            unexpected(&[("from", step.from.is_some()), ("to", step.to.is_some())])?;
            let path = required("path", step.path.clone())?;
            let value = step
                .value
                .clone()
                .ok_or_else(|| DomainError::invalid("a `default` step needs `value`".to_owned()))?;
            Ok(m::MigrationStep::Default { path, value })
        }
        "drop" => {
            unexpected(&[
                ("from", step.from.is_some()),
                ("to", step.to.is_some()),
                ("value", step.value.is_some()),
            ])?;
            Ok(m::MigrationStep::Drop {
                path: required("path", step.path.clone())?,
            })
        }
        other => Err(DomainError::invalid(format!(
            "`{other}` is not a migration step; the set is `rename`, `default`, `drop`"
        ))),
    }
}

fn migrations(
    declared: Option<Vec<dto::GraphTypeMigrationDto>>,
) -> Result<Vec<m::MigrationSpec>, DomainError> {
    let mut out: Vec<m::MigrationSpec> = Vec::new();
    for migration in declared.unwrap_or_default() {
        if out.iter().any(|m| m.type_id == migration.type_id) {
            return Err(DomainError::invalid(format!(
                "`{}` carries more than one migration; declare one plan per type",
                migration.type_id
            )));
        }
        let mut steps = Vec::with_capacity(migration.steps.len());
        for step in &migration.steps {
            steps.push(migration_step(step)?);
        }
        out.push(m::MigrationSpec {
            type_id: migration.type_id,
            steps,
        });
    }
    Ok(out)
}

/// `options.on_existing` / `options.revalidate`, refusing an unknown mode
/// rather than defaulting it: a caller who misspells `update` must not be
/// silently served the rejecting behaviour they were trying to leave.
fn registration_options(
    options: Option<dto::GraphTypeRegisterOptionsDto>,
) -> Result<m::TypeRegistrationOptions, DomainError> {
    let Some(options) = options else {
        return Ok(m::TypeRegistrationOptions::default());
    };
    let on_existing = match options.on_existing.as_deref() {
        None | Some("reject") => m::OnExisting::Reject,
        Some("update") => m::OnExisting::Update,
        Some(other) => {
            return Err(DomainError::invalid(format!(
                "`{other}` is not an accepted value for `options.on_existing`; it takes                  `reject` or `update`"
            )));
        }
    };
    Ok(m::TypeRegistrationOptions {
        on_existing,
        revalidate: options.revalidate.unwrap_or(false),
        dry_run: false,
        migrations: Vec::new(),
    })
}

#[tracing::instrument(skip_all, fields(user.id = %ctx.subject_id()))]
pub async fn register_types(
    Extension(ctx): Extension<SecurityContext>,
    Extension(services): Extension<Arc<GraphServices>>,
    Json(request): Json<dto::GraphRegisterTypesRequest>,
) -> ApiResult<Json<Vec<dto::GraphRegisteredTypeDto>>> {
    let mut options = registration_options(request.options)?;
    options.migrations = migrations(request.migrations)?;
    let batch: Vec<m::TypeRegistration> = request.types.into_iter().map(Into::into).collect();
    let registered = services.register_types_with(&ctx, batch, options).await?;
    Ok(Json(registered.into_iter().map(Into::into).collect()))
}

/// The dry run: every verdict, no write.
#[tracing::instrument(skip_all, fields(user.id = %ctx.subject_id()))]
pub async fn type_compatibility(
    Extension(ctx): Extension<SecurityContext>,
    Extension(services): Extension<Arc<GraphServices>>,
    Json(request): Json<dto::GraphRegisterTypesRequest>,
) -> ApiResult<Json<dto::GraphTypeCompatibilityDto>> {
    // Re-validation defaults *on* here and off on the write path: the whole
    // point of asking is to be told whether the change would go through, and
    // a dry run that skipped the row check would answer a different question
    // from the one the update will ask.
    let revalidate = request
        .options
        .as_ref()
        .and_then(|options| options.revalidate)
        .unwrap_or(true);
    // A dry run takes migrations as well, and that is the point: "what would
    // this plan do to my rows" is the question worth asking before it runs.
    let plans = migrations(request.migrations)?;
    let batch: Vec<m::TypeRegistration> = request.types.into_iter().map(Into::into).collect();
    let items = services
        .type_compatibility(&ctx, batch, revalidate, plans)
        .await?;
    Ok(Json(dto::GraphTypeCompatibilityDto {
        items: items.into_iter().map(Into::into).collect(),
    }))
}

/// Readiness. No `SecurityContext`: the matrix keeps this endpoint answering
/// when the authorization resolver is the thing that is down, and it reports
/// capabilities rather than content.
///
/// Always `200`. Readiness is a state to read, not a request that failed —
/// the body says what is wrong, and a caller that wants a status code reads
/// `ready`.
#[tracing::instrument(skip_all)]
pub async fn readiness(
    Extension(services): Extension<Arc<GraphServices>>,
) -> ApiResult<Json<dto::GraphReadinessDto>> {
    Ok(Json(services.readiness().await.into()))
}

#[tracing::instrument(skip_all, fields(user.id = %ctx.subject_id()))]
pub async fn list_source_namespaces(
    Extension(ctx): Extension<SecurityContext>,
    Extension(services): Extension<Arc<GraphServices>>,
) -> ApiResult<Json<dto::GraphSourceNamespaceListDto>> {
    let items = services.list_source_namespaces(&ctx).await?;
    Ok(Json(dto::GraphSourceNamespaceListDto {
        items: items.into_iter().map(Into::into).collect(),
    }))
}

#[tracing::instrument(skip_all, fields(user.id = %ctx.subject_id()))]
pub async fn transfer_source_namespace(
    Extension(ctx): Extension<SecurityContext>,
    Extension(services): Extension<Arc<GraphServices>>,
    Path(namespace): Path<String>,
    Json(request): Json<dto::GraphTransferNamespaceRequest>,
) -> ApiResult<Json<dto::GraphSourceNamespaceDto>> {
    let row = services
        .transfer_source_namespace(&ctx, &namespace, &request.owner_principal)
        .await?;
    Ok(Json(row.into()))
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
