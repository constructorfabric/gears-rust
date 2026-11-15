//! The `OpenAPI` operations `register_dispatcher_routes` declares for the
//! forwarding routes that read a request body or answer without one, read back
//! through a registry that records every registered `OperationSpec`.

use axum::Router;
use axum::http::Method;

use super::register_dispatcher_routes;
use super::route_spec_recorder::{
    RecordingOpenApiRegistry, declared_method_responses, declared_responses,
};

#[test]
fn the_forwarded_publish_route_declares_its_full_response_list() {
    let registry = RecordingOpenApiRegistry::default();
    let _router = register_dispatcher_routes(Router::new(), &registry);

    assert_eq!(
        declared_responses(&registry, "/event-broker/v1/events"),
        vec![
            (
                200,
                "application/json",
                "Proxied verbatim from the resolved instance - see the underlying operation's own \
                 documentation for the concrete response shape."
                    .to_owned()
            ),
            (
                413,
                "application/problem+json",
                "Payload Too Large - the request body is over the api-gateway request body limit \
                 (`defaults.body_limit_bytes`) or over the dispatcher's forwarding body cap."
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
            (
                503,
                "application/problem+json",
                "Service Unavailable".to_owned()
            ),
        ]
    );
}

#[test]
fn the_forwarded_batch_route_declares_its_full_response_list() {
    let registry = RecordingOpenApiRegistry::default();
    let _router = register_dispatcher_routes(Router::new(), &registry);

    assert_eq!(
        declared_responses(&registry, "/event-broker/v1/events:batch"),
        vec![
            (
                200,
                "application/json",
                "Proxied verbatim from the resolved instance - see the underlying operation's own \
                 documentation for the concrete response shape."
                    .to_owned()
            ),
            (
                413,
                "application/problem+json",
                "Payload Too Large - the request body is over the api-gateway request body limit \
                 (`defaults.body_limit_bytes`) or over the dispatcher's forwarding body cap."
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
            (
                503,
                "application/problem+json",
                "Service Unavailable".to_owned()
            ),
        ]
    );
}

#[test]
fn the_forwarded_producer_register_route_declares_its_full_response_list() {
    let registry = RecordingOpenApiRegistry::default();
    let _router = register_dispatcher_routes(Router::new(), &registry);

    assert_eq!(
        declared_responses(&registry, "/event-broker/v1/producers"),
        vec![
            (
                200,
                "application/json",
                "Proxied verbatim from the resolved instance - see the underlying operation's own \
                 documentation for the concrete response shape."
                    .to_owned()
            ),
            (
                413,
                "application/problem+json",
                "Payload Too Large - the request body is over the api-gateway request body limit \
                 (`defaults.body_limit_bytes`) or over the dispatcher's forwarding body cap."
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
            (
                503,
                "application/problem+json",
                "Service Unavailable".to_owned()
            ),
        ]
    );
}

#[test]
fn the_forwarded_producer_reset_route_declares_its_full_response_list() {
    let registry = RecordingOpenApiRegistry::default();
    let _router = register_dispatcher_routes(Router::new(), &registry);

    assert_eq!(
        declared_responses(&registry, "/event-broker/v1/producers/{id}"),
        vec![
            (
                200,
                "",
                "Proxied verbatim from the resolved instance - see the underlying operation's own \
                 documentation for the concrete response shape."
                    .to_owned()
            ),
            (
                413,
                "application/problem+json",
                "Payload Too Large - the request body is over the api-gateway request body limit \
                 (`defaults.body_limit_bytes`) or over the dispatcher's forwarding body cap."
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
            (
                503,
                "application/problem+json",
                "Service Unavailable".to_owned()
            ),
        ]
    );
}

#[test]
fn the_forwarded_consumer_group_create_route_declares_its_full_response_list() {
    let registry = RecordingOpenApiRegistry::default();
    let _router = register_dispatcher_routes(Router::new(), &registry);

    assert_eq!(
        declared_method_responses(&registry, &Method::POST, "/event-broker/v1/consumer-groups"),
        vec![
            (
                200,
                "application/json",
                "Proxied verbatim from the resolved instance - see the underlying operation's own \
                 documentation for the concrete response shape."
                    .to_owned()
            ),
            (
                413,
                "application/problem+json",
                "Payload Too Large - the request body is over the api-gateway request body limit \
                 (`defaults.body_limit_bytes`) or over the dispatcher's forwarding body cap."
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
            (
                503,
                "application/problem+json",
                "Service Unavailable".to_owned()
            ),
        ]
    );
}

#[test]
fn the_forwarded_join_route_declares_its_full_response_list() {
    let registry = RecordingOpenApiRegistry::default();
    let _router = register_dispatcher_routes(Router::new(), &registry);

    assert_eq!(
        declared_method_responses(&registry, &Method::POST, "/event-broker/v1/subscriptions"),
        vec![
            (
                200,
                "application/json",
                "Proxied verbatim from the resolved instance - see the underlying operation's own \
                 documentation for the concrete response shape."
                    .to_owned()
            ),
            (
                413,
                "application/problem+json",
                "Payload Too Large - the request body is over the api-gateway request body limit \
                 (`defaults.body_limit_bytes`) or over the dispatcher's forwarding body cap."
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
            (
                503,
                "application/problem+json",
                "Service Unavailable".to_owned()
            ),
        ]
    );
}

#[test]
fn the_forwarded_seek_route_declares_its_full_response_list() {
    let registry = RecordingOpenApiRegistry::default();
    let _router = register_dispatcher_routes(Router::new(), &registry);

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
                "Proxied verbatim from the resolved instance - see the underlying operation's own \
                 documentation for the concrete response shape."
                    .to_owned()
            ),
            (
                413,
                "application/problem+json",
                "Payload Too Large - the request body is over the api-gateway request body limit \
                 (`defaults.body_limit_bytes`) or over the dispatcher's forwarding body cap."
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
            (
                503,
                "application/problem+json",
                "Service Unavailable".to_owned()
            ),
        ]
    );
}

#[test]
fn the_forwarded_consumer_group_delete_route_declares_its_full_response_list() {
    let registry = RecordingOpenApiRegistry::default();
    let _router = register_dispatcher_routes(Router::new(), &registry);

    assert_eq!(
        declared_method_responses(
            &registry,
            &Method::DELETE,
            "/event-broker/v1/consumer-groups/{id}"
        ),
        vec![
            (
                204,
                "",
                "Proxied verbatim from the resolved instance - see the underlying operation's own \
                 documentation for the concrete response shape."
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
            (
                503,
                "application/problem+json",
                "Service Unavailable".to_owned()
            ),
        ]
    );
}

#[test]
fn the_forwarded_subscription_leave_route_declares_its_full_response_list() {
    let registry = RecordingOpenApiRegistry::default();
    let _router = register_dispatcher_routes(Router::new(), &registry);

    assert_eq!(
        declared_method_responses(
            &registry,
            &Method::DELETE,
            "/event-broker/v1/subscriptions/{id}"
        ),
        vec![
            (
                204,
                "",
                "Proxied verbatim from the resolved instance - see the underlying operation's own \
                 documentation for the concrete response shape."
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
            (
                503,
                "application/problem+json",
                "Service Unavailable".to_owned()
            ),
        ]
    );
}
