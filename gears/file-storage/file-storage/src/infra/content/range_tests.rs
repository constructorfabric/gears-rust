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
