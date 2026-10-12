//! Pure per-grain period-end remeasurement, with optional net unrealized contra.

use super::translate::ensure_same_spec;
use crate::domain::exact_money::{ExactAmount, ExactError};
use bss_ledger_sdk::{Side, money::PostedMoney};
use rust_decimal::Decimal;
use toolkit_macros::domain_model;

/// Which monetary grain class a revaluation run covers. The non-monetary
/// `CONTRACT_LIABILITY` is deliberately absent (ASC 830 / IAS 21 — design §4.5).
/// One scope = one run + one entry + one idempotency family (`business_id =
/// period_id:scope`), so the three monetary classes revalue independently.
#[domain_model]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RevaluationScope {
    /// Open AR (`ar_invoice_balance`) — a monetary **asset** (debit-normal).
    Ar,
    /// Unapplied prepayment (`unallocated_balance`) — a monetary **liability**
    /// owed back to the customer (credit-normal).
    Unallocated,
    /// Customer wallet (`reusable_credit_subbalance`) — a monetary **liability**
    /// held for the customer (credit-normal).
    ReusableCredit,
}

impl RevaluationScope {
    /// The grain's normal balance side: AR is an asset (debit-normal); the
    /// customer-owed monetary liabilities are credit-normal. Drives the sign of
    /// the per-grain adjusting leg.
    #[must_use]
    pub const fn normal_side(self) -> Side {
        match self {
            RevaluationScope::Ar => Side::Debit,
            RevaluationScope::Unallocated | RevaluationScope::ReusableCredit => Side::Credit,
        }
    }

    /// The scope token used in the idempotency `business_id` (`period_id:scope`)
    /// for both the `FX_REVALUATION` run and the `FX_REVAL_REVERSAL` reversal.
    #[must_use]
    pub const fn as_token(self) -> &'static str {
        match self {
            RevaluationScope::Ar => "AR",
            RevaluationScope::Unallocated => "UNALLOCATED",
            RevaluationScope::ReusableCredit => "REUSABLE_CREDIT",
        }
    }

    /// The three covered scopes, in a stable order (the run iterates these).
    #[must_use]
    pub const fn all() -> [RevaluationScope; 3] {
        [
            RevaluationScope::Ar,
            RevaluationScope::Unallocated,
            RevaluationScope::ReusableCredit,
        ]
    }
}

/// One grain's historical and remeasured functional values with stored metadata.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RevaluationPosition {
    pub normal_side: Side,
    pub carried_functional: PostedMoney,
    pub remeasured_functional: PostedMoney,
}

/// Positive functional money and the posting side that carries its sign.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RevaluationLine {
    pub side: Side,
    pub functional: PostedMoney,
}

/// Ordered grain movements and optional net balancing contra.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Revaluation {
    pub grain_lines: Vec<Option<RevaluationLine>>,
    /// Absent when the net is zero, including cancelling real grain movements.
    pub fx_unrealized: Option<RevaluationLine>,
}

impl Revaluation {
    /// True only when there are no grain movements and no contra to post.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.grain_lines.iter().all(Option::is_none) && self.fx_unrealized.is_none()
    }
}

/// Malformed remeasurement or exact arithmetic/final-boundary failure.
#[domain_model]
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RevaluationError {
    #[error("carried functional balance must be >= 0")]
    NegativeCarriedFunctional,
    #[error("remeasured functional value must be >= 0")]
    NegativeRemeasured,
    #[error(transparent)]
    Exact(#[from] ExactError),
}

/// Return the opposite posting side.
const fn opposite(side: Side) -> Side {
    match side {
        Side::Debit => Side::Credit,
        Side::Credit => Side::Debit,
    }
}

/// Compute an exact per-grain delta; positive movements use its normal side.
fn adjust_leg(pos: &RevaluationPosition) -> Result<Option<RevaluationLine>, RevaluationError> {
    ensure_same_spec(
        pos.carried_functional.currency(),
        pos.remeasured_functional.currency(),
    )?;
    if pos.carried_functional.amount() < Decimal::ZERO {
        return Err(RevaluationError::NegativeCarriedFunctional);
    }
    if pos.remeasured_functional.amount() < Decimal::ZERO {
        return Err(RevaluationError::NegativeRemeasured);
    }
    let delta = ExactAmount::from_decimal(pos.remeasured_functional.amount())
        .checked_sub(&ExactAmount::from_decimal(pos.carried_functional.amount()))?;
    if delta == ExactAmount::from_decimal(Decimal::ZERO) {
        return Ok(None);
    }
    let (side, magnitude) = if delta.is_negative() {
        (
            opposite(pos.normal_side),
            ExactAmount::from_decimal(Decimal::ZERO).checked_sub(&delta)?,
        )
    } else {
        (pos.normal_side, delta)
    };
    Ok(Some(RevaluationLine {
        side,
        functional: magnitude.into_posted_exact(pos.carried_functional.currency().clone())?,
    }))
}

/// Remeasure every grain independently, then net exactly into one optional contra.
/// Cancelling movements still post their grain legs. Empty results invent no currency.
/// # Errors
/// Rejects mismatched functional specs including zeros, negative values, or final overflow.
pub fn remeasure(positions: &[RevaluationPosition]) -> Result<Revaluation, RevaluationError> {
    for pos in positions {
        ensure_same_spec(
            positions[0].carried_functional.currency(),
            pos.carried_functional.currency(),
        )?;
        ensure_same_spec(
            pos.carried_functional.currency(),
            pos.remeasured_functional.currency(),
        )?;
    }
    let mut grain_lines = Vec::with_capacity(positions.len());
    let mut net = ExactAmount::from_decimal(Decimal::ZERO);
    for pos in positions {
        let leg = adjust_leg(pos)?;
        if let Some(line) = &leg {
            let exact = ExactAmount::from_decimal(line.functional.amount());
            net = match line.side {
                Side::Debit => net.checked_add(&exact)?,
                Side::Credit => net.checked_sub(&exact)?,
            };
        }
        grain_lines.push(leg);
    }
    let fx_unrealized = if net == ExactAmount::from_decimal(Decimal::ZERO) {
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
        Some(RevaluationLine {
            side,
            functional: magnitude
                .into_posted_exact(positions[0].carried_functional.currency().clone())?,
        })
    };
    Ok(Revaluation {
        grain_lines,
        fx_unrealized,
    })
}

#[cfg(test)]
#[path = "revaluation_tests.rs"]
mod revaluation_tests;
