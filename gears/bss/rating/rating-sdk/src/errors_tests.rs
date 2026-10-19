#![allow(clippy::expect_used, clippy::unwrap_used)]

use super::{ResultResource, UsageResource, reason};

#[test]
fn an_invalid_argument_names_its_resource() {
    let err = ResultResource::invalid_argument()
        .with_field_violation("quantity", "not canonical", reason::INVALID_REQUEST)
        .create();
    assert_eq!(err.status_code(), 400);
    assert_eq!(err.resource_type(), Some("gts.cf.bss.rating.result.v1~"));
}

#[test]
fn a_hidden_object_is_not_found() {
    let err = UsageResource::not_found("Inbox entry not found")
        .with_resource("inbox_entry")
        .create();
    assert_eq!(err.status_code(), 404);
    assert_eq!(err.resource_type(), Some("gts.cf.bss.rating.usage.v1~"));
}
