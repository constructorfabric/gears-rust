//! Pure FX translation (design §4.2): convert a balanced set of transaction
//! amounts to the functional currency at a single locked rate (banker's
//! rounding), then close the per-entry functional rounding **residual**
//! deterministically onto an anchor line so the functional column balances by
//! construction. Per-line `amount × rate` rounding can leave a residual
//! (`|residual| ≤ lines − 1` minor units) even when the transaction column is
//! exact; a single deterministic plug onto a real anchor line (e.g. the AR leg,
//! whose functional dwarfs the residual) closes it. No infra; the `RateLocker`
//! feeds it the locked rate and picks the anchor.

use bss_ledger_sdk::Side;
use toolkit_macros::domain_model;

use crate::domain::money_math::{checked_minor, round_half_even};

/// Micro scale: `rate_micro` is the functional-per-unit-transaction rate × 1e6.
const MICRO: i128 = 1_000_000;

/// A line to translate: its transaction amount (minor units) and DR/CR side.
#[domain_model]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FxLine {
    pub amount_minor: i64,
    pub side: Side,
}

/// A functional-translation failure.
#[domain_model]
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum FxTranslateError {
    #[error("FX rate must be positive (rate_micro > 0)")]
    RateNonPositive,
    #[error("anchor line index is out of bounds")]
    AnchorOutOfBounds,
    #[error("functional residual would drive the anchor line non-positive")]
    ResidualExceedsAnchor,
    #[error("translated functional amount out of range: {0}")]
    Overflow(i128),
}

/// Translate a single transaction amount to functional at `rate_micro`, banker's
/// rounding (`amount × rate_micro / 1e6`, ties to even). Public so the
/// unrealized-revaluation run (Group H2) can remeasure one grain's transaction
/// balance at the period-end rate without going through [`translate_entry`]
/// (which closes a multi-line residual it does not need).
///
/// Rejects a non-positive `rate_micro` up front: a rate `<= 0` is never a valid
/// FX quote, and letting it through would flip the sign of (or zero out) the
/// translated amount and post a wrong revaluation/allocation entry. The
/// [`translate_entry`] path already guards this; guarding here closes the same
/// hole for the direct single-amount callers (e.g. the revaluation run), which
/// resolve a rate straight from the local store that the provider-sync path
/// upserts.
///
/// # Errors
/// - [`FxTranslateError::RateNonPositive`] if `rate_micro <= 0`.
/// - [`FxTranslateError::Overflow`] if the translated amount exceeds `i64`.
pub fn translate_amount(amount_minor: i64, rate_micro: i64) -> Result<i64, FxTranslateError> {
    if rate_micro <= 0 {
        return Err(FxTranslateError::RateNonPositive);
    }
    let raw = round_half_even(i128::from(amount_minor) * i128::from(rate_micro), MICRO);
    checked_minor(raw).map_err(|_| FxTranslateError::Overflow(raw))
}

/// Translate every line at `rate_micro` and close the per-entry functional
/// residual onto `anchor` (the index of a real line whose functional absorbs the
/// small residual), so `SUM(DR.functional) == SUM(CR.functional)` exactly. Returns
/// the functional amount per line, in the input order, each `> 0`.
///
/// # Errors
/// - [`FxTranslateError::RateNonPositive`] if `rate_micro <= 0`.
/// - [`FxTranslateError::AnchorOutOfBounds`] if `anchor >= lines.len()`.
/// - [`FxTranslateError::ResidualExceedsAnchor`] if the residual would drive the
///   anchor's functional `<= 0` (a misuse — the anchor must be a substantial line).
/// - [`FxTranslateError::Overflow`] if a translated amount exceeds `i64`.
pub fn translate_entry(
    lines: &[FxLine],
    rate_micro: i64,
    anchor: usize,
) -> Result<Vec<i64>, FxTranslateError> {
    if rate_micro <= 0 {
        return Err(FxTranslateError::RateNonPositive);
    }
    if anchor >= lines.len() {
        return Err(FxTranslateError::AnchorOutOfBounds);
    }
    let mut func: Vec<i64> = Vec::with_capacity(lines.len());
    for l in lines {
        func.push(translate_amount(l.amount_minor, rate_micro)?);
    }
    // net = SUM(DR.functional) − SUM(CR.functional); the rounding residual.
    let net: i128 = lines
        .iter()
        .zip(&func)
        .map(|(l, &f)| match l.side {
            Side::Debit => i128::from(f),
            Side::Credit => -i128::from(f),
        })
        .sum();
    // Close the residual onto the anchor so the new net is exactly zero:
    // a DR anchor moves net by +Δ (want Δ = −net); a CR anchor by −Δ (want Δ = net).
    let adj = match lines[anchor].side {
        Side::Debit => -net,
        Side::Credit => net,
    };
    let new_anchor = i128::from(func[anchor]) + adj;
    if new_anchor <= 0 {
        return Err(FxTranslateError::ResidualExceedsAnchor);
    }
    func[anchor] = checked_minor(new_anchor).map_err(|_| FxTranslateError::Overflow(new_anchor))?;
    Ok(func)
}

#[cfg(test)]
#[path = "translate_tests.rs"]
mod translate_tests;
