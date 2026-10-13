#![allow(clippy::expect_used, clippy::unwrap_used)]
use super::*;
use crate::domain::test_support::date;

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
/// D-524: one half-open rule over several days; an absent bound is open.
#[test]
fn sells_on_each_judges_every_day_in_the_half_open_window() {
    let (from, until) = (Some(date("2026-10-01")), Some(date("2026-10-03")));
    assert!(sells_on_each(
        from,
        until,
        &[date("2026-10-01"), date("2026-10-02")]
    ));
    assert!(
        !sells_on_each(from, until, &[date("2026-10-02"), date("2026-10-03")]),
        "the end is outside"
    );
    assert!(
        !sells_on_each(from, until, &[date("2026-09-30"), date("2026-10-01")]),
        "a day before the start"
    );
    assert!(
        sells_on_each(None, until, &[date("2020-01-01"), date("2026-10-02")]),
        "no start bound"
    );
    assert!(
        sells_on_each(from, None, &[date("2026-10-01"), date("2099-12-31")]),
        "no end bound"
    );
    assert!(!sells_on_each(
        None,
        until,
        &[date("2026-10-02"), date("2026-10-03")]
    ));
    assert!(sells_on_each(None, None, &[date("1970-01-01")]));
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
#[test]
fn matrix_18_minor_digits_follow_the_book_currency() {
    assert_eq!(minor_digits("EUR"), 2);
    assert_eq!(minor_digits("USD"), 2);
    assert_eq!(minor_digits("JPY"), 0);
    assert_eq!(minor_digits("KWD"), 3);
}
