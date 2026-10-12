//! Grain totals (coefficient fast path, exact fallback, untrusted state) and
//! the X4 tolerance rule.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use bss_ledger_sdk::{CurrencySpec, MoneyError, PostedMoney, canonical_decimal, parse_decimal};
use rust_decimal::Decimal;

use super::{
    GrainAmount, GrainError, ReconciliationVariance, ToleranceDecision, ar_tolerance_decision,
    psp_tolerance_decision, tolerance_budget,
};
use crate::domain::error::DomainError;
use crate::domain::exact_money::{ExactAmount, ExactError};

fn money(amount: &str, currency: &str, scale: u8) -> PostedMoney {
    PostedMoney::try_new(
        parse_decimal(amount).unwrap(),
        CurrencySpec::try_new(currency.to_owned(), scale).unwrap(),
    )
    .unwrap()
}

fn grain(amount: &str, currency: &str, scale: u8) -> GrainAmount {
    GrainAmount::from_posted(&money(amount, currency, scale))
}

/// The largest 28-digit integer amount the posting contract admits.
const MAX_28: &str = "9999999999999999999999999999";

/// The fold the tie-out ran before the coefficient fast path: one exact
/// fraction add per posting.
fn exact_fold(postings: &[PostedMoney]) -> ExactAmount {
    let mut total = ExactAmount::from_decimal(Decimal::ZERO);
    for posting in postings {
        total = total
            .checked_add(&ExactAmount::from_decimal(posting.amount()))
            .unwrap();
    }
    total
}

fn grain_fold(postings: &[PostedMoney], currency: &str, scale: u8) -> GrainAmount {
    let mut total = GrainAmount::zero(currency, scale);
    for posting in postings {
        total.add_posted(posting);
    }
    total
}

/// Deterministic signed USD@2 postings of mixed size and trailing zeros.
fn postings(count: usize) -> Vec<PostedMoney> {
    let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
    (0..count)
        .map(|_| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let cents = i64::try_from(state >> 40).unwrap() - (1 << 23);
            PostedMoney::try_new(
                Decimal::new(cents, 2),
                CurrencySpec::try_new("USD".to_owned(), 2).unwrap(),
            )
            .unwrap()
        })
        .collect()
}

#[test]
fn the_coefficient_fold_equals_the_exact_fraction_fold() {
    let lines = postings(5_000);
    let fast = grain_fold(&lines, "USD", 2);
    let exact = exact_fold(&lines);
    assert_eq!(fast, GrainAmount::from_exact(exact.clone(), "USD", 2));
    assert!(!fast.differs_from(&GrainAmount::from_exact(exact.clone(), "USD", 2)));
    assert_eq!(fast.text(), exact.canonical_at_scale(2).unwrap());
    assert_eq!(
        fast.to_posted().unwrap(),
        exact
            .into_posted_exact(CurrencySpec::try_new("USD".to_owned(), 2).unwrap())
            .unwrap()
    );
}

#[test]
fn a_coefficient_beyond_i128_falls_back_to_an_exact_fraction() {
    // At scale 28 a 28-digit integer has a 56-digit coefficient.
    let mut total = GrainAmount::zero("XAU", 28);
    total.add_posted(&money(MAX_28, "XAU", 28));
    total.add_posted(&money("0.0000000000000000000000000001", "XAU", 28));
    assert!(!total.is_untrusted());
    assert_eq!(
        total.text(),
        "9999999999999999999999999999.0000000000000000000000000001"
    );
    total.add_posted(&money(&format!("-{MAX_28}"), "XAU", 28));
    assert_eq!(total, grain("0.0000000000000000000000000001", "XAU", 28));
    total.add_posted(&money("-0.0000000000000000000000000001", "XAU", 28));
    assert!(total.is_zero(), "a fraction that returns to zero is zero");
}

#[test]
fn a_running_sum_beyond_i128_falls_back_to_an_exact_fraction() {
    // At scale 10 each term's coefficient is about 10^38, so the second add
    // overflows i128 (about 1.7 × 10^38).
    let term = money(MAX_28, "BIG", 10);
    let mut total = GrainAmount::zero("BIG", 10);
    for _ in 0..3 {
        total.add_posted(&term);
    }
    let expected = exact_fold(&[term.clone(), term.clone(), term.clone()]);
    assert!(!total.is_untrusted());
    assert_eq!(total, GrainAmount::from_exact(expected, "BIG", 10));
    assert!(total.to_posted().is_err(), "beyond the posting contract");

    // Grain + grain takes the same fallback.
    let mut left = GrainAmount::from_posted(&term);
    left.add_grain(&GrainAmount::from_posted(&term));
    let mut right = GrainAmount::from_posted(&term);
    right.add_posted(&term);
    assert_eq!(left, right);
}

