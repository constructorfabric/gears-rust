use super::*;

#[test]
fn test_filter_error_converts_to_problem() {
    use http::StatusCode;

    let err = Error::InvalidFilter("malformed".to_owned());
    let problem: Problem = err.into();

    assert_eq!(problem.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(problem.title, "Invalid Filter");
    assert!(problem.detail.contains("malformed"));
    assert!(problem.code.contains("odata"));
    assert!(problem.code.contains("invalid_filter"));
}

#[test]
fn test_orderby_error_converts_to_problem() {
    use http::StatusCode;

    let err = Error::InvalidOrderByField("unknown".to_owned());
    let problem: Problem = err.into();

    assert_eq!(problem.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(problem.title, "Invalid OrderBy");
    assert!(problem.code.contains("odata"));
    assert!(problem.code.contains("invalid_orderby"));
}

#[test]
fn test_cursor_error_converts_to_problem() {
    use http::StatusCode;

    let err = Error::CursorInvalidBase64;
    let problem: Problem = err.into();

    assert_eq!(problem.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(problem.title, "Invalid Cursor");
    assert!(problem.code.contains("odata"));
    assert!(problem.code.contains("invalid_cursor"));
}
