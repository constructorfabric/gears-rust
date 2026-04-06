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
#[path = "credit_arithmetic_tests.rs"]
mod tests;