#[test]
fn equal_values_in_both_forms_are_equal_and_tie_out() {
    let coefficient = grain("12.5", "EUR", 2);
    let fraction = GrainAmount::from_exact(
        ExactAmount::from_decimal(parse_decimal("12.50").unwrap()),
        "EUR",
        2,
    );
    assert_eq!(coefficient, fraction);
    assert_eq!(fraction, coefficient);
    assert!(!coefficient.differs_from(&fraction));
    assert!(coefficient.differs_from(&grain("12.51", "EUR", 2)));
    assert_ne!(coefficient, grain("12.5", "EUR", 3), "scale is identity");
    assert_ne!(coefficient, grain("12.5", "USD", 2), "currency is identity");
}

#[test]
fn an_untrusted_total_has_no_amount_and_never_ties_out() {
    let placeholder = GrainAmount::untrusted("USD", 2);
    assert!(placeholder.is_untrusted());
    assert!(!placeholder.is_zero());
    assert_eq!(placeholder.text(), "<untrusted>");
    assert_eq!(
        placeholder.to_posted(),
        Err(GrainError::Untrusted("USD".to_owned()))
    );
    // Identity is reflexive; the verdict is not a value comparison.
    assert_eq!(placeholder, placeholder.clone());
    assert!(placeholder.differs_from(&placeholder.clone()));
    assert_ne!(placeholder, GrainAmount::zero("USD", 2));
    assert_ne!(placeholder, GrainAmount::untrusted("USD", 3));

    // Once untrusted, later terms do not bring it back.
    let mut total = grain("1", "USD", 2);
    total.mark_untrusted();
    total.add_posted(&money("1", "USD", 2));
    total.add_grain(&grain("1", "USD", 2));
    assert!(total.is_untrusted());
    assert_eq!((total.currency(), total.currency_scale()), ("USD", 2));
}

#[test]
fn a_metadata_disagreement_taints_instead_of_mixing() {
    for delta in [money("1", "EUR", 2), money("1", "USD", 3)] {
        let mut total = grain("1", "USD", 2);
        total.add_posted(&delta);
        assert!(total.is_untrusted(), "{delta:?}");
    }
    let mut total = grain("1", "USD", 2);
    total.add_grain(&GrainAmount::untrusted("USD", 2));
    assert!(total.is_untrusted(), "an untrusted term taints the sum");
}

#[test]
fn to_posted_matches_the_exact_narrowing_for_every_form() {
    let usd = |scale| CurrencySpec::try_new("USD".to_owned(), scale).unwrap();
    // A coefficient beyond the 96-bit carrier that normalizes into the contract:
    // 9 × 10^28 at scale 2 is 9 × 10^26.
    let mut wide = GrainAmount::zero("USD", 2);
    for _ in 0..9 {
        wide.add_posted(&money("100000000000000000000000000", "USD", 2));
    }
    let expected =
        ExactAmount::from_decimal(Decimal::from(900_000_000_000_000_000_000_000_000_u128))
            .into_posted_exact(usd(2))
            .unwrap();
    assert_eq!(wide.to_posted().unwrap(), expected);

    // A 29-digit integer total is out of range in both forms.
    let mut over = grain(MAX_28, "USD", 0);
    over.add_posted(&money("1", "USD", 0));
    assert_eq!(
        over.to_posted(),
        Err(GrainError::OutOfRange(ExactError::Money(
            MoneyError::AmountOutOfRange
        )))
    );
    assert!(matches!(
        GrainAmount::zero("usd", 2).to_posted(),
        Err(GrainError::Metadata(MoneyError::InvalidCurrency))
    ));
}

#[test]
fn the_budget_is_increments_per_thousand_items_at_the_currency_scale() {
    let spec = |scale| CurrencySpec::try_new("C".to_owned(), scale).unwrap();
    assert_eq!(
        tolerance_budget(1, 500, &spec(2)),
        parse_decimal("0.01").unwrap()
    );
    assert_eq!(
        tolerance_budget(2, 2_999, &spec(2)),
        parse_decimal("0.04").unwrap()
    );
    assert_eq!(tolerance_budget(2, 1_000, &spec(0)), Decimal::from(2));
    assert_eq!(
        tolerance_budget(1, 7_000, &spec(3)),
        parse_decimal("0.007").unwrap()
    );
}

