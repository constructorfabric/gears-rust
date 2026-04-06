use super::*;
use std::time::Duration;

#[test]
fn http_status_without_body() {
    let err = modkit_http::HttpError::HttpStatus {
        status: http::StatusCode::NOT_FOUND,
        body_preview: String::new(),
        content_type: None,
        retry_after: None,
    };
    let msg = format_http_error(&err, "TEST");
    assert_eq!(msg, "TEST HTTP 404 Not Found");
}

#[test]
fn http_status_with_body_excludes_body() {
    let err = modkit_http::HttpError::HttpStatus {
        status: http::StatusCode::INTERNAL_SERVER_ERROR,
        body_preview: "something broke".into(),
        content_type: None,
        retry_after: None,
    };
    let msg = format_http_error(&err, "JWKS");
    // body_preview must NOT appear in the output (security)
    assert_eq!(msg, "JWKS HTTP 500 Internal Server Error");
    assert!(!msg.contains("something broke"));
}

#[test]
fn timeout_error() {
    let err = modkit_http::HttpError::Timeout(Duration::from_secs(30));
    let msg = format_http_error(&err, "OAuth2 token");
    assert_eq!(msg, "OAuth2 token request timed out after 30s");
}

#[test]
fn overloaded_error() {
    let err = modkit_http::HttpError::Overloaded;
    let msg = format_http_error(&err, "PREFIX");
    assert_eq!(msg, "PREFIX request rejected: service overloaded");
}

#[test]
fn service_closed_error() {
    let err = modkit_http::HttpError::ServiceClosed;
    let msg = format_http_error(&err, "PREFIX");
    assert_eq!(msg, "PREFIX service unavailable");
}

#[test]
fn prefix_propagated_to_all_variants() {
    // Verify the prefix appears in output for a sample of variants
    let cases: Vec<modkit_http::HttpError> = vec![
        modkit_http::HttpError::Overloaded,
        modkit_http::HttpError::ServiceClosed,
        modkit_http::HttpError::Timeout(Duration::from_secs(1)),
    ];
    for err in &cases {
        let msg = format_http_error(err, "CTX");
        assert!(msg.starts_with("CTX "), "Expected prefix 'CTX' in: {msg}");
    }
}
