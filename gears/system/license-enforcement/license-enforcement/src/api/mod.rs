//! REST routes that the SDK does not generate: the `OData` lists.

use std::sync::Arc;

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

use crate::domain::error::DomainError;
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
) -> ApiResult<Json<Page<Grant>>> {
    Ok(Json(svc.list_grants(&ctx, &query).await?))
}

async fn list_packs(OData(_query): OData) -> ApiResult<Json<Page<Pack>>> {
    Err(DomainError::NotImplemented("list_packs").into())
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
        .description("Grants in the scope that the PDP gives, with OData filter, order, and cursor")
        .tag(API_TAG)
        .authenticated()
        .no_license_required()
        .query_param_typed("limit", false, "Page size", "integer")
        .query_param("cursor", false, "next_cursor of the previous page")
        .handler(list_grants)
        .json_response_with_schema::<Page<Grant>>(openapi, http::StatusCode::OK, "A page of grants")
        .with_odata_filter::<GrantFilterField>()
        .with_odata_orderby::<GrantOrderField>()
        .standard_errors(openapi)
        .register(router, openapi);

    let router = OperationBuilder::get("/license-enforcement/v1/packs")
        .operation_id("license_enforcement.list_packs")
        .summary("List packs")
        .description("Packs in the scope that the PDP gives. Not implemented yet.")
        .tag(API_TAG)
        .authenticated()
        .no_license_required()
        .handler(list_packs)
        .json_response_with_schema::<Page<Pack>>(openapi, http::StatusCode::OK, "A page of packs")
        .with_odata_filter::<PackFilterField>()
        .standard_errors(openapi)
        .register(router, openapi);

    router.layer(Extension(grants))
}
