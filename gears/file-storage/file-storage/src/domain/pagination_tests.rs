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
fn clamp_limit_applies_default_and_ceiling_and_rejects_zero() {
    let err = clamp_limit(Some(0), 25, 200).expect_err("limit=0 must be rejected");
    assert!(matches!(err, ODataError::InvalidLimit));

    // (requested, default, max) -> effective limit. A misconfigured default above max must
    // still not escape the ceiling.
    for (requested, default, max, expected) in [
        (None, 25, 200, 25),
        (Some(9999), 25, 200, 200),
        (Some(50), 25, 200, 50),
        (None, 500, 200, 200),
    ] {
        assert_eq!(
            clamp_limit(requested, default, max).unwrap(),
            expected,
            "requested={requested:?} default={default} max={max}"
        );
    }
}

#[test]
fn a_cursor_round_trips_its_position_and_direction() {
    let created_at = datetime!(2026-09-27 12:00:00 UTC);
    let id = Uuid::now_v7();
    for direction in [Direction::Forward, Direction::Backward] {
        let token = encode(created_at, id, ID, None, direction).expect("encode");
        let sought = seek(parsed(&token), ID, None).expect("decode");
        assert_eq!(sought.created_at, created_at);
        assert_eq!(sought.id, id);
        assert_eq!(sought.direction, direction);
    }
}

#[test]
fn a_bound_cursor_round_trips_and_rejects_a_different_binding() {
    let id = Uuid::now_v7();
    let a = owner(file_storage_sdk::OwnerKind::User, Uuid::from_u128(1));
    let b = owner(file_storage_sdk::OwnerKind::User, Uuid::from_u128(2));
    let binding_a = files_binding(&a);
    let token = encode(
        datetime!(2026-09-27 12:00:00 UTC),
        id,
        ID,
        binding_a.clone(),
        Direction::Forward,
    )
    .expect("encode");

    let sought =
        seek(parsed(&token), ID, binding_a.as_deref()).expect("decode with matching binding");
    assert_eq!(sought.id, id);

    for (name, binding) in [("other owner", files_binding(&b)), ("no binding", None)] {
        let err = seek(parsed(&token), ID, binding.as_deref()).expect_err(name);
        assert!(matches!(err, ODataError::FilterMismatch), "{name}: {err:?}");
    }

    // A token with no binding must not resume a bound listing either.
    let unbound = encode(
        datetime!(2026-09-27 12:00:00 UTC),
        id,
        ID,
        None,
        Direction::Forward,
    )
    .expect("encode");
    let err = seek(parsed(&unbound), ID, binding_a.as_deref()).expect_err("unbound token");
    assert!(matches!(err, ODataError::FilterMismatch), "{err:?}");
}

/// A hand-built cursor for `ID` ordering; `keys` and `d` are the knobs the table below turns.
fn raw_cursor(keys: &[&str], d: &str) -> CursorV1 {
    CursorV1 {
        k: keys.iter().map(|k| (*k).to_owned()).collect(),
        o: SortDir::Desc,
        s: canonical_order(ID).to_signed_tokens(),
        f: None,
        d: d.to_owned(),
    }
}

