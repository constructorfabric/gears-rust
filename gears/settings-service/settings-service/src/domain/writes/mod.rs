// Created: 2026-09-07 by Constructor Tech
//! The write path: validate, set, revert, remove, clone, batch and the impact
//! report — every value operation taking effect when the caller performs it.

pub mod service;

use serde_json::Value;

use crate::api::precondition::ETag;
use crate::domain::value::StoredValue;

pub use service::{Change, Committed, Gated, ValueWriter, WriteActor};

/// The tag of a scope that holds no row yet.
pub const ABSENT_VALUE_TAG: &str = "absent";

/// The value state tag a write at a scope must present, and the one the
/// administrative read returns in `ETag` for the requested scope.
#[must_use]
pub fn value_state_tag(row: Option<&StoredValue>) -> ETag {
    // @cpt-begin:cpt-cf-settings-service-algo-value-writes-etag:p1:inst-vw-etag-1
    // @cpt-begin:cpt-cf-settings-service-algo-value-writes-etag:p1:inst-vw-etag-2
    // @cpt-begin:cpt-cf-settings-service-algo-value-writes-etag:p1:inst-vw-etag-3
    // A row: its normalized UTC `last_change_at`. No row: a tag stable for the
    // pair and distinct from every row tag, so a first write may create the row
    // only against the caller's knowledge that none existed. The read returns
    // this same tag, distinct from the recency in its body, which may belong to
    // an ancestor's row.
    row.map_or_else(
        || ETag::new(ABSENT_VALUE_TAG),
        |row| ETag::new(row.last_change_at.unix_timestamp_nanos().to_string()),
    )
    // @cpt-end:cpt-cf-settings-service-algo-value-writes-etag:p1:inst-vw-etag-3
    // @cpt-end:cpt-cf-settings-service-algo-value-writes-etag:p1:inst-vw-etag-2
    // @cpt-end:cpt-cf-settings-service-algo-value-writes-etag:p1:inst-vw-etag-1
}

/// The value a row carries, as a pre- or post-image: the inline value, or the
/// reference for a secret, which the audit record masks anyway.
#[must_use]
pub fn image_of(row: &StoredValue) -> Value {
    row.value
        .clone()
        .or_else(|| row.secret_ref.clone().map(Value::String))
        .unwrap_or(Value::Null)
}
