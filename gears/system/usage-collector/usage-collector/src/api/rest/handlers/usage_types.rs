//! REST handlers for the foundation `/usage-collector/v1/usage-types`
//! catalog surface. Each handler is a thin pass-through: it pulls the
//! gateway-resolved `SecurityContext`, dispatches to the domain
//! [`Service`], and lifts `UsageCollectorError` through the host-owned
//! canonical mapping. PDP authorization runs inside each `Service`
//! catalog method.

use std::sync::Arc;

use axum::extract::{Extension, Path};
use axum::http::Uri;
use toolkit::api::canonical_prelude::*;
use toolkit_odata::Page as ODataPage;
use toolkit_security::SecurityContext;
use usage_collector_sdk::{UsageKind, UsageType, UsageTypeGtsId};

use crate::api::rest::dto::{CreateUsageTypeRequest, UsageTypeDto};
use crate::domain::Service;
use crate::domain::validation::metadata_fields_from_wire;
use crate::infra::sdk_error_mapping::usage_collector_error_to_canonical_for_usage_type as usage_collector_error_to_canonical;

/// `POST /usage-collector/v1/usage-types`
///
/// Returns HTTP 201 with the registered usage-type record and a
/// `Location` header at `GET /usage-collector/v1/usage-types/{gts_id}`.
/// Uses `Json<CreateUsageTypeRequest>` (permissive `gts_id: String`)
/// rather than `Json<UsageType>` so a bad `gts_id` surfaces as a
/// canonical `InvalidArgument` `Problem` (with
/// `field_violations[0].reason="INVALID_BASE_GTS_ID"`) instead of
/// axum's default `text/plain` 422.
// @cpt-dod:cpt-cf-usage-collector-dod-usage-type-lifecycle-api-post-usage-types:p1
// @cpt-dod:cpt-cf-usage-collector-dod-foundation-entity-security-context:p1
// @cpt-dod:cpt-cf-usage-collector-dod-foundation-principle-fail-closed:p2
pub async fn handle_create_usage_type(
    // @cpt-begin:cpt-cf-usage-collector-flow-usage-type-lifecycle-register-usage-type:p1:inst-register-usage-type-submit
    Extension(ctx): Extension<SecurityContext>,
    Extension(service): Extension<Arc<Service>>,
    uri: Uri,
    Json(req): Json<CreateUsageTypeRequest>,
) -> ApiResult<impl IntoResponse> {
    let gts_id = UsageTypeGtsId::new(req.gts_id).map_err(usage_collector_error_to_canonical)?;
    let kind: UsageKind = req
        .kind
        .parse()
        .map_err(usage_collector_error_to_canonical)?;
    // @cpt-begin:cpt-cf-usage-collector-flow-usage-type-lifecycle-register-usage-type:p1:inst-register-usage-type-validate-shape
    // @cpt-begin:cpt-cf-usage-collector-flow-usage-type-lifecycle-register-usage-type:p1:inst-register-usage-type-invalid-shape
    let metadata_fields = metadata_fields_from_wire(req.metadata_fields)
        .map_err(usage_collector_error_to_canonical)?;
    // @cpt-end:cpt-cf-usage-collector-flow-usage-type-lifecycle-register-usage-type:p1:inst-register-usage-type-invalid-shape
    // @cpt-end:cpt-cf-usage-collector-flow-usage-type-lifecycle-register-usage-type:p1:inst-register-usage-type-validate-shape
    // @cpt-begin:cpt-cf-usage-collector-flow-usage-type-lifecycle-register-usage-type:p1:inst-register-usage-type-service-call
    let usage_type = service
        .create_usage_type(
            &ctx,
            UsageType {
                gts_id,
                kind,
                metadata_fields,
            },
        )
        .await
        .map_err(usage_collector_error_to_canonical)?;
    // @cpt-end:cpt-cf-usage-collector-flow-usage-type-lifecycle-register-usage-type:p1:inst-register-usage-type-service-call
    // @cpt-end:cpt-cf-usage-collector-flow-usage-type-lifecycle-register-usage-type:p1:inst-register-usage-type-submit
    // @cpt-begin:cpt-cf-usage-collector-flow-usage-type-lifecycle-register-usage-type:p1:inst-register-usage-type-return
    let id_str = usage_type.gts_id.to_string();
    Ok(created_json(UsageTypeDto::from(usage_type), &uri, &id_str))
    // @cpt-end:cpt-cf-usage-collector-flow-usage-type-lifecycle-register-usage-type:p1:inst-register-usage-type-return
}

