//! Route registration for the Chat Engine REST surface.
//!
//! Every endpoint listed in DESIGN §API and `api/http-protocol.json` is
//! wired up here via [`OperationBuilder`]. The function
//! [`register_routes`] is the single mounting point — `module.rs` calls
//! it once, then the gateway owns the rest (request tracing, CORS,
//! body limits, OpenAPI). No raw `Router::route` calls exist in this
//! module.
//!
//! `operation_id` naming follows the `chat_engine.<resource>.<action>`
//! convention from `docs/modkit_unified_system/04_rest_operation_builder.md`.
//!
//! Service DI is attached **once** at the end via `Extension<Arc<…>>`
//! layers, matching the ModKit reference wiring.
//
// @cpt-cf-chat-engine-api-rest-routes:p14
// @cpt-cf-chat-engine-adr-http-client-protocol:p14

use std::sync::Arc;

use axum::{Extension, Router};
use http::StatusCode;
use modkit::api::operation_builder::{LicenseFeature, OperationBuilder};
use modkit::api::OpenApiRegistry;

use crate::api::rest::dto::{
    CreateSessionRequestDto, ExportAcceptedDto, MessageDto, MessageListDto, ReactionListDto,
    ReactionRequestDto, RecreateMessageRequestDto, SearchRequestDto, SearchResultsDto,
    SendMessageRequestDto, SessionDto, ShareRequestDto, ShareResponseDto, SharedSessionDto,
    StreamingEventDto, SummarizeAcceptedDto, SwitchSessionTypeRequestDto, VariantListDto,
};
use crate::api::rest::handlers;
use crate::api::rest::WebhookEmitter;
use crate::domain::service::{
    ExportService, IntelligenceService, MessageService, ReactionService, SearchService,
    SessionService, VariantService,
};

/// API tag used by every Chat Engine endpoint in the generated OpenAPI
/// document.
const API_TAG: &str = "Chat Engine";

/// License feature required by all `cf-chat-engine` endpoints.
///
/// Mirrors the gating policy used by sibling modules
/// (`gts.cf.core.lic.feat.v1~cf.core.global.base.v1`).
pub(crate) struct ChatEngineLicense;

impl AsRef<str> for ChatEngineLicense {
    fn as_ref(&self) -> &'static str {
        "gts.cf.core.lic.feat.v1~cf.core.global.base.v1"
    }
}

impl LicenseFeature for ChatEngineLicense {}

/// Aggregated service handle attached to every authenticated route via
/// `Extension`. Phase 15 owns the constructor; Phase 14 only requires the
/// type signature so the route wiring compiles end-to-end.
#[derive(Clone)]
pub struct ChatEngineServices {
    pub sessions: Arc<SessionService>,
    pub messages: Arc<MessageService>,
    pub variants: Arc<VariantService>,
    pub reactions: Arc<ReactionService>,
    pub search: Arc<SearchService>,
    pub intelligence: Arc<IntelligenceService>,
    pub export: Arc<ExportService>,
}

