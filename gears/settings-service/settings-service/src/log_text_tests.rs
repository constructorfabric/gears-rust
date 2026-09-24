// Created: 2026-09-23 by Virtuozzo International GmbH
//! Tests for the single-line log rendering.

use super::LogSafe;
use crate::domain::error::DomainError;

#[test]
fn line_breaks_and_tabs_are_spelled_out() {
    assert_eq!(
        LogSafe("first\nsecond\r\nthird\tend").to_string(),
        "first\\nsecond\\r\\nthird\\tend"
    );
}

#[test]
fn other_control_characters_are_escaped_as_code_points() {
    // ESC starts a terminal sequence; NUL, DEL and the C1 range are the rest
    // of what a terminal or a log parser may act on.
    assert_eq!(LogSafe("\u{1b}[31mred").to_string(), "\\u{1b}[31mred");
    assert_eq!(
        LogSafe("a\u{0}b\u{7f}c\u{85}d").to_string(),
        "a\\u{0}b\\u{7f}c\\u{85}d"
    );
}

#[test]
fn readable_text_of_any_script_passes_through() {
    // Cyrillic, an em dash and a check mark, spelled as escapes only because the
    // lint on literals asks for it; the rendered text carries the characters.
    let text = "the credential store could not create: \u{445}\u{440}\u{430}\u{43d}\u{438}\u{43b}\u{438}\u{449}\u{435} \u{43d}\u{435}\u{434}\u{43e}\u{441}\u{442}\u{443}\u{43f}\u{43d}\u{43e} \u{2014} retry \u{2713}";
    assert_eq!(LogSafe(text).to_string(), text);
}

#[test]
fn a_dependency_error_with_a_forged_line_stays_on_one_line() {
    // What a misbehaving credential store could hand back, wrapped the way the
    // secret manager wraps it before it reaches a log macro.
    let err = DomainError::Unavailable {
        detail: "the credential store could not create: upstream said 503\n\
                 2026-09-23T10:00:00Z  INFO settings value changed key=forged"
            .to_owned(),
    };
    let rendered = LogSafe(&err).to_string();
    assert!(!rendered.contains('\n'), "{rendered}");
    assert!(
        rendered.contains("503\\n2026-09-23T10:00:00Z"),
        "{rendered}"
    );
}

#[test]
fn bidirectional_format_characters_are_escaped_too() {
    // Not control characters (they are `Cf`, format), yet a terminal that
    // honours them reorders what follows — a dependency's text could make a
    // log line read differently from what it holds.
    for c in [
        '\u{061C}', '\u{200E}', '\u{200F}', '\u{202A}', '\u{202B}', '\u{202C}', '\u{202D}',
        '\u{202E}', '\u{2066}', '\u{2067}', '\u{2068}', '\u{2069}',
    ] {
        let raw = format!("user{c}nimda");
        let shown = super::LogSafe(&raw).to_string();
        assert!(
            !shown.contains(c),
            "U+{:04X} is escaped: {shown:?}",
            u32::from(c)
        );
        assert_eq!(shown, format!("user\\u{{{:x}}}nimda", u32::from(c)));
    }
    // Letters of right-to-left scripts themselves pass through unchanged.
    let shalom = "\u{5e9}\u{5dc}\u{5d5}\u{5dd}";
    assert_eq!(super::LogSafe(shalom).to_string(), shalom);
}
