//! The projection of management conditions onto the SDK's error table
//! ([`policy_engine_sdk::management`] module documentation).
//!
//! | Condition | Category | Reason code |
//! |---|---|---|
//! | A concurrent change the store refused | `aborted` | `CONCURRENT_CHANGE` |
//! | Modification of an active or superseded version | `aborted` | `VERSION_NOT_DRAFT` |
//! | Activation of a draft that fails validation | `failed_precondition`, one violation per finding (type = code) | `VALIDATION_FAILED` |
//! | Bundle name used in the owning tenant; a second open draft | `already_exists` (the code prefixes the detail; the category has no reason field) | `BUNDLE_NAME_TAKEN`, `DRAFT_EXISTS` |
//! | Caller lacks the capability | `permission_denied` | `CAPABILITY_DENIED` |
//! | Malformed input | `invalid_argument` (field violation) | `SEED_NOT_IN_BUNDLE`, `CONTENT_LIMIT_EXCEEDED`, `DUPLICATE_DOCUMENT_NAME` |
//! | Content the caller may not see, or that does not exist | `not_found` | - |
//! | The PDP, the tenant resolver or the types registry could not answer | `service_unavailable` | - |
//! | Storage failure or an undecodable row | `internal` (detail logged, never returned) | - |

use policy_engine_sdk::management::{ManagementError, reason};
use toolkit_canonical_errors::{CanonicalError, Problem};
use toolkit_db::DbError;
use toolkit_macros::domain_model;
use uuid::Uuid;

use crate::domain::model::LimitViolation;
use crate::domain::repos::{RepoError, conflict};
use crate::domain::validation::Finding;

pub use families::{AssignmentResourceError, BundleResourceError, VersionResourceError};

// The `resource_error` expansion generates undocumented public constructors.
#[allow(missing_docs)]
mod families {
    use toolkit_canonical_errors::resource_error;

    /// Error family of bundles, `gts.cf.core.policy_engine.bundle.v1~`
    /// ([`BUNDLE_RESOURCE`](policy_engine_sdk::BUNDLE_RESOURCE)).
    #[resource_error(gts_id!("cf.core.policy_engine.bundle.v1~"))]
    pub struct BundleResourceError;

    /// Error family of bundle versions,
    /// `gts.cf.core.policy_engine.bundle_version.v1~`
    /// ([`BUNDLE_VERSION_RESOURCE`](policy_engine_sdk::BUNDLE_VERSION_RESOURCE)).
    #[resource_error(gts_id!("cf.core.policy_engine.bundle_version.v1~"))]
    pub struct VersionResourceError;

    /// Error family of assignments, `gts.cf.core.policy_engine.assignment.v1~`
    /// ([`ASSIGNMENT_RESOURCE`](policy_engine_sdk::ASSIGNMENT_RESOURCE)).
    #[resource_error(gts_id!("cf.core.policy_engine.assignment.v1~"))]
    pub struct AssignmentResourceError;
}

/// What a `not_found` names.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Subject {
    /// A bundle.
    Bundle(Uuid),
    /// A bundle version.
    Version(Uuid),
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
        Self(internal())
    }
}

/// Content that does not exist or the caller may not see.
#[must_use]
pub fn not_found(subject: Subject) -> ManagementError {
    match subject {
        Subject::Bundle(id) => BundleResourceError::not_found("bundle not found")
            .with_resource(id.to_string())
            .create(),
        Subject::Version(id) => VersionResourceError::not_found("bundle version not found")
            .with_resource(id.to_string())
            .create(),
        Subject::Tenant(id) => BundleResourceError::not_found("owning tenant not found")
            .with_resource(id.to_string())
            .create(),
    }
}

/// A concurrent change committed first; re-read and retry.
#[must_use]
pub fn concurrent_change() -> ManagementError {
    BundleResourceError::aborted("a concurrent change committed first; re-read and retry")
        .with_reason(reason::CONCURRENT_CHANGE)
        .create()
}

/// The version is active or superseded and therefore immutable.
#[must_use]
pub fn version_not_draft() -> ManagementError {
    VersionResourceError::aborted("only a draft version may be changed; seed a new draft")
        .with_reason(reason::VERSION_NOT_DRAFT)
        .create()
}

/// A bundle with the name exists in the owning tenant.
#[must_use]
pub fn bundle_name_taken(name: &str) -> ManagementError {
    BundleResourceError::already_exists(format!(
        "{}: a bundle with this name already exists in the owning tenant",
        reason::BUNDLE_NAME_TAKEN
    ))
    .with_resource(name.to_owned())
    .create()
}

/// The bundle already has an open draft.
#[must_use]
pub fn draft_exists() -> ManagementError {
    VersionResourceError::already_exists(format!(
        "{}: the bundle already has an open draft; replace or delete it first",
        reason::DRAFT_EXISTS
    ))
    .with_resource("version".to_owned())
    .create()
}

/// The seed is not a retained version of the bundle (absent, invisible or
/// of another bundle: the three are indistinguishable).
#[must_use]
pub fn seed_not_in_bundle() -> ManagementError {
    invalid(
        "seed_from",
        "the seed is not a retained version of this bundle",
        reason::SEED_NOT_IN_BUNDLE,
    )
}

