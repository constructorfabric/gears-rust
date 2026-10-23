//! Step 5 of the fixed order: the guards, per line, in order (rating DESIGN 01 §4.4).
//!
//! 0. **Deferred adjustments**: an input naming a contract override, commitment pool,
//!    reservation, coupon or promotion fails `adjustment_unsupported`; a billing currency other
//!    than the binding's fails `fx_not_supported`. Nothing is applied or charged by default.
//! 1. **Non-negative**: a line amount below zero is clamped to zero, the pre-clamp amount kept in
//!    lineage (until Finance decides clamp versus credit, PRD §15).
//! 2. **Precision**: the amount leaves as an exact reduced fraction within the delivery bounds,
//!    else `precision_overflow`; money is never rounded. Only the delivered amount is bounded: the
//!    pre-clamp amount is lineage, an intermediate within the arithmetic budget.
//! 3. **Lineage**: the caller builds it from what these guards return; every field is present.
//!
//! The minimum-fee floor (step 4) is not a line guard: roll-up applies it per price over the
//! child totals (T-D-78).

use crate::error::EvaluationError;
use crate::exact::ExactMoney;
use crate::input::{CoreBinding, FactVersion};
use crate::outcome::{MoneyText, Step, StepRecord};

/// Guard 0: price adjustments are deferred and FX is not supported (DECOMPOSITION §2.3).
///
/// # Errors
///
/// `adjustment_unsupported` for the first adjustment the fact names, then `fx_not_supported` when
/// the billing currency is not the binding's.
// @cpt-begin:cpt-cf-bss-rating-algo-core-step-pipeline:p1:inst-sp-guards
pub fn deferred_adjustments(
    fact: &FactVersion,
    binding: &CoreBinding,
) -> Result<(), EvaluationError> {
    if let Some(adjustment) = fact.adjustments.first() {
        return Err(EvaluationError::AdjustmentUnsupported {
            detail: format!(
                "the fact names {:?} {:?}; price adjustments are deferred and never applied by \
                 default",
                adjustment.kind, adjustment.reference
            ),
        });
    }
    if fact.currency != binding.price.currency.code {
        return Err(EvaluationError::FxNotSupported {
            detail: format!(
                "billing currency {} differs from the price currency {}; nothing is converted",
                fact.currency, binding.price.currency.code
            ),
        });
    }
    Ok(())
}

/// What the amount guards hand to lineage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Guarded {
    /// The delivered amount.
    pub amount: ExactMoney,
    /// Its delivered form, within the delivery bounds.
    pub wire: MoneyText,
    /// The pre-clamp amount, when guard 1 clamped.
    pub clamped_from: Option<MoneyText>,
}

/// Guards 1 and 2 on a line amount, recording each in `steps`.
///
/// # Errors
///
/// `precision_overflow` when the delivered amount leaves the delivery bounds.
pub fn amount(amount: ExactMoney, steps: &mut Vec<StepRecord>) -> Result<Guarded, EvaluationError> {
    let pre = amount.to_lineage();
    let (amount, clamped_from) = if amount.is_negative() {
        (ExactMoney::zero(amount.currency()), Some(pre.clone()))
    } else {
        (amount, None)
    };
    let wire = amount.to_wire("line amount")?;
    steps.push(StepRecord {
        step: Step::NonNegative,
        pre: Some(pre),
        post: Some(wire.clone()),
    });
    steps.push(StepRecord {
        step: Step::Precision,
        pre: Some(wire.clone()),
        post: Some(wire.clone()),
    });
    Ok(Guarded {
        amount,
        wire,
        clamped_from,
    })
}
// @cpt-end:cpt-cf-bss-rating-algo-core-step-pipeline:p1:inst-sp-guards

#[cfg(test)]
#[path = "guards_tests.rs"]
mod tests;