fn buckets(decision: &ToleranceDecision) -> Vec<String> {
    match &decision.variance {
        ReconciliationVariance::Money { by_currency } => by_currency
            .iter()
            .map(|m| format!("{} {}", canonical_decimal(m.amount()), m.currency().code()))
            .collect(),
        ReconciliationVariance::MissingInvoices { count } => vec![format!("missing={count}")],
    }
}

#[test]
fn ar_decision_buckets_absolute_divergences_per_currency() {
    let pairs = [
        (grain("1", "USD", 2), grain("0.99", "USD", 2)),
        (grain("0.5", "USD", 2), grain("0.52", "USD", 2)),
        (grain("10", "JPY", 0), grain("9", "JPY", 0)),
    ];
    let decision =
        ar_tolerance_decision(pairs.iter().map(|(a, b)| (a, b)), false, 2_000, 2).unwrap();
    assert_eq!(buckets(&decision), vec!["1 JPY", "0.03 USD"]);
    assert!(decision.within_tolerance, "JPY 1 <= 4, USD 0.03 <= 0.04");

    let decision =
        ar_tolerance_decision(pairs.iter().map(|(a, b)| (a, b)), false, 2_000, 1).unwrap();
    assert!(!decision.within_tolerance, "USD 0.03 > 0.02");

    let empty = ar_tolerance_decision(std::iter::empty(), false, 0, 1).unwrap();
    assert!(empty.variance.is_zero() && buckets(&empty).is_empty());
    assert!(empty.within_tolerance, "nothing diverged");
}

#[test]
fn ar_decision_hard_defects_are_never_rounding() {
    let clean = (grain("1", "USD", 2), grain("1", "USD", 2));
    let structural = ar_tolerance_decision([(&clean.0, &clean.1)], true, 5_000, 1).unwrap();
    assert!(!structural.within_tolerance, "a structural defect blocks");

    let untrusted = GrainAmount::untrusted("USD", 2);
    let decision = ar_tolerance_decision([(&clean.0, &untrusted)], false, 5_000, 1).unwrap();
    assert!(!decision.within_tolerance, "an untrusted total blocks");
    assert!(buckets(&decision).is_empty(), "it adds no amount");

    let scale_3 = grain("1", "USD", 3);
    let decision = ar_tolerance_decision([(&clean.0, &scale_3)], false, 5_000, 1).unwrap();
    assert!(!decision.within_tolerance, "metadata disagreement blocks");

    // One currency at two scales keeps the first bucket and blocks.
    let a = (grain("1.01", "USD", 2), grain("1", "USD", 2));
    let b = (grain("1.001", "USD", 3), grain("1", "USD", 3));
    let decision = ar_tolerance_decision([(&a.0, &a.1), (&b.0, &b.1)], false, 5_000, 5).unwrap();
    assert_eq!(buckets(&decision), vec!["0.01 USD"]);
    assert!(!decision.within_tolerance);
}

#[test]
fn psp_decision_judges_the_absolute_difference_against_the_settlement_budget() {
    let ledger = money("100.00", "EUR", 2);
    let within = psp_tolerance_decision(&ledger, &money("100.02", "EUR", 2), 2_000, 1).unwrap();
    assert_eq!(buckets(&within), vec!["0.02 EUR"]);
    assert!(within.within_tolerance, "0.02 <= 2 × 0.01");
    let over = psp_tolerance_decision(&ledger, &money("99.97", "EUR", 2), 2_000, 1).unwrap();
    assert_eq!(buckets(&over), vec!["0.03 EUR"]);
    assert!(!over.within_tolerance);
    assert!(matches!(
        psp_tolerance_decision(&ledger, &money("100", "USD", 2), 1, 1),
        Err(DomainError::CurrencyMismatch(_))
    ));
}

/// Fold 200,000 USD@2 postings with the exact-fraction-per-line fold the
/// tie-out ran before and with the grain's coefficient fast path, and print
/// both timings. Run with
/// `cargo test -p cf-gears-bss-ledger --lib grain_fold_timing -- --ignored --nocapture`.
#[test]
#[ignore = "timing helper; prints the before/after fold cost"]
fn grain_fold_timing() {
    let lines = postings(200_000);
    let started = std::time::Instant::now();
    let before = exact_fold(&lines);
    let exact_elapsed = started.elapsed();
    let started = std::time::Instant::now();
    let after = grain_fold(&lines, "USD", 2);
    let grain_elapsed = started.elapsed();
    assert_eq!(after, GrainAmount::from_exact(before, "USD", 2));
    println!(
        "200000 lines: exact fraction per line {} us, coefficient fast path {} us",
        exact_elapsed.as_micros(),
        grain_elapsed.as_micros()
    );
}
