//! Deterministic conversion of a published decimal rate string into the
//! contract's fixed-precision integer `rate_micro = round(rate * 1e6)`. Shared by
//! every rate-provider source plugin.
//!
//! Parses into an EXACT decimal (`rust_decimal::Decimal`, never binary `f64`,
//! whose nearest-representable value can mis-round exact half-way decimals) and
//! rounds half-to-even (banker's rounding) to match the platform ledger rounding
//! default, so a re-fetch of the same published rate yields the same integer.
//! Overflow / non-finite / non-numeric input maps to
//! [`RateProviderError::Internal`] — never a silent truncation.

use bss_ledger_sdk::RateProviderError;
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::{Decimal, RoundingStrategy};

/// Fixed-precision scale: the functional-per-unit-base multiplier is stored `* 1e6`.
const MICRO: i64 = 1_000_000;

/// Convert a published decimal rate string to `rate_micro` (`i64`).
///
/// # Errors
/// [`RateProviderError::Internal`] if the string is not an exact decimal, or the
/// scaled/rounded value does not fit in `i64`.
pub fn rate_to_micro(rate: &str) -> Result<i64, RateProviderError> {
    let parsed = Decimal::from_str_exact(rate.trim()).map_err(|e| {
        RateProviderError::Internal(format!("rate '{rate}' is not an exact decimal: {e}"))
    })?;
    let scaled = parsed.checked_mul(Decimal::from(MICRO)).ok_or_else(|| {
        RateProviderError::Internal(format!("rate '{rate}' overflows when scaled to micro"))
    })?;
    let rounded = scaled.round_dp_with_strategy(0, RoundingStrategy::MidpointNearestEven);
    rounded
        .to_i64()
        .ok_or_else(|| RateProviderError::Internal(format!("rate '{rate}' out of i64 range")))
}

#[cfg(test)]
#[path = "conversion_tests.rs"]
mod tests;
