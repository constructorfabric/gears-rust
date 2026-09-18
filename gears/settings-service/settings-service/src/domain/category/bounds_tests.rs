// Created: 2026-08-25 by Constructor Tech
//! Tests for the descriptive-field bounds.
//!
//! What these pin is that the check answers *before* the driver does. Without
//! it an over-long value is a `500` on Postgres and a stored row on `SQLite`, so
//! the bound is what makes the two environments agree.

use super::{DESCRIPTION_FIELD, NAME_FIELD, validate};
use crate::domain::error::DomainError;

fn field_of(err: DomainError) -> String {
    match err {
        DomainError::Validation { field, .. } => field,
        other => panic!("expected a validation failure, got {other:?}"),
    }
}

#[test]
fn a_name_within_the_bound_is_accepted() {
    assert!(validate("Network", None).is_ok());
}

#[test]
fn an_empty_name_is_refused() {
    // The column is NOT NULL, but an empty string would satisfy that and leave
    // a category no administrator can identify in a list.
    assert_eq!(field_of(validate("", None).unwrap_err()), NAME_FIELD);
}

#[test]
fn a_name_over_the_bound_is_refused() {
    let long = "n".repeat(257);
    assert_eq!(field_of(validate(&long, None).unwrap_err()), NAME_FIELD);
}

#[test]
fn the_name_bound_is_inclusive_at_both_ends() {
    assert!(validate("n", None).is_ok());
    assert!(validate(&"n".repeat(256), None).is_ok());
}

#[test]
fn the_name_bound_counts_characters_not_bytes() {
    // 256 accented characters are 512 bytes. Counting bytes would refuse a name
    // whose author sees exactly the permitted length.
    let multibyte = "\u{e9}".repeat(256);
    assert!(multibyte.len() > 256, "the fixture is multi-byte");
    assert!(validate(&multibyte, None).is_ok());
}

#[test]
fn an_absent_description_is_accepted() {
    assert!(validate("Network", None).is_ok());
}

#[test]
fn an_empty_description_is_accepted() {
    // The bound is an upper one only: the column is nullable and an empty
    // description is a caller declining to write one, not a violation.
    assert!(validate("Network", Some("")).is_ok());
}

#[test]
fn a_description_over_the_bound_is_refused() {
    let long = "d".repeat(4097);
    assert_eq!(
        field_of(validate("Network", Some(&long)).unwrap_err()),
        DESCRIPTION_FIELD
    );
}

#[test]
fn the_description_bound_is_inclusive() {
    assert!(validate("Network", Some(&"d".repeat(4096))).is_ok());
}

#[test]
fn the_name_is_reported_when_both_fields_break_their_bounds() {
    // One violation per response, and the name is the one a caller is likelier
    // to have gotten wrong -- a description overrun is usually a paste.
    let err = validate("", Some(&"d".repeat(4097))).unwrap_err();
    assert_eq!(field_of(err), NAME_FIELD);
}
