#![allow(clippy::expect_used, clippy::unwrap_used)]

use super::*;
use crate::exact::ExactQuantity;

#[test]
fn a_non_canonical_number_is_a_violation_of_its_field() {
    let err = ExactQuantity::parse("+1").unwrap_err();
    let violation = exact_number_violation(&err);
    assert_eq!(violation.field, "quantity");
    assert_eq!(violation.reason, reason::NOT_CANONICAL_NUMBER);
    assert_eq!(violation.description, err.to_string());
    // Any resource carries it; the status is InvalidArgument's.
    let canonical = ResultResource::invalid_argument()
        .with_field_violation(violation.field, violation.description, violation.reason)
        .create();
    assert_eq!(canonical.status_code(), 400);
    assert_eq!(
        canonical.resource_type(),
        Some("gts.cf.bss.rating.result.v1~")
    );
}

#[test]
fn paging_errors_name_their_parameter() {
    let limit = paging_violation(PagingError::LimitOutOfRange);
    assert_eq!(
        (limit.field, limit.reason),
        ("limit", reason::PAGE_LIMIT_OUT_OF_RANGE)
    );
    let cursor = paging_violation(PagingError::InvalidCursor);
    assert_eq!(
        (cursor.field, cursor.reason),
        ("cursor", reason::INVALID_PAGE_CURSOR)
    );
}

#[test]
fn a_hidden_object_is_not_found() {
    let err = UsageResource::not_found("Inbox entry not found")
        .with_resource("inbox_entry")
        .create();
    assert_eq!(err.status_code(), 404);
    assert_eq!(err.resource_type(), Some("gts.cf.bss.rating.usage.v1~"));
}
