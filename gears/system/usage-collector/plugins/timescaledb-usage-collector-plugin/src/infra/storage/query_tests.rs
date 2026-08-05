// Test modules using bare `panic!` opt in explicitly
// (clippy.toml allows unwrap/expect in tests, not panic).
#![allow(clippy::panic)]

use super::{MAX_PAGE_SIZE, effective_page_size};

const DEFAULT: u64 = 100;

#[test]
fn effective_page_size_defaults_when_limit_omitted() {
    // No `limit` -> the store's default page size, unchanged.
    assert_eq!(effective_page_size(None, DEFAULT), DEFAULT);
}

#[test]
fn effective_page_size_passes_a_within_cap_limit_through_unchanged() {
    // A caller `limit` below the cap is honored verbatim.
    assert_eq!(effective_page_size(Some(250), DEFAULT), 250);
}

#[test]
fn effective_page_size_allows_exactly_the_cap() {
    // The cap itself is a legal page size (inclusive bound).
    assert_eq!(
        effective_page_size(Some(MAX_PAGE_SIZE), DEFAULT),
        MAX_PAGE_SIZE
    );
}

#[test]
fn effective_page_size_floors_a_zero_limit_to_one() {
    // A resolved page size of 0 would drive `LIMIT 0+1 = 1`, fetch the
    // look-ahead row, then `truncate(0)` — losing the page tail so
    // `rows.last()` is `None` and the list path 500s with "non-empty page lost
    // its tail". The REST surface cannot deliver a 0 (the toolkit `OData`
    // extractor rejects `limit=0` with `InvalidLimit`), but an in-process SDK
    // caller builds its own `ODataQuery` and reaches neither that check nor
    // the core gateway's `prepare_list_query`. Floor to 1 (the smallest legal
    // page) so both list paths stay sound regardless.
    assert_eq!(effective_page_size(Some(0), DEFAULT), 1);
}

#[test]
fn effective_page_size_floors_a_zero_default_to_one() {
    // Belt-and-suspenders: even a (mis)configured zero default page size can
    // never resolve to a 0 `LIMIT`.
    assert_eq!(effective_page_size(None, 0), 1);
}

#[test]
fn effective_page_size_clamps_a_limit_above_the_cap() {
    // The defense-in-depth backstop: an oversized `limit` that slipped past
    // the core gateway is clamped to the cap, never fed verbatim into
    // `LIMIT n+1 ... fetch_all` (an unbounded full-result-set read).
    assert_eq!(effective_page_size(Some(1_000_000), DEFAULT), MAX_PAGE_SIZE);
    assert_eq!(effective_page_size(Some(u64::MAX), DEFAULT), MAX_PAGE_SIZE);
}
