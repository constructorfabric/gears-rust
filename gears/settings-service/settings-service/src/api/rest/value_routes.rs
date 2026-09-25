// Created: 2026-09-07 by Virtuozzo International GmbH
//! Routes of the write surface.

use std::sync::Arc;

use axum::Router;
use toolkit::api::canonical_prelude::*;
use toolkit::api::operation_builder::{ParamSpec, ResponseHeaderSpec, ResponseHeaderType};
use toolkit::api::{OpenApiRegistry, OperationBuilder};

use crate::api::rest::value_dto::{
    BatchRequest, BatchResultDto, CloneRequest, FallbackResultDto, ImpactReportDto, ImpactRequest,
    PendingSecretDto, SetResultDto, SetValueRequest, StageSecretRequest, ValidateRequest,
    ValidationReportDto,
};
use crate::api::rest::value_handlers as handlers;
use crate::infra::value_writes::WriteCoordinator;

const TAG: &str = "settings-values";

fn if_match_param() -> ParamSpec {
    ParamSpec::header("If-Match").required(true).description(
        "The value state tag the caller last read for this scope, or `absent` when no row \
         existed; a moved value is refused 412",
    )
}

fn step_up_param() -> ParamSpec {
    ParamSpec::header("X-Step-Up-Token")
        .required(false)
        .description(
            "A fresh token from the identity provider proving the caller re-authenticated just \
         now; absent, the bearer token itself is checked. Required by declarations that \
         require step-up",
        )
}

fn tenant_param() -> ParamSpec {
    ParamSpec::query("tenant")
        .required(false)
        .description("Target tenant id; omitted, the caller's own tenant")
}

fn etag_header() -> ResponseHeaderSpec {
    ResponseHeaderSpec::new(
        "ETag",
        "The scope's new value state tag; the next write presents it in `If-Match`",
        ResponseHeaderType::String,
    )
}

