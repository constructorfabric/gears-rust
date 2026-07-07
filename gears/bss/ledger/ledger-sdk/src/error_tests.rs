//! `LedgerError` is the typed view consumers match on — its `From<CanonicalError>`
//! projection and Display strings are the contract; lock them.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use toolkit_canonical_errors::{CanonicalError, resource_error};

use super::LedgerError;

// Minimal resource stubs for constructing canonical errors in tests.
#[resource_error("gts.cf.bss.ledger.entry.v1~")]
struct TestEntry;
#[resource_error("gts.cf.bss.ledger.ledger.v1~")]
struct TestLedger;
#[resource_error("gts.cf.bss.ledger.fiscal_period.v1~")]
struct TestFiscalPeriod;

// ---------------------------------------------------------------------------
// Display
// ---------------------------------------------------------------------------

#[test]
fn display_invalid_argument() {
    let e = LedgerError::InvalidArgument {
        field: "lines".into(),
        code: "UNBALANCED".into(),
        detail: "dr!=cr".into(),
    };
    let s = e.to_string();
    assert!(s.contains("lines"), "{s}");
    assert!(s.contains("UNBALANCED"), "{s}");
    assert!(s.contains("dr!=cr"), "{s}");
}

#[test]
fn display_failed_precondition() {
    let e = LedgerError::FailedPrecondition {
        subject: "fiscal_period".into(),
        code: "PERIOD_CLOSED".into(),
        detail: "closed at 2025-12-31".into(),
    };
    let s = e.to_string();
    assert!(s.contains("fiscal_period"), "{s}");
    assert!(s.contains("PERIOD_CLOSED"), "{s}");
}

#[test]
fn display_aborted() {
    let e = LedgerError::Aborted {
        code: "IDEMPOTENCY_PAYLOAD_CONFLICT".into(),
        detail: "duplicate key".into(),
    };
    let s = e.to_string();
    assert!(s.contains("IDEMPOTENCY_PAYLOAD_CONFLICT"), "{s}");
    assert!(s.contains("duplicate key"), "{s}");
}

#[test]
fn display_not_found() {
    let e = LedgerError::NotFound {
        resource_type: "fiscal_period".into(),
        resource_name: "2025-Q4".into(),
        detail: "no such period".into(),
    };
    let s = e.to_string();
    assert!(s.contains("fiscal_period"), "{s}");
    assert!(s.contains("2025-Q4"), "{s}");
}

#[test]
fn display_resource_exhausted() {
    let e = LedgerError::ResourceExhausted {
        code: "TENANT_POSTING_LOCKED".into(),
        detail: "backpressure".into(),
    };
    let s = e.to_string();
    assert!(s.contains("TENANT_POSTING_LOCKED"), "{s}");
}

#[test]
fn display_permission_denied() {
    let e = LedgerError::PermissionDenied {
        reason: "INSUFFICIENT_SCOPE".into(),
        detail: "missing ledger.write".into(),
    };
    let s = e.to_string();
    assert!(s.contains("INSUFFICIENT_SCOPE"), "{s}");
}

#[test]
fn display_unauthenticated() {
    let e = LedgerError::Unauthenticated {
        detail: "no bearer token".into(),
    };
    let s = e.to_string();
    assert!(s.contains("no bearer token"), "{s}");
}

#[test]
fn display_unavailable() {
    let e = LedgerError::Unavailable {
        detail: "db offline".into(),
    };
    let s = e.to_string();
    assert!(s.contains("db offline"), "{s}");
}

#[test]
fn display_internal() {
    let e = LedgerError::Internal {
        detail: "unexpected state".into(),
    };
    let s = e.to_string();
    assert!(s.contains("unexpected state"), "{s}");
}

// ---------------------------------------------------------------------------
// From<CanonicalError> projection
// ---------------------------------------------------------------------------

#[test]
fn projects_invalid_argument_field_violation() {
    let canonical = TestEntry::invalid_argument()
        .with_field_violation(
            "lines",
            "debit does not equal credit",
            "LEDGER_ENTRY_UNBALANCED",
        )
        .create();
    match LedgerError::from(canonical) {
        LedgerError::InvalidArgument {
            field,
            code,
            detail,
        } => {
            assert_eq!(field, "lines");
            assert_eq!(code, "LEDGER_ENTRY_UNBALANCED");
            assert_eq!(detail, "debit does not equal credit");
        }
        other => panic!("wrong projection: {other:?}"),
    }
}

#[test]
fn projects_invalid_argument_format_to_empty_field_code() {
    // Format arm: resource_error builder with_format → Format variant.
    let canonical = TestLedger::invalid_argument()
        .with_format("malformed request body")
        .create();
    match LedgerError::from(canonical) {
        LedgerError::InvalidArgument { field, code, .. } => {
            assert!(
                field.is_empty(),
                "field should be empty for Format, got {field:?}"
            );
            assert!(
                code.is_empty(),
                "code should be empty for Format, got {code:?}"
            );
        }
        other => panic!("wrong projection: {other:?}"),
    }
}

