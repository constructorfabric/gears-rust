#![allow(clippy::expect_used, clippy::unwrap_used)]

use axum::http::{HeaderMap, HeaderValue, header};
use toolkit_canonical_errors::Problem;

use super::{header_condition, item_condition};
use crate::domain::registry_service::MAX_KEY_LEN;
use crate::domain::validator::IfNoneMatch;

fn headers(lines: &[&[u8]]) -> HeaderMap {
    let mut map = HeaderMap::new();
    for line in lines {
        map.append(
            header::IF_NONE_MATCH,
            HeaderValue::from_bytes(line).expect("a header value"),
        );
    }
    map
}

fn tags(tags: &[&str]) -> IfNoneMatch {
    IfNoneMatch::Validators(tags.iter().map(|tag| (*tag).to_owned()).collect())
}

/// The refused field, or a panic naming what was accepted instead.
fn refused_field<T: std::fmt::Debug>(
    result: Result<T, toolkit_canonical_errors::CanonicalError>,
) -> String {
    let problem = Problem::from(result.expect_err("refused"));
    assert_eq!(problem.status, Some(400));
    problem.context["field_violations"][0]["field"]
        .as_str()
        .expect("a field violation")
        .to_owned()
}

#[test]
fn no_header_is_no_condition() {
    assert_eq!(header_condition(&HeaderMap::new()).expect("read"), None);
}

#[test]
fn tags_are_read_across_lines_and_commas_with_weak_ones_compared_weakly() {
    let map = headers(&[br#""a", W/"b""#, br#""c""#]);
    assert_eq!(
        header_condition(&map).expect("read"),
        Some(tags(&["a", "b", "c"]))
    );
}

/// RFC 9110 §5.6.1.2: a recipient ignores empty list elements.
#[test]
fn empty_list_elements_are_ignored() {
    let map = headers(&[br#""a",, ,"b","#]);
    assert_eq!(
        header_condition(&map).expect("read"),
        Some(tags(&["a", "b"]))
    );
}

/// RFC 9110 permits an empty opaque tag; it names no validator, so it matches
/// nothing rather than being refused.
#[test]
fn an_empty_opaque_tag_is_a_condition_that_matches_nothing() {
    assert_eq!(item_condition("\"\"").expect("read"), tags(&[""]));
}

#[test]
fn a_comma_inside_quotes_is_part_of_the_tag() {
    let map = headers(&[br#""a,b", "c""#]);
    assert_eq!(
        header_condition(&map).expect("read"),
        Some(tags(&["a,b", "c"]))
    );
}

#[test]
fn a_lone_wildcard_is_any() {
    let map = headers(&[b" * "]);
    assert_eq!(
        header_condition(&map).expect("read"),
        Some(IfNoneMatch::Any)
    );
}

/// Each of these would otherwise have been read as no condition, or as part of
/// one, and answered unconditionally.
#[test]
fn an_unreadable_header_is_refused_rather_than_ignored() {
    let over = format!("\"{}\"", "v".repeat(MAX_KEY_LEN + 1));
    for lines in [
        vec![b"*, \"a\"".as_slice()],
        vec![b"*".as_slice(), b"\"a\"".as_slice()],
        vec![b"unquoted".as_slice()],
        vec![b"\"a\", b".as_slice()],
        vec![b"\"a\"b\"".as_slice()],
        vec![b"\"a b\"".as_slice()],
        vec![b"\"a\tb\"".as_slice()],
        vec![b" , ".as_slice()],
        vec![b"".as_slice()],
        vec![b"\"caf\xc3\xa9\"".as_slice()],
        vec![over.as_bytes()],
    ] {
        assert_eq!(
            refused_field(header_condition(&headers(&lines))),
            "If-None-Match",
            "{lines:?}"
        );
    }
}

#[test]
fn a_header_tag_at_the_bound_is_read() {
    let at = "v".repeat(MAX_KEY_LEN);
    let map = headers(&[format!("\"{at}\"").as_bytes()]);
    assert_eq!(header_condition(&map).expect("read"), Some(tags(&[&at])));
}

#[test]
fn a_batch_item_takes_one_entity_tag_weak_or_strong() {
    assert_eq!(item_condition("\"a\"").expect("read"), tags(&["a"]));
    assert_eq!(item_condition(" W/\"a\" ").expect("read"), tags(&["a"]));
}

#[test]
fn a_batch_item_refuses_what_is_not_one_entity_tag() {
    for tag in [
        "*",
        "a",
        "",
        "\"a\", \"b\"",
        "\"a",
        "\"a b\"",
        "\"a\nb\"",
        "\"caf\u{e9}\"",
    ] {
        assert_eq!(
            refused_field(item_condition(tag)),
            "if_none_match",
            "{tag:?}"
        );
    }
}
