//! The `OpenAPI` operations `register_ingest_routes` declares, read back through
//! a registry that records every registered `OperationSpec`.

use std::sync::Mutex;

use axum::Router;
use toolkit::api::OpenApiRegistry;
use toolkit::api::operation_builder::OperationSpec;

use super::register_ingest_routes;

#[derive(Default)]
struct RecordingOpenApiRegistry {
    operations: Mutex<Vec<OperationSpec>>,
}

impl OpenApiRegistry for RecordingOpenApiRegistry {
    fn register_operation(&self, spec: &OperationSpec) {
        self.operations
            .lock()
            .expect("operations lock")
            .push(spec.clone());
    }

    fn ensure_schema_raw(
        &self,
        name: &str,
        _schemas: Vec<(
            String,
            utoipa::openapi::RefOr<utoipa::openapi::schema::Schema>,
        )>,
    ) -> String {
        name.to_owned()
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// `(status, content_type, description)` of every response `path` declares,
/// in registration order. An empty content type is a body-less response.
fn declared_responses(
    registry: &RecordingOpenApiRegistry,
    path: &str,
) -> Vec<(u16, &'static str, String)> {
    let operations = registry.operations.lock().expect("operations lock");
    let [operation] = operations
        .iter()
        .filter(|operation| operation.path == path)
        .collect::<Vec<_>>()[..]
    else {
        panic!("expected one operation on {path}");
    };
    operation
        .responses
        .iter()
        .map(|response| {
            (
                response.status,
                response.content_type,
                response.description.clone(),
            )
        })
        .collect()
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
                 (total payload over the configured byte ceiling)"
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
