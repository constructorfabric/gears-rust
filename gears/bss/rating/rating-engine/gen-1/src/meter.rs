//! Step 2 of the fixed order, the derived meter (rating DESIGN 03 §3.6, §4.3; T-D-76).
//!
//! A usage line's quantity is Products' derived usage type evaluated per UTC-hour granule by the
//! vendored copy of `bss_products_sdk::derived` and summed exactly into the slice
//! ([`DerivedMeterEvaluator`]). The frozen declaration the input carries becomes the vendored
//! evaluator's own type and is checked by it, so a declaration Products would refuse never reaches
//! a granule. Per granule:
//!
//! 1. only `Sum` inputs are rated at launch; a `Peak` or `TimeWeighted` input fails
//!    `unsupported_input_fold` (R-06);
//! 2. each input takes the granule's folded quantity of its raw usage type;
//! 3. the exactness guard re-evaluates the formula over exact fractions and fails
//!    `derived_formula_inexact` where Products' `Decimal` arithmetic would round (V57);
//! 4. `derived::evaluate` gives the output, rounded as the declaration states (Products' rule,
//!    not a money rounding), and it must equal the exact value rounded the same way.
//!
//! The slice quantity is the exact `ExactQuantity` sum of the granule outputs; Products'
//! `evaluate_window`, which sums in `Decimal`, is only the V28 test oracle.

use std::collections::BTreeMap;

use bss_money::exact::ExactAmount;
// @cpt-begin:cpt-cf-bss-rating-algo-core-engine-generations:p1:inst-eg-vendor
// The vendored copy of Products' derived evaluator, as a renamed path crate.
use rating_gen1_derived as derived;
// @cpt-end:cpt-cf-bss-rating-algo-core-engine-generations:p1:inst-eg-vendor
use rust_decimal::Decimal;

use crate::budget::ArithmeticBudget;
use crate::error::EvaluationError;
use crate::exact::ExactQuantity;
use crate::exactness;
use crate::input::{
    DerivedDeclaration, DerivedExpr, FrozenDerivedDeclaration, Granularity, Granule, GranuleFold,
    RoundMode,
};
use crate::outcome::GranuleOutput;

/// Evaluates one frozen derived usage type per UTC-hour granule (DESIGN 03 §3.2).
#[derive(Debug, Clone)]
pub struct DerivedMeterEvaluator<'a> {
    frozen: &'a FrozenDerivedDeclaration,
    declaration: derived::DerivedUsageDeclaration,
    budget: ArithmeticBudget,
}

/// A slice's derived quantity and the granule outputs it sums.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeteredSlice {
    pub quantity: ExactQuantity,
    pub granules: Vec<GranuleOutput>,
}

impl<'a> DerivedMeterEvaluator<'a> {
    // @cpt-begin:cpt-cf-bss-rating-algo-core-model:p1:inst-mod-sum
    /// The evaluator of `frozen` under `budget`.
    ///
    /// # Errors
    ///
    /// `derived_evaluation_failed` when Products' evaluator refuses the declaration,
    /// `unsupported_input_fold` for a `Peak` or `TimeWeighted` input (R-06).
    pub fn new(
        frozen: &'a FrozenDerivedDeclaration,
        budget: ArithmeticBudget,
    ) -> Result<Self, EvaluationError> {
        let declaration = declaration(frozen)?;
        if let Some(input) = frozen
            .declaration
            .inputs
            .iter()
            .find(|input| input.granule_fold != GranuleFold::Sum)
        {
            return Err(EvaluationError::UnsupportedInputFold {
                detail: format!(
                    "input `{}` of derived usage type {}@{} folds {:?}; only Sum inputs are rated \
                     until R-06",
                    input.name, frozen.code, frozen.version, input.granule_fold
                ),
            });
        }
        Ok(Self {
            frozen,
            declaration,
            budget,
        })
    }
    // @cpt-end:cpt-cf-bss-rating-algo-core-model:p1:inst-mod-sum

    /// One granule's output.
    ///
    /// # Errors
    ///
    /// `derived_evaluation_failed` when the granule names a usage type the declaration has no
    /// input for, names one twice, lacks one, or Products refuses it; `derived_formula_inexact`
    /// where Products would round an intermediate (V57).
    pub fn granule(&self, granule: &Granule) -> Result<ExactQuantity, EvaluationError> {
        ExactQuantity::from_exact(&self.granule_exact(granule)?, "derived granule output")
    }

    /// One granule's output as an exact value.
    fn granule_exact(&self, granule: &Granule) -> Result<ExactAmount, EvaluationError> {
        let failed = |detail: String| EvaluationError::DerivedEvaluationFailed {
            detail: format!(
                "derived usage type {}@{}, granule {}: {detail}",
                self.frozen.code, self.frozen.version, granule.granule_start
            ),
        };
        let mut by_type: BTreeMap<&str, &ExactQuantity> = BTreeMap::new();
        for input in &granule.inputs {
            let declared = self
                .frozen
                .declaration
                .inputs
                .iter()
                .any(|declared| declared.usage_type_ref == input.usage_type_ref);
            if !declared || by_type.insert(&input.usage_type_ref, &input.q).is_some() {
                return Err(failed(format!(
                    "usage type {} is not an input of the declaration, or is given twice",
                    input.usage_type_ref
                )));
            }
        }
        let mut exact = BTreeMap::new();
        let mut decimal = BTreeMap::new();
        for input in &self.frozen.declaration.inputs {
            let quantity = by_type
                .get(input.usage_type_ref.as_str())
                .ok_or_else(|| failed(format!("no quantity for input `{}`", input.name)))?;
            let value = Decimal::from_str_exact(quantity.as_str()).map_err(|error| {
                failed(format!(
                    "the quantity {quantity} of input `{}` is not a Decimal the evaluator takes: \
                     {error}",
                    input.name
                ))
            })?;
            exact.insert(input.name.as_str(), quantity.to_exact()?);
            decimal.insert(input.name.clone(), value);
        }
        let formula = &self.frozen.declaration.formula;
        let exact = exactness::round_at(
            &exactness::evaluate(formula, &exact, self.budget)?,
            self.frozen.declaration.output_scale,
            self.frozen.declaration.output_round,
        )?;
        let output = derived::evaluate(&self.declaration, &decimal)
            .map_err(|error| failed(format!("refused by the Products evaluator: {error}")))?;
        if ExactAmount::from_decimal(output) != exact {
            return Err(EvaluationError::DerivedFormulaInexact {
                detail: format!(
                    "derived usage type {}@{}, granule {}: Products returned {output}, the exact \
                     value is {exact}",
                    self.frozen.code, self.frozen.version, granule.granule_start
                ),
            });
        }
        Ok(exact)
    }

