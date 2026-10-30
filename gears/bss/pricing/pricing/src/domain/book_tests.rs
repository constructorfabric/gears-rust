#![allow(clippy::expect_used, clippy::unwrap_used)]
use super::*;
use crate::domain::test_support::date;
use std::collections::BTreeMap;

/// The book rules over the ISO table alone, which most of these tests do not care about.
fn validate(book: &Book) -> Vec<RuleError> {
    super::validate(book, &CurrencyScales::default())
}
fn with_currency(currency: &str) -> Book {
    Book {
        currency: currency.into(),
        ..book()
    }
}
fn xts_at_two() -> CurrencyScales {
    CurrencyScales::try_new(BTreeMap::from([("XTS".to_owned(), 2)])).unwrap()
}

fn book() -> Book {
    Book {
        name: "APAC".into(),
        currency: "SGD".into(),
        valid_from: None,
        valid_until: None,
    }
}
#[test]
fn prototype_l81_book_valid() {
    assert!(validate(&book()).is_empty());
}
#[test]
fn prototype_l82_duplicate_name_is_now_allowed() {
    assert!(
        validate(&Book {
            name: "Default EUR".into(),
            ..book()
        })
        .is_empty()
    );
}
#[test]
fn prototype_l83_currency_uppercase() {
    assert_eq!(
        validate(&Book {
            currency: "eur".into(),
            ..book()
        })[0]
            .code,
        "BOOK_CURRENCY_INVALID"
    );
}
#[test]
fn a_code_the_table_does_not_know_is_refused() {
    let iso_only = CurrencyScales::default();
    for code in ["ZZZ", "XTS", "XAU", "BTC"] {
        let errors = super::validate(&with_currency(code), &iso_only);
        assert_eq!(errors.len(), 1, "an unknown code is one refusal");
        assert_eq!(errors[0].code, "BOOK_CURRENCY_INVALID");
    }
    for code in ["EUR", "USD", "JPY", "KWD", "CHF"] {
        assert!(super::validate(&with_currency(code), &iso_only).is_empty());
    }
}
#[test]
fn a_configured_non_iso_code_is_accepted_only_when_configured() {
    let errors = super::validate(&with_currency("XTS"), &CurrencyScales::default());
    assert_eq!(errors[0].code, "BOOK_CURRENCY_INVALID");
    assert!(super::validate(&with_currency("XTS"), &xts_at_two()).is_empty());
    let errors = super::validate(&with_currency("ZZZ"), &xts_at_two());
    assert_eq!(
        errors[0].code, "BOOK_CURRENCY_INVALID",
        "only the listed codes"
    );
}
#[test]
fn a_write_that_keeps_the_currency_judges_its_shape_only() {
    assert!(validate_shape(&with_currency("ZZZ")).is_empty());
    let errors = validate_shape(&with_currency("zzz"));
    assert_eq!(errors[0].code, "BOOK_CURRENCY_INVALID");
}
#[test]
fn prototype_l84_name_required() {
    assert_eq!(
        validate(&Book {
            name: " ".into(),
            ..book()
        })[0]
            .code,
        "BOOK_NAME_REQUIRED"
    );
}
#[test]
fn prototype_l229_book_validity_half_open() {
    let b = Book {
        valid_from: Some(date("2026-01-01")),
        valid_until: Some(date("2026-12-31")),
        ..book()
    };
    assert!(
        valid_on(&b, date("2026-01-01")),
        "the start is inside (PT-09)"
    );
    assert!(valid_on(&b, date("2026-06-01")));
    assert!(valid_on(&b, date("2026-12-30")));
    assert!(!valid_on(&b, date("2026-12-31")));
    assert!(!valid_on(&b, date("2025-12-31")));
}
#[test]
fn prototype_l230_unbounded_book() {
    assert!(valid_on(&book(), date("2031-01-01")));
}
#[test]
fn prototype_l231_book_validity_invalid() {
    assert_eq!(
        validate(&Book {
            valid_from: Some(date("2026-05-01")),
            valid_until: Some(date("2026-04-01")),
            ..book()
        })[0]
            .code,
        "BOOK_VALIDITY_INVALID"
    );
    // An empty window, its end on its start, is refused too (PT-09).
    assert_eq!(
        validate(&Book {
            valid_from: Some(date("2026-05-01")),
            valid_until: Some(date("2026-05-01")),
            ..book()
        })[0]
            .code,
        "BOOK_VALIDITY_INVALID"
    );
    assert!(
        validate(&Book {
            valid_from: Some(date("2026-05-01")),
            valid_until: Some(date("2026-05-02")),
            ..book()
        })
        .is_empty(),
        "one day is a window"
    );
}
