//! Handlers of the Quota lifecycle endpoints. Each one converts the wire shape,
//! borrows the lifecycle component from the service, and lets the domain order
//! the steps; the domain error lifts to a `Problem`.

use std::sync::Arc;

use axum::Extension;
use axum::http::Uri;
use axum::response::IntoResponse;
use toolkit::api::response::created_json;
use toolkit::api::rest::extract::{Json as JsonBody, Path};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::dto::{CreateQuotaDto, DeactivateOutcomeDto, QuotaViewDto, UpdateQuotaDto};
use super::error::ApiResult;
use super::odata_list::{
    continuation, lower_quota_filter, next_cursor, page_size, reject_select, reject_unknown_params,
    require_order,
};
use crate::domain::Service;
use crate::domain::quotas::{CreateQuotaRequest, UpdateQuotaRequest};
use axum::extract::Query;
use quota_enforcement_sdk::{PageRequest, QuotaId, TenantId};
use toolkit::api::odata::OData;
use toolkit_odata::{Page, PageInfo};

/// `POST /v1/quota-enforcement/quotas`: 201 with the view and a `Location`.
///
/// # Errors
///
/// The domain error of the failed step, as a `Problem`.
// @cpt-flow:cpt-cf-quota-enforcement-flow-quota-create:p1
pub async fn create_quota(
    uri: Uri,
    Extension(ctx): Extension<SecurityContext>,
    Extension(service): Extension<Arc<Service>>,
    JsonBody(body): JsonBody<CreateQuotaDto>,
) -> ApiResult<impl IntoResponse> {
    // @cpt-begin:cpt-cf-quota-enforcement-flow-quota-create:p1:inst-qcr-request
    let quotas = service.quotas()?;
    let request = CreateQuotaRequest::try_from(body)?;
    let view = quotas.create(&ctx, request).await?;
    // @cpt-end:cpt-cf-quota-enforcement-flow-quota-create:p1:inst-qcr-request
    // @cpt-begin:cpt-cf-quota-enforcement-flow-quota-create:p1:inst-qcr-return
    let id = view.quota.id.to_string();
    Ok(created_json(QuotaViewDto::from(view), &uri, &id).into_response())
    // @cpt-end:cpt-cf-quota-enforcement-flow-quota-create:p1:inst-qcr-return
}

/// `GET /v1/quota-enforcement/quotas/{id}`.
///
/// # Errors
///
/// The domain error of the failed step, as a `Problem`.
// @cpt-flow:cpt-cf-quota-enforcement-flow-quota-read:p1
pub async fn get_quota(
    Extension(ctx): Extension<SecurityContext>,
    Extension(service): Extension<Arc<Service>>,
    Path(id): Path<Uuid>,
) -> ApiResult<axum::Json<QuotaViewDto>> {
    // @cpt-begin:cpt-cf-quota-enforcement-flow-quota-read:p1:inst-qrd-request
    let quotas = service.quotas()?;
    let view = quotas.get(&ctx, QuotaId::new(id)).await?;
    // @cpt-end:cpt-cf-quota-enforcement-flow-quota-read:p1:inst-qrd-request
    // @cpt-begin:cpt-cf-quota-enforcement-flow-quota-read:p1:inst-qrd-return
    Ok(axum::Json(QuotaViewDto::from(view)))
    // @cpt-end:cpt-cf-quota-enforcement-flow-quota-read:p1:inst-qrd-return
}

/// `GET /v1/quota-enforcement/quotas`: `$filter`, `$orderby=id asc`, `$top` and a forward cursor.
///
/// # Errors
///
/// A query outside the list's grammar, or the domain error of the failed step, as a `Problem`.
pub async fn list_quotas(
    Extension(ctx): Extension<SecurityContext>,
    Extension(service): Extension<Arc<Service>>,
    Query(params): Query<Vec<(String, String)>>,
    OData(query): OData,
) -> ApiResult<axum::Json<Page<QuotaViewDto>>> {
    reject_unknown_params(&params)?;
    reject_select(query.select.as_ref())?;
    require_order(&query.order, "id")?;
    let quotas = service.quotas()?;
    let limit = page_size(
        query.limit,
        PageRequest::DEFAULT_LIMIT,
        quotas.limits().list_max_limit,
    )?;
    let parts = lower_quota_filter(query.filter.as_deref())?;
    let binding = parts.binding(
        parts
            .tenant_id
            .unwrap_or_else(|| TenantId::new(ctx.subject_tenant_id())),
    );
    let cursor = continuation(query.cursor.as_ref(), QUOTA_ORDER, &binding)?;
    let page = quotas.list(&ctx, parts.into_request(limit, cursor)).await?;
    Ok(axum::Json(Page {
        items: page.items.into_iter().map(QuotaViewDto::from).collect(),
        page_info: PageInfo {
            next_cursor: next_cursor(page.next_cursor, QUOTA_ORDER, &binding)?,
            prev_cursor: None,
            limit: u64::from(limit),
        },
    }))
}

/// The keyset order of the Quota list, as a cursor's signed sort token.
const QUOTA_ORDER: &str = "+id";

/// `PATCH /v1/quota-enforcement/quotas/{id}`.
///
/// # Errors
///
/// The domain error of the failed step, as a `Problem`.
// @cpt-flow:cpt-cf-quota-enforcement-flow-quota-update:p1
pub async fn update_quota(
    Extension(ctx): Extension<SecurityContext>,
    Extension(service): Extension<Arc<Service>>,
    Path(id): Path<Uuid>,
    JsonBody(body): JsonBody<UpdateQuotaDto>,
) -> ApiResult<axum::Json<QuotaViewDto>> {
    // @cpt-begin:cpt-cf-quota-enforcement-flow-quota-update:p1:inst-qup-request
    let quotas = service.quotas()?;
    let view = quotas
        .update(&ctx, QuotaId::new(id), UpdateQuotaRequest::from(body))
        .await?;
    // @cpt-end:cpt-cf-quota-enforcement-flow-quota-update:p1:inst-qup-request
    // @cpt-begin:cpt-cf-quota-enforcement-flow-quota-update:p1:inst-qup-return
    Ok(axum::Json(QuotaViewDto::from(view)))
    // @cpt-end:cpt-cf-quota-enforcement-flow-quota-update:p1:inst-qup-return
}

/// `POST /v1/quota-enforcement/quotas/{id}/deactivate`: 200 with the outcome.
///
/// # Errors
///
/// The domain error of the failed step, as a `Problem`.
pub async fn deactivate_quota(
    Extension(ctx): Extension<SecurityContext>,
    Extension(service): Extension<Arc<Service>>,
    Path(id): Path<Uuid>,
) -> ApiResult<axum::Json<DeactivateOutcomeDto>> {
    let quotas = service.quotas()?;
    let outcome = quotas.deactivate(&ctx, QuotaId::new(id)).await?;
    Ok(axum::Json(DeactivateOutcomeDto::from(outcome)))
}
