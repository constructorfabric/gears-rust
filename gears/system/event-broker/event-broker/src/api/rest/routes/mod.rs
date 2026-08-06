//! `OperationBuilder` route registration (`DESIGN.md:586`). Real ingest/
//! delivery registration (DTOs, `OpenAPI` schemas, `OperationBuilder`
//! chains) lands with #4346 - `register_ingest_routes`/
//! `register_delivery_routes` remain shells for that. `register_dispatcher_routes`
//! is implemented here (`eb-dispatcher-routing`): every route is an opaque
//! forward to a resolved ingest/delivery instance, so its declared response
//! is a placeholder - the authoritative contract lives with #4346's own
//! registration of the same paths. `summary` text is copied verbatim from
//! `DESIGN.md:1022-1041`'s Request Routing table's Purpose column, matching
//! what #4346's own registration of the same path is expected to say - only
//! `description` notes that this particular registration forwards rather
//! than executes the operation. License requirements for these routes are
//! left to whatever #4346 decides for the underlying endpoints; dispatcher
//! forwarding does not invent its own.
//!
//! Two routes register a bare `{id}` axum template rather than DESIGN.md's
//! literal `{id}:reset`/`{id}:seek`: matchit (axum's router, pinned to
//! `=0.8.4` even in the latest published axum release) cannot mix a path
//! parameter with literal text in one segment (design.md D13 - a git-pinned
//! axum fix was investigated and rejected; pulling in *only* the matchit fix
//! would mean vendoring a hand-patched fork). The real wire path is
//! unaffected: `{id}` captures the whole segment value including the colon
//! suffix, and the forwarded request is the client's original, untouched
//! `Request`, whose URI still carries it - see the comments at each
//! registration below.

use axum::Router;
use toolkit::api::{OpenApiRegistry, OperationBuilder, ResponseSpec};

use crate::infra::dispatcher::{proxy, router};

pub fn register_ingest_routes(router: Router) -> Router {
    router
}

pub fn register_delivery_routes(router: Router) -> Router {
    router
}

pub fn register_dispatcher_routes(mut router: Router, openapi: &dyn OpenApiRegistry) -> Router {
    router = register_ingest_forwarding_routes(router, openapi);
    router = register_delivery_forwarding_routes(router, openapi);
    register_shared_forwarding_routes(router, openapi)
}

/// A placeholder success response for an opaque proxy route: the real shape
/// is whatever the resolved instance returns, documented by #4346's own
/// registration of the same path.
fn proxied_response(status: axum::http::StatusCode) -> ResponseSpec {
    ResponseSpec {
        status: status.as_u16(),
        content_type: "application/json",
        description: "Proxied verbatim from the resolved instance - see the underlying \
                       operation's own documentation for the concrete response shape."
            .to_owned(),
        schema_name: None,
    }
}

/// The forwarding note appended via `.description()`, distinct from
/// `summary`, so the declared "what it does" text stays identical to the
/// underlying operation's own registration.
fn forwarded_to(role: &str) -> String {
    format!("Forwarded to the resolved {role} instance.")
}

const INGEST_TAG: &str = "Dispatcher (Ingest)";
const DELIVERY_TAG: &str = "Dispatcher (Delivery)";
const SHARED_TAG: &str = "Dispatcher (Shared)";

