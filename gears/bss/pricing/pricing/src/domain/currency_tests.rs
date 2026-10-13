#![allow(clippy::expect_used, clippy::unwrap_used)]
use super::*;

fn overrides(pairs: &[(&str, u8)]) -> BTreeMap<String, u8> {
    pairs
        .iter()
        .map(|(code, scale)| ((*code).to_owned(), *scale))
        .collect()
}
fn scale_of(scales: &CurrencyScales, code: &str) -> Option<u8> {
    scales.resolve(code).map(|spec| spec.scale())
}
#[test]
fn an_iso_code_resolves_to_its_iso_scale() {
    let scales = CurrencyScales::default();
    assert_eq!(scale_of(&scales, "EUR"), Some(2));
    assert_eq!(scale_of(&scales, "JPY"), Some(0));
    assert_eq!(scale_of(&scales, "KWD"), Some(3));
    assert_eq!(scale_of(&scales, "CLF"), Some(4));
    let spec = scales.resolve("EUR").unwrap();
    assert_eq!(spec.code(), "EUR");
}
#[test]
fn a_code_that_is_not_in_any_table_is_unknown() {
    let scales = CurrencyScales::default();
    assert_eq!(scales.resolve("ZZZ"), None);
    assert_eq!(
        scales.resolve("XTS"),
        None,
        "ISO has no scale for a test code"
    );
    assert_eq!(scales.resolve("XAU"), None, "ISO has no scale for gold");
}
#[test]
fn a_code_must_be_spelled_as_iso_writes_it() {
    let scales = CurrencyScales::try_new(overrides(&[("XTS", 2)])).unwrap();
    for bad in ["eur", "Eur", " EUR", "EUR ", "EURO", "EU", "", "E1R", "xts"] {
        assert_eq!(scales.resolve(bad), None, "a malformed code is unknown");
    }
}
#[test]
fn a_configured_scale_resolves_a_non_iso_code() {
    let scales = CurrencyScales::try_new(overrides(&[("XTS", 2), ("QQQ", 8)])).unwrap();
    let spec = scales.resolve("XTS").unwrap();
    assert_eq!((spec.code(), spec.scale()), ("XTS", 2));
    assert_eq!(scale_of(&scales, "QQQ"), Some(8));
    assert_eq!(scale_of(&scales, "ZZZ"), None, "only the listed codes");
    assert_eq!(scale_of(&scales, "EUR"), Some(2), "ISO codes still resolve");
}
#[test]
fn a_scale_of_28_is_the_highest_accepted() {
    let scales = CurrencyScales::try_new(overrides(&[("XTS", 28)])).unwrap();
    assert_eq!(scale_of(&scales, "XTS"), Some(28));
    assert_eq!(scale_of(&scales, "ZZZ"), None);
}
#[test]
fn an_override_is_refused_for_an_iso_code() {
    for (code, scale) in [("EUR", 3), ("EUR", 2), ("JPY", 2), ("KWD", 3)] {
        let refused = CurrencyScales::try_new(overrides(&[(code, scale)])).unwrap_err();
        assert_eq!(
            refused,
            CurrencyScalesError::IsoCode {
                code: code.to_owned()
            },
            "an ISO code keeps its ISO scale"
        );
    }
}
#[test]
fn an_override_is_refused_for_a_malformed_key() {
    for key in ["USDC", "us", "usd", "Xts", "XT1", "", " XT"] {
        let refused = CurrencyScales::try_new(overrides(&[(key, 6)])).unwrap_err();
        assert_eq!(
            refused,
            CurrencyScalesError::NotACode {
                code: key.to_owned()
            },
            "a key is three uppercase letters"
        );
    }
}
#[test]
fn an_override_is_refused_above_scale_28() {
    for scale in [29, u8::MAX] {
        let refused = CurrencyScales::try_new(overrides(&[("XTS", scale)])).unwrap_err();
        assert_eq!(
            refused,
            CurrencyScalesError::ScaleOutOfRange {
                code: "XTS".to_owned(),
                scale
            },
            "a scale is at most 28"
        );
    }
}
#[test]
fn a_refusal_names_the_key_and_the_cause() {
    let refused = CurrencyScales::try_new(overrides(&[("XTS", 29)])).unwrap_err();
    assert_eq!(
        refused.to_string(),
        "currency_scales key XTS has scale 29: a scale is at most 28"
    );
    let refused = CurrencyScales::try_new(overrides(&[("EUR", 3)])).unwrap_err();
    assert_eq!(
        refused.to_string(),
        "currency_scales key EUR is an ISO 4217 code: its scale comes from ISO 4217 and is never \
         listed here"
    );
    let refused = CurrencyScales::try_new(overrides(&[("usd", 2)])).unwrap_err();
    assert_eq!(
        refused.to_string(),
        "currency_scales key \"usd\" is not a currency code: three uppercase letters"
    );
}
#[test]
fn one_bad_entry_refuses_the_whole_table() {
    // Each bad entry sorts after a good one, so a check of the first entry alone passes them.
    let refused = CurrencyScales::try_new(overrides(&[("QQQ", 2), ("XTS", 29)])).unwrap_err();
    assert_eq!(
        refused,
        CurrencyScalesError::ScaleOutOfRange {
            code: "XTS".to_owned(),
            scale: 29
        },
        "the later entry is judged"
    );
    let refused = CurrencyScales::try_new(overrides(&[("AAA", 2), ("EUR", 3)])).unwrap_err();
    assert_eq!(
        refused,
        CurrencyScalesError::IsoCode {
            code: "EUR".to_owned()
        },
        "the later entry is judged"
    );
}
#[test]
fn an_empty_table_still_resolves_the_iso_codes() {
    let none = CurrencyScales::try_new(BTreeMap::new()).unwrap();
    assert_eq!(scale_of(&none, "EUR"), Some(2), "an ISO code resolves");
    assert_eq!(scale_of(&none, "JPY"), Some(0), "an ISO code resolves");
    assert_eq!(scale_of(&none, "XTS"), None, "a non-ISO code is unknown");
}
