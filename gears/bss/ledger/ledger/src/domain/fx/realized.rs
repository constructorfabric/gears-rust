//! Pure realized-FX (design §3.5 / §4.4): when a cross-currency position
//! **closes** (settle / allocate / refund / chargeback) at a rate ≠ its carried
//! rate, compute the net realized FX as a single `FX_GAIN_LOSS` functional-only
//! line so the close entry's **functional** column balances by construction.
//!
//! Each closing account is relieved at **its own** carried functional value (the
//! grain's `functional_balance_minor`), weighted-average (WAC) **pro-rata** for a
//! partial close (`functional_balance_minor / balance_minor`, banker's rounding —
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

use bss_ledger_sdk::Side;
use toolkit_macros::domain_model;

use crate::domain::money_math::{checked_minor, round_half_even};

/// One account relieved by a close entry, valued at its grain's carried
/// functional. The transaction-relief leg posts on `side` (CR to relieve a
/// debit-normal AR balance, DR to relieve a credit-normal Unallocated balance,
/// …); the realized poster carries the WAC-pro-rata functional relief on the SAME
/// side, then plugs the net imbalance to `FX_GAIN_LOSS`.
#[domain_model]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClosingLeg {
    /// The side the relief leg posts on (the side that moves the grain's
    /// transaction balance toward zero).
    pub side: Side,
    /// The grain's carried functional balance (minor) before this relief
    /// (`functional_balance_minor`). A cross-currency grain always has it
    /// populated (P1 decision 8) — a real carried value. MUST be `>= 0`.
    pub carried_functional_minor: i64,
    /// The grain's transaction balance (minor) before this relief
    /// (`balance_minor`) — the WAC denominator. MUST be `> 0` (a closing grain
    /// holds a positive balance).
    pub carried_transaction_minor: i64,
    /// The transaction amount relieved on this leg (minor): a FULL close relieves
    /// the whole `carried_transaction_minor`; a PARTIAL close relieves less. MUST
    /// be `> 0` and `<= carried_transaction_minor`.
    pub relieved_transaction_minor: i64,
}

/// The realized-FX result: the per-leg functional relief (input order, for the
/// in-txn grain decrement) + the single net `FX_GAIN_LOSS` line (or `None` when
/// the close is at the carried rate — Δ == 0, no realized FX).
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RealizedFx {
    /// Functional relieved per leg (input order) — what the sidecar decrements
    /// from each grain's `functional_balance_minor` in the same close txn.
    pub leg_functional_minor: Vec<i64>,
    /// The balancing `FX_GAIN_LOSS` line, or `None` at the carried rate.
    pub fx_line: Option<RealizedFxLine>,
}

/// The single net `FX_GAIN_LOSS` functional-only line that balances the close
/// entry's functional column. `side` encodes the sign-by-role: a realized **loss**
/// is a **debit** (expense), a **gain** a **credit**; `functional_minor` is always
/// `> 0`. The transaction `amount_minor` of this line is `0` (functional-only) —
/// the caller sets that when it materializes the line.
#[domain_model]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RealizedFxLine {
    pub side: Side,
    pub functional_minor: i64,
}

/// A realized-FX computation failure — every variant is a caller **misuse** (a
/// malformed closing leg), not a business "no rate" condition; the close-path
/// caller maps it to [`crate::domain::error::DomainError::Internal`] (a 500 whose
/// diagnostic stays server-side), exactly like `RateLocker`'s `map_translate_err`.
#[domain_model]
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RealizedFxError {
    #[error("closing leg carried transaction balance must be > 0")]
    NonPositiveCarriedTransaction,
    #[error("closing leg relieved amount must be > 0 and <= the carried transaction balance")]
    RelievedOutOfRange,
    #[error("closing leg carried functional balance must be >= 0")]
    NegativeCarriedFunctional,
    #[error("realized FX amount out of range: {0}")]
    Overflow(i128),
}

/// WAC pro-rata functional relief for one leg — the [`ClosingLeg`] adapter over
/// [`carried_relief`].
fn relieved_functional(leg: &ClosingLeg) -> Result<i64, RealizedFxError> {
    carried_relief(
        leg.carried_functional_minor,
        leg.carried_transaction_minor,
        leg.relieved_transaction_minor,
    )
}

