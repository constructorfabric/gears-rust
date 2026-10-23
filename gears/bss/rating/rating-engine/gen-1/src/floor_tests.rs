#![allow(clippy::expect_used, clippy::unwrap_used)]

//! The floor amounts of feature 09's vectors V34, V41, V50–V52 and V61 (T-D-78).

use time::{Date, Month, OffsetDateTime, Time};

use super::{FloorPart, Interval, PriceTotal, minimum_fee_floor};
use crate::outcome::MoneyText;
use crate::test_input::id;

/// 30 days of September, or 31 days of October.
const SEPTEMBER: u64 = 2_592_000;
const OCTOBER: u64 = 2_678_400;

fn day(month: Month, day: u8) -> OffsetDateTime {
    OffsetDateTime::new_utc(
        Date::from_calendar_date(2026, month, day).unwrap(),
        Time::MIDNIGHT,
    )
}

fn money(numerator: &str, denominator: &str, scale: u8) -> MoneyText {
    MoneyText {
        numerator: numerator.to_owned(),
        denominator: denominator.to_owned(),
        currency: "EUR".to_owned(),
        currency_scale: scale,
    }
}

/// A child's total for `price` at `scale`, covering `[from, to)` of September.
fn total(price: u128, scale: u8, rated: (&str, &str), days: (u8, u8), fee: &str) -> PriceTotal {
    let end = if days.1 > 30 {
        day(Month::October, 1)
    } else {
        day(Month::September, days.1)
    };
    PriceTotal {
        price_id: id(price),
        currency_scale: scale,
        rated: money(rated.0, rated.1, scale),
        covered: vec![Interval {
            start: day(Month::September, days.0),
            end,
        }],
        period_seconds: SEPTEMBER,
        minimum_fee: Some(money(fee, "1", scale)),
    }
}

fn topups(parts: &[FloorPart]) -> Vec<(String, String)> {
    parts
        .iter()
        .map(|part| (part.topup.numerator.clone(), part.topup.denominator.clone()))
        .collect()
}

#[test]
fn v61_a_recurring_price_is_floored_by_its_covered_fraction() {
    let full = minimum_fee_floor(&[total(1, 2, ("10", "1"), (1, 31), "30")]).unwrap();
    assert_eq!(topups(&full), [("20".into(), "1".into())]);
    let half = minimum_fee_floor(&[total(1, 2, ("5", "1"), (16, 31), "30")]).unwrap();
    assert_eq!(
        (half[0].floor.numerator.as_str(), half[0].covered_seconds),
        ("15", 1_296_000)
    );
    assert_eq!(topups(&half), [("10".into(), "1".into())]);
}

#[test]
fn v52_an_ends_on_handover_floors_each_price_by_its_own_share() {
    let parts = minimum_fee_floor(&[
        total(1, 2, ("5", "1"), (1, 15), "30"),
        total(2, 2, ("5", "1"), (15, 31), "30"),
    ])
    .unwrap();
    let floors: Vec<&str> = parts
        .iter()
        .map(|part| part.floor.numerator.as_str())
        .collect();
    assert_eq!(floors, ["14", "16"]);
    assert_eq!(
        topups(&parts),
        [("9".into(), "1".into()), ("11".into(), "1".into())]
    );
}

#[test]
fn v50_v51_the_top_up_is_exact() {
    let v50 = minimum_fee_floor(&[PriceTotal {
        minimum_fee: Some(money("3001", "100", 2)),
        ..total(1, 2, ("2001", "200"), (1, 31), "30")
    }])
    .unwrap();
    assert_eq!(topups(&v50), [("4001".into(), "200".into())]);
    let v51 = minimum_fee_floor(&[PriceTotal {
        covered: vec![Interval {
            start: day(Month::October, 1),
            end: day(Month::October, 22),
        }],
        period_seconds: OCTOBER,
        ..total(1, 2, ("2503", "500"), (1, 31), "30")
    }])
    .unwrap();
    assert_eq!(
        (
            v51[0].floor.numerator.as_str(),
            v51[0].floor.denominator.as_str()
        ),
        ("630", "31")
    );
    assert_eq!(topups(&v51), [("237407".into(), "15500".into())]);
}

