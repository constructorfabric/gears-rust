use super::*;

#[test]
fn normal_computation() {
    // ceil_div(1000 * 1_000_000, 1_000_000) + ceil_div(500 * 3_000_000, 1_000_000)
    // = 1000 + 1500 = 2500
    let result = credits_micro_checked(1000, 500, 1_000_000, 3_000_000).unwrap();
    assert_eq!(result, 2500);
}

#[test]
fn zero_tokens() {
    assert_eq!(
        credits_micro_checked(0, 0, 1_000_000, 3_000_000).unwrap(),
        0
    );
}

#[test]
fn rounding_each_component_ceil_div_independently() {
    // ceil_div(1 * 1, 1_000_000) + ceil_div(1 * 1, 1_000_000)
    // = ceil_div(1, 1M) + ceil_div(1, 1M) = 1 + 1 = 2
    let result = credits_micro_checked(1, 1, 1, 1).unwrap();
    assert_eq!(result, 2);
}

#[test]
fn overflow_tokens_exceeds_max() {
    let result = credits_micro_checked(MAX_TOKENS + 1, 0, 1, 1);
    assert!(matches!(
        result,
        Err(CreditOverflowError::TokensOverflow(_))
    ));
}

#[test]
fn overflow_mult_exceeds_max() {
    let result = credits_micro_checked(1, 0, MAX_MULT + 1, 1);
    assert!(matches!(
        result,
        Err(CreditOverflowError::MultiplierOverflow(_))
    ));
}

#[test]
fn max_bounds_no_overflow() {
    // MAX_TOKENS * MAX_MULT = 10^7 * 10^10 = 10^17 which fits u64
    let result = credits_micro_checked(MAX_TOKENS, MAX_TOKENS, MAX_MULT, MAX_MULT).unwrap();
    // ceil_div(10^17, 10^6) * 2 = 10^11 * 2 = 200_000_000_000
    assert_eq!(result, 200_000_000_000);
}
