//! Tests for the bounded failure-to-metric label mapping.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::borrow::Cow;

use super::{ItemFailure, reason_label};

#[test]
fn a_borrowed_literal_reason_is_its_own_label() {
    for literal in [
        "precondition_failed",
        "dependent_invalid",
        "revalidation_exhausted",
        "activation_write_set_exceeded",
    ] {
        assert_eq!(reason_label(&Cow::Borrowed(literal)), literal);
    }
}

#[test]
fn an_owned_reason_counts_under_the_closed_other_label() {
    let failure = ItemFailure::from_payload(
        r#"{"reason":"precondition_failed","message":"read back off a stored row"}"#,
    );
    assert!(
        matches!(failure.reason, Cow::Owned(_)),
        "from_payload is the owned-reason producer the mapping exists for"
    );
    assert_eq!(reason_label(&failure.reason), "other");
}
