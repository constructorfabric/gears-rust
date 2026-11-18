//! REST routes that the SDK does not generate: the `OData` lists.

use std::sync::Arc;

use axum::extract::Query;
use axum::{Extension, Json, Router};
use license_enforcement_sdk::dto::{GrantFilterField, PackFilterField};
use license_enforcement_sdk::models::{Grant, Pack};
use toolkit::api::OpenApiRegistry;
use toolkit::api::canonical_prelude::{ApiResult, OData};
use toolkit::api::operation_builder::{OperationBuilder, OperationBuilderODataExt};
use toolkit_macros::domain_model;
use toolkit_odata::Page;
use toolkit_odata::filter::{FieldKind, FilterField};
use toolkit_security::SecurityContext;

use crate::domain::types::PackOrderField;
use crate::gear::Grants;

const API_TAG: &str = "License Enforcement";

/// The `$orderby` fields of `GET /grants`; the store refuses the others.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum GrantOrderField {
    Id,
}

impl FilterField for GrantOrderField {
    const FIELDS: &'static [Self] = &[Self::Id];

    fn name(&self) -> &'static str {
        "id"
    }

    fn kind(&self) -> FieldKind {
        FieldKind::Uuid
    }
}

async fn list_grants(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<Grants>>,
    OData(query): OData,
    Query(subtree): Query<Subtree>,
) -> ApiResult<Json<Page<Grant>>> {
    Ok(Json(
        svc.list_grants(&ctx, &query, subtree.include_subtree)
            .await?,
    ))
}

/// `include_subtree` of `GET /grants` and `GET /packs`; the `OData` extractor
/// ignores it.
#[derive(serde::Deserialize)]
struct Subtree {
    #[serde(default)]
    include_subtree: bool,
}

async fn list_packs(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<Grants>>,
    OData(query): OData,
    Query(subtree): Query<Subtree>,
) -> ApiResult<Json<Page<Pack>>> {
    Ok(Json(
        svc.list_packs(&ctx, &query, subtree.include_subtree)
            .await?,
    ))
}

/// Register `GET /grants` and `GET /packs`.
pub fn register_list_routes(
    router: Router,
    openapi: &dyn OpenApiRegistry,
    grants: Arc<Grants>,
) -> Router {
    let router = OperationBuilder::get("/license-enforcement/v1/grants")
        .operation_id("license_enforcement.list_grants")
        .summary("List grants")
        .description("Grants of the caller's tenant, or of its subtree with include_subtree, in the scope that the PDP gives, with OData filter, order, and cursor")
        .tag(API_TAG)
        .authenticated()
        .no_license_required()
        .query_param_typed("limit", false, "Page size", "integer")
        .query_param("cursor", false, "next_cursor of the previous page")
        .query_param_typed(
            "include_subtree",
            false,
            "true lists the grants of the tenants below the caller too; needs read_subtree",
            "boolean",
        )
        .handler(list_grants)
        .json_response_with_schema::<Page<Grant>>(openapi, http::StatusCode::OK, "A page of grants")
        .with_odata_filter::<GrantFilterField>()
        .with_odata_orderby::<GrantOrderField>()
        .standard_errors(openapi)
        .register(router, openapi);

    let router = OperationBuilder::get("/license-enforcement/v1/packs")
        .operation_id("license_enforcement.list_packs")
        .summary("List packs")
        .description("Packs of the caller's tenant, or of its subtree with include_subtree, in the scope that the PDP gives, with OData filter on the members, order, and cursor")
        .tag(API_TAG)
        .authenticated()
        .no_license_required()
        .query_param_typed("limit", false, "Page size", "integer")
        .query_param("cursor", false, "next_cursor of the previous page")
        .query_param_typed(
            "include_subtree",
            false,
            "true lists the packs of the tenants below the caller too; needs read_subtree",
            "boolean",
        )
        .handler(list_packs)
        .json_response_with_schema::<Page<Pack>>(openapi, http::StatusCode::OK, "A page of packs")
        .with_odata_filter::<PackFilterField>()
        .with_odata_orderby::<PackOrderField>()
        .standard_errors(openapi)
        .register(router, openapi);

    router.layer(Extension(grants))
}
