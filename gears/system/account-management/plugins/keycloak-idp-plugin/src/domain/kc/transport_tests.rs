use super::*;

#[test]
fn truncate_short_passthrough() {
    let s = "hello world";
    assert_eq!(truncate_2kb(s), "hello world");
}

#[test]
fn truncate_long_appends_ellipsis() {
    let s: String = "a".repeat(3000);
    let out = truncate_2kb(&s);
    assert!(out.ends_with('\u{2026}'));
    assert_eq!(out.chars().count(), 2049);
}

#[test]
fn truncate_escapes_non_printable_ascii() {
    let s = "ok\x01here";
    let out = truncate_2kb(s);
    assert_eq!(out, "ok\\x01here");
}
