//! Step 2 of the fixed order, the derived meter (rating DESIGN 03 §3.6, T-D-76).
//!
//! A usage line's quantity is Products' derived usage type evaluated per UTC-hour granule by the
//! vendored copy of `bss_products_sdk::derived` and summed exactly into the slice. This module
//! turns the frozen declaration the input carries into the vendored evaluator's own type and has
//! the evaluator check it, so a declaration Products would refuse never reaches a granule. The
//! per-granule evaluation, the `Sum`-only gate (`unsupported_input_fold`) and the exactness gate
//! (`derived_formula_inexact`) are feature 02's pricing semantics (§5.2).

use rating_gen1_derived as derived;

use crate::error::EvaluationError;
use crate::input::{
    DerivedDeclaration, DerivedExpr, FrozenDerivedDeclaration, Granularity, GranuleFold, RoundMode,
};

/// The frozen declaration as the vendored evaluator's type, checked by the evaluator.
///
/// # Errors
///
/// `derived_evaluation_failed` when the evaluator refuses the declaration.
pub fn declaration(
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
