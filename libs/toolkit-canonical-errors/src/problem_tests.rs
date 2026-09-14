//! Tests for canonical `Problem` (RFC 9457) conversions.

use super::*;

#[test]
fn not_found_round_trips_through_problem() {
    let original = CanonicalError::__not_found(crate::context::NotFound::new())
        .with_detail("invoice 42 missing")
        .with_resource_type("invoice")
        .with_resource("42");

    let problem = Problem::from(original.clone());
    let recovered = CanonicalError::try_from(problem).expect("known problem_type");

    assert!(matches!(recovered, CanonicalError::NotFound { .. }));
    assert_eq!(recovered.detail(), original.detail());
    assert_eq!(recovered.resource_type(), Some("invoice"));
    assert_eq!(recovered.resource_name(), Some("42"));
}

#[test]
fn already_exists_round_trips_through_problem() {
    let original = CanonicalError::__already_exists(crate::context::AlreadyExists::new())
        .with_detail("duplicate payment")
        .with_resource_type("payment")
        .with_resource("xyz");

    let problem = Problem::from(original);
    let recovered = CanonicalError::try_from(problem).expect("known problem_type");

    assert!(matches!(recovered, CanonicalError::AlreadyExists { .. }));
    assert_eq!(recovered.resource_type(), Some("payment"));
    assert_eq!(recovered.resource_name(), Some("xyz"));
}

#[test]
fn permission_denied_round_trips_through_problem() {
    let original =
        CanonicalError::__permission_denied(crate::context::PermissionDenied::new("missing scope"))
            .with_detail("forbidden")
            .with_resource_type("invoice")
            .with_resource("42");

    let problem = Problem::from(original);
    let recovered = CanonicalError::try_from(problem).expect("known problem_type");

    assert!(matches!(recovered, CanonicalError::PermissionDenied { .. }));
    assert_eq!(recovered.resource_type(), Some("invoice"));
    assert_eq!(recovered.resource_name(), Some("42"));
}

#[test]
fn status_omitted_on_the_wire_deserializes_to_none() {
    // RFC 9457 §3.1: `status` is advisory and optional. A minimal,
    // spec-compliant Problem may omit it entirely.
    let problem: Problem =
        serde_json::from_str(r#"{"type":"https://example.com/probs/x","title":"X","detail":"d"}"#)
            .expect("status-less Problem must still deserialize");
    assert_eq!(problem.status, None);
}

#[test]
fn try_from_with_no_status_keeps_the_category_default() {
    let problem = Problem {
        problem_type: gts_uri!("cf.core.errors.err.v1~cf.core.err.not_found.v1~").to_owned(),
        title: "Not Found".to_owned(),
        status: None,
        detail: "missing".to_owned(),
        instance: None,
        trace_id: None,
        context: serde_json::json!({}),
        error_code: None,
        error_domain: None,
    };
    let recovered = CanonicalError::try_from(problem).expect("known problem_type");
    assert_eq!(recovered.status_code(), 404);
}

#[test]
fn unknown_problem_type_errors() {
    let problem = Problem {
        problem_type: "gts://something.else.unknown".to_owned(),
        title: "X".to_owned(),
        status: Some(500),
        detail: String::new(),
        instance: None,
        trace_id: None,
        context: serde_json::json!({}),
        error_code: None,
        error_domain: None,
    };
    assert!(CanonicalError::try_from(problem).is_err());
}
