//! Unit tests for the foundation usage-type REST route table.
//!
//! Exercises the DESIGN §3.5 route table
//! (`POST/GET /usage-collector/v1/usage-types`,
//! `GET/DELETE /usage-collector/v1/usage-types/{gts_id}`) against the
//! [`toolkit::api::openapi_registry::OpenApiRegistryImpl`] that
//! [`super::register_usage_type_routes`] populates. Each test pulls the
//! full registered [`toolkit::api::operation_builder::OperationSpec`] and
//! asserts the contract surface that documents the route — operation id,
//! authentication posture, license posture, request body schema,
//! path/query params, success response schema, and standard error
//! coverage — so a regression that silently drops `.authenticated()`,
//! `.no_license_required()`, `.json_request::<…>`,
//! `.json_response_with_schema::<…>`, `.path_param(…)`,
//! `.query_param(…)`, or `.standard_errors()` fails loudly. Tests target
//! the per-resource registrar directly — no `Service` / `ClientHub` is
//! required, since the routes registrar emits routes only (the shared
//! `Extension<Arc<Service>>` layer is attached one level up, in
//! [`crate::api::rest::routes::register_routes`]).

use axum::Router;
use axum::http::{Method, StatusCode};
use toolkit::api::openapi_registry::OpenApiRegistryImpl;
use toolkit::api::operation_builder::{OperationSpec, ParamLocation, RequestBodySchema};

use super::register_usage_type_routes;
use crate::api::rest::dto;

fn registry_and_router() -> (OpenApiRegistryImpl, Router) {
    let registry = OpenApiRegistryImpl::new();
    let router = register_usage_type_routes(Router::new(), &registry);
    (registry, router)
}

fn lookup_spec(registry: &OpenApiRegistryImpl, method: &Method, path: &str) -> OperationSpec {
    let key = format!("{}:{}", method.as_str(), path);
    registry
        .operation_specs
        .get(&key)
        .unwrap_or_else(|| panic!("expected route to be registered: {key}"))
        .value()
        .clone()
}

/// Schema component name `OperationBuilder` produces for a DTO is its
/// `utoipa::ToSchema::name()`.
fn schema_name<T: utoipa::ToSchema>() -> String {
    <T as utoipa::ToSchema>::name().to_string()
}

/// Standard `OperationBuilder::standard_errors` set, kept in sync with
/// `libs/toolkit/src/api/operation_builder.rs::standard_errors`. A route
/// that drops `.standard_errors()` will fail this list.
const STANDARD_ERROR_STATUSES: &[u16] = &[400, 401, 403, 404, 409, 429, 500];

fn assert_standard_errors_registered(spec: &OperationSpec) {
    for status in STANDARD_ERROR_STATUSES {
        let found = spec.responses.iter().any(|r| {
            r.status == *status
                && r.content_type == "application/problem+json"
                && r.schema_name.is_some()
        });
        assert!(
            found,
            "operation `{}` MUST declare standard error response {status} \
             (application/problem+json with a registered schema)",
            spec.path,
        );
    }
}

fn assert_authenticated_and_no_license(spec: &OperationSpec) {
    assert!(
        spec.authenticated,
        "operation `{}:{}` MUST be `.authenticated()` — \
         the foundation surface refuses anonymous callers",
        spec.method, spec.path,
    );
    assert!(
        spec.license_requirement.is_none(),
        "operation `{}:{}` MUST be `.no_license_required()` — \
         the foundation catalog is platform-internal substrate",
        spec.method,
        spec.path,
    );
}

// ---------------------------------------------------------------------------
// Foundation catalog routes — DESIGN §3.5 route table.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn create_usage_type_route_is_registered_with_documented_contract() {
    let (registry, _router) = registry_and_router();
    let spec = lookup_spec(&registry, &Method::POST, "/usage-collector/v1/usage-types");

    assert_eq!(
        spec.operation_id.as_deref(),
        Some("usage_collector.create_usage_type"),
    );
    assert_authenticated_and_no_license(&spec);

    // `.json_request::<CreateUsageTypeRequest>` — required JSON body.
    let body = spec
        .request_body
        .as_ref()
        .expect("create-usage-type route MUST declare a JSON request body");
    assert_eq!(body.content_type, "application/json");
    assert!(body.required, "create-usage-type body MUST be required");
    let expected_request = schema_name::<dto::CreateUsageTypeRequest>();
    match &body.schema {
        RequestBodySchema::Ref { schema_name } => assert_eq!(schema_name, &expected_request),
        other => panic!("expected schema ref to `{expected_request}`, got {other:?}"),
    }

    // 201 Created carrying `UsageTypeDto`.
    let expected_response = schema_name::<dto::UsageTypeDto>();
    let created = spec
        .responses
        .iter()
        .find(|r| r.status == StatusCode::CREATED.as_u16())
        .expect("create-usage-type route MUST declare a 201 Created response");
    assert_eq!(created.content_type, "application/json");
    assert_eq!(
        created.schema_name.as_deref(),
        Some(expected_response.as_str()),
        "create-usage-type 201 MUST point at `UsageTypeDto`",
    );

    assert_standard_errors_registered(&spec);
}

