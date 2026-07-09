//! Unit tests for the foundation usage-record REST route table.
//!
//! Exercises the two record-surface routes
//! (`POST /usage-collector/v1/records`,
//! `POST /usage-collector/v1/records/{id}/deactivate`) against the
//! [`toolkit::api::openapi_registry::OpenApiRegistryImpl`] that
//! [`super::register_usage_record_routes`] populates. Each test pulls the
//! full registered [`toolkit::api::operation_builder::OperationSpec`] and
//! asserts the contract surface that documents the route — operation id,
//! authentication posture, license posture, request body schema, success
//! response schemas, and standard error coverage — so a regression that
//! silently drops `.authenticated()`, `.no_license_required()`,
//! `.json_request::<…>`, `.json_response_with_schema::<…>`, or
//! `.standard_errors()` fails loudly.

use axum::Router;
use axum::http::{Method, StatusCode};
use toolkit::api::openapi_registry::OpenApiRegistryImpl;
use toolkit::api::operation_builder::{OperationSpec, ParamLocation, RequestBodySchema};

use super::register_usage_record_routes;
use crate::api::rest::dto;

fn registry_and_router() -> (OpenApiRegistryImpl, Router) {
    let registry = OpenApiRegistryImpl::new();
    let router = register_usage_record_routes(Router::new(), &registry);
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
         the foundation surface is platform-internal substrate",
        spec.method,
        spec.path,
    );
}

#[tokio::test]
async fn create_usage_records_route_is_registered_with_documented_contract() {
    let (registry, _router) = registry_and_router();
    let spec = lookup_spec(&registry, &Method::POST, "/usage-collector/v1/records");

    assert_eq!(
        spec.operation_id.as_deref(),
        Some("usage_collector.create_usage_records"),
    );
    assert_authenticated_and_no_license(&spec);

    // `.json_request::<CreateUsageRecordsRequest>` — request body declared
    // as a registered schema reference, not inline / multipart.
    let body = spec
        .request_body
        .as_ref()
        .expect("create-records route MUST declare a JSON request body");
    assert_eq!(body.content_type, "application/json");
    assert!(body.required, "create-records body MUST be required");
    let expected_request = schema_name::<dto::CreateUsageRecordsRequest>();
    match &body.schema {
        RequestBodySchema::Ref { schema_name } => assert_eq!(schema_name, &expected_request),
        other => panic!("expected schema ref to `{expected_request}`, got {other:?}"),
    }

    // Two success responses (200 OK and 207 Multi-Status), both bound to
    // the `CreateUsageRecordsResponse` schema.
    let expected_response = schema_name::<dto::CreateUsageRecordsResponse>();
    for status in [StatusCode::OK, StatusCode::MULTI_STATUS] {
        let response = spec
            .responses
            .iter()
            .find(|r| r.status == status.as_u16())
            .unwrap_or_else(|| panic!("create-records route MUST declare a {status} response"));
        assert_eq!(response.content_type, "application/json");
        assert_eq!(
            response.schema_name.as_deref(),
            Some(expected_response.as_str()),
            "create-records {status} MUST point at `CreateUsageRecordsResponse`",
        );
    }

    assert_standard_errors_registered(&spec);
}

#[tokio::test]
async fn get_usage_record_route_is_registered_with_documented_contract() {
    let (registry, _router) = registry_and_router();
    let spec = lookup_spec(&registry, &Method::GET, "/usage-collector/v1/records/{id}");

    assert_eq!(
        spec.operation_id.as_deref(),
        Some("usage_collector.get_usage_record"),
    );
    assert_authenticated_and_no_license(&spec);

    // Path param `id`. Get carries no JSON request body.
    let id_param = spec
        .params
        .iter()
        .find(|p| p.name == "id" && p.location == ParamLocation::Path)
        .expect("get route MUST declare path param `id`");
    assert!(id_param.required, "path param `id` MUST be required");
    assert!(
        spec.request_body.is_none(),
        "get route MUST NOT declare a request body",
    );

    // Single success response — 200 OK with the `UsageRecordDto` schema.
    let expected_response = schema_name::<dto::UsageRecordDto>();
    let ok_resp = spec
        .responses
        .iter()
        .find(|r| r.status == StatusCode::OK.as_u16())
        .expect("get route MUST declare a 200 OK response");
    assert_eq!(ok_resp.content_type, "application/json");
    assert_eq!(
        ok_resp.schema_name.as_deref(),
        Some(expected_response.as_str()),
        "get 200 MUST point at `UsageRecordDto`",
    );

    assert_standard_errors_registered(&spec);
}

#[tokio::test]
async fn deactivate_usage_record_route_is_registered_with_documented_contract() {
    let (registry, _router) = registry_and_router();
    let spec = lookup_spec(
        &registry,
        &Method::POST,
        "/usage-collector/v1/records/{id}/deactivate",
    );

    assert_eq!(
        spec.operation_id.as_deref(),
        Some("usage_collector.deactivate_usage_record"),
    );
    assert_authenticated_and_no_license(&spec);

    // Path param `id`. Deactivate carries no JSON request body.
    let id_param = spec
        .params
        .iter()
        .find(|p| p.name == "id" && p.location == ParamLocation::Path)
        .expect("deactivate route MUST declare path param `id`");
    assert!(id_param.required, "path param `id` MUST be required");
    assert!(
        spec.request_body.is_none(),
        "deactivate route MUST NOT declare a request body",
    );

    // Single success response — 204 No Content.
    let no_content = spec
        .responses
        .iter()
        .find(|r| r.status == StatusCode::NO_CONTENT.as_u16())
        .expect("deactivate route MUST declare a 204 No Content response");
    assert!(
        no_content.schema_name.is_none(),
        "deactivate 204 MUST NOT carry a body schema",
    );

    assert_standard_errors_registered(&spec);
}
