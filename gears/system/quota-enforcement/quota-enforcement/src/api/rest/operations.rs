//! The consumption routes: debit, credit, rollback, and preview.
//!
//! A denial is not an error. All four endpoints answer HTTP 200 with a decision
//! body whenever the gear reached a verdict, and only a failure to decide
//! becomes a `Problem` (PRD section 3.4). A caller that echoes a decision back
//! in its next request is ignored rather than refused, which is why these
//! request bodies do not reject unknown fields.
//!
//! `amount` travels as a signed integer so that zero and negative values reach
//! the domain as an actionable `INVALID_AMOUNT` rather than failing
//! deserialization with an unactionable 422.

use std::sync::Arc;

use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::{Extension, Json, Router};
use quota_enforcement_sdk::{
    CreditRequest, DebitRequest, Decision, DecisionPreview, DecisionResult, EvaluationAttribution,
    PreviewRequest, QuotaId, ResourceProjection, RollbackRequest, RollbackableOperation,
    SubjectClaim, TenantId,
};
use serde_json::{Map, Value};
use toolkit::api::OpenApiRegistry;
use toolkit::api::operation_builder::OperationBuilder;
use toolkit::api::rest::extract::Json as JsonBody;
use toolkit_security::SecurityContext;

use super::error::ApiResult;
use crate::domain::Service;

/// One caller-supplied subject: a scope kind and an opaque identifier.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request)]
#[serde(deny_unknown_fields)]
pub struct SubjectClaimDto {
    /// Scope instance id of the subject.
    pub kind: String,
    /// Opaque subject identifier.
    pub id: String,
}

/// The optional resource projection of a request.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request)]
#[serde(deny_unknown_fields)]
pub struct ResourceProjectionDto {
    /// Registered resource projection type.
    pub r#type: String,
    /// Opaque resource identifier.
    pub id: Option<String>,
    /// Resource metadata, validated against the owner's contract.
    pub metadata: Option<Map<String, Value>>,
}

/// The attribution tuple every subject-based operation carries.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request)]
#[serde(deny_unknown_fields)]
pub struct EvaluationAttributionDto {
    /// Authorized target tenant.
    pub tenant_id: uuid::Uuid,
    /// Registered metric instance id.
    pub metric: String,
    /// Additional subjects beyond the tenant.
    pub subjects: Vec<SubjectClaimDto>,
    /// Operation-level metadata, required even when empty.
    pub metadata: Option<Map<String, Value>>,
    /// Optional resource projection.
    pub resource: Option<ResourceProjectionDto>,
}

impl From<EvaluationAttributionDto> for EvaluationAttribution {
    fn from(value: EvaluationAttributionDto) -> Self {
        Self {
            tenant_id: TenantId::new(value.tenant_id),
            metric: value.metric,
            subjects: value
                .subjects
                .into_iter()
                .map(|claim| SubjectClaim {
                    kind: claim.kind,
                    id: claim.id,
                })
                .collect(),
            metadata: value.metadata,
            resource: value.resource.map(|resource| ResourceProjection {
                r#type: resource.r#type,
                id: resource.id,
                metadata: resource.metadata,
            }),
        }
    }
}

// The four request bodies deliberately do not carry `deny_unknown_fields`:
// server-derived fields a caller echoed back are ignored, and serde has no
// ignore-only-these mode. The nested attribution keeps its own strictness.

/// Charge a metric against every applicable Quota.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request)]
pub struct DebitRequestDto {
    /// Who and what is being charged.
    pub attribution: EvaluationAttributionDto,
    /// Requested amount.
    pub amount: i64,
    /// Client-supplied idempotency key.
    pub idempotency_key: String,
}

/// Return consumption to one Quota.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request)]
pub struct CreditRequestDto {
    /// Authorized target tenant.
    pub tenant_id: uuid::Uuid,
    /// The Quota to credit.
    pub quota_id: uuid::Uuid,
    /// Amount to return.
    pub amount: i64,
    /// Client-supplied idempotency key.
    pub idempotency_key: String,
}

/// Which kind of operation a rollback reverses.
///
/// A direct debit and a lease commit are separate idempotency namespaces, so
/// one caller can hold both under the same key; this says which is meant.
#[derive(Debug, Clone, Copy, Default)]
#[toolkit_macros::api_dto(request)]
pub enum RollbackableOperationDto {
    /// A direct debit. The default, so a request that predates leases keeps
    /// its meaning.
    #[default]
    Debit,
    /// A lease commit.
    LeaseCommit,
}

