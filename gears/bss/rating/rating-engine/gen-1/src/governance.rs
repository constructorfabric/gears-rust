//! Governance pass-through and the rating-time check that remains (rating DESIGN 10 §4.2, §4.4,
//! §4.5; feature 02 §3.7).
//!
//! Approval is Pricing's (the shared `bss-approval` engine): Rating registers nothing in Pricing's
//! publish pipeline (R-12 closed) and keeps one defensive check at evaluation, `rating-val-02`.
//! Every line carries the binding's invoice inputs and null ASC 606 references; a sold-as bundle is
//! dormant (pricing D-411) and fails closed.

use uuid::Uuid;

use crate::error::EvaluationError;
use crate::input::{ChargeKind, CoreBinding, CoreCell, CoreResolve, FactVersion};

/// What a usage record or charge maps onto: the derived meter of a usage binding, the SKU of any
/// other.
#[derive(Debug, PartialEq, Eq)]
enum Target<'a> {
    Meter(&'a str, &'a str),
    Sku(Uuid),
}

fn target(binding: &CoreBinding) -> Target<'_> {
    match (&binding.kind, &binding.meter) {
        (ChargeKind::Usage, Some(meter)) => {
            Target::Meter(meter.usage_type_id.as_str(), meter.version.as_str())
        }
        _ => Target::Sku(binding.sku_id),
    }
}

/// `rating-val-02`: no other cell of the resolve maps onto the same derived meter (or SKU) and
/// dimension value for the line's charge kind. It mirrors Pricing's `METER_DUPLICATE` publish
/// check, so it fails only on a defect; when it does, no line is emitted.
///
/// # Errors
///
/// `non_injective_mapping` naming both cells.
// @cpt-begin:cpt-cf-bss-rating-flow-core-rule-verdicts:p1:inst-rv-interim
pub fn check_injective(
    resolve: &CoreResolve,
    rated: &CoreCell,
    binding: &CoreBinding,
) -> Result<(), EvaluationError> {
    let line = (
        binding.kind,
        target(binding),
        &rated.selection.dimension_value,
    );
    let clash = resolve.cells.iter().find(|cell| {
        cell.selection != rated.selection
            && cell.binding.as_ref().is_some_and(|other| {
                (other.kind, target(other), &cell.selection.dimension_value) == line
            })
    });
    match clash {
        None => Ok(()),
        Some(cell) => Err(EvaluationError::NonInjectiveMapping {
            detail: format!(
                "items {} and {} map to the same {} target and dimension value {:?} \
                 (rating-val-02)",
                rated.selection.item_id,
                cell.selection.item_id,
                kind_name(binding.kind),
                rated.selection.dimension_value
            ),
        }),
    }
}
// @cpt-end:cpt-cf-bss-rating-flow-core-rule-verdicts:p1:inst-rv-interim

/// A line that names a bundle fails closed: Pricing refuses bundle SKUs and the sold-as bundle is
/// deferred (pricing D-411), so no bundle line is ever priced here.
///
/// # Errors
///
/// `unsupported_primitive` when the fact names a bundle.
// @cpt-begin:cpt-cf-bss-rating-algo-core-governance-passthrough:p2:inst-gov-bundle
pub fn check_bundle(fact: &FactVersion) -> Result<(), EvaluationError> {
    match &fact.bundle {
        None => Ok(()),
        Some(bundle) => Err(EvaluationError::UnsupportedPrimitive {
            detail: format!(
                "the line names bundle {bundle:?}; bundle summing is dormant (pricing D-411)"
            ),
        }),
    }
}
// @cpt-end:cpt-cf-bss-rating-algo-core-governance-passthrough:p2:inst-gov-bundle

/// The charge kind as it appears in a `line_key`.
pub fn kind_name(kind: ChargeKind) -> &'static str {
    match kind {
        ChargeKind::Usage => "usage",
        ChargeKind::Recurring => "recurring",
        ChargeKind::OneTime => "one_time",
    }
}

#[cfg(test)]
#[path = "governance_tests.rs"]
mod tests;
