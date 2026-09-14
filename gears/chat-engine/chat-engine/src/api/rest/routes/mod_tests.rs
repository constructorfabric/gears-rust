//! Route-contract tests.
//!
//! `register_routes` needs the full seven-service graph, so it cannot be
//! called from a unit test. These assert instead on `docs/openapi.json`, the
//! document generated from those very registrations and the artifact clients
//! generate their code from — so a wrong declaration here is exactly what
//! would reach them.

use serde_json::Value;

/// The generated gear document, embedded at compile time so the assertions
/// fail on a stale checkout rather than silently skipping.
const SPEC: &str = include_str!("../../../../../docs/openapi.json");

/// The operations that stream. Every one of these handlers returns through
/// `sse_delta_stream_response` or `sse_buffer_reader_response`, both of which
/// set `Content-Type: text/event-stream`.
const STREAMING_OPERATIONS: &[(&str, &str)] = &[
    ("post", "/chat-engine/v1/sessions/{id}/messages"),
    ("post", "/chat-engine/v1/messages/{id}/recreate"),
    ("post", "/chat-engine/v1/sessions/{id}/summarize"),
    ("get", "/chat-engine/v1/messages/{id}/stream"),
];

fn spec() -> Value {
    serde_json::from_str(SPEC).expect("docs/openapi.json is valid JSON")
}

fn ok_content<'a>(spec: &'a Value, method: &str, path: &str) -> &'a Value {
    let response = &spec["paths"][path][method]["responses"]["200"];
    assert!(
        response.is_object(),
        "{method} {path} declares no 200 response"
    );
    &response["content"]
}

#[test]
fn streaming_operations_declare_text_event_stream() {
    let spec = spec();

    for (method, path) in STREAMING_OPERATIONS {
        let content = ok_content(&spec, method, path);
        let media_types: Vec<&str> = content
            .as_object()
            .expect("content object")
            .keys()
            .map(String::as_str)
            .collect();

        assert_eq!(
            media_types,
            ["text/event-stream"],
            "{method} {path} must advertise SSE — declaring application/json makes \
             generated clients parse the stream as one JSON body"
        );
    }
}

#[test]
fn streaming_operations_reference_the_event_schema() {
    let spec = spec();

    for (method, path) in STREAMING_OPERATIONS {
        let schema = &ok_content(&spec, method, path)["text/event-stream"]["schema"]["$ref"];
        assert_eq!(
            schema.as_str(),
            Some("#/components/schemas/StreamingEventDto"),
            "{method} {path} should describe its frames with StreamingEventDto"
        );
    }
}

#[test]
fn non_streaming_operations_stay_json() {
    let spec = spec();

    // The listing shares a path with the streaming POST; it must not have been
    // swept along by the change.
    let content = ok_content(&spec, "get", "/chat-engine/v1/sessions/{id}/messages");
    assert!(content.get("application/json").is_some());
    assert!(content.get("text/event-stream").is_none());
}
