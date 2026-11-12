//! The `OpenAPI` operations `register_ingest_routes` declares, read back through
//! a registry that records every registered `OperationSpec`.

use axum::Router;
use toolkit::api::operation_builder::RouteTimeout;

use crate::config::checks::{DISPATCHER_PUBLISH_ROUTE_TIMEOUT_SECS, PUBLISH_ROUTE_TIMEOUT_SECS};

use super::route_spec_recorder::{RecordingOpenApiRegistry, declared_responses};
use super::{register_delivery_routes, register_dispatcher_routes, register_ingest_routes};

/// The declared request timeout of the single operation registered on `path`.
fn declared_timeout(registry: &RecordingOpenApiRegistry, path: &str) -> RouteTimeout {
    let operations = registry.operations.lock().expect("operations lock");
    let [operation] = operations
        .iter()
        .filter(|operation| operation.path == path)
        .collect::<Vec<_>>()[..]
    else {
        panic!("expected one operation on {path}");
    };
    operation.request_timeout
}

#[test]
fn ingest_publish_routes_declare_the_publish_timeout() {
    use std::time::Duration;

    let registry = RecordingOpenApiRegistry::default();
    let _router = register_ingest_routes(Router::new(), &registry);

    // Both publish routes declare the 60s deadline (so a `Prefer: wait=N` hold
    // stays inside it); other routes inherit the gateway default.
    let publish = RouteTimeout::Custom(Duration::from_secs(PUBLISH_ROUTE_TIMEOUT_SECS));
    assert_eq!(
        declared_timeout(&registry, "/event-broker/v1/events"),
        publish
    );
    assert_eq!(
        declared_timeout(&registry, "/event-broker/v1/events:batch"),
        publish
    );
    assert_eq!(
        declared_timeout(&registry, "/event-broker/v1/topics"),
        RouteTimeout::Inherit
    );
}

#[test]
fn delivery_stream_routes_disable_the_request_timeout() {
    let registry = RecordingOpenApiRegistry::default();
    let _router = register_delivery_routes(Router::new(), &registry);

    // Long-lived streaming routes must never be cut by the unary deadline.
    assert_eq!(
        declared_timeout(&registry, "/event-broker/v1/events:stream"),
        RouteTimeout::Disabled
    );
    assert_eq!(
        declared_timeout(&registry, "/event-broker/v1/events:sse"),
        RouteTimeout::Disabled
    );
}

#[test]
fn dispatcher_forwarding_routes_declare_the_cluster_timeouts() {
    use std::time::Duration;

    let registry = RecordingOpenApiRegistry::default();
    let _router = register_dispatcher_routes(Router::new(), &registry);

    // A cluster publish crosses two gateways, so the dispatcher's outer
    // deadline (75s) must exceed the upstream ingest deadline (60s).
    let dispatcher_publish =
        RouteTimeout::Custom(Duration::from_secs(DISPATCHER_PUBLISH_ROUTE_TIMEOUT_SECS));
    assert_eq!(
        declared_timeout(&registry, "/event-broker/v1/events"),
        dispatcher_publish
    );
    assert_eq!(
        declared_timeout(&registry, "/event-broker/v1/events:batch"),
        dispatcher_publish
    );

    // Forwarded streaming routes stay deadline-free, as on the ingest/delivery
    // hosts themselves.
    assert_eq!(
        declared_timeout(&registry, "/event-broker/v1/events:stream"),
        RouteTimeout::Disabled
    );
    assert_eq!(
        declared_timeout(&registry, "/event-broker/v1/events:sse"),
        RouteTimeout::Disabled
    );
}