fn register_ingest_forwarding_routes(mut router: Router, openapi: &dyn OpenApiRegistry) -> Router {
    router = OperationBuilder::post("/event-broker/v1/events")
        .operation_id("event_broker.dispatcher.publish_event")
        .summary("Publish single event")
        .description(forwarded_to("ingest"))
        .tag(INGEST_TAG)
        .authenticated()
        .no_license_required()
        .handler(proxy::handle)
        .response(proxied_response(axum::http::StatusCode::OK))
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);

    router = OperationBuilder::post("/event-broker/v1/events:batch")
        .operation_id("event_broker.dispatcher.publish_batch")
        .summary("Publish batch of events")
        .description(forwarded_to("ingest"))
        .tag(INGEST_TAG)
        .authenticated()
        .no_license_required()
        .handler(proxy::handle)
        .response(proxied_response(axum::http::StatusCode::OK))
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);

    router = OperationBuilder::post("/event-broker/v1/producers")
        .operation_id("event_broker.dispatcher.register_producer")
        .summary("Register a producer (mint producer_id)")
        .description(forwarded_to("ingest"))
        .tag(INGEST_TAG)
        .authenticated()
        .no_license_required()
        .handler(proxy::handle)
        .response(proxied_response(axum::http::StatusCode::OK))
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);

    router = OperationBuilder::get("/event-broker/v1/producers/{id}/cursors")
        .operation_id("event_broker.dispatcher.get_producer_cursors")
        .summary("Read per-(topic,partition) last_sequence for desync recovery")
        .description(forwarded_to("ingest"))
        .tag(INGEST_TAG)
        .path_param("id", "Producer id")
        .authenticated()
        .no_license_required()
        .handler(proxy::handle)
        .response(proxied_response(axum::http::StatusCode::OK))
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);

    // Registered as bare `{id}` (see module doc comment above - matchit
    // limitation). `{id}` captures "xyz:reset" whole; the real `:reset` wire
    // path survives because `proxy::handle` forwards the untouched `Request`.
    router = OperationBuilder::post("/event-broker/v1/producers/{id}")
        .operation_id("event_broker.dispatcher.reset_producer")
        .summary("Operator-driven chain reset (preserves producer_id)")
        .description(forwarded_to("ingest"))
        .tag(INGEST_TAG)
        .path_param(
            "id",
            "Producer id, with the literal \":reset\" suffix (matchit limitation)",
        )
        .authenticated()
        .no_license_required()
        .handler(proxy::handle)
        .response(proxied_response(axum::http::StatusCode::OK))
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);

    router
}

