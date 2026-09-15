use super::*;

#[test]
fn parses_inclusive_range() {
    assert_eq!(
        parse("bytes=0-1023"),
        Some(ByteRange::Inclusive {
            start: 0,
            end: 1023
        })
    );
}

#[test]
fn parses_open_ended_range() {
    assert_eq!(
        parse("bytes=512-"),
        Some(ByteRange::OpenEnded { start: 512 })
    );
}

#[test]
fn parses_suffix_range() {
    assert_eq!(parse("bytes=-256"), Some(ByteRange::Suffix { length: 256 }));
}

#[test]
fn rejects_multi_range() {
    assert_eq!(parse("bytes=0-1,2-3"), None);
}

#[test]
fn rejects_missing_unit_prefix() {
    assert_eq!(parse("0-1023"), None);
}

#[test]
fn rejects_bare_dash() {
    assert_eq!(parse("bytes=-"), None);
}

#[test]
fn accepts_single_byte_range() {
    // `bytes=0-0` (first byte only) is syntactically valid: start == end.
    assert_eq!(
        parse("bytes=0-0"),
        Some(ByteRange::Inclusive { start: 0, end: 0 })
    );
}

#[test]
fn rejects_start_greater_than_end() {
    // RFC 9110 §14.1.1: last-byte-pos < first-byte-pos is invalid syntax and
    // MUST be ignored by the recipient, not treated as unsatisfiable.
    assert_eq!(parse("bytes=5-2"), None);
}

#[test]
fn rejects_leading_plus_sign() {
    assert_eq!(parse("bytes=+0-+5"), None);
    assert_eq!(parse("bytes=+0-5"), None);
    assert_eq!(parse("bytes=0-+5"), None);
    assert_eq!(parse("bytes=-+5"), None);
    assert_eq!(parse("bytes=+5-"), None);
}

#[test]
fn rejects_interior_whitespace() {
    // Outer OWS around the *entire* header value (e.g. a trailing space on
    // `"bytes=0-1023 "`) is legitimately stripped by `header.trim()` before
    // the `bytes=` grammar is even applied — that's generic HTTP field-value
    // parsing, not part of this test. These cases are all whitespace
    // *inside* the range-set itself, which the ABNF never allows.
    assert_eq!(parse("bytes= 0-1023"), None);
    assert_eq!(parse("bytes=0 -1023"), None);
    assert_eq!(parse("bytes=0- 1023"), None);
    assert_eq!(parse("bytes=0 -"), None);
    assert_eq!(parse("bytes=- 256"), None);
    assert_eq!(parse("bytes=512 -"), None);
}

#[test]
fn rejects_non_digit_garbage() {
    assert_eq!(parse("bytes=0x10-0x20"), None);
    assert_eq!(parse("bytes=1e3-2e3"), None);
}

#[test]
fn multi_range_ignores_whitespace_around_comma() {
    // Still unsupported (falls back to full body) regardless of OWS placement.
    assert_eq!(parse("bytes=0-1, 2-3"), None);
    assert_eq!(parse("bytes=0-1 , 2-3"), None);
}

#[test]
fn resolve_inclusive_clamps_end_to_total() {
    let r = ByteRange::Inclusive { start: 2, end: 100 };
    assert_eq!(r.resolve(10), Some((2, 9)));
}

#[test]
fn resolve_suffix_returns_tail() {
    let r = ByteRange::Suffix { length: 3 };
    assert_eq!(r.resolve(10), Some((7, 9)));
}

#[test]
fn resolve_open_ended_past_end_is_unsatisfiable() {
    let r = ByteRange::OpenEnded { start: 10 };
    assert_eq!(r.resolve(10), None);
}

#[test]
fn resolve_on_empty_content_is_none() {
    assert_eq!(ByteRange::OpenEnded { start: 0 }.resolve(0), None);
}
