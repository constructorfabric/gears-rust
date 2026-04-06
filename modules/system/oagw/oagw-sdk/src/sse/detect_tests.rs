use super::*;
use http::HeaderValue;

#[test]
fn detects_event_stream() {
    let mut headers = HeaderMap::new();
    headers.insert(
        http::header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream"),
    );
    assert!(is_server_events_response(&headers));
}

#[test]
fn detects_event_stream_with_charset() {
    let mut headers = HeaderMap::new();
    headers.insert(
        http::header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream; charset=utf-8"),
    );
    assert!(is_server_events_response(&headers));
}

#[test]
fn rejects_json() {
    let mut headers = HeaderMap::new();
    headers.insert(
        http::header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    assert!(!is_server_events_response(&headers));
}

#[test]
fn rejects_missing_content_type() {
    let headers = HeaderMap::new();
    assert!(!is_server_events_response(&headers));
}
