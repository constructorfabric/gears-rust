//! The `OpenAPI` operations `register_delivery_routes` declares for JOIN, seek,
//! consumer-group create, list, read and delete, and the two stream transports,
//! read back through a registry that records every registered `OperationSpec`.

use axum::Router;
use axum::http::Method;

use super::register_delivery_routes;
use super::route_spec_recorder::{
    RecordingOpenApiRegistry, declared_method_responses, declared_responses,
};

#[test]
fn the_join_route_declares_its_full_response_list() {
    let registry = RecordingOpenApiRegistry::default();
    let _router = register_delivery_routes(Router::new(), &registry);

    // `error_429` after `standard_errors` replaces the standard 429, which
    // moves it to the end of the list.
    assert_eq!(
        declared_method_responses(&registry, &Method::POST, "/event-broker/v1/subscriptions"),
        vec![
            (
                201,
                "application/json",
                "Subscription created; assignment computed".to_owned()
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
                 shape: a missing, unknown or mistyped field, or an unknown enum value."
                    .to_owned()
            ),
            (
                501,
                "application/problem+json",
                "Unimplemented - `consumer_group` is a named group (a valid consumer-group id \
                 whose suffix is not a UUID); only anonymous groups are served."
                    .to_owned()
            ),
            (
                503,
                "application/problem+json",
                "ServiceUnavailable, retryable - the broker's database or cluster cache is \
                 unavailable: the consumer group read, the event-type candidate query of an \
                 interest group, the registered-type lookup on a topic with more than 256 event \
                 types, the topic lookup, or a routing-marker write."
                    .to_owned()
            ),
            (400, "application/problem+json", "Bad Request".to_owned()),
            (401, "application/problem+json", "Unauthorized".to_owned()),
            (403, "application/problem+json", "Forbidden".to_owned()),
            (404, "application/problem+json", "Not Found".to_owned()),
            (409, "application/problem+json", "Conflict".to_owned()),
            (
                500,
                "application/problem+json",
                "Internal Server Error".to_owned()
            ),
            (
                429,
                "application/problem+json",
                "Too Many Requests".to_owned()
            ),
        ]
    );
}

#[test]
fn the_stream_route_declares_its_full_response_list() {
    let registry = RecordingOpenApiRegistry::default();
    let _router = register_delivery_routes(Router::new(), &registry);

    assert_eq!(
        declared_responses(&registry, "/event-broker/v1/events:stream"),
        vec![
            (
                200,
                "multipart/mixed",
                "Long-lived multipart/mixed event stream (Transfer-Encoding chunked)".to_owned()
            ),
            (
                406,
                "application/problem+json",
                "Accept header excludes multipart/mixed".to_owned()
            ),
            (
                410,
                "application/problem+json",
                "SubscriptionTerminated".to_owned()
            ),
            (
                503,
                "application/problem+json",
                "ServiceUnavailable, retryable - the broker's database is unavailable: the \
                 consumer group read, a cursor read, or the event-type candidate query of an \
                 interest group the open evaluates."
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
fn the_sse_route_declares_its_full_response_list() {
    let registry = RecordingOpenApiRegistry::default();
    let _router = register_delivery_routes(Router::new(), &registry);

    assert_eq!(
        declared_responses(&registry, "/event-broker/v1/events:sse"),
        vec![
            (
                200,
                "text/event-stream",
                "Long-lived text/event-stream of frames".to_owned()
            ),
            (
                406,
                "application/problem+json",
                "Accept header excludes text/event-stream".to_owned()
            ),
            (
                410,
                "application/problem+json",
                "SubscriptionTerminated".to_owned()
            ),
            (
                503,
                "application/problem+json",
                "ServiceUnavailable, retryable - the broker's database is unavailable: the \
                 consumer group read, a cursor read, or the event-type candidate query of an \
                 interest group the open evaluates."
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
fn the_seek_route_declares_its_full_response_list() {
    let registry = RecordingOpenApiRegistry::default();
    let _router = register_delivery_routes(Router::new(), &registry);

    assert_eq!(
        declared_method_responses(
            &registry,
            &Method::POST,
            "/event-broker/v1/subscriptions/{id}"
        ),
        vec![
            (
                200,
                "application/json",
                "Cursor seeded/updated; resolved integer offsets per partition".to_owned()
            ),
            (
                412,
                "application/problem+json",
                "TopologyVersionMismatch - `topology_version` differs from the subscription's \
                 current one (a concurrent rebalance moved the assignment); the caller re-reads \
                 the subscription and re-seeks."
                    .to_owned()
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
                 shape: a missing, unknown or mistyped field."
                    .to_owned()
            ),
            (
                503,
                "application/problem+json",
                "ServiceUnavailable, retryable - the broker's database or the topic's storage \
                 backend is unavailable: the consumer group read, the topic lookup, the \
                 resolution of a position on the backend, or a cursor write."
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
fn the_consumer_group_get_route_declares_its_full_response_list() {
    let registry = RecordingOpenApiRegistry::default();
    let _router = register_delivery_routes(Router::new(), &registry);

    assert_eq!(
        declared_method_responses(
            &registry,
            &Method::GET,
            "/event-broker/v1/consumer-groups/{id}"
        ),
        vec![
            (200, "application/json", "Consumer group record".to_owned()),
            (
                501,
                "application/problem+json",
                "Unimplemented - `id` is a named group (a valid consumer-group id whose suffix \
                 is not a UUID); only anonymous groups are served."
                    .to_owned()
            ),
            (
                503,
                "application/problem+json",
                "ServiceUnavailable, retryable - the broker's database is unavailable: the \
                 consumer group read."
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
fn the_consumer_group_delete_route_declares_its_full_response_list() {
    let registry = RecordingOpenApiRegistry::default();
    let _router = register_delivery_routes(Router::new(), &registry);

    assert_eq!(
        declared_method_responses(
            &registry,
            &Method::DELETE,
            "/event-broker/v1/consumer-groups/{id}"
        ),
        vec![
            (204, "", "Deleted".to_owned()),
            (
                501,
                "application/problem+json",
                "Unimplemented - `id` is a named group (a valid consumer-group id whose suffix \
                 is not a UUID); only anonymous groups are served."
                    .to_owned()
            ),
            (
                503,
                "application/problem+json",
                "ServiceUnavailable, retryable - the broker's database is unavailable: the \
                 consumer group read, or the consumer group delete."
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
fn the_consumer_group_create_route_declares_its_full_response_list() {
    let registry = RecordingOpenApiRegistry::default();
    let _router = register_delivery_routes(Router::new(), &registry);

    // The handler parses the body itself, so a non-empty body that is not a JSON
    // `CreateConsumerGroupRequest` is a 400 and neither 415 nor 422 is declared.
    assert_eq!(
        declared_method_responses(&registry, &Method::POST, "/event-broker/v1/consumer-groups"),
        vec![
            (201, "application/json", "Created".to_owned()),
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
                 consumer group insert."
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
fn the_consumer_group_list_route_declares_its_full_response_list() {
    let registry = RecordingOpenApiRegistry::default();
    let _router = register_delivery_routes(Router::new(), &registry);

    assert_eq!(
        declared_method_responses(&registry, &Method::GET, "/event-broker/v1/consumer-groups"),
        vec![
            (
                200,
                "application/json",
                "Page of consumer groups".to_owned()
            ),
            (
                503,
                "application/problem+json",
                "ServiceUnavailable, retryable - the broker's database is unavailable: the \
                 consumer group list read."
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
