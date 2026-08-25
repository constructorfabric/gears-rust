//! REST route registration for the credstore module.

use std::sync::Arc;

use axum::Router;
use axum::http::StatusCode;
use toolkit::api::{OpenApiRegistry, OperationBuilder, ParamLocation, ParamSpec};

use super::dto::{CreateSecretRequestDto, GetSecretResponseDto, UpdateSecretRequestDto};
use super::handlers::{self, ConcreteService};

const TAG: &str = "Credential Store";

/// Mandatory `If-Match` precondition header for writes/deletes (RFC 7232
/// §3.1). `*` requires the secret to exist (an explicit last-writer-wins
/// overwrite); a quoted `"<id>.<version>"` `ETag` requires the current
/// version to match, otherwise the operation fails with
/// `409 OPTIMISTIC_LOCK_FAILURE`. A missing header is a
/// `400 IF_MATCH_REQUIRED`.
fn if_match_param() -> ParamSpec {
    ParamSpec {
        name: "If-Match".to_owned(),
        location: ParamLocation::Header,
        required: true,
        description: Some(
            "Mandatory optimistic-concurrency precondition (RFC 7232). `*` requires the \
             secret to exist (explicit last-writer-wins overwrite); a quoted \
             `\"<id>.<version>\"` ETag requires the current version to match, otherwise \
             the request fails with `409 OPTIMISTIC_LOCK_FAILURE`. A missing header is a \
             `400 IF_MATCH_REQUIRED`."
                .to_owned(),
        ),
        param_type: "string".to_owned(),
        array: false,
    }
}

/// Register all REST routes for the credstore module.
pub fn register_routes(
    router: Router,
    openapi: &dyn OpenApiRegistry,
    svc: Arc<ConcreteService>,
) -> Router {
    let router = OperationBuilder::post("/credstore/v1/secrets")
        .operation_id("credstore.create_secret")
        .summary("Create a secret")
        .description("Create a new secret for the authenticated tenant.")
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .json_request::<CreateSecretRequestDto>(
            openapi,
            "Secret reference, value, and sharing mode",
        )
        .handler(handlers::create_secret)
        .no_content_response(StatusCode::CREATED, "Secret created (see Location header)")
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_409(openapi)
        .error_500(openapi)
        .error_503(openapi)
        .register(router, openapi);

    let router = OperationBuilder::put("/credstore/v1/secrets/{ref}")
        .operation_id("credstore.put_secret")
        .summary("Update a secret by reference")
        .description(
            "Update an existing secret for the authenticated tenant. Requires `If-Match` \
             and never creates: a missing target fails the precondition (409); create via \
             `POST /credstore/v1/secrets`.",
        )
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .path_param(
            "ref",
            "Secret reference (`[a-zA-Z0-9_-]+`, maximum length 255 characters)",
        )
        .param(if_match_param())
        .json_request::<UpdateSecretRequestDto>(openapi, "Secret value and sharing mode")
        .handler(handlers::put_secret)
        .no_content_response(StatusCode::NO_CONTENT, "Secret stored")
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_409(openapi)
        .error_500(openapi)
        .error_503(openapi)
        .register(router, openapi);

    let router = OperationBuilder::get("/credstore/v1/secrets/{ref}")
        .operation_id("credstore.get_secret")
        .summary("Get a secret by reference")
        .description("Retrieve a secret for the authenticated tenant, with walk-up resolution.")
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .path_param(
            "ref",
            "Secret reference (`[a-zA-Z0-9_-]+`, maximum length 255 characters)",
        )
        .handler(handlers::get_secret)
        .json_response_with_schema::<GetSecretResponseDto>(
            openapi,
            StatusCode::OK,
            "Resolved secret value and metadata",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .error_503(openapi)
        .register(router, openapi);

    let router = OperationBuilder::delete("/credstore/v1/secrets/{ref}")
        .operation_id("credstore.delete_secret")
        .summary("Delete a secret by reference")
        .description("Delete a secret owned by the authenticated tenant.")
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .path_param(
            "ref",
            "Secret reference (`[a-zA-Z0-9_-]+`, maximum length 255 characters)",
        )
        .param(if_match_param())
        .handler(handlers::delete_secret)
        .no_content_response(StatusCode::NO_CONTENT, "Secret deleted")
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_409(openapi)
        .error_500(openapi)
        .error_503(openapi)
        .register(router, openapi);

    router.layer(axum::Extension(svc))
}

#[cfg(test)]
#[path = "routes_tests.rs"]
mod tests;