fn register_delivery_forwarding_routes(
    mut router: Router,
    openapi: &dyn OpenApiRegistry,
) -> Router {
    router = OperationBuilder::get("/event-broker/v1/events:stream")
        .operation_id("event_broker.dispatcher.stream_events")
        .summary("Multipart event stream (long-lived, one event per part)")
        .description(forwarded_to("delivery"))
        .tag(DELIVERY_TAG)
        .authenticated()
        .no_license_required()
        .handler(router::handle)
        .response(proxied_response(axum::http::StatusCode::OK))
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);

    router = OperationBuilder::get("/event-broker/v1/events:sse")
        .operation_id("event_broker.dispatcher.sse_events")
        .summary("SSE event stream (opt-in, browser-native)")
        .description(forwarded_to("delivery"))
        .tag(DELIVERY_TAG)
        .authenticated()
        .no_license_required()
        .handler(router::handle)
        .response(proxied_response(axum::http::StatusCode::OK))
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);

    router = OperationBuilder::post("/event-broker/v1/consumer-groups")
        .operation_id("event_broker.dispatcher.create_consumer_group")
        .summary("Create anonymous consumer group (broker-minted id)")
        .description(forwarded_to("delivery"))
        .tag(DELIVERY_TAG)
        .authenticated()
        .no_license_required()
        .handler(router::handle)
        .response(proxied_response(axum::http::StatusCode::OK))
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);

    router = OperationBuilder::get("/event-broker/v1/consumer-groups")
        .operation_id("event_broker.dispatcher.list_consumer_groups")
        .summary("List consumer groups")
        .description(forwarded_to("delivery"))
        .tag(DELIVERY_TAG)
        .authenticated()
        .no_license_required()
        .handler(router::handle)
        .response(proxied_response(axum::http::StatusCode::OK))
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);

    router = OperationBuilder::get("/event-broker/v1/consumer-groups/{id}")
        .operation_id("event_broker.dispatcher.get_consumer_group")
        .summary("Read a consumer group")
        .description(forwarded_to("delivery"))
        .tag(DELIVERY_TAG)
        .path_param("id", "Consumer group id")
        .authenticated()
        .no_license_required()
        .handler(router::handle)
        .response(proxied_response(axum::http::StatusCode::OK))
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);

    router = OperationBuilder::delete("/event-broker/v1/consumer-groups/{id}")
        .operation_id("event_broker.dispatcher.delete_consumer_group")
        .summary("Delete a consumer group (only if no active members)")
        .description(forwarded_to("delivery"))
        .tag(DELIVERY_TAG)
        .path_param("id", "Consumer group id")
        .authenticated()
        .no_license_required()
        .handler(router::handle)
        .response(proxied_response(axum::http::StatusCode::NO_CONTENT))
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);

    router = OperationBuilder::post("/event-broker/v1/subscriptions")
        .operation_id("event_broker.dispatcher.join_subscription")
        .summary("JOIN - create subscription against a consumer group")
        .description(forwarded_to("delivery"))
        .tag(DELIVERY_TAG)
        .authenticated()
        .no_license_required()
        .handler(router::handle)
        .response(proxied_response(axum::http::StatusCode::OK))
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);

    router = OperationBuilder::get("/event-broker/v1/subscriptions")
        .operation_id("event_broker.dispatcher.list_subscriptions")
        .summary("List active subscriptions")
        .description(forwarded_to("delivery"))
        .tag(DELIVERY_TAG)
        .authenticated()
        .no_license_required()
        .handler(router::handle)
        .response(proxied_response(axum::http::StatusCode::OK))
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);

    router = OperationBuilder::get("/event-broker/v1/subscriptions/{id}")
        .operation_id("event_broker.dispatcher.get_subscription")
        .summary("Read a single subscription")
        .description(forwarded_to("delivery"))
        .tag(DELIVERY_TAG)
        .path_param("id", "Subscription id")
        .authenticated()
        .no_license_required()
        .handler(router::handle)
        .response(proxied_response(axum::http::StatusCode::OK))
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);

    router = OperationBuilder::delete("/event-broker/v1/subscriptions/{id}")
        .operation_id("event_broker.dispatcher.leave_subscription")
        .summary("LEAVE - terminate a subscription")
        .description(forwarded_to("delivery"))
        .tag(DELIVERY_TAG)
        .path_param("id", "Subscription id")
        .authenticated()
        .no_license_required()
        .handler(router::handle)
        .response(proxied_response(axum::http::StatusCode::NO_CONTENT))
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);

    // Registered as bare `{id}` - same matchit limitation and mechanism as
    // `/v1/producers/{id}` above.
    router = OperationBuilder::post("/event-broker/v1/subscriptions/{id}")
        .operation_id("event_broker.dispatcher.seek_subscription")
        .summary(
            "SEEK - set per-partition starting cursor; accepts integer offsets and sentinels \
             including \"at:<timestamp>\"",
        )
        .description(forwarded_to("delivery"))
        .tag(DELIVERY_TAG)
        .path_param(
            "id",
            "Subscription id, with the literal \":seek\" suffix (matchit limitation)",
        )
        .authenticated()
        .no_license_required()
        .handler(router::handle)
        .response(proxied_response(axum::http::StatusCode::OK))
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);

    router
}

fn register_shared_forwarding_routes(mut router: Router, openapi: &dyn OpenApiRegistry) -> Router {
    // Shared routes forward to the ingest service (design.md D2 - an
    // arbitrary but documented pick between two equally-valid targets).
    router = OperationBuilder::get("/event-broker/v1/topics")
        .operation_id("event_broker.dispatcher.list_topics")
        .summary("List topics")
        .description(forwarded_to("ingest"))
        .tag(SHARED_TAG)
        .authenticated()
        .no_license_required()
        .handler(proxy::handle)
        .response(proxied_response(axum::http::StatusCode::OK))
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);

    router = OperationBuilder::get("/event-broker/v1/topics/segments")
        .operation_id("event_broker.dispatcher.list_topic_segments")
        .summary("Get topic segment manifest for a (topic, partition)")
        .description(forwarded_to("ingest"))
        .tag(SHARED_TAG)
        .authenticated()
        .no_license_required()
        .handler(proxy::handle)
        .response(proxied_response(axum::http::StatusCode::OK))
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);

    router = OperationBuilder::get("/event-broker/v1/event-types")
        .operation_id("event_broker.dispatcher.list_event_types")
        .summary("List event types")
        .description(forwarded_to("ingest"))
        .tag(SHARED_TAG)
        .authenticated()
        .no_license_required()
        .handler(proxy::handle)
        .response(proxied_response(axum::http::StatusCode::OK))
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);

    router
}

#[cfg(test)]
#[path = "routes_tests.rs"]
mod routes_tests;
