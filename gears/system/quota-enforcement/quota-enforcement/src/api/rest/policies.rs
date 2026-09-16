//! Platform policy routes over the shared operator service.
use super::error::ApiResult;
use crate::domain::{DomainError, Service};
use axum::http::{StatusCode, Uri};
use axum::response::IntoResponse;
use axum::{Extension, Router};
use quota_enforcement_sdk::{
    MetricId, PageRequest, PolicyId, PolicyPatch, PolicyScope, PolicySpec, PolicyVersion,
    PolicyVersionMeta,
};
use serde::Deserialize;
use serde_json::Value;
use std::sync::Arc;
use toolkit::api::operation_builder::OperationBuilder;
use toolkit::api::rest::extract::{Json as JsonBody, Path};
use toolkit::api::{OpenApiRegistry, response::created_json};
use toolkit_contract::query::QueryParamsExtractor;
use toolkit_security::SecurityContext;

/// Exact policy scope. Global remains a string-compatible path ID.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request, response)]
#[serde(tag = "kind", deny_unknown_fields)]
pub enum PolicyScopeDto {
    /// Platform fallback.
    Global,
    /// Metric override.
    Metric {
        /// Registered metric instance identifier.
        metric: String,
    },
}
impl TryFrom<PolicyScopeDto> for PolicyScope {
    type Error = DomainError;
    fn try_from(value: PolicyScopeDto) -> Result<Self, Self::Error> {
        match value {
            PolicyScopeDto::Global => Ok(Self::Global),
            PolicyScopeDto::Metric { metric } => Ok(Self::Metric {
                metric: MetricId::parse(&metric).map_err(|_| DomainError::InvalidPolicy {
                    field: "scope.metric",
                    reason: "METRIC_INVALID",
                    detail: "invalid metric identifier".into(),
                })?,
            }),
        }
    }
}
impl From<PolicyScope> for PolicyScopeDto {
    fn from(value: PolicyScope) -> Self {
        match value {
            PolicyScope::Global => Self::Global,
            PolicyScope::Metric { metric } => Self::Metric {
                metric: metric.to_string(),
            },
        }
    }
}

/// Public create input. Trusted schemas and creator identity cannot be submitted.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request)]
#[serde(deny_unknown_fields)]
pub struct CreatePolicyDto {
    /// Exact scope.
    pub scope: PolicyScopeDto,
    /// Registered engine ID.
    pub engine_id: String,
    /// Engine configuration.
    pub engine_config: Value,
    /// Requested evaluation timeout, not a persisted effective clamp.
    pub timeout_ms: Option<u64>,
    /// Operator description.
    pub description: Option<String>,
    /// Version comment.
    pub comment: Option<String>,
}
impl TryFrom<CreatePolicyDto> for PolicySpec {
    type Error = DomainError;
    fn try_from(value: CreatePolicyDto) -> Result<Self, Self::Error> {
        Ok(Self {
            scope: value.scope.try_into()?,
            engine_id: value.engine_id,
            engine_config: value.engine_config,
            timeout_ms: value.timeout_ms,
            description: value.description,
            comment: value.comment,
        })
    }
}

/// Public conditional update input.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request)]
#[serde(deny_unknown_fields)]
pub struct UpdatePolicyDto {
    /// Expected active version.
    pub if_match_version: u32,
    /// Replacement engine.
    pub engine_id: Option<String>,
    /// Replacement complete configuration.
    pub engine_config: Option<Value>,
    /// Requested timeout.
    pub timeout_ms: Option<u64>,
    /// Version comment.
    pub comment: Option<String>,
}
impl From<UpdatePolicyDto> for PolicyPatch {
    fn from(value: UpdatePolicyDto) -> Self {
        Self {
            if_match_version: value.if_match_version,
            engine_id: value.engine_id,
            engine_config: value.engine_config,
            timeout_ms: value.timeout_ms,
            comment: value.comment,
        }
    }
}

/// Rollback command.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request)]
#[serde(deny_unknown_fields)]
pub struct RollbackPolicyDto {
    /// Retained version to activate.
    pub target_version: u32,
    /// Transition audit comment.
    pub comment: Option<String>,
}

/// Optional delete audit comment.
#[derive(Debug, Clone, Default, Deserialize, toolkit_contract::QueryParams)]
#[serde(deny_unknown_fields)]
pub struct DeleteQuery {
    /// Transition comment.
    pub comment: Option<String>,
}
/// Optional retained version selector.
#[derive(Debug, Clone, Default, Deserialize, toolkit_contract::QueryParams)]
#[serde(deny_unknown_fields)]
pub struct ReadQuery {
    /// Omit for active version.
    pub version: Option<u32>,
}
/// History pagination.
#[derive(Debug, Clone, Default, Deserialize, toolkit_contract::QueryParams)]
#[serde(deny_unknown_fields)]
pub struct HistoryQuery {
    /// Bounded page size.
    pub limit: Option<u32>,
    /// Opaque storage-issued cursor.
    pub cursor: Option<String>,
}

