//! Unit tests for the typed annotation overlay's pre-write screen + the
//! `AnnotationTarget` parse. The transactional upsert/audit path is exercised
//! end-to-end in `tests/postgres_entry_annotation.rs` (testcontainers).
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use super::{AnnotationService, AnnotationTarget};
use crate::domain::error::DomainError;

/// `target_kind` parses `ENTRY` / `LINE` and rejects anything else.
#[test]
fn target_parse_round_trips() {
    assert_eq!(
        AnnotationTarget::parse("ENTRY").unwrap(),
        AnnotationTarget::Entry
    );
    assert_eq!(
        AnnotationTarget::parse("LINE").unwrap(),
        AnnotationTarget::Line
    );
    assert_eq!(AnnotationTarget::Entry.as_str(), "ENTRY");
    assert_eq!(AnnotationTarget::Line.as_str(), "LINE");
    assert!(matches!(
        AnnotationTarget::parse("entry"),
        Err(DomainError::InvalidRequest(_))
    ));
    assert!(matches!(
        AnnotationTarget::parse("BOGUS"),
        Err(DomainError::InvalidRequest(_))
    ));
}

/// `AnnotationService::new` / `default` build without a DB (stateless audit store).
#[test]
fn service_builds_stateless() {
    let _ = AnnotationService::new();
    let _ = AnnotationService::default();
}

/// A clean free-text `description` passes the pre-write PII screen.
#[test]
fn screen_accepts_clean_description() {
    let v = serde_json::json!("reconciled against export 2026-06 batch 42");
    assert!(
        AnnotationService::screen_description_for_pii(&v).is_ok(),
        "a benign note must pass the PII screen"
    );
}

/// A `description` carrying an email is rejected BEFORE any write.
#[test]
fn screen_rejects_email_in_description() {
    let v = serde_json::json!("refund requested by jane.doe@example.com");
    assert!(
        matches!(
            AnnotationService::screen_description_for_pii(&v),
            Err(DomainError::PiiInMetadataValue(_))
        ),
        "an email in the description must be rejected as PiiInMetadataValue"
    );
}

/// A prohibited PII KEY (object value) is also rejected by the screen.
#[test]
fn screen_rejects_prohibited_key() {
    let v = serde_json::json!({ "customer_name": "Ada Lovelace" });
    assert!(
        matches!(
            AnnotationService::screen_description_for_pii(&v),
            Err(DomainError::PiiInMetadataValue(_))
        ),
        "a prohibited PII key must be rejected as PiiInMetadataValue"
    );
}
