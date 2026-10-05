//! The kept catalog browse envelope, backed by authorized SKU reads.
use super::{ApiState, require_authenticated};
use crate::infra::catalog_provider::{BrowseCatalogProvider, invalid};
use authz_resolver_sdk::PolicyEnforcer;
use axum::{
    Extension, Json, Router,
    extract::{Query, rejection::QueryRejection},
    http::StatusCode,
};
use bss_pricing_sdk::product_catalog::{CatalogSku, CatalogTaxCategory};
use std::sync::Arc;
use toolkit::api::{OpenApiRegistry, operation_builder::OperationBuilder};
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;
use uuid::Uuid;

// `$filter` is the served query name.
#[allow(unknown_lints, de0803_api_snake_case)]
#[toolkit_macros::api_dto(request)]
struct BrowseQuery {
    kind: String,
    #[serde(rename = "$filter")]
    filter: Option<String>,
    limit: Option<u32>,
    cursor: Option<String>,
}
#[toolkit_macros::api_dto(response)]
struct BrowsePageInfo {
    next_cursor: Option<String>,
}
#[toolkit_macros::api_dto(response)]
struct BrowsePage {
    rows: Vec<BrowseRow>,
    page_info: BrowsePageInfo,
}
#[toolkit_macros::api_dto(response)]
#[serde(untagged)]
enum BrowseRow {
    Sku(Box<SkuRow>),
    Tax(TaxRow),
}
#[toolkit_macros::api_dto(response)]
struct TaxRow {
    code: String,
    display_name: String,
}
impl From<CatalogTaxCategory> for TaxRow {
    fn from(r: CatalogTaxCategory) -> Self {
        Self {
            code: r.code,
            display_name: r.display_name,
        }
    }
}
/// Compatibility projection consumed by the unchanged REST row parser.
#[toolkit_macros::api_dto(response)]
struct SkuRow {
    entity_kind: String,
    entity_id: Uuid,
    entity_code: Option<String>,
    name: String,
    lifecycle_state: String,
    deprecated: bool,
    composition_pending: bool,
    sellable: Option<bool>,
    deprecation_provenance: Option<String>,
    replaced_by_sku_id: Option<Uuid>,
    region_scope: String,
    brand_scope: String,
    sku_type: Option<String>,
    plan_tier_label: Option<String>,
    metering_unit: Option<String>,
    usage_type_ref: Option<String>,
    display_attributes: Option<String>,
    category_paths: Option<String>,
}
impl From<CatalogSku> for SkuRow {
    fn from(s: CatalogSku) -> Self {
        Self {
            entity_kind: "sku".into(),
            entity_id: s.sku_id,
            entity_code: Some(s.sku_code),
            name: s.name,
            lifecycle_state: s.status,
            deprecated: s.deprecated,
            composition_pending: false,
            sellable: Some(s.sellable),
            deprecation_provenance: None,
            replaced_by_sku_id: None,
            region_scope: String::new(),
            brand_scope: String::new(),
            sku_type: Some(s.sku_type),
            plan_tier_label: s.plan_tier,
            metering_unit: s.metering_unit,
            usage_type_ref: s.usage_type_ref,
            display_attributes: None,
            category_paths: None,
        }
    }
}
/// Register the catalog door and its kept query and envelope contract.
// `$filter` is a free string. `with_odata_filter` would publish a typed filter schema.
#[allow(unknown_lints, de0802_use_odata_ext)]
pub(crate) fn router(state: Arc<ApiState>, openapi: &dyn OpenApiRegistry) -> Router {
    OperationBuilder::get("/bss-products/v1/browse")
        .operation_id("bss_products.browse")
        .summary("Browse published and deprecated SKUs")
        .tag("SKUs")
        .authenticated()
        .no_license_required()
        .query_param_typed("kind", true, "sku or tax_category", "string")
        .query_param_typed(
            "$filter",
            false,
            "OData over entity_id/sku_id, entity_code/sku_code and name",
            "string",
        )
        .query_param_typed("limit", false, "Page size, 1 to 200; default 50", "integer")
        .query_param_typed("cursor", false, "Exclusive SKU code continuation", "string")
        .handler(browse)
        .json_response_with_schema::<BrowsePage>(
            openapi,
            StatusCode::OK,
            "Catalog rows and page_info",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_500(openapi)
        .error_503(openapi)
        .register(Router::new(), openapi)
        .layer(Extension(state))
}
async fn browse(
    Extension(state): Extension<Arc<ApiState>>,
    Extension(enforcer): Extension<PolicyEnforcer>,
    ctx: Option<Extension<SecurityContext>>,
    query: Result<Query<BrowseQuery>, QueryRejection>,
) -> Result<Json<BrowsePage>, CanonicalError> {
    let ctx = require_authenticated(ctx)?;
    let provider = BrowseCatalogProvider::new(state.db.db(), Arc::new(enforcer));
    // The one PDP evaluation of this request: the reads below take its scope (RS-41).
    let scope = provider.scope(&ctx).await?;
    let Query(q) = query.map_err(|e| invalid("query", e.body_text()))?;
    match q.kind.as_str() {
        "sku" => {
            let page = provider
                .browse(
                    &scope,
                    ctx.subject_tenant_id(),
                    q.filter.as_deref(),
                    q.limit.unwrap_or(50),
                    q.cursor.as_deref(),
                )
                .await?;
            Ok(Json(BrowsePage {
                rows: page
                    .items
                    .into_iter()
                    .map(|s| BrowseRow::Sku(Box::new(s.into())))
                    .collect(),
                page_info: BrowsePageInfo {
                    next_cursor: page.next_cursor,
                },
            }))
        }
        "tax_category" => {
            if q.filter.is_some() || q.limit.is_some() || q.cursor.is_some() {
                return Err(invalid(
                    "query",
                    "tax_category returns the complete distinct dictionary",
                ));
            }
            Ok(Json(BrowsePage {
                rows: provider
                    .tax_categories(&scope, ctx.subject_tenant_id())
                    .await?
                    .into_iter()
                    .map(|r| BrowseRow::Tax(r.into()))
                    .collect(),
                page_info: BrowsePageInfo { next_cursor: None },
            }))
        }
        _ => Err(invalid("kind", "expected sku or tax_category")),
    }
}

#[cfg(test)]
#[path = "browse_tests.rs"]
mod browse_tests;
