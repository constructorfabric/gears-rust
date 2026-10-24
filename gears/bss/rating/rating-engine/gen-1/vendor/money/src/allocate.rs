//! Deterministic proportional allocation in major units. Shares are rounded
//! once at the stored currency scale; the exact residual follows the pinned
//! Last or Largest rule, including signed residuals in zero-weight slots.

use rust_decimal::Decimal;

use crate::PostedMoney;
use crate::exact::{ExactAmount, ExactError};
use crate::money::validate_amount;

/// Where the rounding remainder is assigned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Residual {
    /// Last index (canonical-last built line), including a zero-weight slot.
    Last,
    /// Largest weight; ties broken by the lowest index.
    Largest,
}

/// Allocate signed money using exact proportions and `HALF_EVEN` posted shares.
///
/// All outputs retain the total's stored currency and scale. The exact rounding
/// residual goes to the selected slot, which can then have the opposite sign
/// from the total. Callers own any additional share-sign admission policy.
/// Recognition also enforces its deployment segment ceiling before calling.
///
/// # Errors
/// Returns [`ExactError::EmptyWeights`], [`ExactError::NegativeWeight`],
/// [`ExactError::DivisionByZero`] for zero total weight, or
/// [`ExactError::TooManyTerms`] for a count beyond the existing i32 segment-number
/// range. Bounded-decimal, final share and arithmetic failures retain their
/// [`ExactError::Money`] or [`ExactError::ArithmeticLimit`] classification.
pub fn allocate(
    total: &PostedMoney,
    weights: &[Decimal],
    disposition: Residual,
) -> Result<Vec<PostedMoney>, ExactError> {
    validate_count(weights.len())?;
    let mut sum = ExactAmount::from_decimal(Decimal::ZERO);
    for &weight in weights {
        if weight < Decimal::ZERO {
            return Err(ExactError::NegativeWeight);
        }
        // The 28-digit coefficient bound, applied to a dimensionless decimal
        // without assigning a fabricated currency.
        validate_amount(weight)?;
        sum = sum.checked_add(&ExactAmount::from_decimal(weight))?;
    }

    let exact_total = ExactAmount::from_decimal(total.amount());
    let mut shares = Vec::with_capacity(weights.len());
    let mut placed = ExactAmount::from_decimal(Decimal::ZERO);
    for &weight in weights {
        let share = exact_total
            .checked_mul(&ExactAmount::from_decimal(weight))?
            .checked_div(&sum)?
            .round_half_even(total.currency().clone())?;
        placed = placed.checked_add(&ExactAmount::from_decimal(share.amount()))?;
        shares.push(share);
    }

    let residual = exact_total.checked_sub(&placed)?;
    let index = match disposition {
        Residual::Last => shares.len().checked_sub(1),
        Residual::Largest => weights
            .iter()
            .enumerate()
            .max_by(|(ia, a), (ib, b)| a.cmp(b).then(ib.cmp(ia)))
            .map(|(index, _)| index),
    };
    let slot = index
        .and_then(|index| shares.get_mut(index))
        .ok_or(ExactError::EmptyWeights)?;
    // Narrow only the final adjusted share, never the aggregate or residual.
    *slot = ExactAmount::from_decimal(slot.amount())
        .checked_add(&residual)?
        .into_posted_exact(total.currency().clone())?;
    Ok(shares)
}

/// Reject counts outside the stored segment-number range without allocating.
fn validate_count(count: usize) -> Result<(), ExactError> {
    if count == 0 {
        return Err(ExactError::EmptyWeights);
    }
    i32::try_from(count).map_err(|_| ExactError::TooManyTerms)?;
    Ok(())
}

#[cfg(test)]
#[path = "allocate_tests.rs"]
mod tests;
