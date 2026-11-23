//! Localized quota text (P-D-268): English is required, a tag has the package's form, a map and a
//! text have caps, and a read falls back to English and names the tag it served.
#![allow(clippy::unwrap_used, clippy::expect_used)]
use super::*;

fn text(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(tag, text)| ((*tag).to_owned(), (*text).to_owned()))
        .collect()
}

#[test]
fn a_missing_locale_falls_back_to_english() {
    let both = text(&[("en", "A"), ("ru", "B")]);
    assert_eq!(resolve(&both, Some("de")), ("A", "en"));
    assert_eq!(resolve(&both, Some("ru")), ("B", "ru"));
    assert_eq!(resolve(&both, None), ("A", "en"));
}

#[test]
fn a_bad_tag_is_validation() {
    for tag in ["EN", "e", "en_US", "english", "en-", "en-toolongsubtag", ""] {
        let refused = check_localized("metric_ref.display_name", &text(&[("en", "A"), (tag, "B")]))
            .unwrap_err();
        assert_eq!(
            (refused.code, refused.field.as_str()),
            ("VALIDATION", "metric_ref.display_name"),
            "{tag:?}"
        );
    }
    for tag in ["ru", "pt-BR", "zh-Hant-TW", "sgn-ASE"] {
        check_localized("f", &text(&[("en", "A"), (tag, "B")])).unwrap();
    }
}

#[test]
fn more_than_32_locales_is_too_long() {
    let mut many = text(&[("en", "A")]);
    for n in 0..31 {
        many.insert(format!("en-x{n}"), "B".to_owned());
    }
    assert_eq!(many.len(), 32);
    check_localized("f", &many).unwrap();
    many.insert("en-y".to_owned(), "B".to_owned());
    let refused = check_localized("f", &many).unwrap_err();
    assert_eq!(
        (refused.code, refused.field.as_str()),
        ("FIELD_TOO_LONG", "f")
    );
    let long = text(&[("en", &"a".repeat(2001))]);
    assert_eq!(
        check_localized("f", &long).unwrap_err().code,
        "FIELD_TOO_LONG"
    );
    check_localized("f", &text(&[("en", &"\u{e9}".repeat(2000))])).unwrap();
}

#[test]
fn english_is_required_and_not_blank() {
    for missing in [
        text(&[("ru", "B")]),
        text(&[("en", "")]),
        text(&[("en", "   ")]),
    ] {
        let refused = check_localized("metric_ref.description", &missing).unwrap_err();
        assert_eq!(
            (refused.code, refused.field.as_str()),
            ("QUOTA_DISPLAY_TEXT_MISSING", "metric_ref.description")
        );
    }
}
