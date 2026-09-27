use time::macros::datetime;
use uuid::Uuid;

use toolkit_odata::Error as ODataError;

use super::*;

const ID: &str = FILES_ID_FIELD;

fn owner(kind: file_storage_sdk::OwnerKind, id: Uuid) -> OwnerFilter {
    OwnerFilter {
        owner_kind: kind,
        owner_id: id,
    }
}

#[test]
fn clamp_limit_zero_is_rejected() {
    let err = clamp_limit(Some(0), 25, 200).expect_err("limit=0 must be rejected");
    assert!(matches!(err, ODataError::InvalidLimit));
}

#[test]
fn clamp_limit_none_falls_back_to_default() {
    assert_eq!(clamp_limit(None, 25, 200).unwrap(), 25);
}

#[test]
fn clamp_limit_above_max_is_clamped_down() {
    assert_eq!(clamp_limit(Some(9999), 25, 200).unwrap(), 200);
}

#[test]
fn clamp_limit_within_range_passes_through() {
    assert_eq!(clamp_limit(Some(50), 25, 200).unwrap(), 50);
}

#[test]
fn clamp_limit_default_above_max_is_also_clamped() {
    // Defensive: a misconfigured default above max should still not escape
    // the ceiling (mirrors `FileStorageConfig::validate()`'s own invariant,
    // which should make this unreachable in practice).
    assert_eq!(clamp_limit(None, 500, 200).unwrap(), 200);
}

#[test]
fn a_cursor_round_trips_its_position() {
    let created_at = datetime!(2026-09-27 12:00:00 UTC);
    let id = Uuid::now_v7();
    let token = encode(created_at, id, ID, None).expect("encode");
    let seek = decode(&token, ID, None).expect("decode");
    assert_eq!(seek.created_at, created_at);
    assert_eq!(seek.id, id);
}

#[test]
fn a_bound_cursor_round_trips_and_rejects_a_different_binding() {
    let created_at = datetime!(2026-09-27 12:00:00 UTC);
    let id = Uuid::now_v7();
    let a = owner(file_storage_sdk::OwnerKind::User, Uuid::from_u128(1));
    let b = owner(file_storage_sdk::OwnerKind::User, Uuid::from_u128(2));
    let binding_a = files_binding(&a);
    let token = encode(created_at, id, ID, binding_a.clone()).expect("encode");

    let seek = decode(&token, ID, binding_a.as_deref()).expect("decode with matching binding");
    assert_eq!(seek.id, id);

    let binding_b = files_binding(&b);
    let err =
        decode(&token, ID, binding_b.as_deref()).expect_err("different owner must be rejected");
    assert!(matches!(err, ODataError::FilterMismatch));

    let err = decode(&token, ID, None).expect_err("an unbound replay must be rejected too");
    assert!(matches!(err, ODataError::FilterMismatch));
}

#[test]
fn an_unbound_cursor_is_rejected_against_a_bound_listing() {
    let created_at = datetime!(2026-09-27 12:00:00 UTC);
    let id = Uuid::now_v7();
    let token = encode(created_at, id, ID, None).expect("encode");
    let a = owner(file_storage_sdk::OwnerKind::User, Uuid::from_u128(1));
    let err = decode(&token, ID, files_binding(&a).as_deref())
        .expect_err("a token with no binding must not resume a bound listing");
    assert!(matches!(err, ODataError::FilterMismatch));
}

#[test]
fn a_cursor_from_a_different_id_field_is_rejected() {
    let created_at = datetime!(2026-09-27 12:00:00 UTC);
    let id = Uuid::now_v7();
    let token = encode(created_at, id, FILES_ID_FIELD, None).expect("encode");
    let err = decode(&token, VERSIONS_ID_FIELD, None)
        .expect_err("a cursor issued for a different listing's order must be rejected");
    assert!(matches!(err, ODataError::OrderMismatch));
}

#[test]
fn a_garbage_token_is_rejected() {
    for token in ["", "not-base64url-json", "e30"] {
        assert!(decode(token, ID, None).is_err(), "{token:?}");
    }
}

#[test]
fn a_backward_direction_cursor_is_rejected() {
    let cursor = CursorV1 {
        k: vec![
            "2026-09-27T12:00:00.000000000Z".to_owned(),
            Uuid::now_v7().to_string(),
        ],
        o: SortDir::Desc,
        s: canonical_order(ID).to_signed_tokens(),
        f: None,
        d: "bwd".to_owned(),
    };
    let token = cursor.encode().expect("encode");
    let err = decode(&token, ID, None).expect_err("bwd must be rejected");
    assert!(matches!(err, ODataError::InvalidCursor));
}

#[test]
fn a_cursor_naming_the_wrong_number_of_keys_is_rejected() {
    let cursor = CursorV1 {
        k: vec!["2026-09-27T12:00:00.000000000Z".to_owned()],
        o: SortDir::Desc,
        s: canonical_order(ID).to_signed_tokens(),
        f: None,
        d: FORWARD.to_owned(),
    };
    let token = cursor.encode().expect("encode");
    let err = decode(&token, ID, None).expect_err("must name exactly 2 keys");
    assert!(matches!(err, ODataError::CursorInvalidKeys));
}

#[test]
fn files_and_versions_bindings_differ_for_the_same_uuid() {
    // An owner's `owner_id` and some file's `file_id` can coincidentally
    // share the same UUID; the two bindings must not collide.
    let shared_id = Uuid::now_v7();
    let o = owner(file_storage_sdk::OwnerKind::User, shared_id);
    assert_ne!(files_binding(&o), versions_binding(shared_id));
}
