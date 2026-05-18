//! DB-error → [`DomainError`] classification ladder.
//!
//! Lives in `infra/` because the classifier reads `sea_orm::DbErr`
//! SQLSTATE codes and `modkit_db::DbError` variant discriminants —
//! both forbidden inside `domain/` by the project-wide Dylint rules
//! (`DE0301`, `DE0309`). Keeping the classifier here lets
//! `domain::error::DomainError` stay pure (no `sea_orm`/`modkit_db`
//! imports, `#[domain_model]` enforced) while still routing DB
//! failures onto the AIP-193 canonical categories.
//!
//! The boundary mapping between [`DomainError`] and the public
//! [`AccountManagementError`](account_management_sdk::error::AccountManagementError)
//! / [`CanonicalError`](modkit_canonical_errors::CanonicalError) wire
//! envelope lives in [`crate::infra::sdk_error_mapping`]. This module
//! is classifier-only.
//!
//! # Lift vs classify
//!
//! Two entry points convert raw DB errors into [`DomainError`]:
//!
//! - [`classify_db_err_to_domain`] — used by
//!   `with_serializable_retry` after the retry budget is exhausted, so
//!   the surviving SQLSTATE is final and not retryable.
//! - [`From<DbError> for DomainError`] — used by non-transactional code
//!   paths (`repo.db.conn()?`, `transaction_with_config` bodies that
//!   don't need retry); routes Sea variants through the same
//!   classifier, IO outages straight to `ServiceUnavailable`, and
//!   anything else to `Internal` with a redacted diagnostic.

use modkit_db::DbError;
use modkit_db::secure::is_unique_violation;
use sea_orm::DbErr;
use tracing::warn;

use crate::domain::error::DomainError;
use crate::infra::error_conv::{
    is_check_violation, is_db_availability_error, is_serialization_failure, redacted_db_diagnostic,
};

// ---------------------------------------------------------------------------
// DbErr → DomainError classification.
// ---------------------------------------------------------------------------

/// Classify a raw [`DbErr`] into a typed [`DomainError`].
///
/// Used by the retry helper after every retryable contention has been
/// retried and the surviving error is final. The ladder mirrors the
/// AIP-193 mapping the boundary [`From<DomainError> for CanonicalError`]
/// applies, but expressed as typed `DomainError` variants so domain
/// code stays free of `sea_orm` references:
///
/// - SQLSTATE `40001` (post-retry) → [`DomainError::Aborted`] with
///   `reason = "SERIALIZATION_CONFLICT"`.
/// - Unique-violation (`23505` / `SQLite` `2067`) →
///   [`DomainError::AlreadyExists`].
/// - Check-violation (`23514` / `SQLite` `275`) →
///   [`DomainError::Validation`]. The DB-side `CHECK` predicates are
///   the last line of defence behind the service-layer validators
///   (which can short-circuit in degraded mode when the GTS schema
///   is not yet registered); routing these to `Validation` keeps the
///   public envelope at HTTP 400 instead of collapsing to a 500 the
///   client cannot retry-correct.
/// - Typed availability signal (pool timeout, transport drop) →
///   [`DomainError::ServiceUnavailable`].
/// - Anything else → [`DomainError::Internal`] with a
///   [`redacted_db_diagnostic`] string (no DSN / driver text leaks).
#[allow(
    clippy::cognitive_complexity,
    reason = "flat classification ladder; branchy warn! paths only, no logic"
)]
pub(crate) fn classify_db_err_to_domain(db_err: DbErr) -> DomainError {
    if is_serialization_failure(&db_err) {
        warn!(
            target: "am.db",
            error = %db_err,
            "serialization failure (retry-exhausted) mapped to DomainError::Aborted"
        );
        return DomainError::Aborted {
            reason: "SERIALIZATION_CONFLICT".to_owned(),
            detail: "serialization conflict; retry budget exhausted".to_owned(),
        };
    }
    if is_unique_violation(&db_err) {
        warn!(
            target: "am.db",
            error = %db_err,
            "unique-constraint violation mapped to DomainError::AlreadyExists"
        );
        return DomainError::AlreadyExists {
            detail: "request conflicts with existing state".to_owned(),
        };
    }
    if is_check_violation(&db_err) {
        // The driver-emitted constraint name can carry schema-internal
        // structure (`ck_tenants_root_depth`,
        // `ck_conversion_requests_actor_invariant`) so we log it on
        // `am.db` for operator correlation but keep the public
        // `Validation` detail generic — the canonical-errors boundary
        // ships `Validation::detail` straight into the public
        // `Problem.detail` field via `with_field_violation`.
        warn!(
            target: "am.db",
            error = %db_err,
            "check-constraint violation mapped to DomainError::Validation"
        );
        return DomainError::Validation {
            detail: "request violates a server-side validation constraint".to_owned(),
        };
    }
    let wrapped = DbError::Sea(db_err);
    if is_db_availability_error(&wrapped) {
        warn!(
            target: "am.db",
            diagnostic = redacted_db_diagnostic(&wrapped),
            "DB availability failure mapped to DomainError::ServiceUnavailable"
        );
        return DomainError::ServiceUnavailable {
            detail: redacted_db_diagnostic(&wrapped).to_owned(),
            retry_after: None,
            cause: None,
        };
    }
    let redacted = redacted_db_diagnostic(&wrapped);
    warn!(
        target: "am.db",
        diagnostic = redacted,
        "unclassified DB error mapped to DomainError::Internal"
    );
    DomainError::Internal {
        diagnostic: format!("unclassified database error: {redacted}"),
        cause: None,
    }
}

// ---------------------------------------------------------------------------
// DbError → DomainError lift.
// ---------------------------------------------------------------------------

impl From<DbError> for DomainError {
    /// Lift a [`DbError`] into the appropriate domain-internal variant.
    ///
    /// Routing:
    ///
    /// * `DbError::Sea(_)` → [`classify_db_err_to_domain`]. Non-transactional
    ///   paths (`repo.db.conn()?`) classify eagerly because there is no
    ///   retry helper to consult the raw `DbErr`.
    /// * Non-Sea variants that satisfy [`is_db_availability_error`]
    ///   (currently `DbError::Io(_)`) → [`DomainError::ServiceUnavailable`]
    ///   directly. They don't carry a `DbErr` for retry to inspect,
    ///   but they signal a transient infra outage that must surface
    ///   as HTTP 503, not HTTP 500.
    /// * Everything else → [`DomainError::Internal`] with a
    ///   [`redacted_db_diagnostic`] string. The raw error is preserved
    ///   on the `cause` chain for the audit trail, but the
    ///   user-visible diagnostic carries only the variant kind so
    ///   DSN / env-var / driver text cannot leak.
    fn from(err: DbError) -> Self {
        match err {
            DbError::Sea(db) => classify_db_err_to_domain(db),
            other if is_db_availability_error(&other) => Self::ServiceUnavailable {
                detail: redacted_db_diagnostic(&other).to_owned(),
                retry_after: None,
                cause: Some(Box::new(other)),
            },
            other => Self::Internal {
                diagnostic: redacted_db_diagnostic(&other).to_owned(),
                cause: Some(Box::new(other)),
            },
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "canonical_mapping_tests.rs"]
mod canonical_mapping_tests;
