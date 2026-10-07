use crate::api::rest::types::ConcreteIntake;
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

/// @cpt-dod:cpt-cf-construct-dod-record-intake-route:p1
pub fn register_routes(
    mut router: Router,
    openapi: &dyn OpenApiRegistry,
    intake: Arc<ConcreteIntake>,
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

    router.layer(Extension(intake))
}
