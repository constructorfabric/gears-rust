//! Step 3 of the fixed order: the binding's model formula over exact fractions (rating DESIGN
//! 03 §4.1, T-D-74, T-D-80).
//!
//! `flat` → `amount`; `per_unit` → `unit_amount × q`. Each amount is a reduced fraction in major
//! units tagged with the price's currency and stored scale; nothing is scaled by
//! `10^currency_scale` or rounded. The tiered and package formulas (`graduated`, `volume`,
//! `package`) and their band placement are feature 02's pricing semantics (§5.2); until they land
//! in this generation a price with one of those models fails closed with `unsupported_primitive`.

use crate::budget::ArithmeticBudget;
use crate::error::EvaluationError;
use crate::exact::{ExactMoney, ExactQuantity};
use crate::input::{CoreCurrency, CorePriceModel};
use crate::outcome::ModelKind;

/// The amount a model gives for a slice, and the quantity it priced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Priced {
    pub kind: ModelKind,
    pub amount: ExactMoney,
    /// The quantity the formula used; none for a flat price.
    pub quantity: Option<ExactQuantity>,
}

/// Prices `quantity` with `model` in `currency`.
///
/// # Errors
///
/// `unsupported_primitive` for a model this build of the generation does not price yet,
/// `precision_overflow` beyond the budget.
pub fn price(
    model: &CorePriceModel,
    currency: &CoreCurrency,
    quantity: &ExactQuantity,
    budget: ArithmeticBudget,
) -> Result<Priced, EvaluationError> {
    match model {
        CorePriceModel::Flat { amount } => Ok(Priced {
            kind: ModelKind::Flat,
            amount: ExactMoney::from_price(*amount, currency),
            quantity: None,
        }),
        CorePriceModel::PerUnit { unit_amount } => Ok(Priced {
            kind: ModelKind::PerUnit,
            amount: ExactMoney::from_price(*unit_amount, currency)
                .times_quantity(quantity, budget, "per_unit")?,
            quantity: Some(quantity.clone()),
        }),
        CorePriceModel::Volume { .. } => Err(not_yet("volume")),
        CorePriceModel::Graduated { .. } => Err(not_yet("graduated")),
        CorePriceModel::Package { .. } => Err(not_yet("package")),
    }
}

fn not_yet(model: &str) -> EvaluationError {
    EvaluationError::UnsupportedPrimitive {
        detail: format!("the {model} model is not priced by this build of generation 1"),
    }
}

#[cfg(test)]
#[path = "model_tests.rs"]
mod tests;