#[tokio::test]
async fn list_usage_types_route_is_registered_with_documented_contract() {
    let (registry, _router) = registry_and_router();
    let spec = lookup_spec(&registry, &Method::GET, "/usage-collector/v1/usage-types");

    assert_eq!(
        spec.operation_id.as_deref(),
        Some("usage_collector.list_usage_types"),
    );
    assert_authenticated_and_no_license(&spec);

    // Pagination query params: `limit` (integer, optional) + `cursor`
    // (string, optional).
    let limit = spec
        .params
        .iter()
        .find(|p| p.name == "limit" && p.location == ParamLocation::Query)
        .expect("list route MUST declare query param `limit`");
    assert!(!limit.required, "`limit` MUST be optional");
    assert_eq!(limit.param_type, "integer");

    let cursor = spec
        .params
        .iter()
        .find(|p| p.name == "cursor" && p.location == ParamLocation::Query)
        .expect("list route MUST declare query param `cursor`");
    assert!(!cursor.required, "`cursor` MUST be optional");
    assert_eq!(cursor.param_type, "string");

    // OData query params advertised by `with_odata_filter` /
    // `with_odata_orderby` / `with_odata_select` — pins TOOLKIT-ODATA-001.
    for odata_param in ["$filter", "$orderby", "$select"] {
        let p = spec
            .params
            .iter()
            .find(|p| p.name == odata_param && p.location == ParamLocation::Query)
            .unwrap_or_else(|| {
                panic!("list route MUST declare query param `{odata_param}`");
            });
        assert!(!p.required, "`{odata_param}` MUST be optional");
        assert_eq!(p.param_type, "string", "`{odata_param}` MUST be a string");
    }

    // No JSON request body on a GET.
    assert!(
        spec.request_body.is_none(),
        "list route MUST NOT declare a request body",
    );

    // 200 OK carries a page envelope. The `Page<T>` schema name is
    // toolkit-internal; assert the surface contract rather than the name.
    let ok = spec
        .responses
        .iter()
        .find(|r| r.status == StatusCode::OK.as_u16())
        .expect("list route MUST declare a 200 OK response");
    assert_eq!(ok.content_type, "application/json");
    assert!(
        ok.schema_name.is_some(),
        "list 200 MUST be bound to a registered schema",
    );

    assert_standard_errors_registered(&spec);
}

#[tokio::test]
async fn get_usage_type_route_is_registered_with_documented_contract() {
    let (registry, _router) = registry_and_router();
    let spec = lookup_spec(
        &registry,
        &Method::GET,
        "/usage-collector/v1/usage-types/{gts_id}",
    );

    assert_eq!(
        spec.operation_id.as_deref(),
        Some("usage_collector.get_usage_type"),
    );
    assert_authenticated_and_no_license(&spec);

    let gts_id_param = spec
        .params
        .iter()
        .find(|p| p.name == "gts_id" && p.location == ParamLocation::Path)
        .expect("get route MUST declare path param `gts_id`");
    assert!(
        gts_id_param.required,
        "path param `gts_id` MUST be required"
    );

    assert!(
        spec.request_body.is_none(),
        "get route MUST NOT declare a request body",
    );

    let expected_response = schema_name::<dto::UsageTypeDto>();
    let ok = spec
        .responses
        .iter()
        .find(|r| r.status == StatusCode::OK.as_u16())
        .expect("get route MUST declare a 200 OK response");
    assert_eq!(ok.content_type, "application/json");
    assert_eq!(
        ok.schema_name.as_deref(),
        Some(expected_response.as_str()),
        "get 200 MUST point at `UsageTypeDto`",
    );

    assert_standard_errors_registered(&spec);
}

#[tokio::test]
async fn delete_usage_type_route_is_registered_with_documented_contract() {
    let (registry, _router) = registry_and_router();
    let spec = lookup_spec(
        &registry,
        &Method::DELETE,
        "/usage-collector/v1/usage-types/{gts_id}",
    );

    assert_eq!(
        spec.operation_id.as_deref(),
        Some("usage_collector.delete_usage_type"),
    );
    assert_authenticated_and_no_license(&spec);

    let gts_id_param = spec
        .params
        .iter()
        .find(|p| p.name == "gts_id" && p.location == ParamLocation::Path)
        .expect("delete route MUST declare path param `gts_id`");
    assert!(
        gts_id_param.required,
        "path param `gts_id` MUST be required"
    );

    assert!(
        spec.request_body.is_none(),
        "delete route MUST NOT declare a request body",
    );

    let no_content = spec
        .responses
        .iter()
        .find(|r| r.status == StatusCode::NO_CONTENT.as_u16())
        .expect("delete route MUST declare a 204 No Content response");
    assert!(
        no_content.schema_name.is_none(),
        "delete 204 MUST NOT carry a body schema",
    );

    assert_standard_errors_registered(&spec);
}
