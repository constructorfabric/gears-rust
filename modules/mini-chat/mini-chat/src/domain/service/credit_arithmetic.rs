// Called from QuotaService which is not yet wired into the turn handler.
// Remove `dead_code` allows once QuotaService is live.

use modkit_macros::domain_model;

#[allow(dead_code)]
/// Maximum tokens accepted by credit arithmetic (10 million).
pub const MAX_TOKENS: u64 = 10_000_000;
#[allow(dead_code)]
/// Maximum multiplier accepted by credit arithmetic (10 billion).
pub const MAX_MULT: u64 = 10_000_000_000;
#[allow(dead_code)]
/// Divisor for micro-credit computation.
pub const DIVISOR: u64 = 1_000_000;

/// Error returned when credit arithmetic overflows safe bounds.
#[domain_model]
#[allow(dead_code, clippy::enum_variant_names)]
#[derive(Debug, thiserror::Error)]
pub enum CreditOverflowError {
    #[error("tokens {0} exceed MAX_TOKENS {MAX_TOKENS}")]
    TokensOverflow(u64),
    #[error("multiplier {0} exceeds MAX_MULT {MAX_MULT}")]
    MultiplierOverflow(u64),
    #[error("arithmetic overflow in checked_mul")]
    ArithmeticOverflow,
}

/// Integer ceiling division: `ceil(a / b)` with checked arithmetic.
///
/// Returns 0 when `a == 0`.
#[allow(dead_code, clippy::integer_division)]
pub fn ceil_div_checked(a: u64, b: u64) -> Result<u64, CreditOverflowError> {
    debug_assert!(b != 0, "ceil_div_checked: divisor must be non-zero");
    if a == 0 || b == 0 {
        return Ok(0);
    }
    a.checked_add(b - 1)
        .map(|n| n / b)
        .ok_or(CreditOverflowError::ArithmeticOverflow)
}

/// Compute credits in micro-credits:
///
/// ```text
/// ceil_div(input_tokens * input_mult, DIVISOR) + ceil_div(output_tokens * output_mult, DIVISOR)
/// ```
///
/// Each component uses `ceil_div` independently. Returns `i64` because
/// `quota_usage` columns are `BIGINT`.
#[allow(dead_code)]
pub fn credits_micro_checked(
    input_tokens: u64,
    output_tokens: u64,
    input_mult: u64,
    output_mult: u64,
) -> Result<i64, CreditOverflowError> {
    if input_tokens > MAX_TOKENS {
        return Err(CreditOverflowError::TokensOverflow(input_tokens));
    }
    if output_tokens > MAX_TOKENS {
        return Err(CreditOverflowError::TokensOverflow(output_tokens));
    }
    if input_mult > MAX_MULT {
        return Err(CreditOverflowError::MultiplierOverflow(input_mult));
    }
    if output_mult > MAX_MULT {
        return Err(CreditOverflowError::MultiplierOverflow(output_mult));
    }

    let input_product = input_tokens
        .checked_mul(input_mult)
        .ok_or(CreditOverflowError::ArithmeticOverflow)?;
    let output_product = output_tokens
        .checked_mul(output_mult)
        .ok_or(CreditOverflowError::ArithmeticOverflow)?;

    let input_credits = ceil_div_checked(input_product, DIVISOR)?;
    let output_credits = ceil_div_checked(output_product, DIVISOR)?;

    let total = input_credits
        .checked_add(output_credits)
        .ok_or(CreditOverflowError::ArithmeticOverflow)?;

    // Safe cast: max is ceil_div(10M * 10B, 1M) * 2 ≈ 200B which fits i64.
    #[allow(clippy::cast_possible_wrap)]
    Ok(total as i64)
}

#[cfg(test)]
mod tests {
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
}