impl From<RollbackableOperationDto> for RollbackableOperation {
    fn from(value: RollbackableOperationDto) -> Self {
        match value {
            RollbackableOperationDto::Debit => Self::Debit,
            RollbackableOperationDto::LeaseCommit => Self::LeaseCommit,
        }
    }
}

/// Reverse a committed debit.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request)]
pub struct RollbackRequestDto {
    /// Attribution of the debit being reversed.
    pub attribution: EvaluationAttributionDto,
    /// Which kind of operation the original key names: a direct `debit`, or a
    /// `lease_commit`. They are separate idempotency namespaces, so the same
    /// key may address one of each. Defaults to `debit`.
    #[serde(default)]
    pub original_operation: RollbackableOperationDto,
    /// Key the original debit was committed under.
    pub original_idempotency_key: String,
    /// Client-supplied key of this rollback.
    pub idempotency_key: String,
}

/// Evaluate without mutating anything.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request)]
pub struct PreviewRequestDto {
    /// Who and what would be charged.
    pub attribution: EvaluationAttributionDto,
    /// Amount to test.
    pub amount: i64,
}

/// The verdict of an evaluation.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request, response)]
#[serde(tag = "outcome")]
pub enum DecisionResultDto {
    /// Within every applicable cap.
    Allowed,
    /// At least one Quota would be exceeded.
    Denied {
        /// Every violating Quota.
        violated_quota_ids: Vec<uuid::Uuid>,
        /// Closed reason token.
        reason: String,
    },
}

/// One Quota's share of a decision's plan.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct QuotaDebitPlanDto {
    /// The Quota.
    pub quota_id: uuid::Uuid,
    /// Amount the plan charges it.
    pub amount: u64,
}

/// A decision as the API renders it.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct DecisionDto {
    /// The verdict.
    pub result: DecisionResultDto,
    /// What the plan charged. Empty on a denial.
    pub debit_plan: Vec<QuotaDebitPlanDto>,
    /// Engine-supplied detail.
    pub diagnostics: Map<String, Value>,
}

impl From<Decision> for DecisionDto {
    fn from(value: Decision) -> Self {
        Self {
            result: match value.result {
                DecisionResult::Allowed => DecisionResultDto::Allowed,
                DecisionResult::Denied {
                    violated_quota_ids,
                    reason,
                } => DecisionResultDto::Denied {
                    violated_quota_ids: violated_quota_ids
                        .into_iter()
                        .map(QuotaId::as_uuid)
                        .collect(),
                    reason,
                },
            },
            debit_plan: value
                .debit_plan
                .into_iter()
                .map(|(quota_id, plan)| QuotaDebitPlanDto {
                    quota_id: quota_id.as_uuid(),
                    amount: plan.amount,
                })
                .collect(),
            diagnostics: value.diagnostics.into_iter().collect(),
        }
    }
}

/// A decision that was never applied.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct DecisionPreviewDto {
    /// The verdict.
    pub result: DecisionResultDto,
    /// What the plan would have charged.
    pub debit_plan: Vec<QuotaDebitPlanDto>,
    /// Engine-supplied detail.
    pub diagnostics: Map<String, Value>,
    /// Always `true`; a dry run can never be mistaken for a commit.
    pub preview: bool,
}

impl From<DecisionPreview> for DecisionPreviewDto {
    fn from(value: DecisionPreview) -> Self {
        let decision = DecisionDto::from(value.decision);
        Self {
            result: decision.result,
            debit_plan: decision.debit_plan,
            diagnostics: decision.diagnostics,
            preview: value.preview,
        }
    }
}

async fn debit(
    Extension(ctx): Extension<SecurityContext>,
    Extension(service): Extension<Arc<Service>>,
    JsonBody(body): JsonBody<DebitRequestDto>,
) -> ApiResult<impl IntoResponse> {
    let decision = service
        .operations()?
        .debit(
            &ctx,
            DebitRequest {
                attribution: body.attribution.into(),
                amount: body.amount,
                idempotency_key: body.idempotency_key,
            },
        )
        .await?;
    Ok(Json(DecisionDto::from(decision)))
}

