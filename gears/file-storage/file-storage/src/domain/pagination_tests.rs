use time::macros::datetime;
use uuid::Uuid;

use toolkit_odata::Error as ODataError;

use super::*;

const ID: &str = FILES_ID_FIELD;

/// Parse a wire token the way the `OData` extractor does before the gear sees it.
fn parsed(token: &str) -> CursorV1 {
    CursorV1::decode(token).expect("decode cursor token")
}

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
    let sought = seek(parsed(&token), ID, None).expect("decode");
    assert_eq!(sought.created_at, created_at);
    assert_eq!(sought.id, id);
    assert_eq!(sought.direction, Direction::Forward);
}

#[test]
fn a_backward_cursor_round_trips_its_position_and_direction() {
    let created_at = datetime!(2026-09-27 12:00:00 UTC);
    let id = Uuid::now_v7();
    let token = encode(created_at, id, ID, None, Direction::Backward).expect("encode");
    let sought = seek(parsed(&token), ID, None).expect("decode");
    assert_eq!(sought.created_at, created_at);
    assert_eq!(sought.id, id);
    assert_eq!(sought.direction, Direction::Backward);
}

#[test]
fn a_bound_cursor_round_trips_and_rejects_a_different_binding() {
    let created_at = datetime!(2026-09-27 12:00:00 UTC);
    let id = Uuid::now_v7();
    let a = owner(file_storage_sdk::OwnerKind::User, Uuid::from_u128(1));
    let b = owner(file_storage_sdk::OwnerKind::User, Uuid::from_u128(2));
    let binding_a = files_binding(&a);
    let token = encode(created_at, id, ID, binding_a.clone(), Direction::Forward).expect("encode");

    let sought =
        seek(parsed(&token), ID, binding_a.as_deref()).expect("decode with matching binding");
    assert_eq!(sought.id, id);

    let binding_b = files_binding(&b);
    let err = seek(parsed(&token), ID, binding_b.as_deref())
        .expect_err("different owner must be rejected");
    assert!(matches!(err, ODataError::FilterMismatch));

    let err = seek(parsed(&token), ID, None).expect_err("an unbound replay must be rejected too");
    assert!(matches!(err, ODataError::FilterMismatch));
}

#[test]
fn an_unbound_cursor_is_rejected_against_a_bound_listing() {
    let created_at = datetime!(2026-09-27 12:00:00 UTC);
    let id = Uuid::now_v7();
    let token = encode(created_at, id, ID, None, Direction::Forward).expect("encode");
    let a = owner(file_storage_sdk::OwnerKind::User, Uuid::from_u128(1));
    let err = seek(parsed(&token), ID, files_binding(&a).as_deref())
        .expect_err("a token with no binding must not resume a bound listing");
    assert!(matches!(err, ODataError::FilterMismatch));
}

#[test]
fn a_cursor_from_a_different_id_field_is_rejected() {
    let created_at = datetime!(2026-09-27 12:00:00 UTC);
    let id = Uuid::now_v7();
    let token = encode(created_at, id, FILES_ID_FIELD, None, Direction::Forward).expect("encode");
    let err = seek(parsed(&token), VERSIONS_ID_FIELD, None)
        .expect_err("a cursor issued for a different listing's order must be rejected");
    assert!(matches!(err, ODataError::OrderMismatch));
}

