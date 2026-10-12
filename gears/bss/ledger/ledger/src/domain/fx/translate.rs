//! Exact major-unit FX translation and deterministic functional anchor balancing.

use crate::domain::exact_money::{ExactAmount, ExactError};
use bss_ledger_sdk::{
    Side,
    money::{CurrencySpec, PostedMoney},
};
use rust_decimal::Decimal;
use toolkit_macros::domain_model;

/// A positive transaction posting and its posting side.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FxLine {
    pub amount: PostedMoney,
    pub side: Side,
}

/// Translation validation and final-boundary failures, retaining exact diagnostics.
#[domain_model]
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum FxTranslateError {
    #[error("FX rate must be positive")]
    RateNonPositive,
    #[error("anchor line index is out of bounds")]
    AnchorOutOfBounds,
    #[error("transaction line must be positive")]
    NonPositiveLine,
    #[error("functional residual would drive the anchor line non-positive")]
    ResidualExceedsAnchor,
    #[error(transparent)]
    Exact(#[from] ExactError),
}

/// Validate matching stored monetary metadata, including zero values.
/// # Errors
/// Returns the named currency or scale mismatch.
pub(crate) fn ensure_same_spec(
    left: &CurrencySpec,
    right: &CurrencySpec,
) -> Result<(), ExactError> {
    Ok(left.ensure_same(right)?)
}

/// Validate a positive bounded quote without rounding or limiting its fractional digits.
fn validate_rate(rate: Decimal) -> Result<(), FxTranslateError> {
    if rate <= Decimal::ZERO {
        return Err(FxTranslateError::RateNonPositive);
    }
    bss_money::validate_amount(rate).map_err(ExactError::from)?;
    Ok(())
}

/// Translate quote-major/base-major exactly and round once HalfEven at the target scale.
/// Identity applies only to the same code and stored scale at rate one.
/// # Errors
/// Rejects nonpositive/out-of-contract quotes or a final amount outside the money bounds.
pub fn translate_amount(
    source: &PostedMoney,
    rate: Decimal,
    target: CurrencySpec,
) -> Result<PostedMoney, FxTranslateError> {
    validate_rate(rate)?;
    if source.currency() == &target && rate == Decimal::ONE {
        return Ok(source.clone());
    }
    Ok(ExactAmount::from_decimal(source.amount())
        .checked_mul(&ExactAmount::from_decimal(rate))?
        .round_half_even(target)?)
}

/// Translate lines in order and close the exact functional residual onto the chosen anchor.
/// Inputs use one stored transaction spec; sides carry signs and all legs are positive.
/// # Errors
/// Rejects malformed inputs, metadata mismatch, invalid quotes, nonpositive adjusted
/// anchors and out-of-range final postings. Sums are never narrowed before cancellation.
pub fn translate_entry(
    lines: &[FxLine],
    rate: Decimal,
    target: CurrencySpec,
    anchor: usize,
) -> Result<Vec<PostedMoney>, FxTranslateError> {
    validate_rate(rate)?;
    if anchor >= lines.len() {
        return Err(FxTranslateError::AnchorOutOfBounds);
    }
    for line in lines {
        ensure_same_spec(lines[0].amount.currency(), line.amount.currency())?;
        if line.amount.amount() <= Decimal::ZERO {
            return Err(FxTranslateError::NonPositiveLine);
        }
    }
    let mut functional = Vec::with_capacity(lines.len());
    let mut net = ExactAmount::from_decimal(Decimal::ZERO);
    for line in lines {
        let exact = ExactAmount::from_decimal(line.amount.amount())
            .checked_mul(&ExactAmount::from_decimal(rate))?
            .round_half_even_exact(target.scale())?;
        net = match line.side {
            Side::Debit => net.checked_add(&exact)?,
            Side::Credit => net.checked_sub(&exact)?,
        };
        functional.push(exact);
    }
    let old_anchor = &functional[anchor];
    let adjusted = match lines[anchor].side {
        Side::Debit => old_anchor.checked_sub(&net)?,
        Side::Credit => old_anchor.checked_add(&net)?,
    };
    if adjusted.is_negative() || adjusted == ExactAmount::from_decimal(Decimal::ZERO) {
        return Err(FxTranslateError::ResidualExceedsAnchor);
    }
    functional[anchor] = adjusted;
    functional
        .into_iter()
        .map(|value| {
            value
                .into_posted_exact(target.clone())
                .map_err(FxTranslateError::from)
        })
        .collect()
}

#[cfg(test)]
#[path = "translate_tests.rs"]
mod translate_tests;
