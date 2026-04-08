use std::sync::Arc;

use axum::Extension;
use modkit::api::prelude::*;
use modkit_security::SecurityContext;
use serde::Serialize;
use utoipa::ToSchema;

use crate::domain::model::quota::{PeriodResult, QuotaStatusResult, TierResult};
use crate::domain::service::{actions, resources};
use crate::domain::stream_events::{QuotaPeriod, QuotaTier};
use crate::module::AppServices;

// ════════════════════════════════════════════════════════════════════════════
// Response DTOs
// ════════════════════════════════════════════════════════════════════════════

#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct QuotaStatusResponse {
    pub tiers: Vec<QuotaTierStatus>,
    pub warning_threshold_pct: u8,
}

#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct QuotaTierStatus {
    pub tier: QuotaTier,
    pub periods: Vec<QuotaPeriodStatus>,
}

#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct QuotaPeriodStatus {
    pub period: QuotaPeriod,
    pub limit_credits_micro: i64,
    pub used_credits_micro: i64,
    pub remaining_credits_micro: i64,
    pub remaining_percentage: u8,
    #[serde(with = "time::serde::rfc3339")]
    pub next_reset: time::OffsetDateTime,
    pub warning: bool,
    pub exhausted: bool,
}

impl modkit::api::api_dto::ResponseApiDto for QuotaStatusResponse {}

// ════════════════════════════════════════════════════════════════════════════
// Domain → DTO mapping
// ════════════════════════════════════════════════════════════════════════════

impl From<QuotaStatusResult> for QuotaStatusResponse {
    fn from(r: QuotaStatusResult) -> Self {
        Self {
            tiers: r.tiers.into_iter().map(QuotaTierStatus::from).collect(),
            warning_threshold_pct: r.warning_threshold_pct,
        }
    }
}

impl From<TierResult> for QuotaTierStatus {
    fn from(t: TierResult) -> Self {
        Self {
            tier: t.tier,
            periods: t.periods.into_iter().map(QuotaPeriodStatus::from).collect(),
        }
    }
}

impl From<PeriodResult> for QuotaPeriodStatus {
    fn from(p: PeriodResult) -> Self {
        Self {
            period: p.period,
            limit_credits_micro: p.limit_credits_micro,
            used_credits_micro: p.used_credits_micro,
            remaining_credits_micro: p.remaining_credits_micro,
            remaining_percentage: p.remaining_percentage,
            next_reset: p.next_reset,
            warning: p.warning,
            exhausted: p.exhausted,
        }
    }
}

// ════════════════════════════════════════════════════════════════════════════
// Handler
// ════════════════════════════════════════════════════════════════════════════

/// GET /mini-chat/v1/quota/status
#[tracing::instrument(skip(svc, ctx))]
pub(crate) async fn get_quota_status(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<AppServices>>,
) -> ApiResult<JsonBody<QuotaStatusResponse>> {
    // Permission check — quota data is user-specific via tenant_id + user_id,
    // not ORM scope. ensure_owner intersects with subject_id for defence-in-depth.
    let scope = svc
        .enforcer
        .access_scope(&ctx, &resources::USER_QUOTA, actions::READ, None)
        .await
        .map_err(crate::domain::error::DomainError::from)?
        .ensure_owner(ctx.subject_id());

    let tenant_id = ctx.subject_tenant_id();
    let user_id = ctx.subject_id();

    let status = svc
        .quota
        .get_quota_status(&scope, tenant_id, user_id)
        .await?;
    Ok(Json(QuotaStatusResponse::from(status)))
}
