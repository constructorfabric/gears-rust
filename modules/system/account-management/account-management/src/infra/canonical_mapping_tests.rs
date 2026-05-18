//! Tests for the DB-error → `DomainError` classification ladder.
//!
//! Lives in `infra/` so the test code can import `sea_orm::DbErr`
//! and `modkit_db::DbError` directly — both forbidden inside `domain/`
//! by Dylint rules. The tests pin the contract that
//! `with_serializable_retry`'s post-retry classifier and
//! `From<DbError> for DomainError` produce the right typed
//! `DomainError` variants for each SQLSTATE / outage signal.

use modkit_canonical_errors::{CanonicalError, Problem};

use super::classify_db_err_to_domain;
use crate::domain::error::DomainError;
use crate::infra::sdk_error_mapping::account_management_error_to_canonical;

/// Route a `DomainError` through the two-hop pipeline
/// (`DomainError → AccountManagementError → CanonicalError`). The
/// temporary `From<DomainError> for CanonicalError` bridge has been
/// retired; tests that pin the wire-envelope shape must drive both
/// hops explicitly.
fn canonical_of(domain: DomainError) -> CanonicalError {
    account_management_error_to_canonical(domain.into())
}

#[test]
fn classify_serialization_conflict_yields_aborted() {
    use sea_orm::{DbErr, RuntimeErr};
    // Mirrors `infra::error_conv::is_serialization_failure` detection:
    // a Postgres SQLSTATE 40001 surfaced through `RuntimeErr::Internal`
    // after `with_serializable_retry` exhausted its budget.
    let db_err = DbErr::Exec(RuntimeErr::Internal(
        "error returned from database: error with SQLSTATE 40001".into(),
    ));
    let domain = classify_db_err_to_domain(db_err);
    let DomainError::Aborted { reason, .. } = domain else {
        panic!("expected DomainError::Aborted");
    };
    assert_eq!(reason, "SERIALIZATION_CONFLICT");
}

#[test]
fn classify_serialization_conflict_canonical_status_409() {
    use sea_orm::{DbErr, RuntimeErr};
    let db_err = DbErr::Exec(RuntimeErr::Internal(
        "error returned from database: error with SQLSTATE 40001".into(),
    ));
    let canonical = canonical_of(classify_db_err_to_domain(db_err));
    assert_eq!(canonical.status_code(), 409);
    let CanonicalError::Aborted { ctx, .. } = canonical else {
        panic!("expected Aborted");
    };
    assert_eq!(ctx.reason, "SERIALIZATION_CONFLICT");
}

#[test]
fn classify_unique_violation_yields_already_exists() {
    use sea_orm::{DbErr, RuntimeErr};
    // String-based fallback path of `is_unique_violation` — Postgres
    // duplicate-key text surfaced through `RuntimeErr::Internal`.
    let db_err = DbErr::Exec(RuntimeErr::Internal(
        "duplicate key value violates unique constraint \"ux_tenants\"".into(),
    ));
    let domain = classify_db_err_to_domain(db_err);
    assert!(matches!(domain, DomainError::AlreadyExists { .. }));
    let canonical = canonical_of(domain);
    assert_eq!(canonical.status_code(), 409);
    assert!(matches!(canonical, CanonicalError::AlreadyExists { .. }));
}

#[test]
fn classify_check_violation_yields_validation_400() {
    use sea_orm::{DbErr, RuntimeErr};
    // String-based detection of a Postgres CHECK violation surfaced
    // through `RuntimeErr::Internal`. Without this classification the
    // failure would fall to the unclassified arm → `Internal` (HTTP
    // 500); the regression guard pins HTTP 400 + `Validation` so a
    // degraded-mode short-circuit in `validate_tenant_name_via_gts`
    // (schema not yet registered → `Ok(())`) still produces a
    // client-actionable error when the DB-side CHECK fires.
    let db_err = DbErr::Exec(RuntimeErr::Internal(
        "new row for relation \"tenants\" violates check constraint \"ck_tenants_root_depth\""
            .into(),
    ));
    let domain = classify_db_err_to_domain(db_err);
    assert!(
        matches!(domain, DomainError::Validation { .. }),
        "CHECK constraint violations MUST map to Validation, got {domain:?}"
    );
    let canonical = canonical_of(domain);
    assert_eq!(canonical.status_code(), 400);
}

#[test]
fn classify_check_violation_sqlite_lowercased_message_yields_validation() {
    use sea_orm::{DbErr, RuntimeErr};
    // SQLite emits the constraint failure with different capitalisation
    // ("CHECK constraint failed: …"); the lowercase substring search
    // in `is_check_violation` MUST catch both. Pin the SQLite shape
    // explicitly so a future refactor that swaps to typed
    // `SqlErr::CheckConstraintViolation` detection without keeping the
    // fallback does not regress on this engine.
    let db_err = DbErr::Exec(RuntimeErr::Internal(
        "CHECK constraint failed: ck_conversion_requests_actor_invariant".into(),
    ));
    let domain = classify_db_err_to_domain(db_err);
    assert!(matches!(domain, DomainError::Validation { .. }));
}

#[test]
fn classify_availability_yields_service_unavailable() {
    use sea_orm::{ConnAcquireErr, DbErr};
    let db_err = DbErr::ConnectionAcquire(ConnAcquireErr::Timeout);
    let domain = classify_db_err_to_domain(db_err);
    assert!(matches!(domain, DomainError::ServiceUnavailable { .. }));
    let canonical = canonical_of(domain);
    assert_eq!(canonical.status_code(), 503);
}

