use super::*;

#[test]
fn test_with_trace_context() {
    use http::StatusCode;

    let problem = Problem::new(StatusCode::NOT_FOUND, "Not Found", "Resource not found")
        .with_trace_context("/tests/v1/users/123");

    assert_eq!(problem.instance, "/tests/v1/users/123");
    // trace_id may or may not be set depending on tracing context
}

#[test]
fn test_with_request_context() {
    use axum::http::Uri;
    use http::StatusCode;

    let uri: Uri = "/tests/v1/users/123".parse().unwrap();
    let problem = Problem::new(StatusCode::NOT_FOUND, "Not Found", "Resource not found")
        .with_request_context(&uri);

    assert_eq!(problem.instance, "/tests/v1/users/123");
}