#[test]
fn the_single_publish_route_declares_its_full_response_list() {
    let registry = RecordingOpenApiRegistry::default();
    let _router = register_ingest_routes(Router::new(), &registry);

    assert_eq!(
        declared_responses(&registry, "/event-broker/v1/events"),
        vec![
            (
                201,
                "",
                "Stored - `Prefer: wait=N` was satisfied: the backend has the event".to_owned()
            ),
            (
                202,
                "",
                "Accepted, durably enqueued in the outbox (or `Prefer: wait=N` elapsed first)"
                    .to_owned()
            ),
            (
                200,
                "",
                "Idempotent duplicate - the event is already durable; nothing is written"
                    .to_owned()
            ),
            (
                412,
                "application/problem+json",
                "SequenceViolation - the event neither advances nor duplicates the producer \
                 chain's committed `last_sequence`."
                    .to_owned()
            ),
            (
                413,
                "application/problem+json",
                "EventTooLarge - the event's encoding is over the 60 KiB per-event limit; or \
                 Payload Too Large - the request body is over the request body limit \
                 (api-gateway `defaults.body_limit_bytes`)."
                    .to_owned()
            ),
            (
                422,
                "application/problem+json",
                "SchemaViolation - the event `data` does not satisfy the event type's \
                 `data_schema`."
                    .to_owned()
            ),
            (
                503,
                "application/problem+json",
                "ServiceUnavailable, retryable - the topic is not yet provisioned, the broker's \
                 database or ingest pipeline is unavailable (including a producer-chain write \
                 that exhausted its contention retries), or no ingest instance is reachable."
                    .to_owned()
            ),
            (400, "application/problem+json", "Bad Request".to_owned()),
            (401, "application/problem+json", "Unauthorized".to_owned()),
            (403, "application/problem+json", "Forbidden".to_owned()),
            (404, "application/problem+json", "Not Found".to_owned()),
            (409, "application/problem+json", "Conflict".to_owned()),
            (
                429,
                "application/problem+json",
                "Too Many Requests".to_owned()
            ),
            (
                500,
                "application/problem+json",
                "Internal Server Error".to_owned()
            ),
        ]
    );
}

#[test]
fn the_batch_route_declares_its_full_response_list() {
    let registry = RecordingOpenApiRegistry::default();
    let _router = register_ingest_routes(Router::new(), &registry);

    assert_eq!(
        declared_responses(&registry, "/event-broker/v1/events:batch"),
        vec![
            (
                201,
                "",
                "Stored - `Prefer: wait=N` was satisfied: the backend has every event".to_owned()
            ),
            (
                202,
                "",
                "Accepted (or `Prefer: wait=N` elapsed first)".to_owned()
            ),
            (
                200,
                "",
                "Every event of the batch is a duplicate; nothing is written".to_owned()
            ),
            (
                412,
                "application/problem+json",
                "SequenceViolation - atomic batch rejection: a run neither advances nor \
                 duplicates its chain's committed `last_sequence`. Nothing is enqueued."
                    .to_owned()
            ),
            (
                413,
                "application/problem+json",
                "BatchTooLarge (more events than the configured max) or BatchPayloadTooLarge \
                 (total payload over the configured byte ceiling), or EventTooLarge (an event's \
                 encoding over the 60 KiB per-event limit), or Payload Too Large (the request \
                 body over the request body limit, api-gateway `defaults.body_limit_bytes`)"
                    .to_owned()
            ),
            (
                422,
                "application/problem+json",
                "SchemaViolation - an event's `data` does not satisfy its event type's \
                 `data_schema` (atomic batch rejection)"
                    .to_owned()
            ),
            (
                503,
                "application/problem+json",
                "ServiceUnavailable, retryable - the batch's topic is not yet provisioned, the \
                 broker's database or ingest pipeline is unavailable (including a producer-chain \
                 write that exhausted its contention retries), or no ingest instance is \
                 reachable. Nothing is enqueued."
                    .to_owned()
            ),
            (400, "application/problem+json", "Bad Request".to_owned()),
            (401, "application/problem+json", "Unauthorized".to_owned()),
            (403, "application/problem+json", "Forbidden".to_owned()),
            (404, "application/problem+json", "Not Found".to_owned()),
            (409, "application/problem+json", "Conflict".to_owned()),
            (
                429,
                "application/problem+json",
                "Too Many Requests".to_owned()
            ),
            (
                500,
                "application/problem+json",
                "Internal Server Error".to_owned()
            ),
        ]
    );
}

