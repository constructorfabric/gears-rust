//! ISO 4217 scale table tests.
#![allow(clippy::unwrap_used)]

use super::{iso_codes, iso_scale};

#[test]
fn common_currencies_have_their_iso_scale() {
    assert_eq!(iso_scale("EUR"), Some(2));
    assert_eq!(iso_scale("USD"), Some(2));
    assert_eq!(iso_scale("JPY"), Some(0));
    assert_eq!(iso_scale("KWD"), Some(3));
    assert_eq!(iso_scale("CLF"), Some(4));
}

#[test]
fn huf_has_two_minor_digits() {
    // ISO 4217 gives HUF exponent 2, although the forint is cash-rounded to whole units.
    assert_eq!(iso_scale("HUF"), Some(2));
}

#[test]
fn every_zero_three_and_four_digit_code_is_listed() {
    for code in [
        "BIF", "CLP", "DJF", "GNF", "ISK", "JPY", "KMF", "KRW", "PYG", "RWF", "UGX", "UYI", "VND",
        "VUV", "XAF", "XOF", "XPF",
    ] {
        assert_eq!(iso_scale(code), Some(0), "an exponent 0 code is wrong");
    }
    for code in ["BHD", "IQD", "JOD", "KWD", "LYD", "OMR", "TND"] {
        assert_eq!(iso_scale(code), Some(3), "an exponent 3 code is wrong");
    }
    for code in ["CLF", "UYW"] {
        assert_eq!(iso_scale(code), Some(4), "an exponent 4 code is wrong");
    }
}

#[test]
fn codes_without_a_minor_unit_have_no_scale() {
    for code in [
        "XAG", "XAU", "XBA", "XBB", "XBC", "XBD", "XDR", "XPD", "XPT", "XSU", "XTS", "XUA", "XXX",
    ] {
        assert_eq!(
            iso_scale(code),
            None,
            "a code with no minor unit has a scale"
        );
    }
}

#[test]
fn unknown_and_malformed_codes_have_no_scale() {
    for code in [
        "ZZZ", "XBT", "BTC", "USDC", "eur", "Eur", " EUR", "EUR ", "EU", "", "\u{20ac}",
    ] {
        assert_eq!(
            iso_scale(code),
            None,
            "an unlisted or malformed code has a scale"
        );
    }
}

#[test]
fn the_table_lists_exactly_the_active_codes() {
    // Walk every three-letter code so a dropped or extra entry changes a count.
    let (mut zero, mut two, mut three, mut four) = (0, 0, 0, 0);
    for a in b'A'..=b'Z' {
        for b in b'A'..=b'Z' {
            for c in b'A'..=b'Z' {
                let code = String::from_utf8(vec![a, b, c]).unwrap();
                match iso_scale(&code) {
                    Some(0) => zero += 1,
                    Some(2) => two += 1,
                    Some(3) => three += 1,
                    Some(4) => four += 1,
                    Some(_) => panic!("a scale other than 0, 2, 3 or 4"),
                    None => {}
                }
            }
        }
    }
    assert_eq!((zero, two, three, four), (17, 139, 7, 2));
}

#[test]
fn the_code_list_is_the_table_in_code_order() {
    let listed: Vec<(&str, u8)> = iso_codes().collect();
    assert_eq!(listed.len(), 165);
    assert!(
        listed.windows(2).all(|pair| pair[0].0 < pair[1].0),
        "the codes are listed once each, in code order"
    );
    for (code, scale) in &listed {
        assert_eq!(iso_scale(code), Some(*scale), "{code}");
    }
    // Every three-letter code with a scale is listed.
    for a in b'A'..=b'Z' {
        for b in b'A'..=b'Z' {
            for c in b'A'..=b'Z' {
                let code = String::from_utf8(vec![a, b, c]).unwrap();
                assert_eq!(
                    iso_scale(&code).is_some(),
                    listed.iter().any(|(listed, _)| *listed == code),
                    "{code}"
                );
            }
        }
    }
}
