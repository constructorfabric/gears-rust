use super::*;

#[test]
fn test_filter_error_mapping() {
    use http::StatusCode;

    let error = ODataError::InvalidFilter("malformed expression".to_owned());
    let problem = odata_error_to_problem(&error, "/user-management/v1/users", None);

    assert_eq!(problem.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(problem.code.contains("invalid_filter"));
    assert_eq!(problem.instance, "/user-management/v1/users");
}

#[test]
fn test_orderby_error_mapping() {
    use http::StatusCode;

    let error = ODataError::InvalidOrderByField("unknown_field".to_owned());
    let problem = odata_error_to_problem(&error, "/user-management/v1/users", None);

    assert_eq!(problem.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(problem.code.contains("invalid_orderby"));
}

#[test]
fn test_cursor_error_mapping() {
    use http::StatusCode;

    let error = ODataError::CursorInvalidBase64;
    let problem = odata_error_to_problem(
        &error,
        "/user-management/v1/users",
        Some("trace123".to_owned()),
    );

    assert_eq!(problem.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(problem.code.contains("invalid_cursor"));
    assert_eq!(problem.trace_id, Some("trace123".to_owned()));
}

#[test]
fn test_gts_code_format() {
    let error = ODataError::InvalidFilter("test".to_owned());
    let problem = odata_error_to_problem(&error, "/user-management/v1/test", None);

    // Verify the code follows GTS format
    assert!(problem.code.starts_with("gts.hx.core.errors.err.v1~"));
    assert!(problem.code.contains("odata"));
}