#[test]
fn the_producer_register_route_declares_its_full_response_list() {
    let registry = RecordingOpenApiRegistry::default();
    let _router = register_ingest_routes(Router::new(), &registry);

    assert_eq!(
        declared_responses(&registry, "/event-broker/v1/producers"),
        vec![
            (
                201,
                "application/json",
                "Producer registered; id minted".to_owned()
            ),
            (
                413,
                "application/problem+json",
                "Payload Too Large - the request body is over the request body limit \
                 (api-gateway `defaults.body_limit_bytes`)."
                    .to_owned()
            ),
            (
                415,
                "application/problem+json",
                "Unsupported Media Type - the request `Content-Type` is missing or is not a JSON \
                 media type (`application/json` or an `application/<subtype>+json` type)."
                    .to_owned()
            ),
            (
                422,
                "application/problem+json",
                "Unprocessable Entity - the body is valid JSON but does not fit the request \
                 shape: a missing, unknown or mistyped field, or an unknown `mode` value."
                    .to_owned()
            ),
            (
                503,
                "application/problem+json",
                "ServiceUnavailable, retryable - the broker's database is unavailable: the \
                 producer insert."
                    .to_owned()
            ),
            (400, "application/problem+json", "Bad Request".to_owned()),
            (401, "application/problem+json", "Unauthorized".to_owned()),
            (403, "application/problem+json", "Forbidden".to_owned()),
            (404, "application/problem+json", "Not Found".to_owned()),
            (409, "application/problem+json", "Conflict".to_owned()),
            (
                429,
                "application/problem+json",
                "Too Many Requests".to_owned()
            ),
            (
                500,
                "application/problem+json",
                "Internal Server Error".to_owned()
            ),
        ]
    );
}

#[test]
fn the_producer_reset_route_declares_its_full_response_list() {
    let registry = RecordingOpenApiRegistry::default();
    let _router = register_ingest_routes(Router::new(), &registry);

    // The handler parses the body itself, so a non-empty body that is not a JSON
    // `ResetProducerRequest` is a 400 and neither 415 nor 422 is declared.
    assert_eq!(
        declared_responses(&registry, "/event-broker/v1/producers/{id}"),
        vec![
            (200, "", "Reset applied; audit record emitted".to_owned()),
            (
                413,
                "application/problem+json",
                "Payload Too Large - the request body is over the request body limit \
                 (api-gateway `defaults.body_limit_bytes`)."
                    .to_owned()
            ),
            (
                503,
                "application/problem+json",
                "ServiceUnavailable, retryable - the broker's database is unavailable: the \
                 producer read, or the producer chain delete."
                    .to_owned()
            ),
            (400, "application/problem+json", "Bad Request".to_owned()),
            (401, "application/problem+json", "Unauthorized".to_owned()),
            (403, "application/problem+json", "Forbidden".to_owned()),
            (404, "application/problem+json", "Not Found".to_owned()),
            (409, "application/problem+json", "Conflict".to_owned()),
            (
                429,
                "application/problem+json",
                "Too Many Requests".to_owned()
            ),
            (
                500,
                "application/problem+json",
                "Internal Server Error".to_owned()
            ),
        ]
    );
}

#[test]
fn the_producer_cursors_route_declares_its_full_response_list() {
    let registry = RecordingOpenApiRegistry::default();
    let _router = register_ingest_routes(Router::new(), &registry);

    assert_eq!(
        declared_responses(&registry, "/event-broker/v1/producers/{id}/cursors"),
        vec![
            (
                200,
                "application/json",
                "Producer cursors (topics may be empty)".to_owned()
            ),
            (
                503,
                "application/problem+json",
                "ServiceUnavailable, retryable - the broker's database is unavailable: the \
                 producer read, or the producer cursor read."
                    .to_owned()
            ),
            (400, "application/problem+json", "Bad Request".to_owned()),
            (401, "application/problem+json", "Unauthorized".to_owned()),
            (403, "application/problem+json", "Forbidden".to_owned()),
            (404, "application/problem+json", "Not Found".to_owned()),
            (409, "application/problem+json", "Conflict".to_owned()),
            (
                429,
                "application/problem+json",
                "Too Many Requests".to_owned()
            ),
            (
                500,
                "application/problem+json",
                "Internal Server Error".to_owned()
            ),
        ]
    );
}