/// WAC pro-rata functional relief for a single closing position:
/// `carried_functional × relieved / carried_transaction`, banker's rounding. A
/// full close (`relieved == carried_transaction`) relieves exactly
/// `carried_functional` (the formula is `f × t / t = f`, no drift).
///
/// The shared carried-rate primitive (decision 3): the realized-FX poster reaches
/// it through a [`ClosingLeg`] (allocate close, F1), and the chargeback functional
/// **carry-forward** (F3) calls it directly — a chargeback relieves the closing
/// grain at this WAC value and stamps the SAME value on the counter-leg, so the
/// entry's functional column nets to zero (a reclassification carries the
/// historical cost basis; realized FX is recognised only at a cash in/out point).
///
/// # Errors
/// [`RealizedFxError`] when `carried_transaction_minor <= 0`,
/// `relieved_transaction_minor` is out of `(0, carried_transaction_minor]`,
/// `carried_functional_minor < 0`, or the product overflows `i64`.
pub fn carried_relief(
    carried_functional_minor: i64,
    carried_transaction_minor: i64,
    relieved_transaction_minor: i64,
) -> Result<i64, RealizedFxError> {
    if carried_transaction_minor <= 0 {
        return Err(RealizedFxError::NonPositiveCarriedTransaction);
    }
    if relieved_transaction_minor <= 0 || relieved_transaction_minor > carried_transaction_minor {
        return Err(RealizedFxError::RelievedOutOfRange);
    }
    if carried_functional_minor < 0 {
        return Err(RealizedFxError::NegativeCarriedFunctional);
    }
    let raw = round_half_even(
        i128::from(carried_functional_minor) * i128::from(relieved_transaction_minor),
        i128::from(carried_transaction_minor),
    );
    checked_minor(raw).map_err(|_| RealizedFxError::Overflow(raw))
}

/// Compute the realized FX for a close that relieves `legs`. Each leg is relieved
/// at its OWN grain's carried functional (WAC pro-rata for a partial close); the
/// net functional imbalance between the relief legs is plugged to a single
/// `FX_GAIN_LOSS` line so the close entry's functional column balances by
/// construction.
///
/// Sign-by-role — the relief legs sum `Δ = Σ_DR_func − Σ_CR_func`:
/// - `Δ < 0` (debit relief short) → **DEBIT** `FX_GAIN_LOSS` of `|Δ|` (a realized
///   **loss**), so `DR_total` rises to meet `CR_total`.
/// - `Δ > 0` (credit relief short) → **CREDIT** `FX_GAIN_LOSS` of `Δ` (a realized
///   **gain**).
/// - `Δ == 0` (closed at the carried rate) → `fx_line = None`, no realized FX.
///
/// An empty `legs` (no close) yields `fx_line = None` and an empty relief vector.
///
/// **Carried value is read ONLY from the per-grain inputs** — never by rescanning
/// `journal_line`, never by averaging across grains (spec §3.5).
///
/// # Errors
/// [`RealizedFxError`] on a malformed closing leg (non-positive carried balance,
/// out-of-range relieved amount, negative carried functional, or an `i64`
/// overflow).
pub fn realize(legs: &[ClosingLeg]) -> Result<RealizedFx, RealizedFxError> {
    let mut leg_functional_minor = Vec::with_capacity(legs.len());
    // Σ_DR_func − Σ_CR_func over the relief legs (i128 so a wide multi-leg close
    // cannot overflow the accumulator before the final i64 narrow).
    let mut net: i128 = 0;
    for leg in legs {
        let f = relieved_functional(leg)?;
        net += match leg.side {
            Side::Debit => i128::from(f),
            Side::Credit => -i128::from(f),
        };
        leg_functional_minor.push(f);
    }
    // Plug the net imbalance to FX_GAIN_LOSS so DR_total == CR_total over the
    // whole entry (relief legs + the FX line).
    let fx_line = match net.cmp(&0) {
        std::cmp::Ordering::Equal => None,
        std::cmp::Ordering::Less => Some(RealizedFxLine {
            side: Side::Debit,
            functional_minor: checked_minor(-net).map_err(|_| RealizedFxError::Overflow(-net))?,
        }),
        std::cmp::Ordering::Greater => Some(RealizedFxLine {
            side: Side::Credit,
            functional_minor: checked_minor(net).map_err(|_| RealizedFxError::Overflow(net))?,
        }),
    };
    Ok(RealizedFx {
        leg_functional_minor,
        fx_line,
    })
}

#[cfg(test)]
#[path = "realized_tests.rs"]
mod realized_tests;
