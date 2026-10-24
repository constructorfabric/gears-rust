//! Route registration of the Policy Administration REST API.
//!
//! [`register_routes`] is the single entry point: it wires every operation
//! onto the platform's `OperationBuilder` and layers the
//! [`PolicyManagementClientV1`] every handler reads through an
//! `axum::Extension`. Every route is `.authenticated()`; there is no
//! anonymous route and no decision or evaluate endpoint.

use std::sync::Arc;

use axum::Router;
use axum::http::StatusCode;
use policy_engine_sdk::management::PolicyManagementClientV1;
use policy_engine_sdk::management::odata::{AssignmentFilterField, BundleFilterField};
use toolkit::api::OpenApiRegistry;
use toolkit::api::operation_builder::{OperationBuilder, OperationBuilderODataExt};

use super::dto::{
    PolicyEngineAssignmentDto, PolicyEngineBundleDto, PolicyEngineCreateAssignmentRequestDto,
    PolicyEngineCreateBundleRequestDto, PolicyEngineCreateDraftVersionRequestDto,
    PolicyEngineReplaceDraftContentRequestDto, PolicyEngineUpdateAssignmentRequestDto,
    PolicyEngineUpdateBundleRequestDto, PolicyEngineVersionDetailDto, PolicyEngineVersionDto,
};
use super::handlers;

/// `OpenAPI` tag of every route registered here.
const TAG: &str = "Policy Engine";

/// The `dry_run` query parameter of the publish operations.
const DRY_RUN: &str = "When `true`, run every check of the operation and write nothing: `204` when \
     it would succeed now, otherwise the problem it would return. Advisory: the operation \
     checks again when it runs.";

/// The `204` a passing dry run answers.
const DRY_RUN_PASSED: &str = "Dry run: the operation would succeed now";

