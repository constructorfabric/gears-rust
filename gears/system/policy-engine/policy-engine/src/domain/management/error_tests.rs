#![allow(clippy::unwrap_used, clippy::expect_used)]

use policy_engine_sdk::management::{ManagementError, reason};
use toolkit_canonical_errors::Problem;
use toolkit_db::DbError;
use uuid::Uuid;

use super::*;
use crate::domain::repos::{RepoError, conflict};
use crate::domain::validation::Finding;

#[test]
fn families_match_the_sdk_resource_types() {
    let bundle = not_found(Subject::Bundle(Uuid::nil()));
    assert_eq!(
        bundle.resource_type(),
        Some(policy_engine_sdk::BUNDLE_RESOURCE)
    );
    let version = not_found(Subject::Version(Uuid::nil()));
    assert_eq!(
        version.resource_type(),
        Some(policy_engine_sdk::BUNDLE_VERSION_RESOURCE)
    );
    let assignment = AssignmentResourceError::not_found("x")
        .with_resource("y")
        .create();
    assert_eq!(
        assignment.resource_type(),
        Some(policy_engine_sdk::ASSIGNMENT_RESOURCE)
    );
}

#[test]
fn reason_codes_travel_where_the_sdk_table_says() {
    let cases: Vec<(ManagementError, u16, &str)> = vec![
        (concurrent_change(), 409, reason::CONCURRENT_CHANGE),
        (version_not_draft(), 409, reason::VERSION_NOT_DRAFT),
        (bundle_name_taken("n"), 409, reason::BUNDLE_NAME_TAKEN),
        (draft_exists(), 409, reason::DRAFT_EXISTS),
        (seed_not_in_bundle(), 400, reason::SEED_NOT_IN_BUNDLE),
        (
            content_limit_exceeded(&[]),
            400,
            reason::CONTENT_LIMIT_EXCEEDED,
        ),
        (validation_failed(&[]), 400, reason::VALIDATION_FAILED),
        (capability_denied(), 403, reason::CAPABILITY_DENIED),
    ];
    for (err, status, code) in cases {
        assert_eq!(err.status_code(), status, "{code}");
        assert_eq!(reason_of(&err).as_deref(), Some(code));
    }
    assert_eq!(reason_of(&not_found(Subject::Tenant(Uuid::nil()))), None);
    assert_eq!(unavailable("down").status_code(), 503);
    assert_eq!(internal().status_code(), 500);
}

#[test]
fn repository_failures_project_onto_the_table() {
    let cases = vec![
        (RepoError::NotFound, 404, None),
        (
            RepoError::conflict(conflict::VERSION_NOT_DRAFT),
            409,
            Some(reason::VERSION_NOT_DRAFT),
        ),
        (
            RepoError::conflict(conflict::DRAFT_EXISTS),
            409,
            Some(reason::DRAFT_EXISTS),
        ),
        (
            RepoError::conflict(conflict::CONCURRENT_ACTIVATION),
            409,
            Some(reason::CONCURRENT_CHANGE),
        ),
        (
            RepoError::conflict(conflict::BUNDLE_NAME_TAKEN),
            409,
            Some(reason::BUNDLE_NAME_TAKEN),
        ),
        (RepoError::Database("boom".to_owned()), 500, None),
    ];
    for (err, status, code) in cases {
        let label = err.to_string();
        let mapped = repo(err, Subject::Version(Uuid::nil()));
        assert_eq!(mapped.status_code(), status, "{label}");
        assert_eq!(reason_of(&mapped).as_deref(), code, "{label}");
    }
}

#[test]
fn validation_failure_lists_every_finding() {
    let findings = vec![
        Finding {
            document_name: Some("a".to_owned()),
            code: policy_engine_sdk::management::finding::SYNTAX_ERROR,
            message: "bad".to_owned(),
        },
        Finding {
            document_name: None,
            code: policy_engine_sdk::management::finding::LIMIT_EXCEEDED,
            message: "big".to_owned(),
        },
    ];
    let err = validation_failed(&findings);
    let problem = serde_json::to_value(Problem::from(err)).unwrap();
    let violations = problem["context"]["violations"].as_array().unwrap();
    assert_eq!(violations.len(), 2);
    assert_eq!(violations[0]["subject"], "a");
    assert_eq!(violations[1]["subject"], "version");
    assert!(
        violations
            .iter()
            .all(|v| v["type"] == reason::VALIDATION_FAILED)
    );
}

#[test]
fn database_errors_never_leak_detail() {
    let failure = ManagementFailure::from(DbError::Other(anyhow::anyhow!("secret dsn")));
    let err = ManagementError::from(failure);
    assert_eq!(err.status_code(), 500);
    assert!(!err.detail().contains("secret"));
}
