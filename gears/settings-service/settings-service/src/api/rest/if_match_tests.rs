// Created: 2026-09-24 by Virtuozzo International GmbH
//! The one reading of `If-Match` every mutation handler shares.

use axum::http::{HeaderMap, HeaderValue, header};

use super::if_match;

fn headers(value: &str) -> HeaderMap {
    let mut h = HeaderMap::new();
    h.insert(
        header::IF_MATCH,
        HeaderValue::from_str(value).expect("a header value"),
    );
    h
}

#[test]
fn the_headers_framing_comes_off_and_nothing_else() {
    // Quotes and padding are the header's; the tag is what is inside.
    assert_eq!(
        if_match(&headers("1789462318419665000")),
        Some("1789462318419665000")
    );
    assert_eq!(
        if_match(&headers("\"1789462318419665000\"")),
        Some("1789462318419665000")
    );
    assert_eq!(
        if_match(&headers("  \"1789462318419665000\"  ")),
        Some("1789462318419665000")
    );
    assert_eq!(if_match(&headers("absent")), Some("absent"));
    assert_eq!(if_match(&headers("\"absent\"")), Some("absent"));
}

#[test]
fn a_weak_validator_is_not_the_tag() {
    // `If-Match` is a strong comparison: the `W/` stays on and can never equal
    // a tag this service minted.
    assert_eq!(
        if_match(&headers("W/\"1789462318419665000\"")),
        Some("W/\"1789462318419665000")
    );
}

#[test]
fn an_absent_header_is_absent() {
    assert_eq!(if_match(&HeaderMap::new()), None);
}