/// Public version view; persisted validation snapshots remain server-owned.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct PolicyDto {
    /// Stable opaque ID.
    pub policy_id: String,
    /// Immutable version number.
    pub version: u32,
    /// Exact scope.
    pub scope: PolicyScopeDto,
    /// Registered engine.
    pub engine_id: String,
    /// Validated source configuration.
    pub engine_config: Value,
    /// Requested evaluation timeout.
    pub timeout_ms: Option<u64>,
    /// Operator description.
    pub description: Option<String>,
    /// Lifecycle state.
    #[schema(value_type = String)]
    pub state: quota_enforcement_sdk::PolicyVersionState,
    /// Creation time.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: time::OffsetDateTime,
    /// Authenticated creator.
    pub created_by: String,
    /// Immutable version comment.
    pub comment: Option<String>,
}
impl From<PolicyVersion> for PolicyDto {
    fn from(v: PolicyVersion) -> Self {
        Self {
            policy_id: v.policy_id.to_string(),
            version: v.version,
            scope: v.scope.into(),
            engine_id: v.engine_id,
            engine_config: v.engine_config,
            timeout_ms: v.timeout_ms,
            description: v.description,
            state: v.state,
            created_at: v.created_at,
            created_by: v.created_by,
            comment: v.comment,
        }
    }
}
/// One retained version's history metadata.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct VersionMetaDto {
    /// Version number.
    pub version: u32,
    /// Lifecycle state.
    #[schema(value_type = String)]
    pub state: quota_enforcement_sdk::PolicyVersionState,
    /// Immutable creator.
    pub created_by: String,
    /// Immutable creation time.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: time::OffsetDateTime,
    /// Version comment.
    pub comment: Option<String>,
}
impl From<PolicyVersionMeta> for VersionMetaDto {
    fn from(v: PolicyVersionMeta) -> Self {
        Self {
            version: v.version,
            state: v.state,
            created_by: v.created_by,
            created_at: v.created_at,
            comment: v.comment,
        }
    }
}
/// A page of retained versions.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct PolicyHistoryDto {
    /// Ordered versions.
    pub items: Vec<VersionMetaDto>,
    /// Next page cursor.
    pub next_cursor: Option<String>,
}

