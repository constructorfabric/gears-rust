use http::HeaderMap;

/// Check if the response headers indicate an SSE stream.
///
/// Returns `true` when `Content-Type` starts with `text/event-stream`.
#[must_use]
pub fn is_server_events_response(headers: &HeaderMap) -> bool {
    headers
        .get(http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.starts_with("text/event-stream"))
}

#[cfg(test)]
#[path = "detect_tests.rs"]
mod detect_tests;
