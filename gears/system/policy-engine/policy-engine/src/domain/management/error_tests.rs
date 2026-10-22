#![allow(clippy::unwrap_used, clippy::expect_used)]

use policy_engine_sdk::management::{ManagementError, ManagementResource, finding};
use toolkit_db::DbError;
use uuid::Uuid;

use super::*;
use crate::domain::repos::{RepoError, conflict};
use crate::domain::validation::Finding;

#[test]
fn absences_name_what_was_looked_for() {
    let id = Uuid::from_u128(7);
    let cases = [
        (Subject::Bundle(BundleId(id)), ManagementResource::Bundle),
        (Subject::Version(VersionId(id)), ManagementResource::Version),
        (Subject::Tenant(id), ManagementResource::Tenant),
    ];
    for (subject, resource) in cases {
        assert_eq!(
            not_found(subject),
            ManagementError::NotFound { resource, id }
        );
    }
}

#[test]
fn repository_failures_project_onto_typed_errors() {
    let bundle = BundleId(Uuid::from_u128(3));
    let version = Subject::Version(VersionId(Uuid::nil()));
    let cases = vec![
        (
            repo(RepoError::NotFound, version),
            ManagementError::NotFound {
                resource: ManagementResource::Version,
                id: Uuid::nil(),
            },
        ),
        (
            repo(RepoError::conflict(conflict::VERSION_NOT_DRAFT), version),
            ManagementError::VersionNotDraft,
        ),
        (
            repo(
                RepoError::conflict(conflict::DRAFT_EXISTS),
                Subject::Bundle(bundle),
            ),
            ManagementError::DraftExists { bundle_id: bundle },
        ),
        (
            repo(
                RepoError::conflict(conflict::CONCURRENT_ACTIVATION),
                version,
            ),
            ManagementError::ConcurrentChange,
        ),
        (
            repo(RepoError::Database("boom".to_owned()), version),
            ManagementError::Internal,
        ),
    ];
    for (mapped, expected) in cases {
        assert_eq!(mapped, expected);
    }
}

#[test]
fn a_conflict_naming_caller_input_is_never_reported_without_it() {
    // Name and assignment conflicts are mapped where the input is known; a
    // generic projection must not invent an empty name.
    for code in [conflict::BUNDLE_NAME_TAKEN, conflict::ASSIGNMENT_EXISTS] {
        assert_eq!(
            repo(
                RepoError::conflict(code),
                Subject::Bundle(BundleId(Uuid::nil()))
            ),
            ManagementError::Internal,
            "{code}"
        );
    }
}

#[test]
fn validation_failure_carries_every_finding() {
    let findings = vec![
        Finding {
            document_name: Some("a".to_owned()),
            code: finding::SYNTAX_ERROR,
            message: "bad".to_owned(),
        },
        Finding {
            document_name: None,
            code: finding::LIMIT_EXCEEDED,
            message: "big".to_owned(),
        },
    ];
    let ManagementError::ValidationFailed { findings: sent } = validation_failed(&findings) else {
        panic!("expected a validation failure");
    };
    let sent: Vec<_> = sent
        .iter()
        .map(|f| (f.document_name.as_deref(), f.code.as_str()))
        .collect();
    assert_eq!(
        sent,
        [
            (Some("a"), finding::SYNTAX_ERROR),
            (None, finding::LIMIT_EXCEEDED)
        ]
    );
}

#[test]
fn authorisation_failures_keep_the_callers_wording_for_a_denial() {
    assert_eq!(
        authz(AuthzError::Denied, || ManagementError::CapabilityDenied),
        ManagementError::CapabilityDenied
    );
    assert_eq!(
        authz(AuthzError::Unavailable("pdp down".to_owned()), || {
            ManagementError::CapabilityDenied
        }),
        ManagementError::Unavailable
    );
}

#[test]
fn database_errors_never_leak_detail() {
    let failure = ManagementFailure::from(DbError::Other(anyhow::anyhow!("secret dsn")));
    assert_eq!(ManagementError::from(failure), ManagementError::Internal);
}
