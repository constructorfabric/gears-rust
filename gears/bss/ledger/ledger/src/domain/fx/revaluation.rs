//! Pure unrealized-revaluation (design §3.6 / §4.5): at period end a Mode-B
//! ledger (= ledger of record, decision 4) remeasures every foreign-currency
//! **monetary** grain `{AR, UNALLOCATED, REUSABLE_CREDIT}` at the period-end rate
//! against its **carried** functional value and books the difference to a contra
//! `FX_UNREALIZED` line so the entry's functional column balances by
//! construction. The whole entry is **functional-only** (`amount_minor = 0` on
//! every line) — it adjusts the functional column only and passes the
//! transaction-balance check trivially (zero in-scope lines, design §4.5).
//!
//! `CONTRACT_LIABILITY` is deliberately **excluded** — it is non-monetary (a
//! deferred performance obligation), which ASC 830 / IAS 21 does not remeasure.
//!
//! This is the unrealized sibling of [`crate::domain::fx::realized`] and shares
//! its shape: the per-grain remeasure legs move each grain's functional carrying
//! value, and the net imbalance plugs to a **single** `FX_UNREALIZED` line
//! (sign-by-role, like `FX_GAIN_LOSS`). Unlike realized FX — which is permanent,
//! booked at a cash in/out point — a revaluation is a **temporary** remeasurement
//! that is **reversed** on the first day of the next OPEN period (decision 7); a
//! foreign position's true gain/loss is realized only when it closes (§4.4). The
//! reversal is a fresh `FX_REVAL_REVERSAL` JE built by the run layer (Group H3)
//! by negating this entry's posted lines — it does not use this module.
//!
//! Pure: no infra. The run (Group H2) enumerates the carried grain values,
//! resolves the period-end rate, translates each grain's transaction balance to
//! its remeasured functional value, then feeds the positions here.

use bss_ledger_sdk::Side;
use toolkit_macros::domain_model;

use crate::domain::money_math::checked_minor;

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

/// One monetary grain to remeasure at period end. The run reads the grain's
/// carried functional value and computes its period-end remeasured functional
/// value (the grain's transaction balance valued at the period-end rate), then
/// asks this module for the adjusting leg.
#[domain_model]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RevaluationPosition {
    /// The grain's normal balance side (asset = debit, liability = credit), from
    /// the run's [`RevaluationScope::normal_side`].
    pub normal_side: Side,
    /// The grain's carried functional balance (minor) before remeasurement
    /// (`functional_balance_minor`). A cross-currency grain always has it
    /// populated (P1 decision 8) — a real carried value. MUST be `>= 0`.
    pub carried_functional_minor: i64,
    /// The grain's period-end remeasured functional value (minor): the grain's
    /// transaction balance valued at the period-end rate. MUST be `>= 0`.
    pub remeasured_functional_minor: i64,
}

/// A single functional-only remeasurement line: a `side` + a positive
/// `functional_minor`. Used both for a per-grain adjusting leg and for the net
/// `FX_UNREALIZED` contra line. The transaction `amount_minor` of the
/// materialized line is `0` (functional-only) — the caller sets that.
#[domain_model]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RevaluationLine {
    pub side: Side,
    pub functional_minor: i64,
}

/// The result of remeasuring a set of same-scope positions: the per-position
/// adjusting leg (input order; `None` where the grain is already at the
/// period-end rate, Δ == 0) plus the single net `FX_UNREALIZED` contra line
/// (`None` when every Δ == 0, i.e. nothing to post).
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Revaluation {
    /// Per-position adjusting leg in input order; `None` for a grain at the
    /// period-end rate (no movement).
    pub grain_lines: Vec<Option<RevaluationLine>>,
    /// The balancing `FX_UNREALIZED` contra line, or `None` when the whole run
    /// nets to zero (no entry to post).
    pub fx_unrealized: Option<RevaluationLine>,
}

impl Revaluation {
    /// True when there is nothing to post (every grain at the period-end rate).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.fx_unrealized.is_none()
    }
}

/// A revaluation computation failure — every variant is a caller **misuse** (a
/// malformed position), not a business condition; the run maps it to
/// [`crate::domain::error::DomainError::Internal`] (a 500 whose diagnostic stays
/// server-side), exactly like [`crate::domain::fx::realized::RealizedFxError`].
#[domain_model]
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RevaluationError {
    #[error("revaluation position carried functional balance must be >= 0")]
    NegativeCarriedFunctional,
    #[error("revaluation position remeasured functional value must be >= 0")]
    NegativeRemeasured,
    #[error("revaluation amount out of range: {0}")]
    Overflow(i128),
}