// @cpt-begin:cpt-cf-quota-enforcement-flow-policy-write:p1:inst-pw-request
async fn create(
    uri: Uri,
    Extension(ctx): Extension<SecurityContext>,
    Extension(service): Extension<Arc<Service>>,
    JsonBody(body): JsonBody<CreatePolicyDto>,
) -> ApiResult<impl IntoResponse> {
    let version = service.policies()?.create(&ctx, body.try_into()?).await?;
    let id = version.policy_id.to_string();
    Ok(created_json(PolicyDto::from(version), &uri, &id).into_response())
}
// @cpt-end:cpt-cf-quota-enforcement-flow-policy-write:p1:inst-pw-request
// @cpt-begin:cpt-cf-quota-enforcement-flow-policy-write:p1:inst-pw-request
async fn update(
    Extension(ctx): Extension<SecurityContext>,
    Extension(service): Extension<Arc<Service>>,
    Path(id): Path<String>,
    JsonBody(body): JsonBody<UpdatePolicyDto>,
) -> ApiResult<axum::Json<PolicyDto>> {
    Ok(axum::Json(
        service
            .policies()?
            .update(&ctx, PolicyId::new(id), body.into())
            .await?
            .into(),
    ))
}
// @cpt-end:cpt-cf-quota-enforcement-flow-policy-write:p1:inst-pw-request
async fn read(
    Extension(ctx): Extension<SecurityContext>,
    Extension(service): Extension<Arc<Service>>,
    Path(id): Path<String>,
    QueryParamsExtractor(query): QueryParamsExtractor<ReadQuery>,
) -> ApiResult<axum::Json<PolicyDto>> {
    Ok(axum::Json(
        service
            .policies()?
            .read(&ctx, &PolicyId::new(id), query.version)
            .await?
            .into(),
    ))
}
// @cpt-begin:cpt-cf-quota-enforcement-flow-policy-rollback:p1:inst-prd-rollback-request
async fn rollback(
    Extension(ctx): Extension<SecurityContext>,
    Extension(service): Extension<Arc<Service>>,
    Path(id): Path<String>,
    JsonBody(body): JsonBody<RollbackPolicyDto>,
) -> ApiResult<axum::Json<PolicyDto>> {
    Ok(axum::Json(
        service
            .policies()?
            .rollback(&ctx, PolicyId::new(id), body.target_version, body.comment)
            .await?
            .into(),
    ))
}
// @cpt-end:cpt-cf-quota-enforcement-flow-policy-rollback:p1:inst-prd-rollback-request
// @cpt-begin:cpt-cf-quota-enforcement-flow-policy-delete:p1:inst-prd-delete-request
async fn delete(
    Extension(ctx): Extension<SecurityContext>,
    Extension(service): Extension<Arc<Service>>,
    Path(id): Path<String>,
    QueryParamsExtractor(query): QueryParamsExtractor<DeleteQuery>,
) -> ApiResult<StatusCode> {
    service
        .policies()?
        .delete(&ctx, PolicyId::new(id), query.comment)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
// @cpt-end:cpt-cf-quota-enforcement-flow-policy-delete:p1:inst-prd-delete-request
async fn history(
    Extension(ctx): Extension<SecurityContext>,
    Extension(service): Extension<Arc<Service>>,
    Path(id): Path<String>,
    QueryParamsExtractor(query): QueryParamsExtractor<HistoryQuery>,
) -> ApiResult<axum::Json<PolicyHistoryDto>> {
    let page = service
        .policies()?
        .list(
            &ctx,
            &PolicyId::new(id),
            PageRequest {
                limit: query.limit.unwrap_or(PageRequest::DEFAULT_LIMIT),
                cursor: query.cursor,
            },
        )
        .await?;
    Ok(axum::Json(PolicyHistoryDto {
        items: page.items.into_iter().map(Into::into).collect(),
        next_cursor: page.next_cursor,
    }))
}

/// Mount all policy operations before attaching the shared service extension.
pub fn register(mut router: Router, openapi: &dyn OpenApiRegistry) -> Router {
    let prefix = super::routes::PATH_PREFIX;
    router = OperationBuilder::post(format!("{prefix}/policies"))
        .operation_id("quota_enforcement.create_policy")
        .summary("Create a resolution policy")
        .tag("Policies")
        .authenticated()
        .no_license_required()
        .json_request::<CreatePolicyDto>(openapi, "Policy specification")
        .handler(create)
        .json_response_with_schema::<PolicyDto>(openapi, StatusCode::CREATED, "Created policy")
        .standard_errors(openapi)
        .register(router, openapi);
    router = OperationBuilder::patch(format!("{prefix}/policies/{{id}}"))
        .operation_id("quota_enforcement.update_policy")
        .summary("Create a policy version")
        .tag("Policies")
        .authenticated()
        .no_license_required()
        .path_param("id", "Opaque policy ID, including global")
        .json_request::<UpdatePolicyDto>(openapi, "Conditional update")
        .handler(update)
        .json_response_with_schema::<PolicyDto>(openapi, StatusCode::OK, "New active version")
        .standard_errors(openapi)
        .register(router, openapi);
    router = OperationBuilder::get(format!("{prefix}/policies/{{id}}"))
        .operation_id("quota_enforcement.get_policy")
        .summary("Read a policy version")
        .tag("Policies")
        .authenticated()
        .no_license_required()
        .path_param("id", "Opaque policy ID, including global")
        .query_params_from::<ReadQuery>()
        .handler(read)
        .json_response_with_schema::<PolicyDto>(openapi, StatusCode::OK, "Policy version")
        .standard_errors(openapi)
        .register(router, openapi);
    router = OperationBuilder::get(format!("{prefix}/policies/{{id}}/versions"))
        .operation_id("quota_enforcement.list_policy_versions")
        .summary("Read policy history")
        .tag("Policies")
        .authenticated()
        .no_license_required()
        .path_param("id", "Opaque policy ID, including global")
        .query_params_from::<HistoryQuery>()
        .handler(history)
        .json_response_with_schema::<PolicyHistoryDto>(openapi, StatusCode::OK, "Version history")
        .standard_errors(openapi)
        .register(router, openapi);
    router = OperationBuilder::post(format!("{prefix}/policies/{{id}}/rollback"))
        .operation_id("quota_enforcement.rollback_policy")
        .summary("Reactivate a retained policy version")
        .tag("Policies")
        .authenticated()
        .no_license_required()
        .path_param("id", "Opaque policy ID, including global")
        .json_request::<RollbackPolicyDto>(openapi, "Rollback command")
        .handler(rollback)
        .json_response_with_schema::<PolicyDto>(openapi, StatusCode::OK, "Active policy version")
        .standard_errors(openapi)
        .register(router, openapi);
    OperationBuilder::delete(format!("{prefix}/policies/{{id}}"))
        .operation_id("quota_enforcement.delete_policy")
        .summary("Soft-delete a metric policy")
        .tag("Policies")
        .authenticated()
        .no_license_required()
        .path_param("id", "Opaque policy ID")
        .query_params_from::<DeleteQuery>()
        .handler(delete)
        .no_content_response(StatusCode::NO_CONTENT, "Deleted or already deleted")
        .standard_errors(openapi)
        .register(router, openapi)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "policies_tests.rs"]
mod policies_tests;
