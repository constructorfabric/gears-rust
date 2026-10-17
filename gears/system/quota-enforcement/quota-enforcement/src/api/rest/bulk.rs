//! The bulk Quota endpoints: one tenant's creates, updates, or
//! deactivations, all or nothing under one envelope key.
//!
//! Each item carries the single-item body unchanged (`quota` for a create,
//! `patch` for an update), so the single-item validation reports the same
//! field violations, pointed at `items[index]`.

use std::sync::Arc;

use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::{Extension, Json, Router};
use quota_enforcement_sdk::{BulkCreated, BulkDeactivated, BulkUpdated, QuotaId, TenantId};
use toolkit::api::OpenApiRegistry;
use toolkit::api::operation_builder::OperationBuilder;
use toolkit::api::rest::extract::Json as JsonBody;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::dto::{CreateQuotaDto, UpdateQuotaDto};
use super::error::ApiResult;
use crate::domain::Service;
use crate::domain::quotas::{
    BulkCreateItem, BulkCreateRequest, BulkDeactivateItem, BulkDeactivateRequest, BulkUpdateItem,
    BulkUpdateRequest, CreateQuotaRequest, UpdateQuotaRequest,
};

/// One draft of a bulk create.
#[derive(Debug, Clone, PartialEq, Eq)]
#[toolkit_macros::api_dto(request)]
#[serde(deny_unknown_fields)]
pub struct BulkCreateItemDto {
    /// Identifies the item in the outcome and in errors; no replay of its own.
    #[serde(default)]
    pub idempotency_key: Option<String>,
    /// The Quota, as `POST /quotas` takes it.
    pub quota: CreateQuotaDto,
}

/// Body of `POST /quotas/bulk-create`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[toolkit_macros::api_dto(request)]
#[serde(deny_unknown_fields)]
pub struct BulkCreateQuotasDto {
    /// The one tenant every draft names.
    pub tenant_id: Uuid,
    /// The envelope idempotency key.
    pub idempotency_key: String,
    /// The drafts, in submission order.
    pub items: Vec<BulkCreateItemDto>,
}

/// One patch of a bulk update.
#[derive(Debug, Clone, PartialEq, Eq)]
#[toolkit_macros::api_dto(request)]
#[serde(deny_unknown_fields)]
pub struct BulkUpdateItemDto {
    /// Identifies the item in the outcome and in errors; no replay of its own.
    #[serde(default)]
    pub idempotency_key: Option<String>,
    /// The Quota to patch.
    pub quota_id: Uuid,
    /// The patch, as `PATCH /quotas/{id}` takes it.
    pub patch: UpdateQuotaDto,
}

/// Body of `POST /quotas/bulk-update`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[toolkit_macros::api_dto(request)]
#[serde(deny_unknown_fields)]
pub struct BulkUpdateQuotasDto {
    /// The one tenant every Quota belongs to.
    pub tenant_id: Uuid,
    /// The envelope idempotency key.
    pub idempotency_key: String,
    /// The patches, in submission order.
    pub items: Vec<BulkUpdateItemDto>,
}

/// One Quota of a bulk deactivate.
#[derive(Debug, Clone, PartialEq, Eq)]
#[toolkit_macros::api_dto(request)]
#[serde(deny_unknown_fields)]
pub struct BulkDeactivateItemDto {
    /// Identifies the item in the outcome and in errors; no replay of its own.
    #[serde(default)]
    pub idempotency_key: Option<String>,
    /// The Quota to deactivate.
    pub quota_id: Uuid,
}

/// Body of `POST /quotas/bulk-deactivate`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[toolkit_macros::api_dto(request)]
#[serde(deny_unknown_fields)]
pub struct BulkDeactivateQuotasDto {
    /// The one tenant every Quota belongs to.
    pub tenant_id: Uuid,
    /// The envelope idempotency key.
    pub idempotency_key: String,
    /// The Quotas, in submission order.
    pub items: Vec<BulkDeactivateItemDto>,
}

/// One created Quota.
#[derive(Debug, Clone, PartialEq, Eq)]
#[toolkit_macros::api_dto(response)]
pub struct BulkCreatedItemDto {
    /// Position of the item in the request.
    pub index: usize,
    /// The item's key, when it carried one.
    pub idempotency_key: Option<String>,
    /// The server-assigned identifier.
    pub quota_id: Uuid,
}

/// Outcome of `POST /quotas/bulk-create`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[toolkit_macros::api_dto(response)]
pub struct BulkCreatedDto {
    /// One entry per item, in submission order.
    pub items: Vec<BulkCreatedItemDto>,
}

