//! Tests for the ISO 4217 money primitives.

use super::{
    CurrencyCode, DEFAULT_MINOR_UNIT, MinorAmount, RateMinor, check_decimal, check_scale,
    fraction_digits, is_expressible,
};
use crate::domain::error::DomainError;

fn code(raw: &str) -> CurrencyCode {
    CurrencyCode::new(raw).expect("test currency code is well-formed")
}

#[test]
fn one_currency_has_one_spelling() {
    // `currency` is a scope-key axis: if `usd` and `USD` were two values, two
    // rows on the same key would both pass the duplicate-key index.
    assert_eq!(code("usd"), code("USD"));
    assert_eq!(code(" uSd ").as_str(), "USD");
}

#[test]
fn a_code_that_is_not_three_letters_is_refused() {
    for raw in ["US", "USDX", "US1", "", "U$D"] {
        assert!(
            matches!(CurrencyCode::new(raw), Err(DomainError::CurrencyInvalid(_))),
            "expected {raw} to be refused"
        );
    }
}

#[test]
fn the_zero_decimal_currencies_take_no_minor_unit() {
    assert_eq!(code("JPY").minor_unit(), 0);
    assert_eq!(code("KRW").minor_unit(), 0);
    assert_eq!(code("ISK").minor_unit(), 0);
}

/// The two fifth-subdivided codes are on the fallback, and that is the decision.
///
/// MGA and MRU divide by five, so their minor unit is not a power of ten at all;
/// the register, the representable-amount reading and the processors disagree
/// about what to call it. The launch subsets exist to keep a guess out of the
/// only operand of the precision bound.
#[test]
fn a_fifth_subdivided_code_is_left_on_the_named_fallback() {
    assert_eq!(code("MGA").minor_unit(), DEFAULT_MINOR_UNIT);
    assert_eq!(code("MRU").minor_unit(), DEFAULT_MINOR_UNIT);
}

#[test]
fn the_three_decimal_currencies_take_three() {
    assert_eq!(code("BHD").minor_unit(), 3);
    assert_eq!(code("KWD").minor_unit(), 3);
    assert_eq!(code("OMR").minor_unit(), 3);
}

#[test]
fn a_code_outside_the_launch_subset_falls_back_to_two() {
    // The table is honest about being partial, so the fallback is the
    // behaviour that actually ships for most codes and is tested as such.
    assert_eq!(code("USD").minor_unit(), DEFAULT_MINOR_UNIT);
    assert_eq!(code("ZWG").minor_unit(), DEFAULT_MINOR_UNIT);
}

#[test]
fn jpy_rejects_two_decimals_while_bhd_accepts_three() {
    // The whole reason `PRECISION_EXCEEDED` exists: there is no flat
    // two-decimal rule, so a fixed scale would be wrong in both directions.
    assert!(check_decimal(&code("JPY"), "100.00").is_err());
    assert!(check_decimal(&code("BHD"), "1.234").is_ok());
    assert!(check_decimal(&code("JPY"), "100").is_ok());
    assert!(check_decimal(&code("BHD"), "1.2345").is_err());
}

#[test]
fn precision_is_judged_on_the_declared_scale_not_the_value() {
    // `100.00` on JPY is worth exactly 100 yen, and is still refused: the
    // author has declared a two-decimal price on a currency with no decimals,
    // and freezing that into the read model would leave every consumer to
    // re-derive which digits were real.
    assert!(!is_expressible(&code("JPY"), 2));
    assert!(matches!(
        check_scale(&code("JPY"), 2),
        Err(DomainError::PrecisionExceeded(_))
    ));
}

#[test]
fn a_scale_below_the_minor_unit_is_expressible() {
    // Under-declaring is not an error: USD 5 is a lawful price.
    assert!(is_expressible(&code("USD"), 0));
    assert!(check_decimal(&code("USD"), "5").is_ok());
}