#[test]
fn the_topic_list_route_declares_its_full_response_list() {
    let registry = RecordingOpenApiRegistry::default();
    let _router = register_ingest_routes(Router::new(), &registry);

    assert_eq!(
        declared_responses(&registry, "/event-broker/v1/topics"),
        vec![
            (200, "application/json", "Page of topics".to_owned()),
            (
                503,
                "application/problem+json",
                "ServiceUnavailable, retryable - the broker's database is unavailable: the \
                 topic list read, or a topic's event type read."
                    .to_owned()
            ),
            (400, "application/problem+json", "Bad Request".to_owned()),
            (401, "application/problem+json", "Unauthorized".to_owned()),
            (403, "application/problem+json", "Forbidden".to_owned()),
            (404, "application/problem+json", "Not Found".to_owned()),
            (409, "application/problem+json", "Conflict".to_owned()),
            (
                429,
                "application/problem+json",
                "Too Many Requests".to_owned()
            ),
            (
                500,
                "application/problem+json",
                "Internal Server Error".to_owned()
            ),
        ]
    );
}

#[test]
fn the_topic_segments_route_declares_its_full_response_list() {
    let registry = RecordingOpenApiRegistry::default();
    let _router = register_ingest_routes(Router::new(), &registry);

    assert_eq!(
        declared_responses(&registry, "/event-broker/v1/topics/segments"),
        vec![
            (200, "application/json", "Segment manifest".to_owned()),
            (
                503,
                "application/problem+json",
                "ServiceUnavailable, retryable - the broker's database or the topic's storage \
                 backend is unavailable: the topic's event type read, the topic lookup, or the \
                 segment read on the backend."
                    .to_owned()
            ),
            (400, "application/problem+json", "Bad Request".to_owned()),
            (401, "application/problem+json", "Unauthorized".to_owned()),
            (403, "application/problem+json", "Forbidden".to_owned()),
            (404, "application/problem+json", "Not Found".to_owned()),
            (409, "application/problem+json", "Conflict".to_owned()),
            (
                429,
                "application/problem+json",
                "Too Many Requests".to_owned()
            ),
            (
                500,
                "application/problem+json",
                "Internal Server Error".to_owned()
            ),
        ]
    );
}

#[test]
fn the_event_types_route_declares_its_full_response_list() {
    let registry = RecordingOpenApiRegistry::default();
    let _router = register_ingest_routes(Router::new(), &registry);

    assert_eq!(
        declared_responses(&registry, "/event-broker/v1/event-types"),
        vec![
            (200, "application/json", "Page of event types".to_owned()),
            (
                503,
                "application/problem+json",
                "ServiceUnavailable, retryable - the specification store is unavailable, or no \
                 ingest instance is reachable."
                    .to_owned()
            ),
            (400, "application/problem+json", "Bad Request".to_owned()),
            (401, "application/problem+json", "Unauthorized".to_owned()),
            (403, "application/problem+json", "Forbidden".to_owned()),
            (404, "application/problem+json", "Not Found".to_owned()),
            (409, "application/problem+json", "Conflict".to_owned()),
            (
                429,
                "application/problem+json",
                "Too Many Requests".to_owned()
            ),
            (
                500,
                "application/problem+json",
                "Internal Server Error".to_owned()
            ),
        ]
    );
}

#[test]
fn the_single_publish_route_declares_only_the_prefer_header() {
    use toolkit::api::operation_builder::ParamLocation;

    let registry = RecordingOpenApiRegistry::default();
    let _router = register_ingest_routes(Router::new(), &registry);

    let operations = registry.operations.lock().expect("operations lock");
    let [operation] = operations
        .iter()
        .filter(|operation| operation.path == "/event-broker/v1/events")
        .collect::<Vec<_>>()[..]
    else {
        panic!("expected one operation on /event-broker/v1/events");
    };
    // The only parameter is the optional `Prefer: wait=N` request header; there
    // is no query parameter on the publish route.
    assert_eq!(
        operation
            .params
            .iter()
            .map(|param| (param.name.as_str(), param.location.clone(), param.required))
            .collect::<Vec<_>>(),
        vec![("Prefer", ParamLocation::Header, false)]
    );
}
