//! The batch debit route.
//!
//! As on the other consumption routes, a denial is not an error: a batch any
//! item of which the policy refuses answers HTTP 200 with `result: denied`
//! and every item's decision. Item amounts travel as signed integers, so a
//! zero or negative one reaches the domain as an actionable `INVALID_AMOUNT`
//! naming the item rather than failing deserialization with a 422. `mode` is
//! required.

use std::sync::Arc;

use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::{Extension, Json, Router};
use quota_enforcement_sdk::{
    BatchDebitRequest, BatchDecision, BatchItemOutcome, BatchItemRequest, BatchMode, BatchResult,
};
use toolkit::api::OpenApiRegistry;
use toolkit::api::operation_builder::OperationBuilder;
use toolkit::api::rest::extract::Json as JsonBody;
use toolkit_security::SecurityContext;

use super::error::ApiResult;
use super::operations::{DecisionDto, EvaluationAttributionDto};
use crate::domain::Service;

// The request bodies do not reject unknown fields, for the reason the
// consumption bodies do not: a caller echoing server-derived fields back is
// ignored rather than refused.

/// How a batch's items relate to each other.
#[derive(Debug, Clone, Copy)]
#[toolkit_macros::api_dto(request)]
pub enum BatchModeDto {
    /// All or nothing.
    Atomic,
    /// Partial success; answers 501 until it ships.
    Independent,
}

impl From<BatchModeDto> for BatchMode {
    fn from(value: BatchModeDto) -> Self {
        match value {
            BatchModeDto::Atomic => Self::Atomic,
            BatchModeDto::Independent => Self::Independent,
        }
    }
}

/// One item: a debit of its own metric.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request)]
pub struct BatchItemRequestDto {
    /// Who and what the item charges; every item names the same tenant.
    pub attribution: EvaluationAttributionDto,
    /// Requested amount.
    pub amount: i64,
    /// The item's own key, unique within the batch; identification only.
    pub idempotency_key: String,
}

/// Debit several metrics as one logical operation.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request)]
pub struct BatchDebitRequestDto {
    /// How the items relate; only `atomic` is implemented.
    pub mode: BatchModeDto,
    /// The items, in the order they are evaluated.
    pub items: Vec<BatchItemRequestDto>,
    /// The envelope's idempotency key.
    pub idempotency_key: String,
}

impl From<BatchDebitRequestDto> for BatchDebitRequest {
    fn from(value: BatchDebitRequestDto) -> Self {
        Self {
            mode: value.mode.into(),
            items: value
                .items
                .into_iter()
                .map(|item| BatchItemRequest {
                    attribution: item.attribution.into(),
                    amount: item.amount,
                    idempotency_key: item.idempotency_key,
                })
                .collect(),
            idempotency_key: value.idempotency_key,
        }
    }
}

/// The batch-level verdict.
#[derive(Debug, Clone, Copy)]
#[toolkit_macros::api_dto(response)]
pub enum BatchResultDto {
    /// Every item was allowed and applied.
    Allowed,
    /// At least one item was denied; nothing moved.
    Denied,
}

/// One item's decision.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct BatchItemOutcomeDto {
    /// The item's own key.
    pub idempotency_key: String,
    /// The item's decision; diagnostic only on a denied batch.
    pub decision: DecisionDto,
}

/// A batch outcome as the API renders it.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct BatchDecisionDto {
    /// The batch-level verdict.
    pub result: BatchResultDto,
    /// One outcome per item, in submission order.
    pub items: Vec<BatchItemOutcomeDto>,
}

impl From<BatchDecision> for BatchDecisionDto {
    fn from(value: BatchDecision) -> Self {
        Self {
            result: match value.result {
                BatchResult::Allowed => BatchResultDto::Allowed,
                BatchResult::Denied => BatchResultDto::Denied,
            },
            items: value
                .items
                .into_iter()
                .map(
                    |BatchItemOutcome {
                         idempotency_key,
                         decision,
                     }| BatchItemOutcomeDto {
                        idempotency_key,
                        decision: decision.into(),
                    },
                )
                .collect(),
        }
    }
}

async fn batch_debit(
    Extension(ctx): Extension<SecurityContext>,
    Extension(service): Extension<Arc<Service>>,
    JsonBody(body): JsonBody<BatchDebitRequestDto>,
) -> ApiResult<impl IntoResponse> {
    let decision = service.operations()?.batch_debit(&ctx, body.into()).await?;
    Ok(Json(BatchDecisionDto::from(decision)))
}

/// Mount the batch debit endpoint.
// @cpt-dod:cpt-cf-quota-enforcement-dod-batch-debit-endpoint:p1
pub fn register(router: Router, openapi: &dyn OpenApiRegistry) -> Router {
    let prefix = super::routes::PATH_PREFIX;
    OperationBuilder::post(format!("{prefix}/operations/batch-debit"))
        .operation_id("quota_enforcement.batch_debit")
        .summary("Debit several metrics as one logical operation")
        .description(
            "Evaluates every item in order against the counters as the earlier allowed items \
             left them, and applies the union of their plans only when every item is allowed. \
             A denial is HTTP 200 with `result: denied` and every item's decision; nothing \
             moves. Every item names the same tenant (400 BATCH_TENANT_MIXED); an empty batch \
             answers 400 BATCH_EMPTY, a batch over the configured size 400 BULK_TOO_LARGE, \
             `mode: independent` 501, and a batch outlasting its timeout 504 BATCH_TIMEOUT. \
             Replaying the envelope key returns the stored outcome.",
        )
        .tag("Operations")
        .authenticated()
        .no_license_required()
        .json_request::<BatchDebitRequestDto>(openapi, "The batch to charge")
        .handler(batch_debit)
        .json_response_with_schema::<BatchDecisionDto>(
            openapi,
            StatusCode::OK,
            "The batch decision",
        )
        .standard_errors(openapi)
        .register(router, openapi)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "batch_tests.rs"]
mod tests;
