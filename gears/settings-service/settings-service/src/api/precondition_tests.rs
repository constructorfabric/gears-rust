// Created: 2026-08-12 by Constructor Tech
//! Tests for conditional-write evaluation.
//!
//! Acceptance criteria: FEATURE `gear-foundation.md` §6 — a mutating request
//! without `If-Match` is refused, and a stale `If-Match` is refused
//! distinguishably.

use serde_json::Value;
use toolkit_canonical_errors::{CanonicalError, Problem};

use super::{ETag, evaluate};
use crate::domain::error::DomainError;
use crate::field;

fn status_of(err: DomainError) -> u64 {
    let doc: Value =
        serde_json::to_value(Problem::from(CanonicalError::from(err))).expect("serializes");
    doc["status"].as_u64().expect("status is a number")
}

#[test]
fn a_matching_tag_proceeds_and_carries_the_tag_forward() {
    let current = ETag::new("v1");
    let proceed = evaluate(Some("v1"), &current).expect("matching tag proceeds");
    assert_eq!(
        proceed.current, current,
        "the handler must echo the tag the check passed against, not recompute one"
    );
}

#[test]
fn an_absent_header_is_refused_as_precondition_required() {
    let err = evaluate(None, &ETag::new("v1")).expect_err("must be refused");
    assert!(matches!(err, DomainError::PreconditionRequired { .. }));
}

#[test]
fn a_stale_tag_is_refused_as_precondition_failed() {
    let err = evaluate(Some("v1"), &ETag::new("v2")).expect_err("must be refused");
    assert!(matches!(err, DomainError::PreconditionFailed { .. }));
}

#[test]
fn absent_and_stale_are_different_statuses() {
    // The distinction the whole module exists for. A client that got the same
    // status for both would either retry a request it must first fix, or give up
    // on a race it should have re-read and won.
    let absent = status_of(evaluate(None, &ETag::new("v1")).unwrap_err());
    let stale = status_of(evaluate(Some("old"), &ETag::new("v1")).unwrap_err());

    assert_eq!(
        absent, 428,
        "a missing If-Match is 428 Precondition Required"
    );
    assert_eq!(stale, 412, "a stale If-Match is 412 Precondition Failed");
    assert_ne!(absent, stale);
}

#[test]
fn neither_status_is_the_category_default() {
    // Both categories default to 400. Without the explicit transport overrides
    // these would be indistinguishable from a malformed body, so this pins that
    // the overrides are actually applied rather than silently dropped.
    let absent = CanonicalError::from(evaluate(None, &ETag::new("v1")).unwrap_err());
    let stale = CanonicalError::from(evaluate(Some("old"), &ETag::new("v1")).unwrap_err());
    assert_ne!(absent.status_code(), 400);
    assert_ne!(stale.status_code(), 400);
}

#[test]
fn the_missing_header_violation_names_the_header() {
    // A caller reading the problem document must be able to see which header to
    // add without consulting prose.
    let doc: Value = serde_json::to_value(Problem::from(CanonicalError::from(
        evaluate(None, &ETag::new("v1")).unwrap_err(),
    )))
    .expect("serializes");
    let violation = &doc["context"]["field_violations"][0];
    assert_eq!(violation["field"], super::IF_MATCH_HEADER);
    assert_eq!(violation["reason"], field::IF_MATCH_REQUIRED);
}

#[test]
fn a_tag_is_compared_verbatim() {
    // Tags are opaque. Trimming or unquoting here would make two spellings of
    // one tag and admit a write based on state the caller never read.
    for supplied in [" v1", "v1 ", "\"v1\"", "V1"] {
        assert!(
            evaluate(Some(supplied), &ETag::new("v1")).is_err(),
            "`{supplied}` must not be treated as equal to `v1`"
        );
    }
}

#[test]
fn an_empty_header_is_stale_not_absent() {
    // `If-Match: ` is a supplied header carrying a tag that matches nothing. The
    // client did opt into the check, so this is a failed precondition rather
    // than a request it forgot to make.
    let err = evaluate(Some(""), &ETag::new("v1")).expect_err("must be refused");
    assert!(matches!(err, DomainError::PreconditionFailed { .. }));
}