/// The opposite posting side.
const fn opposite(side: Side) -> Side {
    match side {
        Side::Debit => Side::Credit,
        Side::Credit => Side::Debit,
    }
}

/// Compute the per-grain adjusting leg for one position. The carrying value moves
/// from `carried` to `remeasured`; the magnitude `|Δ|` posts on the grain's
/// **normal** side when the carrying value RISES (Δ > 0, the grain is worth more
/// in functional terms) and on the **opposite** side when it FALLS (Δ < 0).
/// `Δ == 0` → no leg.
fn adjust_leg(pos: &RevaluationPosition) -> Result<Option<RevaluationLine>, RevaluationError> {
    if pos.carried_functional_minor < 0 {
        return Err(RevaluationError::NegativeCarriedFunctional);
    }
    if pos.remeasured_functional_minor < 0 {
        return Err(RevaluationError::NegativeRemeasured);
    }
    let delta =
        i128::from(pos.remeasured_functional_minor) - i128::from(pos.carried_functional_minor);
    Ok(match delta.cmp(&0) {
        std::cmp::Ordering::Equal => None,
        std::cmp::Ordering::Greater => Some(RevaluationLine {
            side: pos.normal_side,
            functional_minor: checked_minor(delta)
                .map_err(|_| RevaluationError::Overflow(delta))?,
        }),
        std::cmp::Ordering::Less => Some(RevaluationLine {
            side: opposite(pos.normal_side),
            functional_minor: checked_minor(-delta)
                .map_err(|_| RevaluationError::Overflow(-delta))?,
        }),
    })
}

/// Remeasure a set of **same-scope** positions at the period-end rate. Each
/// grain's carrying value is adjusted from its carried functional to its
/// remeasured functional; the net imbalance across the grain legs plugs to a
/// single `FX_UNREALIZED` line so `DR_total == CR_total` over the whole entry.
///
/// Sign-by-role — the grain legs sum `Δ = Σ_DR_func − Σ_CR_func`:
/// - `Δ < 0` (debit side short) → **DEBIT** `FX_UNREALIZED` of `|Δ|` (a net
///   unrealized **loss** raises `DR_total` to meet `CR_total`).
/// - `Δ > 0` (credit side short) → **CREDIT** `FX_UNREALIZED` of `Δ` (a net
///   unrealized **gain**).
/// - `Δ == 0` (no movement) → `fx_unrealized = None`, nothing to post.
///
/// An empty `positions` yields an empty result (`fx_unrealized = None`).
///
/// # Errors
/// [`RevaluationError`] on a malformed position (negative carried/remeasured
/// functional, or an `i64` overflow on the magnitude).
pub fn remeasure(positions: &[RevaluationPosition]) -> Result<Revaluation, RevaluationError> {
    let mut grain_lines = Vec::with_capacity(positions.len());
    // Σ_DR_func − Σ_CR_func over the grain legs (i128 so a wide multi-grain run
    // cannot overflow the accumulator before the final i64 narrow).
    let mut net: i128 = 0;
    for pos in positions {
        let leg = adjust_leg(pos)?;
        if let Some(l) = leg {
            net += match l.side {
                Side::Debit => i128::from(l.functional_minor),
                Side::Credit => -i128::from(l.functional_minor),
            };
        }
        grain_lines.push(leg);
    }
    let fx_unrealized = match net.cmp(&0) {
        std::cmp::Ordering::Equal => None,
        std::cmp::Ordering::Less => Some(RevaluationLine {
            side: Side::Debit,
            functional_minor: checked_minor(-net).map_err(|_| RevaluationError::Overflow(-net))?,
        }),
        std::cmp::Ordering::Greater => Some(RevaluationLine {
            side: Side::Credit,
            functional_minor: checked_minor(net).map_err(|_| RevaluationError::Overflow(net))?,
        }),
    };
    Ok(Revaluation {
        grain_lines,
        fx_unrealized,
    })
}

#[cfg(test)]
#[path = "revaluation_tests.rs"]
mod revaluation_tests;