impl From<BulkCreated> for BulkCreatedDto {
    fn from(value: BulkCreated) -> Self {
        Self {
            items: value
                .items
                .into_iter()
                .map(|item| BulkCreatedItemDto {
                    index: item.index,
                    idempotency_key: item.idempotency_key,
                    quota_id: item.quota_id.as_uuid(),
                })
                .collect(),
        }
    }
}

/// One patched Quota.
#[derive(Debug, Clone, PartialEq, Eq)]
#[toolkit_macros::api_dto(response)]
pub struct BulkUpdatedItemDto {
    /// Position of the item in the request.
    pub index: usize,
    /// The item's key, when it carried one.
    pub idempotency_key: Option<String>,
    /// The patched Quota.
    pub quota_id: Uuid,
    /// Its `record_version` as the envelope committed it.
    pub record_version: u32,
}

/// Outcome of `POST /quotas/bulk-update`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[toolkit_macros::api_dto(response)]
pub struct BulkUpdatedDto {
    /// One entry per item, in submission order.
    pub items: Vec<BulkUpdatedItemDto>,
}

impl From<BulkUpdated> for BulkUpdatedDto {
    fn from(value: BulkUpdated) -> Self {
        Self {
            items: value
                .items
                .into_iter()
                .map(|item| BulkUpdatedItemDto {
                    index: item.index,
                    idempotency_key: item.idempotency_key,
                    quota_id: item.quota_id.as_uuid(),
                    record_version: item.record_version,
                })
                .collect(),
        }
    }
}

/// One deactivated Quota.
#[derive(Debug, Clone, PartialEq, Eq)]
#[toolkit_macros::api_dto(response)]
pub struct BulkDeactivatedItemDto {
    /// Position of the item in the request.
    pub index: usize,
    /// The item's key, when it carried one.
    pub idempotency_key: Option<String>,
    /// The deactivated Quota.
    pub quota_id: Uuid,
    /// The leases this item's deactivation resolved.
    pub resolved_leases: Vec<Uuid>,
}

/// Outcome of `POST /quotas/bulk-deactivate`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[toolkit_macros::api_dto(response)]
pub struct BulkDeactivatedDto {
    /// One entry per item, in submission order.
    pub items: Vec<BulkDeactivatedItemDto>,
}

impl From<BulkDeactivated> for BulkDeactivatedDto {
    fn from(value: BulkDeactivated) -> Self {
        Self {
            items: value
                .items
                .into_iter()
                .map(|item| BulkDeactivatedItemDto {
                    index: item.index,
                    idempotency_key: item.idempotency_key,
                    quota_id: item.quota_id.as_uuid(),
                    resolved_leases: item
                        .resolved_leases
                        .into_iter()
                        .map(quota_enforcement_sdk::LeaseToken::as_uuid)
                        .collect(),
                })
                .collect(),
        }
    }
}

/// The domain request of a bulk create. A draft that does not narrow to a
/// request carries its error to the item checks, after the envelope's own.
fn create_request(body: BulkCreateQuotasDto) -> BulkCreateRequest {
    BulkCreateRequest {
        tenant_id: TenantId::new(body.tenant_id),
        idempotency_key: body.idempotency_key,
        items: body
            .items
            .into_iter()
            .map(|item| BulkCreateItem {
                idempotency_key: item.idempotency_key,
                request: CreateQuotaRequest::try_from(item.quota),
            })
            .collect(),
    }
}

async fn bulk_create_quotas(
    Extension(ctx): Extension<SecurityContext>,
    Extension(service): Extension<Arc<Service>>,
    JsonBody(body): JsonBody<BulkCreateQuotasDto>,
) -> ApiResult<impl IntoResponse> {
    let quotas = service.quotas()?;
    let outcome = quotas.bulk_create(&ctx, create_request(body)).await?;
    Ok(Json(BulkCreatedDto::from(outcome)))
}

async fn bulk_update_quotas(
    Extension(ctx): Extension<SecurityContext>,
    Extension(service): Extension<Arc<Service>>,
    JsonBody(body): JsonBody<BulkUpdateQuotasDto>,
) -> ApiResult<impl IntoResponse> {
    let quotas = service.quotas()?;
    let request = BulkUpdateRequest {
        tenant_id: TenantId::new(body.tenant_id),
        idempotency_key: body.idempotency_key,
        items: body
            .items
            .into_iter()
            .map(|item| BulkUpdateItem {
                idempotency_key: item.idempotency_key,
                quota_id: QuotaId::new(item.quota_id),
                request: Ok(UpdateQuotaRequest::from(item.patch)),
            })
            .collect(),
    };
    let outcome = quotas.bulk_update(&ctx, request).await?;
    Ok(Json(BulkUpdatedDto::from(outcome)))
}