#[test]
fn fraction_digits_counts_what_the_author_wrote() {
    assert_eq!(fraction_digits("100").expect("integer literal"), 0);
    assert_eq!(fraction_digits("100.5").expect("one decimal"), 1);
    assert_eq!(fraction_digits("-100.50").expect("signed literal"), 2);
}

#[test]
fn a_malformed_literal_is_a_bad_request_not_a_precision_failure() {
    // Two different problems reach the author under two different codes; a
    // caller that typed `1.2.3` is not told its currency has too few decimals.
    for raw in ["1.2.3", "abc", "", "1.", ".5", "1..2"] {
        assert!(
            matches!(fraction_digits(raw), Err(DomainError::InvalidRequest(_))),
            "expected {raw} to be malformed"
        );
    }
}

#[test]
fn a_negative_amount_is_refused() {
    // Typed credit rows are Future scope, so a negative price is a mistake
    // caught at the boundary rather than an unsupported feature carried inward.
    assert!(matches!(
        MinorAmount::new(-1),
        Err(DomainError::AmountNegative(_))
    ));
}

#[test]
fn zero_is_a_valid_amount() {
    // The rule is non-negative, not positive: a free trial phase and a
    // zero-rated usage row are both authored as `0`.
    assert_eq!(MinorAmount::new(0).expect("zero is valid").get(), 0);
}

#[test]
fn a_sign_error_is_not_reported_as_a_precision_error() {
    // `fraction_digits` accepts the sign so that `-1.005` on USD is answered by
    // the amount rule, not by the scale rule; the author gets one fault at a
    // time and the two codes stay distinguishable.
    assert_eq!(fraction_digits("-1.00").expect("signed literal"), 2);
    assert!(check_decimal(&code("USD"), "-1.00").is_ok());
    assert!(MinorAmount::new(-100).is_err());
}

// ---------------------------------------------------------------------------
// D-311 — a rate is not an amount.
// ---------------------------------------------------------------------------

/// **The rate a metered plan is actually sold at is expressible.**
///
/// `MinorAmount` is whole ISO-4217 minor units, so `0.0150 USD` per unit had no
/// representation at all — not a refusal, an absence. S3 sells at `$0.023` per
/// GB-month and Lambda at `$0.0000166667` per GB-second; the studio's own seed
/// carries `0.0150`, `0.0230`, `0.0120`.
#[test]
fn a_sub_cent_rate_is_expressible() {
    let usd = CurrencyCode::new("USD").expect("iso");

    let rate = RateMinor::from_major_decimal(&usd, "0.0150").expect("a cent-and-a-half rate");
    // 0.0150 USD = 1.50 minor units = 1_500_000_000 nano-minor.
    assert_eq!(rate.nano_minor(), 1_500_000_000);
}

/// **The three constructors read three scales, and this is the conversion between
/// them.**
///
/// Not a probe of a defect — nothing is broken here — but the pin that makes one
/// visible: a scale changed in any of the three breaks this and nothing else, since
/// every other case in this file exercises one constructor at a time. The failure
/// mode is a caller reaching for the wrong one and being wrong by a factor of a
/// hundred, which this gear has already paid for once in a duplicated rate
/// conversion.
#[test]
fn the_three_constructors_agree_on_one_rate() {
    let usd = CurrencyCode::new("USD").expect("iso");

    let major = RateMinor::from_major_decimal(&usd, "0.01").expect("one cent, in dollars");
    let minor = RateMinor::from_minor_units(1).expect("one cent, in cents");
    let nano = RateMinor::from_nano_minor(1_000_000_000).expect("one cent, as stored");

    assert_eq!(
        major, minor,
        "a major-unit literal of 0.01 USD is one minor unit"
    );
    assert_eq!(
        minor, nano,
        "and one minor unit is 10^9 nano-minor, which is what RATE_SUB_DECIMALS means"
    );
}