/// Mount the Chat Engine REST surface onto the gateway-supplied `router`.
///
/// The function follows the ModKit pattern verbatim:
///
/// - `OperationBuilder::<verb>(path)` chain per endpoint.
/// - `.authenticated()` + `.require_license_features([&ChatEngineLicense])`
///   on every protected route (the only public route is
///   `POST /chat-engine/v1/shared/{share_token}` which uses `.public()`).
/// - `.json_response_with_schema::<…>(openapi, status, desc)` for typed
///   responses; `.json_request::<…>(openapi, desc)` for typed bodies.
/// - `.standard_errors(openapi)` registers the RFC-9457 error variants.
/// - Per-service `Extension` layers attached once at the end.
pub fn register_routes(
    router: Router,
    openapi: &dyn OpenApiRegistry,
    services: ChatEngineServices,
    webhooks: Arc<dyn WebhookEmitter>,
    enable_search: bool,
) -> Router {
    let mut router = router;

    // -------------------------------------------------------------------
    // Session lifecycle
    // -------------------------------------------------------------------

    router = OperationBuilder::post("/chat-engine/v1/sessions")
        .operation_id("chat_engine.session.create")
        .summary("Create a chat session")
        .tag(API_TAG)
        .authenticated()
        .require_license_features([&ChatEngineLicense])
        .json_request::<CreateSessionRequestDto>(openapi, "Session creation parameters")
        .handler(handlers::sessions::create_session)
        .json_response_with_schema::<SessionDto>(openapi, StatusCode::CREATED, "Created session")
        .standard_errors(openapi)
        .register(router, openapi);

    router = OperationBuilder::get("/chat-engine/v1/sessions/{id}")
        .operation_id("chat_engine.session.get")
        .summary("Get a session")
        .tag(API_TAG)
        .authenticated()
        .require_license_features([&ChatEngineLicense])
        .path_param("id", "Session UUID")
        .handler(handlers::sessions::get_session)
        .json_response_with_schema::<SessionDto>(openapi, StatusCode::OK, "Session details")
        .standard_errors(openapi)
        .register(router, openapi);

    router = OperationBuilder::delete("/chat-engine/v1/sessions/{id}")
        .operation_id("chat_engine.session.delete")
        .summary("Delete a session (soft or hard)")
        .tag(API_TAG)
        .authenticated()
        .require_license_features([&ChatEngineLicense])
        .path_param("id", "Session UUID")
        .query_param_typed("hard", false, "When true, perform a cascading hard delete", "boolean")
        .handler(handlers::sessions::delete_session)
        .json_response_with_schema::<SessionDto>(openapi, StatusCode::OK, "Soft-deleted session")
        .no_content_response(StatusCode::NO_CONTENT, "Hard delete completed")
        .standard_errors(openapi)
        .register(router, openapi);

    router = OperationBuilder::post("/chat-engine/v1/sessions/{id}/switch-type")
        .operation_id("chat_engine.session.switch_type")
        .summary("Switch the session type of an existing session")
        .tag(API_TAG)
        .authenticated()
        .require_license_features([&ChatEngineLicense])
        .path_param("id", "Session UUID")
        .json_request::<SwitchSessionTypeRequestDto>(openapi, "Target session type")
        .handler(handlers::variants::switch_session_type)
        .json_response_with_schema::<SessionDto>(openapi, StatusCode::OK, "Updated session")
        .standard_errors(openapi)
        .register(router, openapi);

    // -------------------------------------------------------------------
    // Export / Share
    // -------------------------------------------------------------------

    router = OperationBuilder::post("/chat-engine/v1/sessions/{id}/export")
        .operation_id("chat_engine.session.export")
        .summary("Export a session (returns a download URL)")
        .tag(API_TAG)
        .authenticated()
        .require_license_features([&ChatEngineLicense])
        .path_param("id", "Session UUID")
        .query_param_typed(
            "format",
            false,
            "Export format (`json` or `markdown`)",
            "string",
        )
        .query_param_typed(
            "include_plugin_metadata",
            false,
            "Include plugin-defined per-message metadata in the export",
            "boolean",
        )
        .handler(handlers::export::export_session)
        .json_response_with_schema::<ExportAcceptedDto>(
            openapi,
            StatusCode::ACCEPTED,
            "Export accepted",
        )
        .standard_errors(openapi)
        .register(router, openapi);

    router = OperationBuilder::post("/chat-engine/v1/sessions/{id}/share")
        .operation_id("chat_engine.session.share")
        .summary("Generate a share link for a session")
        .tag(API_TAG)
        .authenticated()
        .require_license_features([&ChatEngineLicense])
        .path_param("id", "Session UUID")
        .json_request::<ShareRequestDto>(openapi, "Share options")
        .handler(handlers::export::create_share)
        .json_response_with_schema::<ShareResponseDto>(
            openapi,
            StatusCode::CREATED,
            "Share link issued",
        )
        .standard_errors(openapi)
        .register(router, openapi);

    router = OperationBuilder::post("/chat-engine/v1/shared/{share_token}")
        .operation_id("chat_engine.session.access_shared")
        .summary("Access a session via a public share token")
        .tag(API_TAG)
        .public()
        .path_param("share_token", "Opaque share token (bearer secret)")
        .handler(handlers::export::access_shared)
        .json_response_with_schema::<SharedSessionDto>(
            openapi,
            StatusCode::OK,
            "Shared session payload",
        )
        // The handler maps share-expired conflicts to 410 Gone via
        // `map_share_error`; document the response explicitly so
        // clients / codegen know about it.
        .problem_response(
            openapi,
            StatusCode::GONE,
            "Share token has expired or been revoked",
        )
        .standard_errors(openapi)
        .register(router, openapi);

    // -------------------------------------------------------------------
    // Search
    //
    // Gated by `enable_search`: the production tsvector/LIKE backends are
    // still stubs that return `Internal` on every call, so registering
    // these routes by default would advertise an endpoint that 500s on
    // every real request. Operators flip the config flag once a real
    // backend lands.
    // -------------------------------------------------------------------

    if enable_search {
        router = OperationBuilder::post("/chat-engine/v1/sessions/{id}/search")
            .operation_id("chat_engine.session.search")
            .summary("Search inside a single session")
            .tag(API_TAG)
            .authenticated()
            .require_license_features([&ChatEngineLicense])
            .path_param("id", "Session UUID")
            .json_request::<SearchRequestDto>(openapi, "Search query")
            .handler(handlers::glue::search_in_session)
            .json_response_with_schema::<SearchResultsDto>(
                openapi,
                StatusCode::OK,
                "Search results",
            )
            .standard_errors(openapi)
            .register(router, openapi);

        router = OperationBuilder::post("/chat-engine/v1/sessions/search")
            .operation_id("chat_engine.sessions.search")
            .summary("Search across all sessions for the current user")
            .tag(API_TAG)
            .authenticated()
            .require_license_features([&ChatEngineLicense])
            .json_request::<SearchRequestDto>(openapi, "Search query")
            .handler(handlers::glue::search_across_sessions)
            .json_response_with_schema::<SearchResultsDto>(
                openapi,
                StatusCode::OK,
                "Search results",
            )
            .standard_errors(openapi)
            .register(router, openapi);
    }

    // -------------------------------------------------------------------
    // Summarize (202 Accepted)
    // -------------------------------------------------------------------

    router = OperationBuilder::post("/chat-engine/v1/sessions/{id}/summarize")
        .operation_id("chat_engine.session.summarize")
        .summary("Trigger an asynchronous session summary")
        .tag(API_TAG)
        .authenticated()
        .require_license_features([&ChatEngineLicense])
        .path_param("id", "Session UUID")
        .handler(handlers::glue::summarize_session)
        .json_response_with_schema::<SummarizeAcceptedDto>(
            openapi,
            StatusCode::ACCEPTED,
            "Summary accepted (use status URL to poll)",
        )
        .standard_errors(openapi)
        .register(router, openapi);

    // -------------------------------------------------------------------
    // Messages
    // -------------------------------------------------------------------

    router = OperationBuilder::post("/chat-engine/v1/sessions/{id}/messages")
        .operation_id("chat_engine.message.send")
        .summary("Send a message and stream the assistant response as NDJSON")
        .tag(API_TAG)
        .authenticated()
        .require_license_features([&ChatEngineLicense])
        .path_param("id", "Session UUID")
        .json_request::<SendMessageRequestDto>(openapi, "Message payload")
        .handler(handlers::glue::send_message_in_session)
        .json_response_with_schema::<StreamingEventDto>(
            openapi,
            StatusCode::OK,
            "NDJSON stream of StreamingEventDto (application/x-ndjson)",
        )
        .standard_errors(openapi)
        .register(router, openapi);

    router = OperationBuilder::get("/chat-engine/v1/sessions/{id}/messages")
        .operation_id("chat_engine.message.list")
        .summary("List messages on the active path of a session")
        .tag(API_TAG)
        .authenticated()
        .require_license_features([&ChatEngineLicense])
        .path_param("id", "Session UUID")
        .query_param_typed(
            "parent_message_id",
            false,
            "Optional parent message UUID for partial listings",
            "string",
        )
        .handler(handlers::glue::list_messages)
        .json_response_with_schema::<MessageListDto>(openapi, StatusCode::OK, "Message list")
        .standard_errors(openapi)
        .register(router, openapi);

    router = OperationBuilder::get("/chat-engine/v1/messages/{id}")
        .operation_id("chat_engine.message.get")
        .summary("Get a single message")
        .tag(API_TAG)
        .authenticated()
        .require_license_features([&ChatEngineLicense])
        .path_param("id", "Message UUID")
        .handler(handlers::glue::get_message)
        .json_response_with_schema::<MessageDto>(openapi, StatusCode::OK, "Message details")
        .standard_errors(openapi)
        .register(router, openapi);

    router = OperationBuilder::delete("/chat-engine/v1/messages/{id}")
        .operation_id("chat_engine.message.delete")
        .summary("Delete a message and its descendants (cascade)")
        .tag(API_TAG)
        .authenticated()
        .require_license_features([&ChatEngineLicense])
        .path_param("id", "Message UUID")
        .handler(handlers::messages::delete_message)
        .json_response_with_schema::<MessageDto>(openapi, StatusCode::OK, "Cascade deletion result")
        .standard_errors(openapi)
        .register(router, openapi);

    router = OperationBuilder::post("/chat-engine/v1/messages/{id}/recreate")
        .operation_id("chat_engine.message.recreate")
        .summary("Recreate an assistant variant (NDJSON stream)")
        .tag(API_TAG)
        .authenticated()
        .require_license_features([&ChatEngineLicense])
        .path_param("id", "Message UUID")
        .json_request::<RecreateMessageRequestDto>(openapi, "Recreate options")
        .handler(handlers::glue::recreate_message)
        .json_response_with_schema::<StreamingEventDto>(
            openapi,
            StatusCode::OK,
            "NDJSON stream of StreamingEventDto (application/x-ndjson)",
        )
        .standard_errors(openapi)
        .register(router, openapi);

    router = OperationBuilder::get("/chat-engine/v1/messages/{id}/variants")
        .operation_id("chat_engine.message.variants")
        .summary("List variants for a message")
        .tag(API_TAG)
        .authenticated()
        .require_license_features([&ChatEngineLicense])
        .path_param("id", "Message UUID")
        .handler(handlers::variants::list_variants)
        .json_response_with_schema::<VariantListDto>(openapi, StatusCode::OK, "Variant list")
        .standard_errors(openapi)
        .register(router, openapi);

    router = OperationBuilder::post("/chat-engine/v1/messages/{id}/reactions")
        .operation_id("chat_engine.message.react")
        .summary("Set or update a reaction on a message")
        .tag(API_TAG)
        .authenticated()
        .require_license_features([&ChatEngineLicense])
        .path_param("id", "Message UUID")
        .json_request::<ReactionRequestDto>(openapi, "Reaction payload")
        .handler(handlers::glue::set_reaction)
        .json_response_with_schema::<ReactionListDto>(
            openapi,
            StatusCode::OK,
            "Updated reaction list",
        )
        .standard_errors(openapi)
        .register(router, openapi);

    // -------------------------------------------------------------------
    // Service & webhook DI attached once at the end.
    // -------------------------------------------------------------------

    router
        .layer(Extension(services.sessions))
        .layer(Extension(services.messages))
        .layer(Extension(services.variants))
        .layer(Extension(services.reactions))
        .layer(Extension(services.search))
        .layer(Extension(services.intelligence))
        .layer(Extension(services.export))
        .layer(Extension(webhooks))
}