#[test]
fn v34_values_of_one_price_share_a_floor_and_own_prices_floor_alone() {
    // Two dimension values of one default price, rated 10 each over the full period: one floor.
    let shared = minimum_fee_floor(&[
        total(1, 2, ("10", "1"), (1, 31), "30"),
        total(1, 2, ("10", "1"), (1, 31), "30"),
    ])
    .unwrap();
    assert_eq!(topups(&shared), [("10".into(), "1".into())]);
    // Two own prices: two floors of 30.
    let own = minimum_fee_floor(&[
        total(1, 2, ("10", "1"), (1, 31), "30"),
        total(2, 2, ("10", "1"), (1, 31), "30"),
    ])
    .unwrap();
    assert_eq!(
        topups(&own),
        [("20".into(), "1".into()), ("20".into(), "1".into())]
    );
    // A fact covering half its period applies half the fee.
    let half = minimum_fee_floor(&[
        total(1, 2, ("5", "1"), (1, 16), "30"),
        total(1, 2, ("5", "1"), (1, 16), "30"),
    ])
    .unwrap();
    assert_eq!(half[0].floor.numerator, "15");
    assert_eq!(topups(&half), [("5".into(), "1".into())]);
}

#[test]
fn v41_two_stored_scales_of_one_price_are_floored_apart() {
    let parts = minimum_fee_floor(&[
        total(1, 2, ("5", "1"), (1, 16), "30"),
        total(1, 3, ("5", "1"), (16, 31), "30"),
    ])
    .unwrap();
    let scales: Vec<(u8, &str, &str)> = parts
        .iter()
        .map(|part| {
            (
                part.currency_scale,
                part.floor.numerator.as_str(),
                part.topup.numerator.as_str(),
            )
        })
        .collect();
    assert_eq!(scales, [(2, "15", "10"), (3, "15", "10")]);
}

#[test]
fn a_rated_sum_above_the_floor_needs_no_top_up_and_no_fee_no_floor() {
    let parts = minimum_fee_floor(&[total(1, 2, ("40", "1"), (1, 31), "30")]).unwrap();
    assert_eq!(topups(&parts), [("0".into(), "1".into())]);
    let mut no_fee = total(1, 2, ("5", "1"), (1, 31), "30");
    no_fee.minimum_fee = None;
    assert!(minimum_fee_floor(&[no_fee]).unwrap().is_empty());
}

#[test]
fn totals_that_disagree_fail_by_name() {
    let reason = |totals: &[PriceTotal]| minimum_fee_floor(totals).unwrap_err().reason_code();
    let base = total(1, 2, ("5", "1"), (1, 16), "30");
    let other_period = PriceTotal {
        period_seconds: OCTOBER,
        ..total(1, 2, ("5", "1"), (16, 31), "30")
    };
    assert_eq!(
        reason(&[base.clone(), other_period]),
        "period_geometry_mismatch"
    );
    let other_fee = total(1, 2, ("5", "1"), (16, 31), "40");
    assert_eq!(reason(&[base.clone(), other_fee]), "binding_mismatch");
    let mut usd = total(1, 2, ("5", "1"), (16, 31), "30");
    usd.rated.currency = "USD".to_owned();
    assert_eq!(reason(&[base.clone(), usd]), "binding_currency_mismatch");
    let mut unreduced = base.clone();
    unreduced.rated = money("10", "2", 2);
    assert_eq!(reason(&[unreduced]), "money_contract_violation");
    let mut mislabelled = base.clone();
    mislabelled.currency_scale = 3;
    assert_eq!(reason(&[mislabelled]), "money_contract_violation");
    let mut beyond = base;
    beyond.period_seconds = 86_400;
    assert_eq!(reason(&[beyond]), "period_geometry_mismatch");
}

#[test]
fn a_malformed_covered_interval_is_refused_not_dropped() {
    let mut reversed = total(1, 2, ("5", "1"), (16, 31), "30");
    reversed.covered[0] = Interval {
        start: reversed.covered[0].end,
        end: reversed.covered[0].start,
    };
    assert_eq!(
        minimum_fee_floor(&[reversed]).unwrap_err().reason_code(),
        "period_geometry_mismatch"
    );
    // Covered time that spans more than the period cannot lie in it.
    let mut wide = total(1, 2, ("5", "1"), (1, 5), "30");
    wide.covered.push(Interval {
        start: day(Month::October, 20),
        end: day(Month::October, 21),
    });
    assert_eq!(
        minimum_fee_floor(&[wide]).unwrap_err().reason_code(),
        "period_geometry_mismatch"
    );
}

#[test]
fn a_rated_sum_equal_to_the_floor_needs_no_top_up() {
    let parts = minimum_fee_floor(&[total(1, 2, ("30", "1"), (1, 31), "30")]).unwrap();
    assert_eq!(topups(&parts), [("0".into(), "1".into())]);
}
