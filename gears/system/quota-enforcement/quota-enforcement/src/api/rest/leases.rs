//! The lease routes: acquire, commit, and release.
//!
//! As on the consumption routes, a denial is not an error: an acquisition the
//! policy refuses answers HTTP 200 with `outcome: "denied"` and its decision.
//! `amount` and `actual_amount` travel as signed integers and `ttl_secs` as an
//! optional one, so a zero, negative, or missing value reaches the domain as
//! an actionable 400 rather than failing deserialization with a 422.

use std::sync::Arc;

use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::{Extension, Json, Router};
use quota_enforcement_sdk::{
    AcquireLeaseOutcome, AcquireLeaseRequest, CommitLeaseRequest, LeaseToken, ReleaseLeaseRequest,
    TenantId,
};
use time::OffsetDateTime;
use toolkit::api::OpenApiRegistry;
use toolkit::api::operation_builder::OperationBuilder;
use toolkit::api::rest::extract::{Json as JsonBody, Path};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::error::ApiResult;
use super::operations::{DecisionDto, EvaluationAttributionDto};
use crate::domain::Service;

// The request bodies do not reject unknown fields, for the reason the
// consumption bodies do not: a caller echoing server-derived fields back is
// ignored rather than refused.

/// Hold capacity against every applicable Quota for a bounded time.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request)]
pub struct AcquireLeaseRequestDto {
    /// Who and what the hold is for.
    pub attribution: EvaluationAttributionDto,
    /// Amount to hold; the worst case the caller may commit.
    pub amount: i64,
    /// How long the hold lasts, in seconds, within the configured window.
    pub ttl_secs: Option<u64>,
    /// Client-supplied idempotency key.
    pub idempotency_key: String,
}

/// Convert an active lease into a debit of what was actually used.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request)]
pub struct CommitLeaseRequestDto {
    /// Tenant that holds the lease.
    pub tenant_id: Uuid,
    /// Amount actually used, at most the reserved amount. Absent keeps the
    /// whole hold; zero keeps nothing and returns every hold.
    pub actual_amount: Option<i64>,
    /// Client-supplied idempotency key of this commit.
    pub idempotency_key: String,
}

/// Return everything an active lease holds.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request)]
pub struct ReleaseLeaseRequestDto {
    /// Tenant that holds the lease.
    pub tenant_id: Uuid,
    /// Client-supplied idempotency key of this release.
    pub idempotency_key: String,
}

/// The answer to an acquisition.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
#[serde(tag = "outcome")]
pub enum AcquireLeaseOutcomeDto {
    /// Every applicable Quota holds the amount until `expires_at`.
    Acquired {
        /// Opaque, server-issued lease token.
        token: Uuid,
        /// When the hold ends unless committed or released first.
        #[serde(with = "time::serde::rfc3339")]
        expires_at: OffsetDateTime,
    },
    /// The policy refused; nothing is held.
    Denied {
        /// The refusing decision.
        decision: DecisionDto,
    },
}

impl From<AcquireLeaseOutcome> for AcquireLeaseOutcomeDto {
    fn from(value: AcquireLeaseOutcome) -> Self {
        match value {
            AcquireLeaseOutcome::Acquired { token, expires_at } => Self::Acquired {
                token: token.as_uuid(),
                expires_at,
            },
            AcquireLeaseOutcome::Denied { decision } => Self::Denied {
                decision: decision.into(),
            },
        }
    }
}

async fn acquire(
    Extension(ctx): Extension<SecurityContext>,
    Extension(service): Extension<Arc<Service>>,
    JsonBody(body): JsonBody<AcquireLeaseRequestDto>,
) -> ApiResult<impl IntoResponse> {
    let outcome = service
        .operations()?
        .acquire_lease(
            &ctx,
            AcquireLeaseRequest {
                attribution: body.attribution.into(),
                amount: body.amount,
                ttl_secs: body.ttl_secs,
                idempotency_key: body.idempotency_key,
            },
        )
        .await?;
    Ok(Json(AcquireLeaseOutcomeDto::from(outcome)))
}

