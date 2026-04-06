use super::*;

#[test]
fn problem_builder_pattern() {
    let p = Problem::new(
        StatusCode::UNPROCESSABLE_ENTITY,
        "Validation Failed",
        "Input validation errors",
    )
    .with_code("VALIDATION_ERROR")
    .with_instance("/users/123")
    .with_trace_id("req-456")
    .with_errors(vec![ValidationViolation {
        message: "Email is required".to_owned(),
        field: "email".to_owned(),
        code: None,
    }]);

    assert_eq!(p.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(p.code, "VALIDATION_ERROR");
    assert_eq!(p.instance, "/users/123");
    assert_eq!(p.trace_id, Some("req-456".to_owned()));
    assert!(p.errors.is_some());
    assert_eq!(p.errors.as_ref().unwrap().len(), 1);
}

#[test]
fn problem_serializes_status_as_u16() {
    let p = Problem::new(StatusCode::NOT_FOUND, "Not Found", "Resource not found");
    let json = serde_json::to_string(&p).unwrap();
    assert!(json.contains("\"status\":404"));
}

#[test]
fn problem_deserializes_status_from_u16() {
    let json =
        r#"{"type":"about:blank","title":"Not Found","status":404,"detail":"Resource not found"}"#;
    let p: Problem = serde_json::from_str(json).unwrap();
    assert_eq!(p.status, StatusCode::NOT_FOUND);
}