#[test]
fn seek_rejects_foreign_and_malformed_cursors() {
    let ts = "2026-09-27T12:00:00.000000000Z";
    let uuid = Uuid::now_v7().to_string();
    let a = owner(file_storage_sdk::OwnerKind::User, Uuid::from_u128(1));
    // The order check comes first: a /files cursor (even a bound one) is an order mismatch on
    // another listing, not a binding mismatch.
    let files_cursor = parsed(
        &encode(
            datetime!(2026-09-27 12:00:00 UTC),
            Uuid::now_v7(),
            FILES_ID_FIELD,
            files_binding(&a),
            Direction::Forward,
        )
        .expect("encode"),
    );
    #[allow(clippy::type_complexity)]
    let cases: Vec<(&str, CursorV1, &str, fn(&ODataError) -> bool)> = vec![
        (
            "another listing",
            files_cursor.clone(),
            VERSIONS_ID_FIELD,
            |e| matches!(e, ODataError::OrderMismatch),
        ),
        (
            "another unbound listing",
            files_cursor,
            RETENTION_RULES_ID_FIELD,
            |e| matches!(e, ODataError::OrderMismatch),
        ),
        (
            "unrecognized direction",
            raw_cursor(&[ts, &uuid], "sideways"),
            ID,
            |e| matches!(e, ODataError::InvalidCursor),
        ),
        (
            "wrong number of keys",
            raw_cursor(&[ts], FORWARD),
            ID,
            |e| matches!(e, ODataError::CursorInvalidKeys),
        ),
    ];
    for (name, cursor, id_field, is_expected) in cases {
        let err = seek(cursor, id_field, None).expect_err(name);
        assert!(is_expected(&err), "{name}: got {err:?}");
    }
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
fn finish_page_orders_items_and_builds_cursors() {
    let position = |secs: i64, id: u128, direction| Seek {
        created_at: datetime!(2026-09-27 12:00:00 UTC) + time::Duration::seconds(secs),
        id: Uuid::from_u128(id),
        direction,
    };
    // (name, rows in repo query order, after, expected ids in canonical order, next, prev)
    #[allow(clippy::type_complexity)]
    let cases: Vec<(&str, &[(i64, u128)], Option<Seek>, &[u128], bool, bool)> = vec![
        (
            "first page with more rows",
            &[(40, 4), (30, 3), (20, 2), (10, 1)],
            None,
            &[4, 3, 2],
            true,
            false,
        ),
        (
            "first page without more rows",
            &[(30, 3), (20, 2), (10, 1)],
            None,
            &[3, 2, 1],
            false,
            false,
        ),
        (
            "forward page with a cursor always has a prev cursor",
            &[(30, 3), (20, 2), (10, 1)],
            Some(position(50, 5, Direction::Forward)),
            &[3, 2, 1],
            false,
            true,
        ),
        (
            // Backward repo query order is ascending, closest to the cursor first.
            "backward query restores canonical order",
            &[(30, 3), (40, 4), (50, 5), (60, 6)],
            Some(position(20, 2, Direction::Backward)),
            &[5, 4, 3],
            true,
            true,
        ),
        (
            "backward query without further rows",
            &[(30, 3), (40, 4)],
            Some(position(20, 2, Direction::Backward)),
            &[4, 3],
            true,
            false,
        ),
        (
            "empty result",
            &[],
            Some(position(20, 2, Direction::Forward)),
            &[],
            false,
            false,
        ),
    ];
    for (name, specs, after, expected, has_next, has_prev) in cases {
        let page = finish_page(rows(specs), 3, after, ID, None, row_key).expect(name);
        let got: Vec<Uuid> = page.items.iter().map(|r| r.id).collect();
        let expected: Vec<Uuid> = expected.iter().map(|&i| Uuid::from_u128(i)).collect();
        assert_eq!(got, expected, "{name}: items");
        assert_eq!(
            page.page_info.next_cursor.is_some(),
            has_next,
            "{name}: next"
        );
        assert_eq!(
            page.page_info.prev_cursor.is_some(),
            has_prev,
            "{name}: prev"
        );
        // The emitted tokens resume in the matching direction.
        if let Some(next) = &page.page_info.next_cursor {
            let sought = seek(parsed(next), ID, None).expect("decode next_cursor");
            assert_eq!(sought.direction, Direction::Forward, "{name}");
        }
        if let Some(prev) = &page.page_info.prev_cursor {
            let sought = seek(parsed(prev), ID, None).expect("decode prev_cursor");
            assert_eq!(sought.direction, Direction::Backward, "{name}");
        }
    }
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
fn page_by_id_pages_and_their_cursors() {
    let fwd = |id: &str| Some((id.to_owned(), Direction::Forward));
    let bwd = |id: &str| Some((id.to_owned(), Direction::Backward));
    // (name, input, limit, after, expected items, has next_cursor, has prev_cursor)
    #[allow(clippy::type_complexity)]
    let cases: Vec<(
        &str,
        &[&str],
        u64,
        Option<(String, Direction)>,
        &[&str],
        bool,
        bool,
    )> = vec![
        (
            "first page sorts",
            &["c", "a", "d", "b"],
            3,
            None,
            &["a", "b", "c"],
            true,
            false,
        ),
        (
            "whole list in one page",
            &["b", "a"],
            2,
            None,
            &["a", "b"],
            false,
            false,
        ),
        ("empty list", &[], 3, None, &[], false, false),
        (
            "forward past the end",
            &["a", "b"],
            2,
            fwd("z"),
            &[],
            false,
            false,
        ),
        (
            "backward keeps the items nearest the cursor, ascending",
            &["a", "b", "c", "d", "e"],
            2,
            bwd("e"),
            &["c", "d"],
            true,
            true,
        ),
        (
            "backward page reaching the start",
            &["a", "b", "c", "d"],
            5,
            bwd("c"),
            &["a", "b"],
            true,
            false,
        ),
    ];
    for (name, input, limit, after, expected, has_next, has_prev) in cases {
        let page = page_by_id(names(input), limit, after, String::as_str).expect("page");
        assert_eq!(ids(&page), expected, "{name}");
        assert_eq!(page.page_info.limit, limit, "{name}: limit");
        assert_eq!(
            page.page_info.next_cursor.is_some(),
            has_next,
            "{name}: next"
        );
        assert_eq!(
            page.page_info.prev_cursor.is_some(),
            has_prev,
            "{name}: prev"
        );
    }
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
fn storage_seek_rejects_malformed_cursors() {
    let storages_cursor = |k: &[&str], f: Option<&str>, d: &str| CursorV1 {
        k: k.iter().map(|s| (*s).to_owned()).collect(),
        o: SortDir::Asc,
        s: storages_order().to_signed_tokens(),
        f: f.map(str::to_owned),
        d: d.to_owned(),
    };
    let other_listing = parsed(
        &encode(
            datetime!(2026-09-27 12:00:00 UTC),
            Uuid::now_v7(),
            FILES_ID_FIELD,
            None,
            Direction::Forward,
        )
        .expect("encode"),
    );
    #[allow(clippy::type_complexity)]
    let cases: Vec<(&str, CursorV1, fn(&ODataError) -> bool)> = vec![
        (
            "carries a binding",
            storages_cursor(&["a"], Some("abc"), FORWARD),
            |e| matches!(e, ODataError::FilterMismatch),
        ),
        (
            "two keys",
            storages_cursor(&["a", "b"], None, FORWARD),
            |e| matches!(e, ODataError::CursorInvalidKeys),
        ),
        (
            "unrecognized direction",
            storages_cursor(&["a"], None, "sideways"),
            |e| matches!(e, ODataError::InvalidCursor),
        ),
        ("cursor of another listing", other_listing, |e| {
            matches!(e, ODataError::OrderMismatch)
        }),
    ];
    for (name, cursor, is_expected) in cases {
        let err = storage_seek(cursor).expect_err(name);
        assert!(is_expected(&err), "{name}: got {err:?}");
    }
}
