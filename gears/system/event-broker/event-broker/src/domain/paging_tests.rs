use super::{KeysetPage, PageCursor, PageDirection, PageLimit, PageRequest};
use crate::domain::error::{DomainError, ErrorCode};

/// Five keys, deliberately unsorted: the pager owns the order.
fn letters() -> Vec<String> {
    ["d", "a", "e", "c", "b"].map(String::from).to_vec()
}

fn limit(n: u32) -> PageLimit {
    PageLimit::try_from(n).expect("a non-zero limit is valid")
}

fn fwd(key: &str) -> PageCursor {
    PageCursor {
        key: key.to_owned(),
        direction: PageDirection::Forward,
    }
}

fn bwd(key: &str) -> PageCursor {
    PageCursor {
        key: key.to_owned(),
        direction: PageDirection::Backward,
    }
}

fn page_at(request: &PageRequest) -> KeysetPage<String> {
    request.page(letters(), Clone::clone)
}

#[test]
fn a_first_page_that_holds_everything_links_nowhere() {
    let page = page_at(&PageRequest::first(PageLimit::default()));

    assert_eq!(page.items, ["a", "b", "c", "d", "e"]);
    assert_eq!(page.next, None);
    assert_eq!(page.prev, None);
    assert_eq!(page.limit, limit(25));
}

#[test]
fn a_forward_walk_links_each_page_to_its_neighbours() {
    let first = page_at(&PageRequest::first(limit(2)));
    assert_eq!(first.items, ["a", "b"]);
    assert_eq!(first.next, Some(fwd("b")));
    assert_eq!(first.prev, None);
    assert_eq!(first.limit, limit(2));

    let middle = page_at(&PageRequest::at(fwd("b"), limit(2)));
    assert_eq!(middle.items, ["c", "d"]);
    assert_eq!(middle.next, Some(fwd("d")));
    assert_eq!(middle.prev, Some(bwd("c")));
    assert_eq!(middle.limit, limit(2));

    let last = page_at(&PageRequest::at(fwd("d"), limit(2)));
    assert_eq!(last.items, ["e"]);
    assert_eq!(last.next, None);
    assert_eq!(last.prev, Some(bwd("e")));
    assert_eq!(last.limit, limit(2));
}

#[test]
fn a_backward_walk_links_each_page_to_its_neighbours() {
    let middle = page_at(&PageRequest::at(bwd("e"), limit(2)));
    assert_eq!(middle.items, ["c", "d"]);
    assert_eq!(middle.next, Some(fwd("d")));
    assert_eq!(middle.prev, Some(bwd("c")));
    assert_eq!(middle.limit, limit(2));

    let first = page_at(&PageRequest::at(bwd("c"), limit(2)));
    assert_eq!(first.items, ["a", "b"]);
    assert_eq!(first.next, Some(fwd("b")));
    assert_eq!(first.prev, None);
    assert_eq!(first.limit, limit(2));

    // Fewer than `limit` items remain before the cursor: a short first page.
    let short = page_at(&PageRequest::at(bwd("b"), limit(2)));
    assert_eq!(short.items, ["a"]);
    assert_eq!(short.next, Some(fwd("a")));
    assert_eq!(short.prev, None);
    assert_eq!(short.limit, limit(2));
}

#[test]
fn a_zero_limit_is_an_invalid_query() {
    let Err(DomainError::Validation { code, message }) = PageLimit::try_from(0) else {
        panic!("a zero limit must be a validation error");
    };

    assert_eq!(code, ErrorCode::InvalidQuery);
    assert_eq!(message, "limit must be at least 1");
}

#[test]
fn a_limit_is_clamped_to_its_bounds() {
    assert_eq!(u32::from(limit(500)), 200);
    assert_eq!(u32::from(limit(200)), 200);
    assert_eq!(u32::from(limit(1)), 1);
    assert_eq!(u32::from(PageLimit::clamped(0)), 1);
    assert_eq!(u32::from(PageLimit::clamped(10_000)), 200);
    assert_eq!(u32::from(PageLimit::clamped(7)), 7);
    assert_eq!(u32::from(PageLimit::default()), 25);
}

#[test]
fn a_cursor_round_trips_through_its_token() {
    // The key holds `:` itself, so parsing must split at the first one only.
    let cursor = bwd("gts.x.a.v1~x:y");

    let token = cursor.to_string();

    assert_eq!(token, "bwd:gts.x.a.v1~x:y");
    assert_eq!(token.parse::<PageCursor>().ok(), Some(cursor));
    assert_eq!("fwd:b".parse::<PageCursor>().ok(), Some(fwd("b")));
}

#[test]
fn a_malformed_cursor_is_an_invalid_query() {
    for token in ["not-a-cursor", "up:b", ""] {
        let Err(DomainError::Validation { code, message }) = token.parse::<PageCursor>() else {
            panic!("{token:?} must be a validation error");
        };

        assert_eq!(code, ErrorCode::InvalidQuery, "{token:?}");
        assert_eq!(message, "cursor is not a valid page cursor", "{token:?}");
    }
}
