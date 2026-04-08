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
mod tests {
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
}
