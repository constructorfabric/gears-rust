//! Tests for the gear's one canonical rendering rule and its `SHA-256`
//! digest (P-D-201).
//!
//! Every case here asserts a **string or a byte vector**, never a predicate
//! over two of them: a suite that only checked "these two renderings agree"
//! would pass just as happily against a function that returned a constant.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::fmt::Write as _;

use serde_json::json;

use super::{canonical_rendering, content_digest};

/// Lowercase hex, so a golden vector can be read and re-typed by a human and
/// compared against any `sha256sum` on any machine.
fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut out, byte| {
        write!(out, "{byte:02x}").expect("writing to a String cannot fail");
        out
    })
}

/// Keys are sorted lexicographically at **every** object level and no
/// insignificant whitespace survives.
///
/// A canonicalizer that sorted the outermost keys and then handed each value
/// to `serde_json`'s own `Display` would pass a flat case and fail this one,
/// which is exactly the shortcut this test refuses. Whitespace is asserted
/// by the same literal: the expected string carries none, so a renderer that
/// pretty-printed anything at all fails here rather than at a golden vector
/// three slices later.
#[test]
fn keys_sort_at_every_level_and_no_whitespace_survives() {
    let as_first_sent: serde_json::Value =
        serde_json::from_str("{ \"outer\" : { \"z\" : 1 ,\n \"a\" : 2 } , \"first\" : true }")
            .expect("the case's own input must be valid JSON");

    assert_eq!(
        canonical_rendering(&as_first_sent),
        r#"{"first":true,"outer":{"a":2,"z":1}}"#
    );
}

/// Numbers render as bare decimal strings with no trailing zeroes, and a
/// timestamp string is carried through verbatim at microsecond precision.
///
/// §4.3 states both clauses. The number half is what keeps a client library
/// that renders `1` on one attempt and `1.0` on the next from forking a
/// digest. The timestamp half pins that nothing here reformats an instant:
/// the caller renders `RFC 3339` in `UTC` and this module carries the string
/// it was given, so the precision decision stays with the column's owner.
#[test]
fn numbers_carry_no_trailing_zeroes_and_a_timestamp_string_passes_through() {
    let value = json!({
        "count": 1.0,
        "negative": -7,
        "published_at": "2026-08-27T11:04:05.123456Z",
        "weight_kg": 1.5,
    });

    assert_eq!(
        canonical_rendering(&value),
        "{\"count\":1,\"negative\":-7,\"published_at\":\"2026-08-27T11:04:05.123456Z\",\
         \"weight_kg\":1.5}",
        "1.0 renders as 1, a fraction keeps its digits, and the instant is untouched"
    );
}

/// The golden vector: one fixed input, its exact rendering, and its exact
/// `SHA-256`.
///
/// The rendering and the digest are pinned **separately and by literal**, so
/// the vector holds each of them independently: a change to the rendering
/// fails the first assertion, a change to the digest primitive fails only
/// the second, and a reader can tell the two apart without reading this
/// crate. The digest below was computed outside `Rust` and can be
/// reproduced by anyone:
///
/// ```text
/// printf '%s' '{"brand_id":"3f8f6a1e-0000-4000-8000-000000000001","name":"Fibre 500","product_code":null,"published_at":"2026-08-27T11:04:05.123456Z","weight_kg":1.5}' | sha256sum
/// # e252632893610a1207b4844a24a1aec1682c8a4b7b5242bd7a26b082b1e77c35
/// ```
///
/// The input deliberately exercises every clause at once: an explicit `null`,
/// unsorted input keys, a fraction, and an `RFC 3339` timestamp string.
#[test]
fn the_golden_vector_pins_the_rendering_and_the_digest_independently() {
    let content = json!({
        "name": "Fibre 500",
        "weight_kg": 1.5,
        "product_code": null,
        "brand_id": "3f8f6a1e-0000-4000-8000-000000000001",
        "published_at": "2026-08-27T11:04:05.123456Z",
    });

    let rendered = canonical_rendering(&content);

    assert_eq!(
        rendered,
        "{\"brand_id\":\"3f8f6a1e-0000-4000-8000-000000000001\",\"name\":\"Fibre 500\",\
         \"product_code\":null,\"published_at\":\"2026-08-27T11:04:05.123456Z\",\
         \"weight_kg\":1.5}",
        "the rendering is the digest's whole input and is pinned first"
    );
    assert_eq!(
        hex(&content_digest(&rendered)),
        "e252632893610a1207b4844a24a1aec1682c8a4b7b5242bd7a26b082b1e77c35",
        "the digest must equal the independently computed vector, byte for byte"
    );
    assert_eq!(
        content_digest(&rendered).len(),
        32,
        "a full SHA-256, not a truncation of one"
    );
}

/// A string value is escaped the way `JSON` escapes strings, so a value
/// carrying a quote cannot forge the rendering's own punctuation.
///
/// Without escaping, a `name` of `a","brand_id":"b-2` would render as two
/// fields and let one content's digest be spelled by another's payload — the
/// injection a canonical rendering assembled by concatenation invites.
#[test]
fn a_string_value_cannot_forge_the_renderings_punctuation() {
    let hostile = json!({ "name": "a\",\"brand_id\":\"b-2", "brand_id": "b-1" });
    let honest = json!({ "name": "a", "brand_id": "b-2" });

    assert_eq!(
        canonical_rendering(&hostile),
        "{\"brand_id\":\"b-1\",\"name\":\"a\\\",\\\"brand_id\\\":\\\"b-2\"}",
        "the quotes inside the value are escaped, not passed through as structure"
    );
    assert_ne!(
        content_digest(&canonical_rendering(&hostile)),
        content_digest(&canonical_rendering(&honest))
    );
}

/// An array is rendered in the order received.
///
/// Pinned as the **current** behaviour rather than as the final rule: §4.3
/// sorts a row collection by the collection's own identifier, and this case
/// is what will go red when the first door whose payload carries a
/// collection arrives — which is the point at which that sort is owed, and
/// the reason it is named as owed rather than pre-built here.
#[test]
fn an_array_is_rendered_in_the_order_received_today() {
    let value = json!({ "tags": ["z", "a", "m"] });

    assert_eq!(
        canonical_rendering(&value),
        r#"{"tags":["z","a","m"]}"#,
        "no collection sort is applied yet and none is owed until a collection exists"
    );
}