    /// The exact sum of the granule outputs of a slice, with each output for lineage.
    ///
    /// # Errors
    ///
    /// As [`Self::granule`]; `quantity_out_of_range` for a sum beyond the bound.
    pub fn slice(&self, granules: &[Granule]) -> Result<MeteredSlice, EvaluationError> {
        let mut sum = ExactAmount::from_coefficient(0, 0);
        let mut outputs = Vec::with_capacity(granules.len());
        for granule in granules {
            let exact = self.granule_exact(granule)?;
            sum = sum
                .checked_add(&exact)
                .map_err(|error| EvaluationError::from_money("derived slice quantity", &error))?;
            outputs.push(GranuleOutput {
                granule_start: granule.granule_start,
                output: ExactQuantity::from_exact(&exact, "derived granule output")?,
            });
        }
        Ok(MeteredSlice {
            quantity: ExactQuantity::from_exact(&sum, "derived slice quantity")?,
            granules: outputs,
        })
    }
}

/// The frozen declaration as the vendored evaluator's type, checked by the evaluator.
///
/// # Errors
///
/// `derived_evaluation_failed` when the evaluator refuses the declaration.
fn declaration(
    frozen: &FrozenDerivedDeclaration,
) -> Result<derived::DerivedUsageDeclaration, EvaluationError> {
    let declaration = to_products(&frozen.declaration);
    derived::validate(&declaration).map_err(|error| EvaluationError::DerivedEvaluationFailed {
        detail: format!(
            "derived usage type {}@{} is refused by the Products evaluator: {error}",
            frozen.code, frozen.version
        ),
    })?;
    Ok(declaration)
}

/// The declaration as the vendored evaluator's type, field by field.
fn to_products(declaration: &DerivedDeclaration) -> derived::DerivedUsageDeclaration {
    derived::DerivedUsageDeclaration {
        output_unit: declaration.output_unit.clone(),
        granularity: match declaration.granularity {
            Granularity::Hour => derived::Granularity::Hour,
        },
        inputs: declaration
            .inputs
            .iter()
            .map(|input| derived::DerivedInput {
                name: input.name.clone(),
                usage_type_ref: input.usage_type_ref.clone(),
                granule_fold: match input.granule_fold {
                    GranuleFold::Sum => derived::GranuleFold::Sum,
                    GranuleFold::Peak => derived::GranuleFold::Peak,
                    GranuleFold::TimeWeighted => derived::GranuleFold::TimeWeighted,
                },
                max_hold_seconds: input.max_hold_seconds,
                unit: input.unit.clone(),
            })
            .collect(),
        formula: expr(&declaration.formula),
        output_scale: declaration.output_scale,
        output_round: round(declaration.output_round),
    }
}

/// The formula as the vendored evaluator's expression tree.
fn expr(expr_: &DerivedExpr) -> derived::Expr {
    let boxed = |operand: &DerivedExpr| Box::new(expr(operand));
    match expr_ {
        DerivedExpr::Input { name } => derived::Expr::Input(name.clone()),
        DerivedExpr::Const { value } => derived::Expr::Const(*value),
        DerivedExpr::Add { left, right } => derived::Expr::Add(boxed(left), boxed(right)),
        DerivedExpr::Sub { left, right } => derived::Expr::Sub(boxed(left), boxed(right)),
        DerivedExpr::Mul { left, right } => derived::Expr::Mul(boxed(left), boxed(right)),
        DerivedExpr::DivConst { operand, divisor } => {
            derived::Expr::DivConst(boxed(operand), *divisor)
        }
        DerivedExpr::Max { operands } => derived::Expr::Max(operands.iter().map(expr).collect()),
        DerivedExpr::Min { operands } => derived::Expr::Min(operands.iter().map(expr).collect()),
        DerivedExpr::Ceil { operand } => derived::Expr::Ceil(boxed(operand)),
        DerivedExpr::Floor { operand } => derived::Expr::Floor(boxed(operand)),
        DerivedExpr::Round {
            operand,
            scale,
            mode,
        } => derived::Expr::Round(boxed(operand), *scale, round(*mode)),
    }
}

/// The rounding mode as the vendored evaluator's.
fn round(mode: RoundMode) -> derived::RoundMode {
    match mode {
        RoundMode::HalfEven => derived::RoundMode::HalfEven,
        RoundMode::HalfUp => derived::RoundMode::HalfUp,
        RoundMode::Up => derived::RoundMode::Up,
        RoundMode::Down => derived::RoundMode::Down,
    }
}

#[cfg(test)]
#[path = "meter_tests.rs"]
mod tests;