async fn commit(
    Extension(ctx): Extension<SecurityContext>,
    Extension(service): Extension<Arc<Service>>,
    Path(token): Path<Uuid>,
    JsonBody(body): JsonBody<CommitLeaseRequestDto>,
) -> ApiResult<impl IntoResponse> {
    let decision = service
        .operations()?
        .commit_lease(
            &ctx,
            CommitLeaseRequest {
                tenant_id: TenantId::new(body.tenant_id),
                token: LeaseToken::new(token),
                actual_amount: body.actual_amount,
                idempotency_key: body.idempotency_key,
            },
        )
        .await?;
    Ok(Json(DecisionDto::from(decision)))
}

async fn release(
    Extension(ctx): Extension<SecurityContext>,
    Extension(service): Extension<Arc<Service>>,
    Path(token): Path<Uuid>,
    JsonBody(body): JsonBody<ReleaseLeaseRequestDto>,
) -> ApiResult<impl IntoResponse> {
    let decision = service
        .operations()?
        .release_lease(
            &ctx,
            ReleaseLeaseRequest {
                tenant_id: TenantId::new(body.tenant_id),
                token: LeaseToken::new(token),
                idempotency_key: body.idempotency_key,
            },
        )
        .await?;
    Ok(Json(DecisionDto::from(decision)))
}

/// Mount the three lease endpoints.
// @cpt-dod:cpt-cf-quota-enforcement-dod-lease-endpoints:p1
pub fn register(mut router: Router, openapi: &dyn OpenApiRegistry) -> Router {
    const TAG: &str = "Leases";
    let prefix = super::routes::PATH_PREFIX;
    router = OperationBuilder::post(format!("{prefix}/leases"))
        .operation_id("quota_enforcement.acquire_lease")
        .summary("Hold capacity against every applicable Quota")
        .description(
            "Evaluates the applicable policy like a debit and, when it allows, holds the amount \
             on every Quota in its plan until the TTL passes. A denial is HTTP 200 with \
             `outcome: denied`. Replaying the key returns the original token or denial. The \
             active-lease cap answers 429 LEASE_INFLIGHT_LIMIT_EXCEEDED; a contended row past \
             the metric's contention timeout answers 409 LEASE_CONTENTION_TIMEOUT.",
        )
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .json_request::<AcquireLeaseRequestDto>(openapi, "The hold to acquire")
        .handler(acquire)
        .json_response_with_schema::<AcquireLeaseOutcomeDto>(
            openapi,
            StatusCode::OK,
            "The lease, or the refusing decision",
        )
        .standard_errors(openapi)
        .register(router, openapi);
    router = OperationBuilder::post(format!("{prefix}/leases/{{token}}/commit"))
        .operation_id("quota_enforcement.commit_lease")
        .summary("Convert an active lease into a debit")
        .description(
            "Keeps `actual_amount` of what the lease holds, apportioned across its holds, and \
             returns the rest, all against the acquisition period. An expired or resolved lease \
             answers 400 LEASE_NOT_ACTIVE; more than was reserved answers 400 \
             OVER_COMMIT_NOT_AUTHORIZED. The commit is reversible by a rollback naming \
             `lease_commit`.",
        )
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .path_param("token", "Lease token")
        .json_request::<CommitLeaseRequestDto>(openapi, "What to keep")
        .handler(commit)
        .json_response_with_schema::<DecisionDto>(openapi, StatusCode::OK, "The decision")
        .standard_errors(openapi)
        .register(router, openapi);
    OperationBuilder::post(format!("{prefix}/leases/{{token}}/release"))
        .operation_id("quota_enforcement.release_lease")
        .summary("Return everything an active lease holds")
        .description(
            "Returns every hold to the acquisition period and commits nothing. An expired or \
             resolved lease answers 400 LEASE_NOT_ACTIVE.",
        )
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .path_param("token", "Lease token")
        .json_request::<ReleaseLeaseRequestDto>(openapi, "The release")
        .handler(release)
        .json_response_with_schema::<DecisionDto>(openapi, StatusCode::OK, "The decision")
        .standard_errors(openapi)
        .register(router, openapi)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "leases_tests.rs"]
mod tests;