/// The draft content exceeds an operational limit; one violation per bound.
#[must_use]
pub fn content_limit_exceeded(violations: &[LimitViolation]) -> ManagementError {
    let describe = |v: &LimitViolation| {
        format!(
            "{:?} limit {} exceeded ({}){}",
            v.kind,
            v.limit,
            v.actual,
            v.document
                .as_deref()
                .map(|d| format!(" by document `{d}`"))
                .unwrap_or_default()
        )
    };
    let mut iter = violations.iter();
    let first = iter.next().map_or_else(
        || "content exceeds an operational limit".to_owned(),
        describe,
    );
    let mut builder = VersionResourceError::invalid_argument().with_field_violation(
        "documents",
        first,
        reason::CONTENT_LIMIT_EXCEEDED,
    );
    for violation in iter {
        builder = builder.with_field_violation(
            "documents",
            describe(violation),
            reason::CONTENT_LIMIT_EXCEEDED,
        );
    }
    builder.create()
}

/// Two documents share a name.
#[must_use]
pub fn duplicate_document_name(document: &str) -> ManagementError {
    invalid(
        "documents.name",
        format!("document name `{document}` is used more than once"),
        policy_engine_sdk::management::finding::DUPLICATE_DOCUMENT_NAME,
    )
}

/// Activation of a draft that fails validation: one violation per finding,
/// its type the finding code.
#[must_use]
pub fn validation_failed(findings: &[Finding]) -> ManagementError {
    let subject = |f: &Finding| {
        f.document_name
            .clone()
            .unwrap_or_else(|| "version".to_owned())
    };
    let describe = |f: &Finding| format!("{}: {}", f.code, f.message);
    let mut iter = findings.iter();
    let mut builder = match iter.next() {
        Some(first) => VersionResourceError::failed_precondition().with_precondition_violation(
            subject(first),
            describe(first),
            reason::VALIDATION_FAILED,
        ),
        None => VersionResourceError::failed_precondition().with_precondition_violation(
            "version",
            "the draft failed validation",
            reason::VALIDATION_FAILED,
        ),
    };
    for finding in iter {
        builder = builder.with_precondition_violation(
            subject(finding),
            describe(finding),
            reason::VALIDATION_FAILED,
        );
    }
    builder.create()
}

/// The caller does not hold the capability the operation requires.
#[must_use]
pub fn capability_denied() -> ManagementError {
    BundleResourceError::permission_denied()
        .with_reason(reason::CAPABILITY_DENIED)
        .create()
}

/// A dependency (PDP, tenant resolver, types registry) could not answer.
#[must_use]
pub fn unavailable(detail: impl std::fmt::Display) -> ManagementError {
    tracing::warn!(%detail, "policy management dependency unavailable");
    CanonicalError::service_unavailable()
        .with_detail("a dependency of policy management is unavailable; retry later")
        .create()
}

/// An internal failure; the detail is logged by the caller, never returned.
#[must_use]
pub fn internal() -> ManagementError {
    CanonicalError::internal("policy management failed internally").create()
}

/// An internal failure whose detail is logged here.
#[must_use]
pub fn internal_logged(detail: impl std::fmt::Display) -> ManagementError {
    tracing::error!(%detail, "policy management internal failure");
    internal()
}

/// A listing query refused by the store.
#[must_use]
pub fn invalid_query(err: &toolkit_odata::Error) -> ManagementError {
    invalid("query", err.to_string(), "INVALID_QUERY")
}

/// Projects a repository failure; `subject` is what an absence names.
#[must_use]
pub fn repo(err: RepoError, subject: Subject) -> ManagementError {
    match err {
        RepoError::NotFound => not_found(subject),
        RepoError::Conflict { reason } => conflict_error(reason),
        RepoError::Query(query) => invalid_query(&query),
        RepoError::Database(detail) => internal_logged(detail),
    }
}

fn conflict_error(code: &'static str) -> ManagementError {
    match code {
        conflict::VERSION_NOT_DRAFT => version_not_draft(),
        conflict::BUNDLE_NAME_TAKEN => bundle_name_taken(""),
        conflict::DRAFT_EXISTS => draft_exists(),
        conflict::DUPLICATE_DOCUMENT_NAME => duplicate_document_name(""),
        // A concurrent activation committed first (one-active index):
        // re-read and retry.
        conflict::CONCURRENT_ACTIVATION => concurrent_change(),
        other => internal_logged(format_args!("unexpected repository conflict {other}")),
    }
}

fn invalid(field: &str, description: impl Into<String>, code: &str) -> ManagementError {
    VersionResourceError::invalid_argument()
        .with_field_violation(field.to_owned(), description.into(), code.to_owned())
        .create()
}

/// The stable reason code `err` carries, wherever its category keeps it:
/// the `reason` of `aborted` / `permission_denied`, the first field
/// violation's reason of `invalid_argument`, the first precondition
/// violation's type of `failed_precondition`, the detail prefix of
/// `already_exists`. `None` for categories without one.
#[must_use]
pub fn reason_of(err: &ManagementError) -> Option<String> {
    let problem = Problem::from(err.clone());
    let context = &problem.context;
    if let Some(reason) = context.get("reason").and_then(|r| r.as_str()) {
        return Some(reason.to_owned());
    }
    if let Some(reason) = context
        .get("field_violations")
        .and_then(|v| v.get(0))
        .and_then(|v| v.get("reason"))
        .and_then(|r| r.as_str())
    {
        return Some(reason.to_owned());
    }
    if let Some(kind) = context
        .get("violations")
        .and_then(|v| v.get(0))
        .and_then(|v| v.get("type"))
        .and_then(|r| r.as_str())
    {
        return Some(kind.to_owned());
    }
    if matches!(err, CanonicalError::AlreadyExists { .. }) {
        return problem
            .detail
            .split_once(':')
            .map(|(code, _)| code.to_owned());
    }
    None
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "error_tests.rs"]
mod error_tests;