/// Register the eight write operations.
#[allow(clippy::too_many_lines)]
pub fn register_routes(
    router: Router,
    openapi: &dyn OpenApiRegistry,
    writes: Arc<WriteCoordinator>,
    enforcer: Arc<authz_resolver_sdk::PolicyEnforcer>,
) -> Router {
    let router = OperationBuilder::put("/settings-service/v1/settings/{key}/value")
        .operation_id("settings_service.set_value")
        .summary("Set a setting's value at a scope")
        .description(
            "Store a value at the target scope, effective on the next read. Authorization is \
             decided first; a declaration that requires step-up then needs a fresh \
             re-authentication token, refused 401 with the RFC 9470 challenge when absent or \
             stale, and refuses a service principal outright. The value is validated against \
             the declaration's type, the write is guarded on `If-Match`, and the value and \
             its audit record commit in one transaction.",
        )
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .path_param("key", "The setting key, a URL-encoded GTS type id")
        .param(tenant_param())
        .param(if_match_param())
        .param(step_up_param())
        .json_request::<SetValueRequest>(openapi, "The value to store")
        .handler(handlers::set_value)
        .json_response_with_schema::<SetResultDto>(
            openapi,
            StatusCode::OK,
            "The committed change and its new tag",
        )
        .response_header(etag_header())
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_409(openapi)
        .problem_response(
            openapi,
            http::StatusCode::GONE,
            "Gone: the declaration is retired",
        )
        .problem_response(
            openapi,
            http::StatusCode::PRECONDITION_FAILED,
            "Precondition Failed: the value moved since it was read",
        )
        .problem_response(
            openapi,
            http::StatusCode::PRECONDITION_REQUIRED,
            "Precondition Required: If-Match is missing",
        )
        .error_500(openapi)
        .error_503(openapi)
        .register(router, openapi);

    let router = OperationBuilder::post("/settings-service/v1/settings/{key}/value/revert")
        .operation_id("settings_service.revert_value")
        .summary("Revert a setting's value at a scope")
        .description(
            "Clear the override at the target scope so it falls back - to the nearest ancestor \
             override for a cascading setting at a tenant, otherwise to the Schema Default, \
             which the revert never touches. Same gates and `If-Match` as a set; the response \
             carries the resulting effective value.",
        )
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .path_param("key", "The setting key, a URL-encoded GTS type id")
        .param(tenant_param())
        .param(if_match_param())
        .param(step_up_param())
        .handler(handlers::revert_value)
        .json_response_with_schema::<FallbackResultDto>(
            openapi,
            StatusCode::OK,
            "The change and what the scope resolves to now",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .problem_response(
            openapi,
            http::StatusCode::GONE,
            "Gone: the declaration is retired",
        )
        .problem_response(
            openapi,
            http::StatusCode::PRECONDITION_FAILED,
            "Precondition Failed",
        )
        .problem_response(
            openapi,
            http::StatusCode::PRECONDITION_REQUIRED,
            "Precondition Required",
        )
        .error_500(openapi)
        .error_503(openapi)
        .register(router, openapi);

    let router = OperationBuilder::delete("/settings-service/v1/settings/{key}/value")
        .operation_id("settings_service.remove_value")
        .summary("Remove a setting's value at a scope")
        .description(
            "Remove the scope's own row; resolution falls back exactly as after a revert. \
             Declaration removal is a separate, immediate soft-delete and is not reachable here.",
        )
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .path_param("key", "The setting key, a URL-encoded GTS type id")
        .param(tenant_param())
        .param(if_match_param())
        .param(step_up_param())
        .handler(handlers::remove_value)
        .json_response_with_schema::<FallbackResultDto>(
            openapi,
            StatusCode::OK,
            "The change and what the scope resolves to now",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .problem_response(
            openapi,
            http::StatusCode::GONE,
            "Gone: the declaration is retired",
        )
        .problem_response(
            openapi,
            http::StatusCode::PRECONDITION_FAILED,
            "Precondition Failed",
        )
        .problem_response(
            openapi,
            http::StatusCode::PRECONDITION_REQUIRED,
            "Precondition Required",
        )
        .error_500(openapi)
        .error_503(openapi)
        .register(router, openapi);

    let router = OperationBuilder::post("/settings-service/v1/settings/{key}/value/clone")
        .operation_id("settings_service.clone_value")
        .summary("Clone a setting's effective value from another scope")
        .description(
            "Copy the effective value resolved at `from` - the caller's own tenant when omitted \
             - as an explicit override at the target, with no continuing link. Both ends are \
             authorized and must lie within the caller's subtree; a secret-classified setting \
             is refused 400 with `secret_not_cloneable`.",
        )
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .path_param("key", "The setting key, a URL-encoded GTS type id")
        .param(tenant_param())
        .param(if_match_param())
        .param(step_up_param())
        .json_request::<CloneRequest>(openapi, "The source scope")
        .handler(handlers::clone_value)
        .json_response_with_schema::<SetResultDto>(
            openapi,
            StatusCode::OK,
            "The committed change at the target",
        )
        .response_header(etag_header())
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_409(openapi)
        .problem_response(
            openapi,
            http::StatusCode::GONE,
            "Gone: the declaration is retired",
        )
        .problem_response(
            openapi,
            http::StatusCode::PRECONDITION_FAILED,
            "Precondition Failed",
        )
        .problem_response(
            openapi,
            http::StatusCode::PRECONDITION_REQUIRED,
            "Precondition Required",
        )
        .error_500(openapi)
        .error_503(openapi)
        .register(router, openapi);

    let router = OperationBuilder::post("/settings-service/v1/settings/batch")
        .operation_id("settings_service.batch_set")
        .summary("Set several settings in one call")
        .description(
            "At most five hundred changes, each with its own key, target tenant, operation, \
             value and `if_match`. `op` is `set` (the default, so a client that never sends it \
             is unaffected) or `revert`, which clears the scope's own override and carries no \
             value; a key that is not a setting key, a `set` without a value, a `revert` with \
             one, or an unknown word rejects that change alone with `invalid`, and a `revert` of a scope holding no override \
             is rejected `not_found` alone. Step-up is verified once for the request when any \
             target declaration requires it; each change then commits on its own, with no \
             atomicity across changes, and the answer carries one entry per change - \
             committed with its new tag and its `operation`, or rejected with one of a fixed \
             vocabulary of codes: `invalid`, \
             `if_match_required`, `stale`, `conflict`, `forbidden`, `retired`, \
             `not_found`, `unavailable`, `error`. `if_match` is required in \
             effect - a change without one is rejected `if_match_required` on its own while \
             the rest proceed; a first write sends the literal `absent`. `retired` covers a \
             declaration retired after the change was assembled, since the check runs again \
             as each change commits. For a secret-trait setting the value may be \
             `{ \"pending_id\": \"...\" }`, naming a secret staged through `/secret-stage` \
             before the step-up redirect: the staged entry is adopted, and a token that is \
             unknown, expired, or was staged by another subject or for another setting or \
             tenant rejects that change `invalid`.",
        )
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .param(step_up_param())
        .json_request::<BatchRequest>(openapi, "The changes, in order")
        .handler(handlers::batch_set)
        .json_response_with_schema::<BatchResultDto>(
            openapi,
            StatusCode::OK,
            "One entry per change",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_500(openapi)
        .error_503(openapi)
        .register(router, openapi);

    let router = OperationBuilder::post("/settings-service/v1/settings/{key}/secret-stage")
        .operation_id("settings_service.stage_secret")
        .summary("Stage a secret ahead of the batch")
        .description(
            "Store a secret-trait value in the credential store before the caller leaves for \
             step-up, and answer with an opaque single-use token to send in place of the value \
             in the following batch. It exists because a browser cannot carry a secret's \
             plaintext across the identity provider's redirect: the token can cross it safely, \
             since only this service can resolve it and it expires within minutes. The entry is \
             created under this service's own principal, exactly as an ordinary secret write \
             creates it, so the no-reveal-path guarantee is unchanged and the credential-store \
             reference is never returned. Requires the same authorization a set requires and \
             no step-up, since nothing live changes until the batch commits. A non-secret \
             declaration is refused 400 with `not_a_secret`.",
        )
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .path_param("key", "The setting key, a URL-encoded GTS type id")
        .param(tenant_param())
        .json_request::<StageSecretRequest>(openapi, "The secret value to stage")
        .handler(handlers::stage_secret)
        .json_response_with_schema::<PendingSecretDto>(
            openapi,
            StatusCode::OK,
            "The token and its expiry",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .problem_response(
            openapi,
            http::StatusCode::GONE,
            "Gone: the declaration is retired",
        )
        .error_500(openapi)
        .error_503(openapi)
        .register(router, openapi);

    let router = OperationBuilder::post("/settings-service/v1/settings/{key}/validate")
        .operation_id("settings_service.validate_value")
        .summary("Check a value without storing it")
        .description(
            "Report whether the candidate would be accepted, with field-level detail, the \
             current effective value and its source at the target, and - for a cascading \
             setting - the descendants the change would affect, in pages. Read-only: needs no \
             step-up, stores nothing, emits no audit record, and is never required before a \
             write.",
        )
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .path_param("key", "The setting key, a URL-encoded GTS type id")
        .param(tenant_param())
        .json_request::<ValidateRequest>(openapi, "The candidate value")
        .handler(handlers::validate_value)
        .json_response_with_schema::<ValidationReportDto>(openapi, StatusCode::OK, "The report")
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .problem_response(
            openapi,
            http::StatusCode::GONE,
            "Gone: the declaration is retired",
        )
        .error_500(openapi)
        .error_503(openapi)
        .register(router, openapi);

    let router = OperationBuilder::post("/settings-service/v1/settings/{key}/impact")
        .operation_id("settings_service.impact")
        .summary("Report the cascading impact of a candidate value")
        .description(
            "Which descendants of the target would see a different effective value under the \
             candidate: the first `limit` in breadth-first order (default 100, at most 500), \
             the total, how many were scanned under the node budget of 5000, and whether the \
             report was truncated. Standalone descendants are omitted from the list and the \
             count; each listed descendant's current value is masked by the setting's \
             classification. A POST because the candidate travels in the body, as it does \
             for `validate` - a value may run to 64 KiB. Read-only, informational, and never \
             blocking a write.",
        )
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .path_param("key", "The setting key, a URL-encoded GTS type id")
        .param(tenant_param())
        .json_request::<ImpactRequest>(openapi, "The candidate value and the page size")
        .handler(handlers::impact)
        .json_response_with_schema::<ImpactReportDto>(openapi, StatusCode::OK, "The bounded report")
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .problem_response(
            openapi,
            http::StatusCode::GONE,
            "Gone: the declaration is retired",
        )
        .error_500(openapi)
        .error_503(openapi)
        .register(router, openapi);

    router
        .layer(axum::Extension(writes))
        .layer(axum::Extension(enforcer))
}

#[cfg(test)]
#[path = "value_routes_tests.rs"]
mod value_routes_tests;
