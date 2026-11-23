//! Exact quota unit arithmetic: canonical amounts, reduced factors, and conversions that refuse an
//! inexact or overflowing result (P-D-267).
#![allow(clippy::unwrap_used)]
use super::*;

fn f(n: &str, d: &str) -> Factor {
    Factor::parse(n, d).unwrap()
}

#[test]
fn gb_and_gib_reach_bytes_by_their_own_factors() {
    let bytes = Factor::ONE;
    let gb = f("1000000000", "1");
    let gib = f("1073741824", "1");
    let twenty = Amount::parse("20").unwrap();
    assert_eq!(
        convert(twenty, gb, bytes).unwrap().to_string(),
        "20000000000"
    );
    assert_eq!(
        convert(twenty, gib, bytes).unwrap().to_string(),
        "21474836480"
    );
    assert_eq!(
        convert(Amount::parse("1").unwrap(), gib, gb),
        Err(UnitError::Inexact)
    );
}

#[test]
fn a_zero_denominator_is_refused() {
    assert_eq!(Factor::parse("1", "0"), Err(UnitError::NotPositive));
    assert_eq!(Factor::parse("0", "1"), Err(UnitError::NotPositive));
}

#[test]
fn a_factor_that_is_not_reduced_is_refused() {
    assert_eq!(Factor::parse("2", "4"), Err(UnitError::NotReduced));
}

#[test]
fn amounts_are_canonical_decimal_integers() {
    for bad in [
        "",
        "-1",
        "+1",
        "01",
        "1.0",
        "1e3",
        " 1",
        "1234567890123456789012345678901234567890",
    ] {
        assert_eq!(Amount::parse(bad), Err(UnitError::NotDecimal), "{bad:?}");
    }
    assert_eq!(Amount::parse("0").unwrap().get(), 0);
    // Thirty-nine digits are canonical, but not every such value fits in 128 bits.
    let max = u128::MAX.to_string();
    assert_eq!(Amount::parse(&max).unwrap().get(), u128::MAX);
    assert_eq!(
        Amount::parse("999999999999999999999999999999999999999"),
        Err(UnitError::Overflow)
    );
}

#[test]
fn an_overflowing_conversion_is_refused_not_wrapped() {
    let huge = Amount::parse(&u128::MAX.to_string()).unwrap();
    assert_eq!(
        convert(huge, f("2", "1"), Factor::ONE),
        Err(UnitError::Overflow)
    );
}

#[test]
fn a_fractional_factor_converts_exactly_when_the_result_is_whole() {
    let half = f("1", "2");
    assert_eq!(
        convert(Amount::parse("4").unwrap(), half, Factor::ONE)
            .unwrap()
            .get(),
        2
    );
    assert_eq!(
        convert(Amount::parse("3").unwrap(), half, Factor::ONE),
        Err(UnitError::Inexact)
    );
}

#[test]
fn every_refusal_names_its_stable_code() {
    for (error, code) in [
        (UnitError::NotDecimal, "QUOTA_UNIT_INVALID"),
        (UnitError::NotPositive, "QUOTA_UNIT_INVALID"),
        (UnitError::NotReduced, "QUOTA_UNIT_INVALID"),
        (UnitError::Inexact, "QUOTA_UNIT_INEXACT"),
        (UnitError::Overflow, "QUOTA_UNIT_INVALID"),
    ] {
        assert_eq!(error.code(), code, "{error}");
    }
}
