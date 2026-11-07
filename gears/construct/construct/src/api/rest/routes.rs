use crate::api::rest::types::{ConcreteIntake, ConcreteProfile, ConcreteSettings};
use crate::api::rest::{dto, handlers};
use crate::domain::planner::manage::ManageFacts;
use axum::http::{StatusCode, header};
use axum::{Extension, Router};
use std::sync::Arc;
use toolkit::api::operation_builder::{CORE_GLOBAL_BASE_LICENSE_FEATURE, LicenseFeature};
use toolkit::api::{OpenApiRegistry, OperationBuilder};

const SUBJECT_SETTINGS_PATH: &str = "/construct/v1/subjects/{subject_id}/settings";
const SUBJECT_SETTINGS_TAG: &str = "Subject settings";
const MANAGE_FACTS_PATH: &str = "/construct/v1/subjects/{subject_id}/facts/manage";
const SUBJECT_FACTS_TAG: &str = "Subject facts";
const PROFILE_PATH: &str = "/construct/v1/subjects/{subject_id}/profile";

struct License;

impl AsRef<str> for License {
    fn as_ref(&self) -> &'static str {
        CORE_GLOBAL_BASE_LICENSE_FEATURE
    }
}

impl LicenseFeature for License {}

/// @cpt-dod:cpt-cf-construct-dod-record-intake-route:p1
pub fn register_routes(
    router: Router,
    openapi: &dyn OpenApiRegistry,
    intake: Arc<ConcreteIntake>,
    settings: Arc<ConcreteSettings>,
    profile: Arc<ConcreteProfile>,
) -> Router {
    // @cpt-begin:cpt-cf-construct-flow-record-intake-submit:p1:inst-submit-send
    let records = OperationBuilder::post("/construct/v1/records")
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
        .register(Router::new(), openapi);
    // @cpt-end:cpt-cf-construct-flow-record-intake-submit:p1:inst-submit-send

    // Every route about one subject answers with `no-store` and the
    // security headers, as the record route does.
    let mut subject = Router::new();

    // @cpt-begin:cpt-cf-construct-flow-subject-settings-view:p1:inst-view-send
    subject = OperationBuilder::get(SUBJECT_SETTINGS_PATH)
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
        .register(subject, openapi);
    // @cpt-end:cpt-cf-construct-flow-subject-settings-view:p1:inst-view-send

    // @cpt-begin:cpt-cf-construct-flow-subject-settings-change:p1:inst-change-send
    subject = OperationBuilder::put(SUBJECT_SETTINGS_PATH)
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
        .register(subject, openapi);
    // @cpt-end:cpt-cf-construct-flow-subject-settings-change:p1:inst-change-send

    // @cpt-begin:cpt-cf-construct-flow-profile-reader-read:p1:inst-profile-send
    subject = OperationBuilder::get(PROFILE_PATH)
        .operation_id("construct.get_profile")
        .summary("Read a subject's profile")
        .description(
            "Read the facts of a subject by category, for a caller the platform lets act for \
             the subject: the subject itself, or an application or agent acting for it. The \
             tenant is the caller's own, or the one named in `tenant`. A category the caller \
             may not read is not listed, like one without facts; nothing is listed while \
             personalization is off or an erasure is under way. A subject the caller may not \
             act for is answered with 404, like one that does not exist.",
        )
        .tag("Profiles")
        .authenticated()
        .require_license_features::<License>([])
        .path_param("subject_id", "The subject whose profile is read")
        .query_param_typed(
            "tenant",
            false,
            "Tenant of the subject, when not the caller's own",
            "string",
        )
        .handler(handlers::get_profile)
        .json_response_with_schema::<dto::ProfileDto>(
            openapi,
            StatusCode::OK,
            "The subject's profile by category",
        )
        .standard_errors(openapi)
        .error_503(openapi)
        .problem_response(
            openapi,
            StatusCode::NOT_IMPLEMENTED,
            "The deployment does not provide the profile read",
        )
        .register(subject, openapi);
    // @cpt-end:cpt-cf-construct-flow-profile-reader-read:p1:inst-profile-send

    router
        .merge(not_stored(records))
        .merge(not_stored(subject))
        .layer(Extension(intake))
        .layer(Extension(settings))
        .layer(Extension(profile))
}

/// The synchronous manage-facts route. The gear registers it together with the planner chain it runs on.
///
/// @cpt-dod:cpt-cf-construct-dod-planner-manage-route:p1
pub fn register_manage_route(
    router: Router,
    openapi: &dyn OpenApiRegistry,
    manage: Arc<ManageFacts>,
) -> Router {
    let facts = OperationBuilder::post(MANAGE_FACTS_PATH)
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
        .register(Router::new(), openapi);
    router.merge(not_stored(facts)).layer(Extension(manage))
}

/// `routes` with every answer, errors included, marked
/// `Cache-Control: no-store`, so no proxy or browser cache keeps one
/// subject's data, and with the response headers OWASP names for a JSON API:
/// no content sniffing, no framing, no content to run, no referrer, no
/// browser features, no cross-origin embedding, no cross-domain policy files
/// and no DNS prefetch. HSTS belongs to the TLS ingress. Every route
/// Construct registers gets the layer, and only they do: the router they are
/// merged into holds other gears' routes too.
fn not_stored(routes: Router) -> Router {
    routes.layer(axum::middleware::map_response(
        |mut response: axum::response::Response| async move {
            let headers = response.headers_mut();
            for (name, value) in [
                (header::CACHE_CONTROL, "no-store"),
                (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
                (header::X_FRAME_OPTIONS, "DENY"),
                (
                    header::CONTENT_SECURITY_POLICY,
                    "default-src 'none'; frame-ancestors 'none'",
                ),
                (header::REFERRER_POLICY, "no-referrer"),
                (
                    header::HeaderName::from_static("permissions-policy"),
                    "camera=(), microphone=(), geolocation=(), interest-cohort=()",
                ),
                (
                    header::HeaderName::from_static("cross-origin-resource-policy"),
                    "same-origin",
                ),
                (
                    header::HeaderName::from_static("x-permitted-cross-domain-policies"),
                    "none",
                ),
                (header::X_DNS_PREFETCH_CONTROL, "off"),
            ] {
                headers.insert(name, header::HeaderValue::from_static(value));
            }
            response
        },
    ))
}
