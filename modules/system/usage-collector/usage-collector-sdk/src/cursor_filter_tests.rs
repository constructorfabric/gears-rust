//! Pin tests for the raw-query cursor filter contract.
//!
//! The canonical-string format that feeds [`raw_query_filter_hash`] is part
//! of the cursor wire contract: any gateway or storage-plugin instance that
//! disagrees on the format will mint cursors the other side cannot validate.
//! These tests freeze a specific `(filters → hex digest)` mapping so that a
//! one-character drift in the format template (or in any of the renderers
//! it invokes) flips a known constant and fails CI rather than silently
//! invalidating every previously minted cursor.

use chrono::{DateTime, Utc};
use uuid::Uuid;

use super::{
    RawQueryFilters, raw_query_effective_order, raw_query_filter_canonical, raw_query_filter_hash,
};

/// Pinned timestamp pair used by the format-stability tests. Nanosecond
/// precision is intentional — the canonical renderer uses
/// `SecondsFormat::Nanos`, so any drop to milli/micro precision flips the
/// hash.
fn pinned_range() -> (DateTime<Utc>, DateTime<Utc>) {
    let from = DateTime::parse_from_rfc3339("2024-01-15T12:34:56.789012345Z")
        .expect("pinned from timestamp parses")
        .with_timezone(&Utc);
    let to = DateTime::parse_from_rfc3339("2024-01-15T13:34:56.789012345Z")
        .expect("pinned to timestamp parses")
        .with_timezone(&Utc);
    (from, to)
}

fn pinned_resource_id() -> Uuid {
    Uuid::parse_str("11111111-2222-3333-4444-555555555555").expect("pinned resource_id parses")
}

fn pinned_subject_id() -> Uuid {
    Uuid::parse_str("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee").expect("pinned subject_id parses")
}

#[test]
fn pin_filter_hash_full_tuple() {
    // Locks the canonical-string format for the fully-populated case
    // (every optional filter set). A drift in any renderer — RFC 3339
    // precision, UUID hyphenation, the `TIME(...)|UT(...)|...` template,
    // the `Some(...)` wrapper — changes both the string and the digest.
    // We pin BOTH because the digest is a derivative: a coordinated drift
    // that updates only the format-template and the hex constant would
    // still pass a digest-only test, while a format-pin test cannot lie
    // about what bytes feed the hasher.
    let (from, to) = pinned_range();
    let filters = RawQueryFilters {
        from,
        to,
        usage_type: Some("compute"),
        resource_id: Some(pinned_resource_id()),
        resource_type: Some("vm"),
        subject_type: Some("user"),
        subject_id: Some(pinned_subject_id()),
    };
    assert_eq!(
        raw_query_filter_canonical(&filters),
        "TIME(2024-01-15T12:34:56.789012345Z,2024-01-15T13:34:56.789012345Z)\
         |UT(Some(compute))\
         |RID(Some(11111111-2222-3333-4444-555555555555))\
         |RT(Some(vm))\
         |ST(Some(user))\
         |SID(Some(aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee))",
    );
    assert_eq!(raw_query_filter_hash(&filters), "24e452192fc799bd");
}

#[test]
fn pin_filter_hash_all_optionals_none() {
    // Companion pin for the "no optional filters" case — exercises the
    // `None` rendering of absent fields, which is a separate branch from
    // the fully-populated case above.
    let (from, to) = pinned_range();
    let filters = RawQueryFilters {
        from,
        to,
        usage_type: None,
        resource_id: None,
        resource_type: None,
        subject_type: None,
        subject_id: None,
    };
    assert_eq!(
        raw_query_filter_canonical(&filters),
        "TIME(2024-01-15T12:34:56.789012345Z,2024-01-15T13:34:56.789012345Z)\
         |UT(None)|RID(None)|RT(None)|ST(None)|SID(None)",
    );
    assert_eq!(raw_query_filter_hash(&filters), "a23622fb370dd38c");
}

#[test]
fn empty_string_optional_does_not_collide_with_none() {
    // axum's `Query` deserializer maps a bare `?usage_type=` (no value) to
    // `Some("")`, while an absent `?usage_type` maps to `None`. Before the
    // `Some(...)`/`None` markers, both rendered as `UT()` in the canonical
    // string and produced the same digest — meaning a cursor minted with
    // one variant of the request silently validated against the other.
    // This test guards that divergence directly so the next refactor of
    // the renderer cannot reintroduce the collision.
    let (from, to) = pinned_range();
    let base = RawQueryFilters {
        from,
        to,
        usage_type: None,
        resource_id: None,
        resource_type: None,
        subject_type: None,
        subject_id: None,
    };
    let empty_usage_type = RawQueryFilters {
        usage_type: Some(""),
        ..base
    };
    let empty_resource_type = RawQueryFilters {
        resource_type: Some(""),
        ..base
    };
    let empty_subject_type = RawQueryFilters {
        subject_type: Some(""),
        ..base
    };
    assert_ne!(
        raw_query_filter_hash(&base),
        raw_query_filter_hash(&empty_usage_type),
        "Some(\"\") and None must hash differently for usage_type",
    );
    assert_ne!(
        raw_query_filter_hash(&base),
        raw_query_filter_hash(&empty_resource_type),
        "Some(\"\") and None must hash differently for resource_type",
    );
    assert_ne!(
        raw_query_filter_hash(&base),
        raw_query_filter_hash(&empty_subject_type),
        "Some(\"\") and None must hash differently for subject_type",
    );
}

#[test]
fn flipping_usage_type_changes_hash() {
    // Same range and UUIDs, only `usage_type` differs — the hash must
    // change. This is the "different inputs → different hashes" companion
    // to the pinned-tuple test: it guards against a regression where the
    // canonical string accidentally drops or coalesces a field.
    let (from, to) = pinned_range();
    let base = RawQueryFilters {
        from,
        to,
        usage_type: Some("compute"),
        resource_id: Some(pinned_resource_id()),
        resource_type: Some("vm"),
        subject_type: Some("user"),
        subject_id: Some(pinned_subject_id()),
    };
    let flipped = RawQueryFilters {
        usage_type: Some("storage"),
        ..base
    };
    assert_ne!(
        raw_query_filter_hash(&base),
        raw_query_filter_hash(&flipped)
    );
}

#[test]
fn flipping_resource_subject_type_changes_hash() {
    // Cross-check that swapping `resource_type` with `subject_type` does
    // not yield the same hash — the canonical string must keep them in
    // distinct positions so a call-site swap is detectable via the hash
    // mismatch surfaced by `validate_cursor_against`.
    let (from, to) = pinned_range();
    let base = RawQueryFilters {
        from,
        to,
        usage_type: Some("compute"),
        resource_id: Some(pinned_resource_id()),
        resource_type: Some("vm"),
        subject_type: Some("user"),
        subject_id: Some(pinned_subject_id()),
    };
    let swapped = RawQueryFilters {
        resource_type: Some("user"),
        subject_type: Some("vm"),
        ..base
    };
    assert_ne!(
        raw_query_filter_hash(&base),
        raw_query_filter_hash(&swapped)
    );
}

#[test]
fn effective_order_is_timestamp_then_id_ascending() {
    // The cursor's `s` field is whatever the order renders as signed
    // tokens; the gateway pins this to `+timestamp,+id` so any plugin
    // minting cursors must agree byte-for-byte.
    let order = raw_query_effective_order();
    assert_eq!(order.to_signed_tokens(), "+timestamp,+id");
}
