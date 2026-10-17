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
    // A misconfigured default above max must still not escape the ceiling.
    assert_eq!(clamp_limit(None, 500, 200).unwrap(), 200);
}

#[test]
fn a_cursor_round_trips_its_position() {
    let created_at = datetime!(2026-09-27 12:00:00 UTC);
    let id = Uuid::now_v7();
    let token = encode(created_at, id, ID, None, Direction::Forward).expect("encode");
    let seek = decode(&token, ID, None).expect("decode");
    assert_eq!(seek.created_at, created_at);
    assert_eq!(seek.id, id);
    assert_eq!(seek.direction, Direction::Forward);
}

#[test]
fn a_backward_cursor_round_trips_its_position_and_direction() {
    let created_at = datetime!(2026-09-27 12:00:00 UTC);
    let id = Uuid::now_v7();
    let token = encode(created_at, id, ID, None, Direction::Backward).expect("encode");
    let seek = decode(&token, ID, None).expect("decode");
    assert_eq!(seek.created_at, created_at);
    assert_eq!(seek.id, id);
    assert_eq!(seek.direction, Direction::Backward);
}

#[test]
fn a_bound_cursor_round_trips_and_rejects_a_different_binding() {
    let created_at = datetime!(2026-09-27 12:00:00 UTC);
    let id = Uuid::now_v7();
    let a = owner(file_storage_sdk::OwnerKind::User, Uuid::from_u128(1));
    let b = owner(file_storage_sdk::OwnerKind::User, Uuid::from_u128(2));
    let binding_a = files_binding(&a);
    let token = encode(created_at, id, ID, binding_a.clone(), Direction::Forward).expect("encode");

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
    let token = encode(created_at, id, ID, None, Direction::Forward).expect("encode");
    let a = owner(file_storage_sdk::OwnerKind::User, Uuid::from_u128(1));
    let err = decode(&token, ID, files_binding(&a).as_deref())
        .expect_err("a token with no binding must not resume a bound listing");
    assert!(matches!(err, ODataError::FilterMismatch));
}

