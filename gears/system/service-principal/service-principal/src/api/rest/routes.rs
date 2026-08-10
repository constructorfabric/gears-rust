//! REST route registration for the service-principal gear.

use std::sync::Arc;

use axum::Router;
use axum::http::StatusCode;
use toolkit::api::{OpenApiRegistry, OperationBuilder};

use super::dto::{
    CreateServicePrincipalRequestDto, ListServicePrincipalsResponseDto,
    ServicePrincipalCredentialsDto,
};
use super::handlers;
use crate::domain::service::Service;

const TAG: &str = "Service Principals";
const TENANT_PARAM: &str = "Owning tenant id (UUID). The caller must be authorized \
    for this tenant (its own or a descendant via a subtree grant).";
const CLIENT_ID_PARAM: &str = "Service-principal client id (`svc-<tenant_id>-<name>`).";

/// Register all REST routes for the service-principal gear.
///
/// NOTE: no `#[must_use]` here — `axum::Router` (the return type) is already
/// `#[must_use]`, so an explicit attribute on this fn would double up and trip
/// `clippy::double_must_use`.
pub fn register_routes(router: Router, openapi: &dyn OpenApiRegistry, svc: Arc<Service>) -> Router {
    let router =
        OperationBuilder::post("/service-principal/v1/tenants/{tenant_id}/service-principals")
            .operation_id("service_principal.create")
            .summary("Create a service principal")
            .description(
                "Create a confidential client_credentials machine identity owned by the tenant. \
             Returns the client secret exactly once.",
            )
            .tag(TAG)
            .authenticated()
            .no_license_required()
            .path_param("tenant_id", TENANT_PARAM)
            .json_request::<CreateServicePrincipalRequestDto>(
                openapi,
                "Service-principal name and scopes",
            )
            .handler(handlers::create)
            .json_response_with_schema::<ServicePrincipalCredentialsDto>(
                openapi,
                StatusCode::CREATED,
                "Created - credentials (secret returned once; Cache-Control: no-store)",
            )
            .standard_errors(openapi)
            .register(router, openapi);

    let router =
        OperationBuilder::get("/service-principal/v1/tenants/{tenant_id}/service-principals")
            .operation_id("service_principal.list")
            .summary("List the tenant's service principals")
            .description(
                "List the tenant's service principals (no secrets). The upstream IdP is \
             unpaginated, so the full collection is returned.",
            )
            .tag(TAG)
            .authenticated()
            .no_license_required()
            .path_param("tenant_id", TENANT_PARAM)
            .handler(handlers::list)
            .json_response_with_schema::<ListServicePrincipalsResponseDto>(
                openapi,
                StatusCode::OK,
                "The tenant's service principals",
            )
            .standard_errors(openapi)
            .register(router, openapi);

    let router = OperationBuilder::post(
        "/service-principal/v1/tenants/{tenant_id}/service-principals/{client_id}/rotate-secret",
    )
    .operation_id("service_principal.rotate_secret")
    .summary("Rotate a service principal's secret")
    .description(
        "Regenerate the secret; the old one stops working. Returns the new secret exactly once.",
    )
    .tag(TAG)
    .authenticated()
    .no_license_required()
    .path_param("tenant_id", TENANT_PARAM)
    .path_param("client_id", CLIENT_ID_PARAM)
    .handler(handlers::rotate_secret)
    .json_response_with_schema::<ServicePrincipalCredentialsDto>(
        openapi,
        StatusCode::OK,
        "Rotated - new credentials (secret returned once; Cache-Control: no-store)",
    )
    .standard_errors(openapi)
    .register(router, openapi);

    // NOTE: the item path deliberately registers no `GET`, so the URL that
    // `create` returns in its `Location` header answers `DELETE` but 405s a
    // `GET`. This is a known, bounded seam rather than an oversight:
    //
    //  * the SPI (`ServicePrincipalClientV1`) exposes no by-id `get` — adding one
    //    means changing the SDK trait and every adapter, beyond this facade's scope;
    //  * RFC 9110 §10.2.2 has `Location` *identify* the created resource; it does
    //    not promise the URL is `GET`-able, and rotate/revoke both address the item
    //    without reading it;
    //  * nothing is write-only: the collection `GET` above enumerates the tenant's
    //    principals in full (the upstream quota bounds a tenant to ~10).
    //
    // Adding by-id read (either an SPI `get`, or serving it from `list` + filter
    // in the facade) is tracked as follow-up work.
    let router = OperationBuilder::delete(
        "/service-principal/v1/tenants/{tenant_id}/service-principals/{client_id}",
    )
    .operation_id("service_principal.revoke")
    .summary("Revoke a service principal")
    .description("Delete the service principal. Idempotent: a missing principal returns 204.")
    .tag(TAG)
    .authenticated()
    .no_license_required()
    .path_param("tenant_id", TENANT_PARAM)
    .path_param("client_id", CLIENT_ID_PARAM)
    .handler(handlers::revoke)
    .no_content_response(StatusCode::NO_CONTENT, "Revoked (or already absent)")
    .standard_errors(openapi)
    .register(router, openapi);

    router.layer(axum::Extension(svc))
}

#[cfg(test)]
#[path = "routes_tests.rs"]
mod tests;