/// Registers the whole Policy Administration REST API and returns `router`
/// with it attached. The only entry point of this module.
#[allow(clippy::too_many_lines)]
pub fn register_routes(
    router: Router,
    openapi: &dyn OpenApiRegistry,
    client: Arc<dyn PolicyManagementClientV1>,
) -> Router {
    // -- Bundles --------------------------------------------------------

    let router = OperationBuilder::post("/policy-engine/v1/bundles")
        .operation_id("policy_engine.create_bundle")
        .summary("Create a bundle")
        .description("Creates a bundle, with no version.")
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .json_request::<PolicyEngineCreateBundleRequestDto>(
            openapi,
            "Owning tenant, name and description",
        )
        .handler(handlers::create_bundle)
        .json_response_with_schema::<PolicyEngineBundleDto>(
            openapi,
            StatusCode::CREATED,
            "Bundle created",
        )
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);

    let router = OperationBuilder::get("/policy-engine/v1/bundles")
        .operation_id("policy_engine.list_bundles")
        .summary("List bundles for the scoped tenant")
        .description(
            "Lists the bundles of the tenants the caller may manage, opaque-cursor paginated. \
             Filter by `name` and `owner_tenant_id` (for example `name eq 'baseline' and \
             owner_tenant_id eq <tenant>`) to find a bundle by its name.",
        )
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .handler(handlers::list_bundles)
        .json_response_with_schema::<toolkit_odata::Page<PolicyEngineBundleDto>>(
            openapi,
            StatusCode::OK,
            "Page of bundles",
        )
        .with_odata_filter::<BundleFilterField>()
        .with_odata_orderby::<BundleFilterField>()
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);

    let router = OperationBuilder::get("/policy-engine/v1/bundles/{id}")
        .operation_id("policy_engine.get_bundle")
        .summary("Read a bundle")
        .description("Reads a bundle by identity.")
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .path_param("id", "Bundle identity")
        .handler(handlers::get_bundle)
        .json_response_with_schema::<PolicyEngineBundleDto>(openapi, StatusCode::OK, "The bundle")
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);

    let router = OperationBuilder::patch("/policy-engine/v1/bundles/{id}")
        .operation_id("policy_engine.update_bundle")
        .summary("Update a bundle")
        .description("Changes a bundle's name or description.")
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .path_param("id", "Bundle identity")
        .json_request::<PolicyEngineUpdateBundleRequestDto>(openapi, "New name or description")
        .handler(handlers::update_bundle)
        .json_response_with_schema::<PolicyEngineBundleDto>(
            openapi,
            StatusCode::OK,
            "The updated bundle",
        )
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);

    // -- Versions ---------------------------------------------------------

    let router = OperationBuilder::post("/policy-engine/v1/bundles/{id}/versions")
        .operation_id("policy_engine.create_draft_version")
        .summary("Create a draft version")
        .description(
            "Creates a draft version with the next ordinal: empty, or seeded from a \
             retained version of the same bundle.",
        )
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .path_param("id", "Bundle identity")
        .json_request::<PolicyEngineCreateDraftVersionRequestDto>(openapi, "Optional seed version")
        .handler(handlers::create_draft_version)
        .json_response_with_schema::<PolicyEngineVersionDto>(
            openapi,
            StatusCode::CREATED,
            "Draft created",
        )
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);

    let router = OperationBuilder::get("/policy-engine/v1/bundles/{id}/versions")
        .operation_id("policy_engine.list_versions")
        .summary("List a bundle's versions")
        .description(
            "Lists the retained versions of a bundle, newest ordinal first, without content, \
             opaque-cursor paginated.",
        )
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .path_param("id", "Bundle identity")
        .handler(handlers::list_versions)
        .json_response_with_schema::<toolkit_odata::Page<PolicyEngineVersionDto>>(
            openapi,
            StatusCode::OK,
            "Page of versions",
        )
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);

    let router = OperationBuilder::get("/policy-engine/v1/bundles/{id}/versions/{version}")
        .operation_id("policy_engine.get_version")
        .summary("Read a version with its documents")
        .description("Reads a version with its documents. Carries content.")
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .path_param("id", "Bundle identity")
        .path_param("version", "Version identity")
        .handler(handlers::get_version)
        .json_response_with_schema::<PolicyEngineVersionDetailDto>(
            openapi,
            StatusCode::OK,
            "The version and its documents",
        )
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);

    let router = OperationBuilder::put("/policy-engine/v1/bundles/{id}/versions/{version}")
        .operation_id("policy_engine.replace_draft_content")
        .summary("Replace a draft's content")
        .description("Replaces the whole content of a draft - every document - in one transaction.")
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .path_param("id", "Bundle identity")
        .path_param("version", "Version identity")
        .json_request::<PolicyEngineReplaceDraftContentRequestDto>(
            openapi,
            "The complete document set",
        )
        .handler(handlers::replace_draft_content)
        .json_response_with_schema::<PolicyEngineVersionDetailDto>(
            openapi,
            StatusCode::OK,
            "The replaced version and its documents",
        )
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);

    let router =
        OperationBuilder::post("/policy-engine/v1/bundles/{id}/versions/{version}/validate")
            .operation_id("policy_engine.validate_version")
            .summary("Validate a version without activating")
            .description(
                "Validates a version's content, in any state, with the checks activation \
             applies. Never activates, never changes the version. Invalid content is the \
             `VALIDATION_FAILED` problem activation would return, with every finding \
             against the document that caused it.",
            )
            .tag(TAG)
            .authenticated()
            .no_license_required()
            .path_param("id", "Bundle identity")
            .path_param("version", "Version identity")
            .handler(handlers::validate_version)
            .no_content_response(StatusCode::NO_CONTENT, "The content is valid")
            .standard_errors(openapi)
            .error_503(openapi)
            .register(router, openapi);

    let router =
        OperationBuilder::post("/policy-engine/v1/bundles/{id}/versions/{version}/activate")
            .operation_id("policy_engine.activate_version")
            .summary("Activate a validated draft")
            .description(
                "Activates a draft: validates it and supersedes the previously active \
             version in the same transaction. Activating the active version succeeds \
             without changes. With `dry_run=true`, runs every check and activates nothing.",
            )
            .tag(TAG)
            .authenticated()
            .no_license_required()
            .path_param("id", "Bundle identity")
            .path_param("version", "Version identity")
            .query_param_typed("dry_run", false, DRY_RUN, "boolean")
            .handler(handlers::activate_version)
            .json_response_with_schema::<PolicyEngineVersionDto>(
                openapi,
                StatusCode::OK,
                "The now-active version",
            )
            .no_content_response(StatusCode::NO_CONTENT, DRY_RUN_PASSED)
            .standard_errors(openapi)
            .error_503(openapi)
            .register(router, openapi);

    let router = OperationBuilder::delete("/policy-engine/v1/bundles/{id}/versions/{version}")
        .operation_id("policy_engine.delete_draft_version")
        .summary("Delete a draft version")
        .description(
            "Deletes a draft with its documents - the only deletion the lifecycle permits.",
        )
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .path_param("id", "Bundle identity")
        .path_param("version", "Version identity")
        .handler(handlers::delete_draft_version)
        .no_content_response(StatusCode::NO_CONTENT, "Draft deleted")
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);

    // -- Assignments --------------------------------------------------------

    let router = OperationBuilder::post("/policy-engine/v1/assignments")
        .operation_id("policy_engine.assign")
        .summary("Assign a bundle to a tenant")
        .description(
            "Assigns a bundle to a tenant; it governs through whichever version is active. \
             With `dry_run=true`, runs every check and assigns nothing.",
        )
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .query_param_typed("dry_run", false, DRY_RUN, "boolean")
        .json_request::<PolicyEngineCreateAssignmentRequestDto>(
            openapi,
            "Bundle, tenant and whether denials are enforced",
        )
        .handler(handlers::assign)
        .json_response_with_schema::<PolicyEngineAssignmentDto>(
            openapi,
            StatusCode::CREATED,
            "Assignment created",
        )
        .no_content_response(StatusCode::NO_CONTENT, DRY_RUN_PASSED)
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);

    let router = OperationBuilder::get("/policy-engine/v1/assignments/{id}")
        .operation_id("policy_engine.get_assignment")
        .summary("Read an assignment")
        .description("Reads an assignment by identity.")
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .path_param("id", "Assignment identity")
        .handler(handlers::get_assignment)
        .json_response_with_schema::<PolicyEngineAssignmentDto>(
            openapi,
            StatusCode::OK,
            "The assignment",
        )
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);

    let router = OperationBuilder::get("/policy-engine/v1/assignments")
        .operation_id("policy_engine.list_assignments")
        .summary("List assignments")
        .description(
            "Lists the assignments the caller administers and the dormant assignments at its \
             tenants it may withdraw, opaque-cursor paginated. Filter by `bundle_id` or by \
             `tenant_id` (the target); a filter naming content the caller cannot see yields \
             an empty page.",
        )
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .handler(handlers::list_assignments)
        .json_response_with_schema::<toolkit_odata::Page<PolicyEngineAssignmentDto>>(
            openapi,
            StatusCode::OK,
            "Page of assignments",
        )
        .with_odata_filter::<AssignmentFilterField>()
        .with_odata_orderby::<AssignmentFilterField>()
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);

    let router = OperationBuilder::patch("/policy-engine/v1/assignments/{id}")
        .operation_id("policy_engine.update_assignment")
        .summary("Update an assignment")
        .description(
            "Sets whether the assignment enforces its bundle's denials or only reports them. \
             With `dry_run=true`, runs every check and changes nothing.",
        )
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .path_param("id", "Assignment identity")
        .query_param_typed("dry_run", false, DRY_RUN, "boolean")
        .json_request::<PolicyEngineUpdateAssignmentRequestDto>(openapi, "The fields to change")
        .handler(handlers::update_assignment)
        .json_response_with_schema::<PolicyEngineAssignmentDto>(
            openapi,
            StatusCode::OK,
            "The updated assignment",
        )
        .no_content_response(StatusCode::NO_CONTENT, DRY_RUN_PASSED)
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);

    let router = OperationBuilder::delete("/policy-engine/v1/assignments/{id}")
        .operation_id("policy_engine.unassign")
        .summary("Remove an assignment")
        .description(
            "Withdraws an assignment; withdrawing one that does not exist succeeds. With \
             `dry_run=true`, runs every check and withdraws nothing.",
        )
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .path_param("id", "Assignment identity")
        .query_param_typed("dry_run", false, DRY_RUN, "boolean")
        .handler(handlers::unassign)
        .no_content_response(
            StatusCode::NO_CONTENT,
            "Assignment removed, or with `dry_run` would be",
        )
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);

    router.layer(axum::Extension(client))
}
