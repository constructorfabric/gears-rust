#![allow(clippy::unwrap_used, clippy::expect_used)]

//! Tests for error handling and conversions.
//!
//! These tests verify domain error conversions including HTTP Problem mapping
//! and anyhow integration. `instance` is currently always `Some("/")` because
//! the per-module `From<DomainError> for Problem` does not have request URI
//! context — middleware will set it to the real request path once
//! `cpt-cf-errors-component-error-middleware` lands.

use modkit_canonical_errors::Problem;
use nodes_registry::domain::error::DomainError;

// (error, expected_status, expected_detail_substring, expected_problem_type)
type TestCase = (DomainError, u16, Option<String>, &'static str);

#[test]
fn test_error_conversion_mapping() {
    let test_id = uuid::Uuid::new_v4();

    let test_cases: Vec<TestCase> = vec![
        (
            DomainError::NodeNotFound(test_id),
            404,
            Some(test_id.to_string()),
            "gts://gts.cf.core.errors.err.v1~cf.core.err.not_found.v1~",
        ),
        (
            DomainError::SysInfoCollectionFailed("Failed to read CPU info".to_owned()),
            500,
            None,
            "gts://gts.cf.core.errors.err.v1~cf.core.err.internal.v1~",
        ),
        (
            DomainError::SysCapCollectionFailed("GPU detection failed".to_owned()),
            500,
            None,
            "gts://gts.cf.core.errors.err.v1~cf.core.err.internal.v1~",
        ),
        (
            DomainError::InvalidInput("Invalid capability key format".to_owned()),
            400,
            // InvalidArgument + with_field_violation sets a generic top-level detail;
            // caller-supplied text lives in context.field_violations[0].description.
            None,
            "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
        ),
        (
            DomainError::Internal("Database connection lost".to_owned()),
            500,
            None,
            "gts://gts.cf.core.errors.err.v1~cf.core.err.internal.v1~",
        ),
    ];

    for (error, expected_status, expected_detail, expected_problem_type) in test_cases {
        let problem: Problem = error.into();

        assert_eq!(problem.status, expected_status, "Status code should match");
        if let Some(expected_detail_content) = expected_detail {
            assert!(
                problem.detail.contains(&expected_detail_content),
                "Detail should contain error info: detail={:?}, expected substring={:?}",
                problem.detail,
                expected_detail_content
            );
        }
        assert_eq!(
            problem.problem_type, expected_problem_type,
            "Problem type should match"
        );
        assert_eq!(
            problem.instance.as_deref(),
            Some("/"),
            "Instance is always \"/\" until middleware lands"
        );
        assert!(
            problem.problem_type.starts_with("gts://"),
            "Problem type should start with gts://"
        );
    }
}

#[test]
fn test_node_not_found_sets_resource_type() {
    let test_id = uuid::Uuid::new_v4();
    let problem: Problem = DomainError::NodeNotFound(test_id).into();
    let rt = problem
        .context
        .get("resource_type")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert_eq!(rt, "gts.cf.nodes_registry.registry.node.v1~");
    let rn = problem
        .context
        .get("resource_name")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert_eq!(rn, test_id.to_string());
}

#[test]
fn test_invalid_input_sets_field_violation() {
    let problem: Problem =
        DomainError::InvalidInput("Invalid capability key format".to_owned()).into();
    let violation = problem
        .context
        .get("field_violations")
        .and_then(|v| v.get(0))
        .expect("expected at least one field violation");
    let reason = violation
        .get("reason")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let description = violation
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert_eq!(reason, "VALIDATION_ERROR");
    assert_eq!(description, "Invalid capability key format");
}

#[test]
fn test_error_into_problem_trait() {
    let node_id = uuid::Uuid::new_v4();
    let problem: Problem = DomainError::NodeNotFound(node_id).into();

    assert_eq!(problem.status, 404);
    assert!(problem.detail.contains(&node_id.to_string()));
    assert_eq!(problem.instance.as_deref(), Some("/"));
}

#[test]
fn test_domain_error_from_anyhow() {
    let anyhow_err = anyhow::anyhow!("something went wrong");
    let domain_err: DomainError = anyhow_err.into();

    match domain_err {
        DomainError::Internal(msg) => {
            assert!(msg.contains("something went wrong"));
        }
        _ => panic!("Expected Internal error"),
    }
}