#[test]
fn a_files_cursor_resumed_on_an_unbound_listing_is_an_order_mismatch() {
    // The order check comes first: a cursor of another listing is not a binding mismatch,
    // even though it carries a binding and the target listing has none.
    let a = owner(file_storage_sdk::OwnerKind::User, Uuid::from_u128(1));
    let token = encode(
        datetime!(2026-09-27 12:00:00 UTC),
        Uuid::now_v7(),
        FILES_ID_FIELD,
        files_binding(&a),
        Direction::Forward,
    )
    .expect("encode");
    let err = seek(parsed(&token), RETENTION_RULES_ID_FIELD, None)
        .expect_err("a /files cursor must not resume /retention-rules");
    assert!(matches!(err, ODataError::OrderMismatch), "got {err:?}");
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
    let sought = seek(parsed(&token), ID, None).expect("bwd must be accepted");
    assert_eq!(sought.direction, Direction::Backward);
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
    let err = seek(cursor, ID, None).expect_err("an unrecognized direction must be rejected");
    assert!(matches!(err, ODataError::InvalidCursor), "got {err:?}");
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
    let err = seek(cursor, ID, None).expect_err("must name exactly 2 keys");
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
    let sought = seek(parsed(&next), ID, None).expect("decode next_cursor");
    assert_eq!(sought.direction, Direction::Forward);

    let after = Seek {
        created_at: datetime!(2026-09-27 12:00:50 UTC),
        id: Uuid::from_u128(9),
        direction: Direction::Forward,
    };
    let fetched2 = rows(&[(30, 3), (20, 2), (10, 1)]);
    let page2 = finish_page(fetched2, 3, Some(after), ID, None, row_key).expect("finish_page");
    let prev = page2.page_info.prev_cursor.expect("a cursor was supplied");
    let sought2 = seek(parsed(&prev), ID, None).expect("decode prev_cursor");
    assert_eq!(sought2.direction, Direction::Backward);
}

fn ids(page: &toolkit_odata::Page<String>) -> Vec<&str> {
    page.items.iter().map(String::as_str).collect()
}

fn names(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| (*s).to_owned()).collect()
}

/// Parse a `next_cursor`/`prev_cursor` token into the position `page_by_id` resumes from.
fn storage_after(token: &str) -> (String, Direction) {
    storage_seek(parsed(token)).expect("storage_seek")
}

#[test]
fn page_by_id_first_page_sorts_and_has_only_a_next_cursor() {
    let page = page_by_id(names(&["c", "a", "d", "b"]), 3, None, String::as_str).expect("page");
    assert_eq!(ids(&page), ["a", "b", "c"]);
    assert_eq!(page.page_info.limit, 3);
    assert!(page.page_info.next_cursor.is_some());
    assert!(page.page_info.prev_cursor.is_none());
}

#[test]
fn page_by_id_whole_list_in_one_page_has_no_cursors() {
    let page = page_by_id(names(&["b", "a"]), 2, None, String::as_str).expect("page");
    assert_eq!(ids(&page), ["a", "b"]);
    assert!(page.page_info.next_cursor.is_none());
    assert!(page.page_info.prev_cursor.is_none());
}

#[test]
fn page_by_id_empty_list_has_no_cursors() {
    let page = page_by_id(Vec::<String>::new(), 3, None, String::as_str).expect("page");
    assert!(page.items.is_empty());
    assert!(page.page_info.next_cursor.is_none());
    assert!(page.page_info.prev_cursor.is_none());
}

#[test]
fn page_by_id_forward_walk_covers_every_item_and_ends_without_next_cursor() {
    let all = ["a", "b", "c", "d", "e"];
    let mut after = None;
    let mut seen: Vec<String> = Vec::new();
    let mut pages = 0;
    loop {
        let page = page_by_id(names(&all), 2, after, String::as_str).expect("page");
        pages += 1;
        assert_eq!(
            page.page_info.prev_cursor.is_some(),
            pages > 1,
            "only the first page lacks a prev_cursor"
        );
        seen.extend(page.items.iter().cloned());
        match page.page_info.next_cursor {
            Some(next) => after = Some(storage_after(&next)),
            None => break,
        }
    }
    assert_eq!(pages, 3);
    assert_eq!(seen, names(&all));
}

#[test]
fn page_by_id_backward_page_keeps_the_items_nearest_the_cursor_in_ascending_order() {
    let all = names(&["a", "b", "c", "d", "e"]);
    let after = Some(("e".to_owned(), Direction::Backward));
    let page = page_by_id(all, 2, after, String::as_str).expect("page");
    assert_eq!(ids(&page), ["c", "d"]);
    assert!(page.page_info.prev_cursor.is_some(), "a, b remain");
    assert!(page.page_info.next_cursor.is_some(), "e lies after");
}

