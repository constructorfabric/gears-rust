//! Golden-vector + edge tests for the exact-decimal `rate_micro` conversion.

use bss_ledger_sdk::RateProviderError;

use super::rate_to_micro;

#[test]
fn typical_ecb_rate() {
    // 1.0856 * 1e6 = 1_085_600 exactly.
    assert_eq!(rate_to_micro("1.0856").unwrap(), 1_085_600);
}

#[test]
fn banker_rounding_half_to_even() {
    // 0.0000015 * 1e6 = 1.5 -> nearest even = 2.
    assert_eq!(rate_to_micro("0.0000015").unwrap(), 2);
    // 0.0000025 * 1e6 = 2.5 -> nearest even = 2.
    assert_eq!(rate_to_micro("0.0000025").unwrap(), 2);
    // 0.0000035 * 1e6 = 3.5 -> nearest even = 4.
    assert_eq!(rate_to_micro("0.0000035").unwrap(), 4);
}

#[test]
fn deterministic_repeat() {
    assert_eq!(
        rate_to_micro("160.85").unwrap(),
        rate_to_micro("160.85").unwrap()
    );
    assert_eq!(rate_to_micro("160.85").unwrap(), 160_850_000);
}

#[test]
fn non_numeric_is_internal_error() {
    assert!(matches!(
        rate_to_micro("abc"),
        Err(RateProviderError::Internal(_))
    ));
}

#[test]
fn overflow_is_internal_error() {
    assert!(matches!(
        rate_to_micro("100000000000000"),
        Err(RateProviderError::Internal(_))
    ));
}

#[test]
fn zero_rate_is_internal_error() {
    // A corrupted or empty feed value must not convert "successfully" to zero.
    assert!(matches!(
        rate_to_micro("0"),
        Err(RateProviderError::Internal(_))
    ));
    assert!(matches!(
        rate_to_micro("0.000000"),
        Err(RateProviderError::Internal(_))
    ));
}

#[test]
fn negative_rate_is_internal_error() {
    assert!(matches!(
        rate_to_micro("-1.5"),
        Err(RateProviderError::Internal(_))
    ));
}

#[test]
fn negative_banker_rounding_result_is_internal_error() {
    // -0.0000025 * 1e6 = -2.5 -> nearest even = -2: rounded correctly, still
    // rejected as non-positive.
    assert!(matches!(
        rate_to_micro("-0.0000025"),
        Err(RateProviderError::Internal(_))
    ));
}

#[test]
fn exact_i64_max_boundary_converts_exactly() {
    // i64::MAX = 9_223_372_036_854_775_807; this rate scales to exactly that.
    assert_eq!(rate_to_micro("9223372036854.775807").unwrap(), i64::MAX);
}

#[test]
fn one_micro_past_i64_max_is_internal_error() {
    assert!(matches!(
        rate_to_micro("9223372036854.775808"),
        Err(RateProviderError::Internal(_))
    ));
}

#[test]
fn exact_i64_min_magnitude_negative_rate_is_internal_error() {
    // The magnitude fits i64 exactly, so this is rejected as non-positive rather
    // than as an overflow.
    assert!(matches!(
        rate_to_micro("-9223372036854.775808"),
        Err(RateProviderError::Internal(_))
    ));
}

#[test]
fn one_micro_past_i64_min_is_internal_error() {
    // This one overflows i64 outright, on top of being negative.
    assert!(matches!(
        rate_to_micro("-9223372036854.775809"),
        Err(RateProviderError::Internal(_))
    ));
}

#[test]
fn a_rate_too_large_to_scale_fails_at_the_scaling_step() {
    // A distinct failure from the i64 cases above: 1e23 parses as an exact
    // `Decimal`, but 1e23 * 1e6 exceeds `Decimal::MAX` (~7.9e28), so the
    // multiplication itself has nowhere to put the result and never reaches the
    // i64 conversion. Asserting the message is what separates the two paths —
    // both are `Internal`, so `matches!` alone cannot tell them apart.
    let err = rate_to_micro("100000000000000000000000").unwrap_err();
    assert!(
        err.to_string().contains("overflows when scaled to micro"),
        "expected the scaling-overflow error, got: {err}"
    );
}
