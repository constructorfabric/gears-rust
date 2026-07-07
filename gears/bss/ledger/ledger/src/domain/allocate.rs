//! Deterministic proportional allocation. Splits `total` across `weights`
//! so the parts sum back to `total` exactly; the rounding residual is
//! placed by a pinned rule so every recompute lands it identically
//! (architecture §4.5, I-11). v1 ships the "last" disposition; "largest"
//! is provided for handlers that need it.

use toolkit_macros::domain_model;

use crate::domain::money_math::{MoneyError, checked_minor, round_half_even};

/// Where the rounding remainder is assigned.
#[domain_model]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Residual {
    /// Last index (canonical-last built line).
    Last,
    /// Largest weight; ties broken by the lowest index.
    Largest,
}

/// Allocate `total` minor units across `weights` proportionally, with
/// banker's-rounded shares and the residual placed by `disposition`.
///
/// # Errors
/// [`MoneyError::EmptyWeights`] if `weights` is empty,
/// [`MoneyError::NegativeWeight`] if any weight is negative,
/// [`MoneyError::DivByZero`] if the weights sum to zero, or
/// [`MoneyError::Overflow`] if a computed share exceeds `i64`.
pub fn allocate(
    total: i64,
    weights: &[i64],
    disposition: Residual,
) -> Result<Vec<i64>, MoneyError> {
    if weights.is_empty() {
        return Err(MoneyError::EmptyWeights);
    }
    if weights.iter().any(|w| *w < 0) {
        return Err(MoneyError::NegativeWeight);
    }
    let sum: i128 = weights.iter().map(|w| i128::from(*w)).sum();
    if sum == 0 {
        return Err(MoneyError::DivByZero);
    }
    let total_i = i128::from(total);

    // Banker's-rounded proportional shares.
    let mut shares: Vec<i64> = weights
        .iter()
        .map(|w| checked_minor(round_half_even(total_i * i128::from(*w), sum)))
        .collect::<Result<_, _>>()?;

    // Residual = total - sum(shares); place it on the chosen index.
    let placed: i128 = shares.iter().map(|s| i128::from(*s)).sum();
    let residual = checked_minor(total_i - placed)?;
    if residual != 0 {
        let idx = match disposition {
            Residual::Last => shares.len() - 1,
            Residual::Largest => weights
                .iter()
                .enumerate()
                .max_by(|(ia, a), (ib, b)| a.cmp(b).then(ib.cmp(ia)))
                .map_or(0, |(i, _)| i),
        };
        shares[idx] = checked_minor(i128::from(shares[idx]) + i128::from(residual))?;
    }
    Ok(shares)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocation_sums_to_total() {
        let parts = allocate(100, &[1, 1, 1], Residual::Last).unwrap();
        assert_eq!(parts.iter().sum::<i64>(), 100);
        assert_eq!(parts, vec![33, 33, 34]); // residual on last
    }

    #[test]
    fn residual_to_largest_weight() {
        let parts = allocate(100, &[1, 8, 1], Residual::Largest).unwrap();
        assert_eq!(parts.iter().sum::<i64>(), 100);
        assert_eq!(parts[1], parts.iter().copied().max().unwrap());
    }

    #[test]
    fn zero_weight_sum_errors() {
        assert_eq!(
            allocate(100, &[0, 0], Residual::Last),
            Err(MoneyError::DivByZero)
        );
    }

    #[test]
    fn largest_residual_with_equal_weights_breaks_tie_to_lowest_index() {
        // Equal weights → residual 1; the Largest rule places it on the
        // highest weight, ties broken by the lowest index (here index 0).
        let parts = allocate(100, &[1, 1, 1], Residual::Largest).unwrap();
        assert_eq!(parts.iter().sum::<i64>(), 100);
        assert_eq!(parts, vec![34, 33, 33]);
    }

    #[test]
    fn empty_weights_is_an_error_not_a_panic() {
        assert_eq!(
            allocate(100, &[], Residual::Last),
            Err(MoneyError::EmptyWeights)
        );
    }

    #[test]
    fn negative_weight_is_an_error_not_a_panic() {
        assert_eq!(
            allocate(100, &[1, -2, 3], Residual::Last),
            Err(MoneyError::NegativeWeight)
        );
    }
}