#[test]
fn idp_unavailable_maps_to_503() {
    // `DomainError::IdpUnavailable` is a dedicated retry-loop sentinel
    // for the bootstrap saga; at the public boundary it collapses back
    // onto the same AIP-193 `ServiceUnavailable` envelope as the
    // generic variant. This regression guard pins two halves of the
    // contract: the HTTP status stays 503 AND the public `detail` is
    // **redacted** to a stable generic string (vendor / SDK / endpoint
    // text from `IdpProvisionFailure::detail` is operator-meaningful but
    // not public-contract and would otherwise leak through the
    // Problem envelope — see `canonical_mapping::IdpUnavailable` arm).
    let domain = DomainError::IdpUnavailable {
        detail: "idp probe timed out (vendor=keycloak, host=internal.example)".to_owned(),
    };
    let canonical = canonical_of(domain);
    assert_eq!(canonical.status_code(), 503);
    // Redaction contract: if the canonical mapping ever stops
    // redacting, vendor strings would surface in public Problem
    // envelopes and the bootstrap retry loop's "match on the typed
    // variant, not on detail text" invariant would be silently
    // weakened. Pin the public string so a future regression that
    // forwards the upstream detail verbatim fails this guard.
    assert_eq!(
        canonical.detail(),
        "IdP plugin unavailable",
        "canonical detail must collapse to the generic public string; provider detail goes to am.domain log only"
    );
}

#[test]
fn classify_unclassified_yields_internal() {
    use sea_orm::DbErr;
    let db_err = DbErr::Custom("unclassified".into());
    let domain = classify_db_err_to_domain(db_err);
    assert!(matches!(domain, DomainError::Internal { .. }));
    let canonical = canonical_of(domain);
    assert_eq!(canonical.status_code(), 500);
}

#[test]
fn dberror_sea_routes_through_classifier() {
    use modkit_db::DbError;
    use sea_orm::DbErr;
    // `DbError::Sea(_)` non-transactional path runs through
    // `classify_db_err_to_domain`; an unclassified inner `DbErr`
    // therefore lands in `Internal`.
    let lifted: DomainError = DbError::Sea(DbErr::Custom("any".into())).into();
    assert!(matches!(lifted, DomainError::Internal { .. }));
}

#[test]
fn dberror_io_routes_to_service_unavailable() {
    use modkit_db::DbError;
    // Regression guard: a transient IO outage MUST surface as 503
    // (ServiceUnavailable), not 500 (Internal). Earlier the `non-Sea`
    // arm fell through to `Internal` and lost the availability signal.
    let io_err = std::io::Error::new(std::io::ErrorKind::ConnectionReset, "reset by peer");
    let lifted: DomainError = DbError::Io(io_err).into();
    let canonical = canonical_of(lifted);
    assert_eq!(canonical.status_code(), 503);
    assert!(matches!(
        canonical,
        CanonicalError::ServiceUnavailable { .. }
    ));
}

#[test]
fn dberror_other_routes_to_internal_with_redacted_diagnostic() {
    use modkit_db::DbError;
    // Non-Sea, non-availability variants fall through to `Internal`.
    // The diagnostic field MUST come from `redacted_db_diagnostic`
    // (no raw DSN / config text leaks).
    let lifted: DomainError =
        DbError::UnknownDsn("postgres://secret_user:secret_pass@host/db".into()).into();
    let canonical = canonical_of(lifted);
    assert_eq!(canonical.status_code(), 500);
    let CanonicalError::Internal { ctx, .. } = canonical else {
        panic!("expected Internal");
    };
    let description = &ctx.description;
    assert!(
        !description.contains("secret_user"),
        "raw DSN leaked into description: {description}"
    );
    assert!(
        !description.contains("secret_pass"),
        "raw DSN leaked into description: {description}"
    );
    assert!(
        description.contains("redacted"),
        "description must come from redacted_db_diagnostic: {description}"
    );
}

/// `DomainError::Internal { diagnostic, .. }` carries the diagnostic
/// string as input to `CanonicalError::internal(...)`. The
/// `feature-errors-observability` contract states the diagnostic
/// **MUST NOT** reach the public `Problem` body — it is meant for
/// audit-only consumption (the `Internal` variant's docstring on
/// `domain::error::DomainError` says so explicitly).
///
/// Pin that contract here by constructing an `Internal` with a
/// distinctive sentinel diagnostic, lifting it into a
/// `CanonicalError`, and asserting the sentinel never appears in the
/// JSON-serialized envelope. This guards against any future change to
/// `modkit_canonical_errors::context::InternalV1` that would drop the
/// `#[serde(skip)]` on `description` and start leaking diagnostics
/// into HTTP responses.
#[test]
fn internal_diagnostic_is_not_serialized_into_canonical_envelope() {
    let sentinel = "INTERNAL-DIAGNOSTIC-SENTINEL-7f3a2c";
    let domain = DomainError::Internal {
        diagnostic: sentinel.to_owned(),
        cause: None,
    };
    let canonical = canonical_of(domain);
    // The public HTTP boundary serializes through `Problem` (RFC 9457
    // envelope), not `CanonicalError` directly. Drive the conversion
    // and serde-encode the resulting envelope to mirror what a REST
    // consumer actually receives over the wire.
    let problem = Problem::from(canonical);
    let envelope = serde_json::to_string(&problem).expect("problem envelope must serialize");
    assert!(
        !envelope.contains(sentinel),
        "Internal diagnostic leaked into the public Problem envelope: {envelope}"
    );
}
