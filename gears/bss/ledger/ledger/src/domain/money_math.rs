//! Centralized integer money math: half-to-even rounding and the i128→i64
//! over-range guard. All amounts are minor units. Pure domain — no infra.

use toolkit_macros::domain_model;

/// Money-math failure. `Overflow` maps to RFC-9457 `AMOUNT_OUT_OF_RANGE`.
#[domain_model]
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum MoneyError {
    #[error("amount out of range: {0}")]
    Overflow(i128),
    #[error("division by zero")]
    DivByZero,
    #[error("allocation weights must be non-empty")]
    EmptyWeights,
    #[error("allocation weights must be non-negative")]
    NegativeWeight,
}

/// Round `numer/denom` to the nearest integer, ties to even (banker's).
/// `denom` MUST be positive — guaranteed by the only caller (`allocate`, which
/// rejects a zero weight-sum first). Used for proportional allocation shares.
#[must_use]
pub(crate) fn round_half_even(numer: i128, denom: i128) -> i128 {
    assert!(denom > 0, "denominator must be positive");
    let q = numer.div_euclid(denom);
    let r = numer.rem_euclid(denom); // 0 <= r < denom
    // Compare `r` to `denom - r` rather than `2 * r` to `denom`, so a large
    // remainder cannot overflow i128.
    let complement = denom - r;
    if r < complement {
        q
    } else if r > complement {
        q + 1
    } else if q % 2 == 0 {
        // exact half: round to even
        q
    } else {
        q + 1
    }
}

/// Narrow an i128 minor-unit value to i64, erroring on over-range.
///
/// # Errors
/// Returns [`MoneyError::Overflow`] if `value` does not fit in an `i64`.
pub fn checked_minor(value: i128) -> Result<i64, MoneyError> {
    i64::try_from(value).map_err(|_| MoneyError::Overflow(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn half_to_even_rounds_ties_to_even() {
        assert_eq!(round_half_even(5, 2), 2); // 2.5 -> 2
        assert_eq!(round_half_even(7, 2), 4); // 3.5 -> 4
        assert_eq!(round_half_even(1, 3), 0); // 0.333 -> 0
        assert_eq!(round_half_even(2, 3), 1); // 0.666 -> 1
    }

    #[test]
    fn half_to_even_handles_negatives() {
        assert_eq!(round_half_even(-5, 2), -2); // -2.5 -> -2 (even)
        assert_eq!(round_half_even(-7, 2), -4); // -3.5 -> -4 (even)
    }

    #[test]
    fn checked_minor_guards_range() {
        assert_eq!(checked_minor(1234), Ok(1234));
        assert_eq!(
            checked_minor(i128::from(i64::MAX) + 1),
            Err(MoneyError::Overflow(i128::from(i64::MAX) + 1))
        );
    }
}
