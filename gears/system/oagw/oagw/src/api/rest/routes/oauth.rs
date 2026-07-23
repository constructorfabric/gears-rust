use axum::Router;
use toolkit::api::OpenApiRegistry;
use toolkit::api::operation_builder::OperationBuilder;

use super::super::dto;
use super::super::handlers;
use super::License;

const API_TAG: &str = "OAGW OAuth";

// These endpoints are always registered regardless of `management_api_enabled`.
// That flag (`writable` elsewhere) gates CRUD on upstreams/routes — the
// GTS-provisioned entities — not the per-user OAuth enrollment flow. A user
// still needs to authorize, complete the callback, and revoke against upstreams
// that were provisioned via GTS with the management API disabled.
pub(super) fn register(mut router: Router, openapi: &dyn OpenApiRegistry) -> Router {
    // GET /oagw/v1/upstreams/{id}/oauth — Connection status (read-only)
    router = OperationBuilder::get("/oagw/v1/upstreams/{id}/oauth")
        .operation_id("oagw.oauth_connection_status")
        .summary("Get OAuth connection status")
        .description("Report whether the caller has a usable OAuth authorization for an upstream")
        .tag(API_TAG)
        .path_param("id", "Upstream GTS identifier")
        .authenticated()
        .require_license_features::<License>([])
        .handler(handlers::oauth::connection_status)
        .json_response_with_schema::<dto::OAuthConnectionStatusResponse>(
            openapi,
            http::StatusCode::OK,
            "OAuth connection status",
        )
        .standard_errors(openapi)
        .register(router, openapi);

    // POST /oagw/v1/upstreams/{id}/oauth/authorize — Begin authorization
    router = OperationBuilder::post("/oagw/v1/upstreams/{id}/oauth/authorize")
        .operation_id("oagw.begin_oauth_authorization")
        .summary("Begin OAuth authorization")
        .description(
            "Start an interactive authorization-code flow and return the browser URL + state",
        )
        .tag(API_TAG)
        .path_param("id", "Upstream GTS identifier")
        .authenticated()
        .require_license_features::<License>([])
        .json_request::<dto::BeginOAuthRequest>(openapi, "Authorization parameters")
        .handler(handlers::oauth::begin_authorization)
        .json_response_with_schema::<dto::BeginOAuthResponse>(
            openapi,
            http::StatusCode::OK,
            "Authorization URL and state",
        )
        .standard_errors(openapi)
        .register(router, openapi);

    // GET /oagw/v1/oauth/callback — Authorization-server redirect target
    // (public: the browser redirect carries no gateway credentials; the
    // acting identity is recovered from the CSRF `state`). Opened directly in
    // the user's browser, so it returns a self-contained HTML completion page
    // for both success and error redirects rather than a JSON/204 body.
    router = OperationBuilder::get("/oagw/v1/oauth/callback")
        .operation_id("oagw.oauth_callback")
        .summary("OAuth authorization callback")
        .description(
            "Authorization-server redirect target: on success, exchanges the authorization \
             code inside OAGW and persists the per-user token (the code never leaves OAGW); \
             on an error redirect (RFC 6749 §4.1.2.1) it completes the browser flow without \
             an exchange. Returns an HTML page describing the outcome.",
        )
        .tag(API_TAG)
        .query_param(
            "code",
            false,
            "Authorization code (present on a successful redirect)",
        )
        .query_param("state", true, "CSRF state from the begin step")
        .query_param(
            "error",
            false,
            "OAuth error code on a failed authorization (e.g. access_denied)",
        )
        .query_param(
            "error_description",
            false,
            "Optional human-readable description accompanying error",
        )
        .public()
        .handler(handlers::oauth::oauth_callback)
        .html_response(
            http::StatusCode::OK,
            "Browser page completing the authorization flow",
        )
        .no_content_response(
            http::StatusCode::FOUND,
            "Redirect to the allowlisted return_to URL (oauth=success|error appended)",
        )
        .standard_errors(openapi)
        .register(router, openapi);

    // DELETE /oagw/v1/upstreams/{id}/oauth — Revoke authorization
    router = OperationBuilder::delete("/oagw/v1/upstreams/{id}/oauth")
        .operation_id("oagw.revoke_oauth_authorization")
        .summary("Revoke OAuth authorization")
        .description("Delete the caller's stored OAuth token for an upstream")
        .tag(API_TAG)
        .path_param("id", "Upstream GTS identifier")
        .authenticated()
        .require_license_features::<License>([])
        .handler(handlers::oauth::revoke_authorization)
        .json_response(http::StatusCode::NO_CONTENT, "Authorization revoked")
        .standard_errors(openapi)
        .register(router, openapi);

    router
}
