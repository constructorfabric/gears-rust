use crate::api::rest::types::{ConcreteService, ConcreteSettings};
use crate::api::rest::{dto, handlers};
use axum::http::StatusCode;
use axum::{Extension, Router};
use std::sync::Arc;
use toolkit::api::operation_builder::{CORE_GLOBAL_BASE_LICENSE_FEATURE, LicenseFeature};
use toolkit::api::{OpenApiRegistry, OperationBuilder};

const SUBJECT_SETTINGS_PATH: &str = "/construct/v1/subjects/{subject_id}/settings";
const SUBJECT_SETTINGS_TAG: &str = "Subject settings";

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
    settings: Arc<ConcreteSettings>,
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

    // @cpt-begin:cpt-cf-construct-flow-subject-settings-view:p1:inst-view-send
    router = OperationBuilder::get(SUBJECT_SETTINGS_PATH)
        .operation_id("construct.get_subject_settings")
        .summary("Read the subject's settings")
        .description(
            "Read the caller's own settings: whether personalization is on, and whether an \
             erasure is under way. A subject without stored settings has the tenant's default. \
             Another subject's settings are refused with 403.",
        )
        .tag(SUBJECT_SETTINGS_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .path_param("subject_id", "The subject: the caller's own subject id")
        .handler(handlers::get_subject_settings)
        .json_response_with_schema::<dto::SubjectSettingsDto>(
            openapi,
            StatusCode::OK,
            "The subject's settings",
        )
        .standard_errors(openapi)
        .register(router, openapi);
    // @cpt-end:cpt-cf-construct-flow-subject-settings-view:p1:inst-view-send

    // @cpt-begin:cpt-cf-construct-flow-subject-settings-change:p1:inst-change-send
    router = OperationBuilder::put(SUBJECT_SETTINGS_PATH)
        .operation_id("construct.put_subject_settings")
        .summary("Change the subject's settings")
        .description(
            "Turn personalization on or off for the caller. Applies from the next request. \
             An erasure under way is not ended by turning personalization on. Another \
             subject's settings are refused with 403.",
        )
        .tag(SUBJECT_SETTINGS_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .path_param("subject_id", "The subject: the caller's own subject id")
        .json_request::<dto::PutSubjectSettingsRequest>(openapi, "The new setting")
        .handler(handlers::put_subject_settings)
        .json_response_with_schema::<dto::SubjectSettingsDto>(
            openapi,
            StatusCode::OK,
            "The settings as stored",
        )
        .standard_errors(openapi)
        .error_422(openapi)
        .register(router, openapi);
    // @cpt-end:cpt-cf-construct-flow-subject-settings-change:p1:inst-change-send

    router.layer(Extension(service)).layer(Extension(settings))
}
