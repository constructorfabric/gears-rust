use crate::api::rest::types::{ConcreteIntake, ConcreteSettings};
use crate::api::rest::{dto, handlers};
use crate::domain::planner::manage::ManageFacts;
use axum::http::StatusCode;
use axum::{Extension, Router};
use std::sync::Arc;
use toolkit::api::operation_builder::{CORE_GLOBAL_BASE_LICENSE_FEATURE, LicenseFeature};
use toolkit::api::{OpenApiRegistry, OperationBuilder};

const SUBJECT_SETTINGS_PATH: &str = "/construct/v1/subjects/{subject_id}/settings";
const SUBJECT_SETTINGS_TAG: &str = "Subject settings";
const MANAGE_FACTS_PATH: &str = "/construct/v1/subjects/{subject_id}/facts/manage";
const SUBJECT_FACTS_TAG: &str = "Subject facts";

struct License;

impl AsRef<str> for License {
    fn as_ref(&self) -> &'static str {
        CORE_GLOBAL_BASE_LICENSE_FEATURE
    }
}

impl LicenseFeature for License {}

/// @cpt-dod:cpt-cf-construct-dod-record-intake-route:p1
pub fn register_routes(
    mut router: Router,
    openapi: &dyn OpenApiRegistry,
    intake: Arc<ConcreteIntake>,
    settings: Arc<ConcreteSettings>,
) -> Router {
    // @cpt-begin:cpt-cf-construct-flow-record-intake-submit:p1:inst-submit-send
    router = OperationBuilder::post("/construct/v1/records")
        .operation_id("construct.submit_record")
        .summary("Send one connector record")
        .description(
            "Take one connector record for the tenant named in `tenant`. The platform must \
             authorize the calling connector for that tenant. Answers 202 when the record is \
             received, 200 when it is a repeat, and 422 when it is refused; a refusal names the \
             record's type, the place in the record and the broken rule.",
        )
        .tag("Records")
        .authenticated()
        .require_license_features::<License>([])
        .query_param_typed("tenant", true, "Tenant the record is for", "string")
        .json_request::<dto::RecordRequest>(openapi, "One connector record")
        .handler(handlers::submit_record)
        .json_response_with_schema::<dto::RecordOutcomeDto>(
            openapi,
            StatusCode::ACCEPTED,
            "Record received",
        )
        .json_response_with_schema::<dto::RecordOutcomeDto>(
            openapi,
            StatusCode::OK,
            "Record is a repeat; nothing changes",
        )
        .standard_errors(openapi)
        .error_422(openapi)
        .error_503(openapi)
        .register(router, openapi);
    // @cpt-end:cpt-cf-construct-flow-record-intake-submit:p1:inst-submit-send

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

    router.layer(Extension(intake)).layer(Extension(settings))
}

/// The synchronous manage-facts route. The gear registers it together with the planner chain it runs on.
///
/// @cpt-dod:cpt-cf-construct-dod-planner-manage-route:p1
pub fn register_manage_route(
    mut router: Router,
    openapi: &dyn OpenApiRegistry,
    manage: Arc<ManageFacts>,
) -> Router {
    router = OperationBuilder::post(MANAGE_FACTS_PATH)
        .operation_id("construct.manage_facts")
        .summary("Store what the subject told an agent")
        .description(
            "Decide how what the subject told an agent changes the subject's profile, store it, and answer what \
             changed, or why nothing was stored. The recent conversation only shows what the message refers to. \
             A subject the caller may not act for answers 404.",
        )
        .tag(SUBJECT_FACTS_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .path_param("subject_id", "The subject: the caller's own subject id")
        .json_request::<dto::ManageFactsRequest>(openapi, "What the subject said")
        .handler(handlers::manage_facts)
        .json_response_with_schema::<dto::ManageFactsResponse>(
            openapi,
            StatusCode::OK,
            "What was stored, or why nothing was",
        )
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);
    router.layer(Extension(manage))
}