#[test]
fn page_by_id_backward_walk_reaches_the_start_without_prev_cursor() {
    let all = ["a", "b", "c", "d", "e"];
    let mut after = Some(("e".to_owned(), Direction::Backward));
    let mut pages: Vec<Vec<String>> = Vec::new();
    loop {
        let page = page_by_id(names(&all), 2, after, String::as_str).expect("page");
        pages.push(page.items.clone());
        match page.page_info.prev_cursor {
            Some(prev) => after = Some(storage_after(&prev)),
            None => break,
        }
    }
    assert_eq!(
        pages,
        vec![names(&["c", "d"]), names(&["a", "b"])],
        "backward pages walk towards the start"
    );
}

#[test]
fn page_by_id_backward_page_that_reaches_the_start_has_no_prev_cursor() {
    let after = Some(("c".to_owned(), Direction::Backward));
    let page = page_by_id(names(&["a", "b", "c", "d"]), 5, after, String::as_str).expect("page");
    assert_eq!(ids(&page), ["a", "b"]);
    assert!(page.page_info.prev_cursor.is_none());
    assert!(page.page_info.next_cursor.is_some());
}

#[test]
fn page_by_id_forward_page_past_the_end_is_empty_without_cursors() {
    let after = Some(("z".to_owned(), Direction::Forward));
    let page = page_by_id(names(&["a", "b"]), 2, after, String::as_str).expect("page");
    assert!(page.items.is_empty());
    assert!(page.page_info.next_cursor.is_none());
    assert!(page.page_info.prev_cursor.is_none());
}

#[test]
fn page_by_id_cursors_round_trip_through_storage_seek() {
    let page = page_by_id(names(&["a", "b", "c"]), 1, None, String::as_str).expect("page");
    let next = page.page_info.next_cursor.expect("more forward");
    let (id, direction) = storage_seek(parsed(&next)).expect("storage_seek");
    assert_eq!((id.as_str(), direction), ("a", Direction::Forward));

    let page = page_by_id(
        names(&["a", "b", "c"]),
        1,
        Some(("b".to_owned(), Direction::Forward)),
        String::as_str,
    )
    .expect("page");
    let prev = page.page_info.prev_cursor.expect("a cursor was supplied");
    let (id, direction) = storage_seek(parsed(&prev)).expect("storage_seek");
    assert_eq!((id.as_str(), direction), ("c", Direction::Backward));
}

#[test]
fn storage_seek_rejects_a_cursor_carrying_a_binding() {
    let cursor = CursorV1 {
        k: vec!["a".to_owned()],
        o: SortDir::Asc,
        s: storages_order().to_signed_tokens(),
        f: Some("abc".to_owned()),
        d: FORWARD.to_owned(),
    };
    let err = storage_seek(cursor).expect_err("storages cursors carry no binding");
    assert!(matches!(err, ODataError::FilterMismatch), "got {err:?}");
}

#[test]
fn storage_seek_rejects_a_cursor_with_two_keys() {
    let cursor = CursorV1 {
        k: vec!["a".to_owned(), "b".to_owned()],
        o: SortDir::Asc,
        s: storages_order().to_signed_tokens(),
        f: None,
        d: FORWARD.to_owned(),
    };
    let err = storage_seek(cursor).expect_err("must name exactly one key");
    assert!(matches!(err, ODataError::CursorInvalidKeys), "got {err:?}");
}

#[test]
fn storage_seek_rejects_an_unrecognized_direction() {
    let cursor = CursorV1 {
        k: vec!["a".to_owned()],
        o: SortDir::Asc,
        s: storages_order().to_signed_tokens(),
        f: None,
        d: "sideways".to_owned(),
    };
    let err = storage_seek(cursor).expect_err("direction must be fwd or bwd");
    assert!(matches!(err, ODataError::InvalidCursor), "got {err:?}");
}

#[test]
fn storage_seek_rejects_a_cursor_of_another_listing() {
    let token = encode(
        datetime!(2026-09-27 12:00:00 UTC),
        Uuid::now_v7(),
        FILES_ID_FIELD,
        None,
        Direction::Forward,
    )
    .expect("encode");
    let err = storage_seek(parsed(&token)).expect_err("a /files cursor must not resume /storages");
    assert!(matches!(err, ODataError::OrderMismatch), "got {err:?}");
}