async fn bulk_deactivate_quotas(
    Extension(ctx): Extension<SecurityContext>,
    Extension(service): Extension<Arc<Service>>,
    JsonBody(body): JsonBody<BulkDeactivateQuotasDto>,
) -> ApiResult<impl IntoResponse> {
    let quotas = service.quotas()?;
    let request = BulkDeactivateRequest {
        tenant_id: TenantId::new(body.tenant_id),
        idempotency_key: body.idempotency_key,
        items: body
            .items
            .into_iter()
            .map(|item| BulkDeactivateItem {
                idempotency_key: item.idempotency_key,
                quota_id: QuotaId::new(item.quota_id),
            })
            .collect(),
    };
    let outcome = quotas.bulk_deactivate(&ctx, request).await?;
    Ok(Json(BulkDeactivatedDto::from(outcome)))
}

/// The ordering and limits every bulk endpoint shares, for the descriptions.
const ENVELOPE_RULES: &str = "All items belong to `tenant_id`; the envelope commits in one \
    transaction or not at all. More than 500 items answers 400 BULK_TOO_LARGE before any \
    authorization; each item is authorized as its single-item operation is; replaying the \
    envelope key returns the stored outcome while every target is still visible to the \
    caller; more items than the configured limit (default 50) answers 400 BULK_TOO_LARGE; \
    the first failing item's error names it as `items[index]`.";

/// Mount the three bulk Quota endpoints.
// @cpt-dod:cpt-cf-quota-enforcement-dod-bulk-endpoints:p2
pub fn register(mut router: Router, openapi: &dyn OpenApiRegistry) -> Router {
    let prefix = super::routes::PATH_PREFIX;
    router = OperationBuilder::post(format!("{prefix}/quotas/bulk-create"))
        .operation_id("quota_enforcement.bulk_create_quotas")
        .summary("Create several Quotas of one tenant, all or none")
        .description(format!(
            "Each item is a `POST /quotas` body. {ENVELOPE_RULES} A draft naming another tenant \
             answers 400 BATCH_TENANT_MIXED."
        ))
        .tag("Quotas")
        .authenticated()
        .no_license_required()
        .json_request::<BulkCreateQuotasDto>(openapi, "The drafts")
        .handler(bulk_create_quotas)
        .json_response_with_schema::<BulkCreatedDto>(openapi, StatusCode::OK, "The created Quotas")
        .standard_errors(openapi)
        .register(router, openapi);

    router = OperationBuilder::post(format!("{prefix}/quotas/bulk-update"))
        .operation_id("quota_enforcement.bulk_update_quotas")
        .summary("Patch several Quotas of one tenant, all or none")
        .description(format!(
            "Each item names a Quota and a `PATCH /quotas/{{id}}` body. {ENVELOPE_RULES} A Quota \
             named twice answers 400 BULK_QUOTA_DUPLICATE."
        ))
        .tag("Quotas")
        .authenticated()
        .no_license_required()
        .json_request::<BulkUpdateQuotasDto>(openapi, "The patches")
        .handler(bulk_update_quotas)
        .json_response_with_schema::<BulkUpdatedDto>(openapi, StatusCode::OK, "The patched Quotas")
        .standard_errors(openapi)
        .register(router, openapi);

    OperationBuilder::post(format!("{prefix}/quotas/bulk-deactivate"))
        .operation_id("quota_enforcement.bulk_deactivate_quotas")
        .summary("Deactivate several Quotas of one tenant, all or none")
        .description(format!(
            "Resolves every active lease of every listed Quota in the same transaction. \
             {ENVELOPE_RULES} A Quota named twice answers 400 BULK_QUOTA_DUPLICATE."
        ))
        .tag("Quotas")
        .authenticated()
        .no_license_required()
        .json_request::<BulkDeactivateQuotasDto>(openapi, "The Quotas")
        .handler(bulk_deactivate_quotas)
        .json_response_with_schema::<BulkDeactivatedDto>(
            openapi,
            StatusCode::OK,
            "The deactivated Quotas and their resolved leases",
        )
        .standard_errors(openapi)
        .register(router, openapi)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "bulk_tests.rs"]
mod tests;
