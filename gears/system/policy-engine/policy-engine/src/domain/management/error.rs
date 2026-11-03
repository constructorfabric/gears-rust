//! The projection of management conditions onto the SDK's typed
//! [`ManagementError`] ([`policy_engine_sdk::management`] module
//! documentation). Every dependency or storage failure is logged here with
//! its cause; the error that leaves the gear carries none.

use policy_engine_sdk::management::{LimitViolation, ManagementError, ManagementResource};
use toolkit_db::DbError;
use toolkit_macros::domain_model;
use uuid::Uuid;

use super::authz::AuthzError;
use crate::domain::model::{BundleId, VersionId};
use crate::domain::repos::{RepoError, conflict};
use crate::domain::validation::Finding;

/// What a `not_found` names.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Subject {
    /// A bundle.
    Bundle(BundleId),
    /// A bundle version.
    Version(VersionId),
    /// An owning tenant the caller may not manage.
    Tenant(Uuid),
}

/// The error type of a management transaction: a [`ManagementError`] that
/// a database failure (begin, commit) also converts into.
#[domain_model]
#[derive(Debug, Clone)]
pub struct ManagementFailure(pub ManagementError);

impl From<ManagementError> for ManagementFailure {
    fn from(err: ManagementError) -> Self {
        Self(err)
    }
}

impl From<ManagementFailure> for ManagementError {
    fn from(failure: ManagementFailure) -> Self {
        failure.0
    }
}

// The database error is logged in full and replaced by a fixed internal
// error: storage detail never reaches a caller.
#[allow(unknown_lints, de1302_error_from_to_string)]
impl From<DbError> for ManagementFailure {
    fn from(err: DbError) -> Self {
        tracing::error!(error = %err, "policy management transaction failed");
        Self(ManagementError::Internal)
    }
}

/// Content that does not exist or the caller may not see.
#[must_use]
pub fn not_found(subject: Subject) -> ManagementError {
    let (resource, id) = match subject {
        Subject::Bundle(id) => (ManagementResource::Bundle, id.0),
        Subject::Version(id) => (ManagementResource::Version, id.0),
        Subject::Tenant(id) => (ManagementResource::Tenant, id),
    };
    ManagementError::NotFound { resource, id }
}

/// The draft content exceeds content bounds; every bound is reported.
#[must_use]
pub fn content_limit_exceeded(violations: &[LimitViolation]) -> ManagementError {
    ManagementError::ContentLimitExceeded {
        violations: violations.to_vec(),
    }
}

/// Activation of a draft that fails validation, with every finding.
#[must_use]
pub fn validation_failed(findings: &[Finding]) -> ManagementError {
    ManagementError::ValidationFailed {
        findings: findings.iter().map(Finding::to_sdk).collect(),
    }
}

/// A dependency (PDP, tenant resolver, types registry) could not answer.
#[must_use]
pub fn unavailable(detail: impl std::fmt::Display) -> ManagementError {
    tracing::warn!(%detail, "policy management dependency unavailable");
    ManagementError::Unavailable
}

/// An internal failure whose detail is logged here.
#[must_use]
pub fn internal_logged(detail: impl std::fmt::Display) -> ManagementError {
    tracing::error!(%detail, "policy management internal failure");
    ManagementError::Internal
}

/// Projects an authorisation failure: `on_denied` for a denial, which each
/// caller words for what it may reveal; unavailable when the decision could
/// not be made.
pub fn authz(err: AuthzError, on_denied: impl FnOnce() -> ManagementError) -> ManagementError {
    match err {
        AuthzError::Denied => on_denied(),
        AuthzError::Unavailable(detail) => unavailable(detail),
    }
}

/// Projects a repository failure; `subject` is what an absence names.
#[must_use]
pub fn repo(err: RepoError, subject: Subject) -> ManagementError {
    match err {
        RepoError::NotFound => not_found(subject),
        RepoError::Conflict { reason } => conflict_error(reason, subject),
        RepoError::Query(query) => ManagementError::InvalidQuery {
            reason: query.to_string(),
        },
        RepoError::TooLarge => ManagementError::SubtreeTooLarge,
        RepoError::Database(detail) => internal_logged(detail),
    }
}

/// A store conflict the caller could not have avoided by itself. Conflicts
/// that name caller input (a bundle name, an assignment) are mapped where
/// that input is known.
fn conflict_error(code: &'static str, subject: Subject) -> ManagementError {
    match (code, subject) {
        (conflict::VERSION_NOT_DRAFT, _) => ManagementError::VersionNotDraft,
        (conflict::DRAFT_EXISTS, Subject::Bundle(bundle_id)) => {
            ManagementError::DraftExists { bundle_id }
        }
        // A concurrent activation committed first (one-active index):
        // re-read and retry.
        (conflict::CONCURRENT_ACTIVATION, _) => ManagementError::ConcurrentChange,
        (other, _) => internal_logged(format_args!("unexpected repository conflict {other}")),
    }
}

/// The wire `error_code` of `err`. Test-only: clients read it from the
/// `Problem`.
#[cfg(test)]
#[must_use]
pub fn reason_of(err: &ManagementError) -> Option<String> {
    toolkit_canonical_errors::Problem::from(err.clone()).error_code
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "error_tests.rs"]
mod error_tests;
