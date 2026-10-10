use crate::api::rest::types::ConcreteService;
use crate::api::rest::{dto, handlers};
use axum::http::StatusCode;
use axum::{Extension, Router};
use std::sync::Arc;
use toolkit::api::operation_builder::{CORE_GLOBAL_BASE_LICENSE_FEATURE, LicenseFeature};
use toolkit::api::{OpenApiRegistry, OperationBuilder};

struct License;

impl AsRef<str> for License {
    fn as_ref(&self) -> &'static str {
        CORE_GLOBAL_BASE_LICENSE_FEATURE
    }
}

impl LicenseFeature for License {}

/// @cpt-dod:cpt-cf-construct-dod-gear-foundation-create-route:p1
pub fn register_routes(
    mut router: Router,
    openapi: &dyn OpenApiRegistry,
    service: Arc<ConcreteService>,
) -> Router {
    // @cpt-begin:cpt-cf-construct-flow-gear-foundation-create-note:p1:inst-create-send
    router = OperationBuilder::post("/construct/v1/foundation-notes")
        .operation_id("construct.create_foundation_note")
        .summary("Create a foundation note")
        .description("Create a note in the tenant of the authenticated subject")
        .tag("Foundation")
        .authenticated()
        .require_license_features::<License>([])
        .json_request::<dto::CreateFoundationNoteRequest>(openapi, "Note to create")
        .handler(handlers::create_note)
        .json_response_with_schema::<dto::FoundationNoteDto>(
            openapi,
            StatusCode::CREATED,
            "Note created",
        )
        .standard_errors(openapi)
        .error_422(openapi)
        .register(router, openapi);
    // @cpt-end:cpt-cf-construct-flow-gear-foundation-create-note:p1:inst-create-send

    router.layer(Extension(service))
}