#[test]
fn a_cursor_from_a_different_id_field_is_rejected() {
    let created_at = datetime!(2026-09-27 12:00:00 UTC);
    let id = Uuid::now_v7();
    let token = encode(created_at, id, FILES_ID_FIELD, None, Direction::Forward).expect("encode");
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
fn a_backward_direction_cursor_is_accepted() {
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
    let seek = decode(&token, ID, None).expect("bwd must be accepted");
    assert_eq!(seek.direction, Direction::Backward);
}

#[test]
fn an_unrecognized_direction_is_rejected() {
    let cursor = CursorV1 {
        k: vec![
            "2026-09-27T12:00:00.000000000Z".to_owned(),
            Uuid::now_v7().to_string(),
        ],
        o: SortDir::Desc,
        s: canonical_order(ID).to_signed_tokens(),
        f: None,
        d: "sideways".to_owned(),
    };
    let token = cursor.encode().expect("encode");
    let err = decode(&token, ID, None).expect_err("an unrecognized direction must be rejected");
    // Rejected by `CursorV1::decode` itself.
    assert!(
        matches!(err, ODataError::CursorInvalidDirection),
        "got {err:?}"
    );
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
    // An owner id and a file id may share a UUID; the bindings must not collide.
    let shared_id = Uuid::now_v7();
    let o = owner(file_storage_sdk::OwnerKind::User, shared_id);
    assert_ne!(files_binding(&o), versions_binding(shared_id));
}

#[derive(Clone)]
struct Row {
    created_at: OffsetDateTime,
    id: Uuid,
}

/// Builds rows from `(offset_seconds, id)` pairs, in exactly the order passed.
fn rows(specs: &[(i64, u128)]) -> Vec<Row> {
    specs
        .iter()
        .map(|&(offset_secs, id)| Row {
            created_at: datetime!(2026-09-27 12:00:00 UTC) + time::Duration::seconds(offset_secs),
            id: Uuid::from_u128(id),
        })
        .collect()
}

fn row_key(r: &Row) -> (OffsetDateTime, Uuid) {
    (r.created_at, r.id)
}

#[test]
fn finish_page_first_forward_page_with_more_has_no_prev_cursor() {
    let fetched = rows(&[(40, 4), (30, 3), (20, 2), (10, 1)]);
    let page = finish_page(fetched, 3, None, ID, None, row_key).expect("finish_page");
    assert_eq!(page.items.len(), 3);
    assert_eq!(page.items[0].id, Uuid::from_u128(4));
    assert_eq!(page.items[2].id, Uuid::from_u128(2));
    assert!(
        page.page_info.next_cursor.is_some(),
        "more rows exist forward"
    );
    assert!(
        page.page_info.prev_cursor.is_none(),
        "no cursor was supplied -- this is the first page"
    );
}

#[test]
fn finish_page_first_forward_page_without_more_has_no_cursors() {
    let fetched = rows(&[(30, 3), (20, 2), (10, 1)]);
    let page = finish_page(fetched, 3, None, ID, None, row_key).expect("finish_page");
    assert_eq!(page.items.len(), 3);
    assert!(page.page_info.next_cursor.is_none());
    assert!(page.page_info.prev_cursor.is_none());
}

#[test]
fn finish_page_forward_page_with_cursor_always_has_prev_cursor() {
    let after = Seek {
        created_at: datetime!(2026-09-27 12:00:50 UTC),
        id: Uuid::from_u128(5),
        direction: Direction::Forward,
    };
    let fetched = rows(&[(30, 3), (20, 2), (10, 1)]);
    let page = finish_page(fetched, 3, Some(after), ID, None, row_key).expect("finish_page");
    assert!(page.page_info.next_cursor.is_none(), "no more rows forward");
    assert!(
        page.page_info.prev_cursor.is_some(),
        "a cursor was supplied, so a predecessor page must exist -- the \
         position it decoded from is proof of it"
    );
}

#[test]
fn finish_page_backward_query_restores_canonical_order() {
    let after = Seek {
        created_at: datetime!(2026-09-27 12:00:20 UTC),
        id: Uuid::from_u128(2),
        direction: Direction::Backward,
    };
    // Backward repo query order: ascending, closest to the cursor first.
    let fetched = rows(&[(30, 3), (40, 4), (50, 5), (60, 6)]);
    let page = finish_page(fetched, 3, Some(after), ID, None, row_key).expect("finish_page");
    assert_eq!(page.items.len(), 3);
    assert_eq!(page.items[0].id, Uuid::from_u128(5));
    assert_eq!(page.items[1].id, Uuid::from_u128(4));
    assert_eq!(page.items[2].id, Uuid::from_u128(3));
    assert!(
        page.page_info.prev_cursor.is_some(),
        "over-fetched -- a further backward page exists"
    );
    assert!(
        page.page_info.next_cursor.is_some(),
        "a cursor was supplied, so a page after this one must exist"
    );
}

#[test]
fn finish_page_backward_query_without_further_rows_has_no_prev_cursor() {
    let after = Seek {
        created_at: datetime!(2026-09-27 12:00:20 UTC),
        id: Uuid::from_u128(2),
        direction: Direction::Backward,
    };
    let fetched = rows(&[(30, 3), (40, 4)]);
    let page = finish_page(fetched, 3, Some(after), ID, None, row_key).expect("finish_page");
    assert_eq!(page.items.len(), 2);
    assert_eq!(page.items[0].id, Uuid::from_u128(4));
    assert_eq!(page.items[1].id, Uuid::from_u128(3));
    assert!(page.page_info.prev_cursor.is_none());
    assert!(page.page_info.next_cursor.is_some());
}

#[test]
fn finish_page_empty_result_builds_no_cursors() {
    let after = Seek {
        created_at: datetime!(2026-09-27 12:00:20 UTC),
        id: Uuid::from_u128(2),
        direction: Direction::Forward,
    };
    let page =
        finish_page(Vec::<Row>::new(), 3, Some(after), ID, None, row_key).expect("finish_page");
    assert!(page.items.is_empty());
    assert!(page.page_info.next_cursor.is_none());
    assert!(page.page_info.prev_cursor.is_none());
}

#[test]
fn finish_page_next_and_prev_cursors_round_trip_directions() {
    let fetched = rows(&[(40, 4), (30, 3), (20, 2), (10, 1)]);
    let page = finish_page(fetched, 3, None, ID, None, row_key).expect("finish_page");
    let next = page.page_info.next_cursor.expect("has_more must be true");
    let seek = decode(&next, ID, None).expect("decode next_cursor");
    assert_eq!(seek.direction, Direction::Forward);

    let after = Seek {
        created_at: datetime!(2026-09-27 12:00:50 UTC),
        id: Uuid::from_u128(9),
        direction: Direction::Forward,
    };
    let fetched2 = rows(&[(30, 3), (20, 2), (10, 1)]);
    let page2 = finish_page(fetched2, 3, Some(after), ID, None, row_key).expect("finish_page");
    let prev = page2.page_info.prev_cursor.expect("a cursor was supplied");
    let seek2 = decode(&prev, ID, None).expect("decode prev_cursor");
    assert_eq!(seek2.direction, Direction::Backward);
}
