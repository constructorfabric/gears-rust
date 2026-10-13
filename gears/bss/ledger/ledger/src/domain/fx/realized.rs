//! Pure realized-FX (design §3.5 / §4.4): when a cross-currency position
//! **closes** (settle / allocate / refund / chargeback) at a rate ≠ its carried
//! rate, compute the net realized FX as a single `FX_GAIN_LOSS` functional-only
//! line so the close entry's **functional** column balances by construction.
//!
//! Each closing account is relieved at **its own** carried functional value (the
//! grain's `functional balance`), weighted-average (WAC) **pro-rata** for a
//! partial close (`functional balance / transaction balance`, banker's rounding —
//! the ratified carried-rate policy, decision 3). The net functional imbalance
//! between the relief legs is the realized gain/loss, plugged to one
//! `FX_GAIN_LOSS` line.
//!
//! **Forbidden (spec §3.5):** rescanning `journal_line`, or averaging across
//! grains — each leg's carried functional is read ONLY from its own grain input,
//! so two grains that received settlements at different rates keep their distinct
//! carried values (no cross-grain blend). The structure here enforces it: every
//! [`ClosingLeg`] carries its own grain values and is relieved independently.
//!
//! No infra; the close-path caller (Phase 2 Group F — deferred with the live
//! S1/S2/S3 functional-stamping hook) reads the carried grain values and the
//! relieved transaction amounts and feeds them here.

use super::translate::ensure_same_spec;
use crate::domain::exact_money::{ExactAmount, ExactError};
use bss_ledger_sdk::{Side, money::PostedMoney};
use rust_decimal::Decimal;
use toolkit_macros::domain_model;

/// One grain's independent carried amounts and positive transaction relief.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClosingLeg {
    pub side: Side,
    pub carried_functional: PostedMoney,
    pub carried_transaction: PostedMoney,
    pub relieved_transaction: PostedMoney,
}

/// Ordered functional relief and optional balancing realized gain/loss.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RealizedFx {
    pub leg_functional: Vec<PostedMoney>,
    pub fx_line: Option<RealizedFxLine>,
}

/// Positive functional money; a loss is debit and a gain credit.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RealizedFxLine {
    pub side: Side,
    pub functional: PostedMoney,
}

/// Malformed closing data or exact arithmetic/final-boundary failure.
#[domain_model]
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RealizedFxError {
    #[error("closing leg carried transaction balance must be > 0")]
    NonPositiveCarriedTransaction,
    #[error("closing leg relieved amount must be > 0 and <= carried transaction")]
    RelievedOutOfRange,
    #[error("closing leg carried functional balance must be >= 0")]
    NegativeCarriedFunctional,
    #[error(transparent)]
    Exact(#[from] ExactError),
}

/// Per-grain WAC relief: exact ratio, rounded once HalfEven at the stored functional scale.
/// Full close returns the historical functional posting unchanged. Chargeback callers
/// carry this same value forward; cash recognition remains the caller's policy.
/// # Errors
/// Rejects transaction metadata mismatch, invalid leg signs/ranges or exact-boundary failures.
pub fn carried_relief(
    carried_functional: &PostedMoney,
    carried_transaction: &PostedMoney,
    relieved_transaction: &PostedMoney,
) -> Result<PostedMoney, RealizedFxError> {
    ensure_same_spec(
        carried_transaction.currency(),
        relieved_transaction.currency(),
    )?;
    if carried_transaction.amount() <= Decimal::ZERO {
        return Err(RealizedFxError::NonPositiveCarriedTransaction);
    }
    if relieved_transaction.amount() <= Decimal::ZERO
        || relieved_transaction.amount() > carried_transaction.amount()
    {
        return Err(RealizedFxError::RelievedOutOfRange);
    }
    if carried_functional.amount() < Decimal::ZERO {
        return Err(RealizedFxError::NegativeCarriedFunctional);
    }
    if relieved_transaction.amount() == carried_transaction.amount() {
        return Ok(carried_functional.clone());
    }
    Ok(ExactAmount::from_decimal(carried_functional.amount())
        .checked_mul(&ExactAmount::from_decimal(relieved_transaction.amount()))?
        .checked_div(&ExactAmount::from_decimal(carried_transaction.amount()))?
        .round_half_even(carried_functional.currency().clone())?)
}

/// Compute independent WAC relief, then exactly net the functional legs and post
/// the positive balancing gain/loss. Empty inputs require no invented currency.
/// # Errors
/// Rejects inconsistent functional specs even on zeros, malformed legs or final overflow.
pub fn realize(legs: &[ClosingLeg]) -> Result<RealizedFx, RealizedFxError> {
    for leg in legs {
        ensure_same_spec(
            legs[0].carried_functional.currency(),
            leg.carried_functional.currency(),
        )?;
    }
    let mut leg_functional = Vec::with_capacity(legs.len());
    let mut net = ExactAmount::from_decimal(Decimal::ZERO);
    for leg in legs {
        let f = carried_relief(
            &leg.carried_functional,
            &leg.carried_transaction,
            &leg.relieved_transaction,
        )?;
        let exact = ExactAmount::from_decimal(f.amount());
        net = match leg.side {
            Side::Debit => net.checked_add(&exact)?,
            Side::Credit => net.checked_sub(&exact)?,
        };
        leg_functional.push(f);
    }
    let fx_line = if net == ExactAmount::from_decimal(Decimal::ZERO) {
        None
    } else {
        let (side, magnitude) = if net.is_negative() {
            (
                Side::Debit,
                ExactAmount::from_decimal(Decimal::ZERO).checked_sub(&net)?,
            )
        } else {
            (Side::Credit, net)
        };
        Some(RealizedFxLine {
            side,
            functional: magnitude
                .into_posted_exact(legs[0].carried_functional.currency().clone())?,
        })
    };
    Ok(RealizedFx {
        leg_functional,
        fx_line,
    })
}

#[cfg(test)]
#[path = "realized_tests.rs"]
mod realized_tests;