#[test]
fn projects_invalid_argument_constraint_to_empty_field_code() {
    let canonical = TestLedger::invalid_argument()
        .with_constraint("constraint violated")
        .create();
    match LedgerError::from(canonical) {
        LedgerError::InvalidArgument { field, code, .. } => {
            assert!(
                field.is_empty(),
                "field should be empty for Constraint, got {field:?}"
            );
            assert!(
                code.is_empty(),
                "code should be empty for Constraint, got {code:?}"
            );
        }
        other => panic!("wrong projection: {other:?}"),
    }
}

#[test]
fn projects_failed_precondition() {
    let canonical = TestFiscalPeriod::failed_precondition()
        .with_precondition_violation("fiscal_period", "period is closed", "PERIOD_CLOSED")
        .create();
    match LedgerError::from(canonical) {
        LedgerError::FailedPrecondition {
            subject,
            code,
            detail,
        } => {
            assert_eq!(subject, "fiscal_period");
            assert_eq!(code, "PERIOD_CLOSED");
            assert_eq!(detail, "period is closed");
        }
        other => panic!("wrong projection: {other:?}"),
    }
}

#[test]
fn projects_aborted() {
    let canonical = TestEntry::aborted("duplicate idempotency key")
        .with_reason("IDEMPOTENCY_PAYLOAD_CONFLICT")
        .create();
    match LedgerError::from(canonical) {
        LedgerError::Aborted { code, detail } => {
            assert_eq!(code, "IDEMPOTENCY_PAYLOAD_CONFLICT");
            assert_eq!(detail, "duplicate idempotency key");
        }
        other => panic!("wrong projection: {other:?}"),
    }
}

#[test]
fn projects_not_found_with_resource_type_and_name() {
    let canonical = TestFiscalPeriod::not_found("period not found")
        .with_resource("2025-Q4")
        .create();
    match LedgerError::from(canonical) {
        LedgerError::NotFound {
            resource_type,
            resource_name,
            detail,
        } => {
            // resource_type comes from gts_type prefix; name is the resource.
            assert!(
                !resource_type.is_empty(),
                "resource_type should not be empty"
            );
            assert_eq!(resource_name, "2025-Q4");
            assert_eq!(detail, "period not found");
        }
        other => panic!("wrong projection: {other:?}"),
    }
}

#[test]
fn projects_not_found_missing_resource_name_to_empty() {
    // not_found with empty resource string → resource_name = "".
    let canonical = TestFiscalPeriod::not_found("not found")
        .with_resource("")
        .create();
    match LedgerError::from(canonical) {
        LedgerError::NotFound { resource_name, .. } => {
            assert!(resource_name.is_empty());
        }
        other => panic!("wrong projection: {other:?}"),
    }
}

#[test]
fn projects_resource_exhausted() {
    let canonical = TestEntry::resource_exhausted("posting quota exceeded")
        .with_quota_violation("TENANT_POSTING_LOCKED", "tenant is locked")
        .create();
    match LedgerError::from(canonical) {
        LedgerError::ResourceExhausted { code, detail } => {
            assert_eq!(code, "TENANT_POSTING_LOCKED");
            assert_eq!(detail, "posting quota exceeded");
        }
        other => panic!("wrong projection: {other:?}"),
    }
}

#[test]
fn projects_permission_denied() {
    let canonical = TestEntry::permission_denied()
        .with_reason("INSUFFICIENT_SCOPE")
        .create();
    match LedgerError::from(canonical) {
        LedgerError::PermissionDenied { reason, .. } => {
            assert_eq!(reason, "INSUFFICIENT_SCOPE");
        }
        other => panic!("wrong projection: {other:?}"),
    }
}

#[test]
fn projects_unauthenticated() {
    let canonical = CanonicalError::unauthenticated()
        .with_reason("NO_BEARER")
        .create();
    match LedgerError::from(canonical) {
        LedgerError::Unauthenticated { detail } => {
            assert!(!detail.is_empty());
        }
        other => panic!("wrong projection: {other:?}"),
    }
}

#[test]
fn projects_service_unavailable() {
    let canonical = CanonicalError::service_unavailable()
        .with_detail("db offline")
        .create();
    match LedgerError::from(canonical) {
        LedgerError::Unavailable { detail } => {
            assert_eq!(detail, "db offline");
        }
        other => panic!("wrong projection: {other:?}"),
    }
}

#[test]
fn projects_internal() {
    let canonical = CanonicalError::internal("something went wrong").create();
    match LedgerError::from(canonical) {
        LedgerError::Internal { detail } => {
            // Internal detail is the redacted string (not the description).
            assert!(!detail.is_empty());
        }
        other => panic!("wrong projection: {other:?}"),
    }
}

#[test]
fn projects_unmodelled_category_to_other() {
    // Cancelled is not modelled in LedgerError — must fall through to Other.
    // We build it via the standard io::Error From impl that produces Internal,
    // then use the Cancelled category which has no explicit arm in LedgerError::from.
    // Use serde_json::Error(io::Error) path that yields Internal (already tested).
    // Instead, build Cancelled via a resource_error struct that exposes no public
    // cancelled() fn — fall back to AlreadyExists which also has no LedgerError arm.
    // AlreadyExists is not modelled in LedgerError — must fall through to Other.
    let canonical = TestEntry::already_exists("dup entry")
        .with_resource("entry-123")
        .create();
    match LedgerError::from(canonical) {
        LedgerError::Other { .. } => {}
        other => panic!("expected Other for AlreadyExists, got {other:?}"),
    }
}
