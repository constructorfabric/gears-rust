#![allow(clippy::expect_used, clippy::unwrap_used)]

use axum::http::HeaderValue;

use super::*;

#[test]
fn the_request_id_is_recorded_when_it_is_short() {
    let mut headers = HeaderMap::new();
    assert_eq!(request_id(&headers), None);
    headers.insert("x-request-id", HeaderValue::from_static("req-1"));
    assert_eq!(request_id(&headers).as_deref(), Some("req-1"));
    headers.insert(
        "x-request-id",
        HeaderValue::from_str(&"r".repeat(MAX_REQUEST_ID_LEN + 1)).unwrap(),
    );
    assert_eq!(request_id(&headers), None);
}

#[test]
fn an_anonymous_caller_is_unauthenticated() {
    assert_eq!(require_context(None).unwrap_err().status_code(), 401);
    let anonymous = Some(Extension(SecurityContext::anonymous()));
    assert_eq!(require_context(anonymous).unwrap_err().status_code(), 401);
}