/// `GET /usage-collector/v1/usage-types`
// @cpt-dod:cpt-cf-usage-collector-dod-usage-type-lifecycle-api-list-usage-types:p1
pub async fn handle_list_usage_types(
    // @cpt-begin:cpt-cf-usage-collector-flow-usage-type-lifecycle-list-usage-types:p1:inst-list-usage-types-submit
    Extension(ctx): Extension<SecurityContext>,
    Extension(service): Extension<Arc<Service>>,
    OData(query): OData,
) -> ApiResult<Json<ODataPage<UsageTypeDto>>> {
    // @cpt-begin:cpt-cf-usage-collector-flow-usage-type-lifecycle-list-usage-types:p1:inst-list-usage-types-service-call
    let page = service
        .list_usage_types(&ctx, &query)
        .await
        .map_err(usage_collector_error_to_canonical)?;
    // @cpt-end:cpt-cf-usage-collector-flow-usage-type-lifecycle-list-usage-types:p1:inst-list-usage-types-service-call
    // @cpt-end:cpt-cf-usage-collector-flow-usage-type-lifecycle-list-usage-types:p1:inst-list-usage-types-submit
    // @cpt-begin:cpt-cf-usage-collector-flow-usage-type-lifecycle-list-usage-types:p1:inst-list-usage-types-return
    Ok(Json(page.map_items(UsageTypeDto::from)))
    // @cpt-end:cpt-cf-usage-collector-flow-usage-type-lifecycle-list-usage-types:p1:inst-list-usage-types-return
}

/// `GET /usage-collector/v1/usage-types/{gts_id}`
// @cpt-dod:cpt-cf-usage-collector-dod-usage-type-lifecycle-api-get-usage-type:p1
pub async fn handle_get_usage_type(
    // @cpt-begin:cpt-cf-usage-collector-flow-usage-type-lifecycle-get-usage-type:p1:inst-get-usage-type-submit
    Extension(ctx): Extension<SecurityContext>,
    Extension(service): Extension<Arc<Service>>,
    Path(gts_id_raw): Path<String>,
) -> ApiResult<Json<UsageTypeDto>> {
    let gts_id = UsageTypeGtsId::new(gts_id_raw).map_err(usage_collector_error_to_canonical)?;
    // @cpt-begin:cpt-cf-usage-collector-flow-usage-type-lifecycle-get-usage-type:p1:inst-get-usage-type-service-call
    let usage_type = service
        .get_usage_type(&ctx, gts_id)
        .await
        .map_err(usage_collector_error_to_canonical)?;
    // @cpt-end:cpt-cf-usage-collector-flow-usage-type-lifecycle-get-usage-type:p1:inst-get-usage-type-service-call
    // @cpt-end:cpt-cf-usage-collector-flow-usage-type-lifecycle-get-usage-type:p1:inst-get-usage-type-submit
    // @cpt-begin:cpt-cf-usage-collector-flow-usage-type-lifecycle-get-usage-type:p1:inst-get-usage-type-return
    Ok(Json(UsageTypeDto::from(usage_type)))
    // @cpt-end:cpt-cf-usage-collector-flow-usage-type-lifecycle-get-usage-type:p1:inst-get-usage-type-return
}

/// `DELETE /usage-collector/v1/usage-types/{gts_id}`
// @cpt-dod:cpt-cf-usage-collector-dod-usage-type-lifecycle-api-delete-usage-type:p1
pub async fn handle_delete_usage_type(
    // @cpt-begin:cpt-cf-usage-collector-flow-usage-type-lifecycle-delete-usage-type:p1:inst-delete-usage-type-pdp-authorize
    Extension(ctx): Extension<SecurityContext>,
    Extension(service): Extension<Arc<Service>>,
    Path(gts_id_raw): Path<String>,
) -> ApiResult<StatusCode> {
    let gts_id = UsageTypeGtsId::new(gts_id_raw).map_err(usage_collector_error_to_canonical)?;
    service
        .delete_usage_type(&ctx, gts_id)
        .await
        .map_err(usage_collector_error_to_canonical)?;
    // @cpt-end:cpt-cf-usage-collector-flow-usage-type-lifecycle-delete-usage-type:p1:inst-delete-usage-type-pdp-authorize
    // @cpt-begin:cpt-cf-usage-collector-flow-usage-type-lifecycle-delete-usage-type:p1:inst-delete-usage-type-spi-delete-return
    Ok(StatusCode::NO_CONTENT)
    // @cpt-end:cpt-cf-usage-collector-flow-usage-type-lifecycle-delete-usage-type:p1:inst-delete-usage-type-spi-delete-return
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "usage_types_tests.rs"]
mod usage_types_tests;
