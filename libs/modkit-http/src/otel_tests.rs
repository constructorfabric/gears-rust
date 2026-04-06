use super::*;
use tracing::info_span;

#[test]
fn test_get_traceparent_none() {
    let headers = HeaderMap::new();
    assert!(get_traceparent(&headers).is_none());
}

#[test]
fn test_get_traceparent_ok() {
    let mut headers = HeaderMap::new();
    headers.insert(
        TRACEPARENT,
        "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"
            .parse()
            .expect("valid header"),
    );

    let tp = get_traceparent(&headers);
    assert!(tp.is_some());
    assert_eq!(
        tp.expect("should be some"),
        "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"
    );
}

#[test]
fn test_parse_trace_id_ok() {
    let traceparent = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01";
    let trace_id = parse_trace_id(traceparent);
    assert_eq!(
        trace_id,
        Some("4bf92f3577b34da6a3ce929d0e0e4736".to_owned())
    );
}

#[test]
fn test_parse_trace_id_invalid() {
    assert!(parse_trace_id("invalid").is_none());
    assert!(parse_trace_id("").is_none());
}

#[test]
#[cfg(not(feature = "otel"))]
fn test_inject_current_span_noop() {
    let mut headers = HeaderMap::new();
    inject_current_span(&mut headers);
    // Should be no-op, no headers added
    assert!(headers.is_empty());
}

#[test]
#[cfg(feature = "otel")]
fn test_inject_current_span_no_panic() {
    use opentelemetry::global;
    use opentelemetry_sdk::propagation::TraceContextPropagator;

    global::set_text_map_propagator(TraceContextPropagator::new());

    let mut headers = http::HeaderMap::new();
    let _span = tracing::info_span!("test").entered();
    // Without full OTEL setup, this may not inject anything, but shouldn't panic
    inject_current_span(&mut headers);
}

#[test]
fn test_set_parent_from_headers_no_panic() {
    let mut headers = HeaderMap::new();
    headers.insert(
        TRACEPARENT,
        "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"
            .parse()
            .expect("valid header"),
    );

    let span = info_span!(
        "test",
        trace_id = tracing::field::Empty,
        parent.trace_id = tracing::field::Empty
    );

    // Should not panic in either mode
    set_parent_from_headers(&span, &headers);
}