/// The two industry rates the decision names, to the digit.
#[test]
fn the_rates_the_decision_names_are_exact() {
    let usd = CurrencyCode::new("USD").expect("iso");

    // S3: $0.023 per GB-month = 2.3 minor units.
    assert_eq!(
        RateMinor::from_major_decimal(&usd, "0.023")
            .expect("s3")
            .nano_minor(),
        2_300_000_000
    );
    // Lambda: $0.0000166667 per GB-second. Six sub-decimals would leave this
    // fractional at 1666.67 micro-minor; nine express it exactly.
    assert_eq!(
        RateMinor::from_major_decimal(&usd, "0.0000166667")
            .expect("lambda")
            .nano_minor(),
        1_666_670
    );
}

/// **A ladder keeps its gradation**, which is the failure truncation caused.
///
/// `0.0150 / 0.0110` and `0.0230 / 0.0120` both collapsed to `0.01` at every
/// band, so two different ladders became the same ladder and the tariff lost its
/// meaning — silently, on a row that looked well-formed.
#[test]
fn two_ladders_that_used_to_collapse_stay_distinct() {
    let usd = CurrencyCode::new("USD").expect("iso");
    let ladder = |a: &str, b: &str| {
        (
            RateMinor::from_major_decimal(&usd, a)
                .expect("band")
                .nano_minor(),
            RateMinor::from_major_decimal(&usd, b)
                .expect("band")
                .nano_minor(),
        )
    };

    let first = ladder("0.0150", "0.0110");
    let second = ladder("0.0230", "0.0120");

    assert_ne!(first, second);
    assert_ne!(first.0, first.1, "the ladder still steps down");
    assert_ne!(second.0, second.1);
}

/// A rate is still non-negative, and zero is still legal — a zero-rated usage
/// row is authored, not absent.
#[test]
fn a_rate_is_non_negative_and_zero_is_legal() {
    let usd = CurrencyCode::new("USD").expect("iso");

    assert_eq!(
        RateMinor::from_major_decimal(&usd, "0")
            .expect("zero is a rate")
            .nano_minor(),
        0
    );
    assert!(RateMinor::from_major_decimal(&usd, "-0.01").is_err());
}

/// **The scale rule is not gone, it is aimed at the rate's own scale.**
///
/// `check_scale` had no caller: the ISO-4217 rule was enforced by `MinorAmount`
/// being unable to hold a violation, so `PRECISION_EXCEEDED` was a declared
/// refusal nothing could raise. A rate *can* hold more digits than it should, so
/// here the refusal is reachable and the code means something.
#[test]
fn a_rate_finer_than_the_stored_scale_is_refused_by_code() {
    let usd = CurrencyCode::new("USD").expect("iso");

    // USD stores 2 + 9 = 11 decimals. The boundary is asserted from both sides,
    // because a case that only checked the refusal would pass equally against a
    // type that refused everything.
    assert!(
        RateMinor::from_major_decimal(&usd, "0.00000000001").is_ok(),
        "eleven decimals is exactly what USD stores"
    );
    let err =
        RateMinor::from_major_decimal(&usd, "0.000000000001").expect_err("a twelfth is not stored");
    assert!(
        matches!(err, DomainError::PrecisionExceeded(_)),
        "the refusal is the declared one: {err:?}"
    );
}

/// The currency's own scale still governs an **amount**, and a rate does not
/// inherit that limit — which is the whole distinction D-311 draws.
#[test]
fn a_currency_with_no_decimals_still_bounds_amounts_but_not_rates() {
    let jpy = CurrencyCode::new("JPY").expect("iso");
    assert_eq!(jpy.minor_unit(), 0);

    // An amount on JPY may not declare decimals.
    assert!(check_decimal(&jpy, "100.00").is_err());
    // A rate on JPY may: it is a multiplier, and what rounds to a whole yen is
    // the product, never the rate.
    assert!(RateMinor::from_major_decimal(&jpy, "0.25").is_ok());
}