async fn credit(
    Extension(ctx): Extension<SecurityContext>,
    Extension(service): Extension<Arc<Service>>,
    JsonBody(body): JsonBody<CreditRequestDto>,
) -> ApiResult<impl IntoResponse> {
    let decision = service
        .operations()?
        .credit(
            &ctx,
            CreditRequest {
                tenant_id: TenantId::new(body.tenant_id),
                quota_id: QuotaId::new(body.quota_id),
                amount: body.amount,
                idempotency_key: body.idempotency_key,
            },
        )
        .await?;
    Ok(Json(DecisionDto::from(decision)))
}

async fn rollback(
    Extension(ctx): Extension<SecurityContext>,
    Extension(service): Extension<Arc<Service>>,
    JsonBody(body): JsonBody<RollbackRequestDto>,
) -> ApiResult<impl IntoResponse> {
    let decision = service
        .operations()?
        .rollback(
            &ctx,
            RollbackRequest {
                attribution: body.attribution.into(),
                original_operation: body.original_operation.into(),
                original_idempotency_key: body.original_idempotency_key,
                idempotency_key: body.idempotency_key,
            },
        )
        .await?;
    Ok(Json(DecisionDto::from(decision)))
}

async fn preview(
    Extension(ctx): Extension<SecurityContext>,
    Extension(service): Extension<Arc<Service>>,
    JsonBody(body): JsonBody<PreviewRequestDto>,
) -> ApiResult<impl IntoResponse> {
    let decision = service
        .operations()?
        .preview(
            &ctx,
            PreviewRequest {
                attribution: body.attribution.into(),
                amount: body.amount,
            },
        )
        .await?;
    Ok(Json(DecisionPreviewDto::from(decision)))
}

/// Mount the four consumption endpoints.
pub fn register(mut router: Router, openapi: &dyn OpenApiRegistry) -> Router {
    const TAG: &str = "Operations";
    let prefix = super::routes::PATH_PREFIX;
    router = OperationBuilder::post(format!("{prefix}/operations/debit"))
        .operation_id("quota_enforcement.debit")
        .summary("Charge a metric against every applicable Quota")
        .description(
            "Evaluates the applicable policy and commits its plan atomically. A denial is \
             returned as HTTP 200 with a decision body. Replaying the idempotency key returns \
             the original decision without re-evaluating anything.",
        )
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .json_request::<DebitRequestDto>(openapi, "The operation to charge")
        .handler(debit)
        .json_response_with_schema::<DecisionDto>(openapi, StatusCode::OK, "The decision")
        .standard_errors(openapi)
        .register(router, openapi);
    router = OperationBuilder::post(format!("{prefix}/operations/credit"))
        .operation_id("quota_enforcement.credit")
        .summary("Return consumption to one Quota")
        .description("Corrective and operator-facing. Evaluates no policy and floors at zero.")
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .json_request::<CreditRequestDto>(openapi, "The amount to return")
        .handler(credit)
        .json_response_with_schema::<DecisionDto>(openapi, StatusCode::OK, "The decision")
        .standard_errors(openapi)
        .register(router, openapi);
    router = OperationBuilder::post(format!("{prefix}/operations/rollback"))
        .operation_id("quota_enforcement.rollback")
        .summary("Reverse a committed debit")
        .description(
            "Restores the counters to what they would have been had the debit never happened, \
             against its own attribution period. The request carries the reversed debit's \
             attribution, which the server re-authorizes.",
        )
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .json_request::<RollbackRequestDto>(openapi, "The operation to reverse")
        .handler(rollback)
        .json_response_with_schema::<DecisionDto>(openapi, StatusCode::OK, "The decision")
        .standard_errors(openapi)
        .register(router, openapi);
    OperationBuilder::post(format!("{prefix}/operations/evaluate"))
        .operation_id("quota_enforcement.evaluate_preview")
        .summary("Evaluate without mutating anything")
        .description("A dry run. Persists nothing and occupies no idempotency key.")
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .json_request::<PreviewRequestDto>(openapi, "The operation to test")
        .handler(preview)
        .json_response_with_schema::<DecisionPreviewDto>(
            openapi,
            StatusCode::OK,
            "The decision, marked as a preview",
        )
        .standard_errors(openapi)
        .register(router, openapi)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "operations_tests.rs"]
mod tests;
