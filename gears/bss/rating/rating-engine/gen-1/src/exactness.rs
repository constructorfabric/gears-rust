//! `DerivedExactnessGuard` (rating DESIGN 03 §4.3, money ADR rule 4, T-D-80; V57).
//!
//! Products evaluates a derived formula with `rust_decimal`, whose checked `Add`, `Sub`, `Mul`
//! and `DivConst` round a result that needs more than its 96-bit coefficient or 28 fractional digits
//! before the declared `output_round` (`1e20 + 1e-10` gives `100000000000000000000.00000000`).
//! Until Products removes that undeclared rounding (R-33), the guard re-evaluates the formula of
//! each granule over exact fractions and requires every `Add`, `Sub`, `Mul` and `DivConst` result
//! to be exactly representable as a `Decimal`; `Max`, `Min` and the declared `Ceil`, `Floor` and
//! `Round` return an operand or a declared rounding and need no check. The exact value, rounded
//! as the declaration states, is what Products must return: the meter compares the two.

// @cpt-begin:cpt-cf-bss-rating-algo-core-model:p1:inst-mod-exactness

use std::collections::BTreeMap;

use bigdecimal::num_traits::{One, Signed, Zero};
use bss_money::exact::{BigInt, ExactAmount, ExactError};

use crate::budget::ArithmeticBudget;
use crate::error::EvaluationError;
use crate::input::{DerivedExpr, RoundMode};

/// The most fractional digits of a `Decimal`, and the most significant digits of Orders' decimal
/// text.
const DECIMAL_DIGITS: u8 = 28;

/// The formula's exact value over `inputs`, every arithmetic step checked to be one Products
/// computes without rounding.
///
/// # Errors
///
/// `derived_formula_inexact` naming the first step that is not exact in `Decimal`,
/// `precision_overflow` beyond the budget, `derived_evaluation_failed` for an input the granule
/// lacks or a zero divisor (the evaluator refuses both first).
pub fn evaluate(
    formula: &DerivedExpr,
    inputs: &BTreeMap<&str, ExactAmount>,
    budget: ArithmeticBudget,
) -> Result<ExactAmount, EvaluationError> {
    match formula {
        DerivedExpr::Input { name } => inputs.get(name.as_str()).cloned().ok_or_else(|| {
            EvaluationError::DerivedEvaluationFailed {
                detail: format!("the granule has no quantity for input `{name}`"),
            }
        }),
        DerivedExpr::Const { value } => Ok(ExactAmount::from_decimal(*value)),
        DerivedExpr::Add { left, right } => {
            let (left, right) = (
                evaluate(left, inputs, budget)?,
                evaluate(right, inputs, budget)?,
            );
            exact_step("Add", left.checked_add(&right))
        }
        DerivedExpr::Sub { left, right } => {
            let (left, right) = (
                evaluate(left, inputs, budget)?,
                evaluate(right, inputs, budget)?,
            );
            exact_step("Sub", left.checked_sub(&right))
        }
        DerivedExpr::Mul { left, right } => {
            let (left, right) = (
                evaluate(left, inputs, budget)?,
                evaluate(right, inputs, budget)?,
            );
            budget.check_operands("derived Mul", &left, &right)?;
            exact_step("Mul", left.checked_mul(&right))
        }
        DerivedExpr::DivConst { operand, divisor } => {
            let operand = evaluate(operand, inputs, budget)?;
            let divisor = ExactAmount::from_decimal(*divisor);
            budget.check_operands("derived DivConst", &operand, &divisor)?;
            exact_step("DivConst", operand.checked_div(&divisor))
        }
        DerivedExpr::Max { operands } => extremum(operands, inputs, budget, Ord::max),
        DerivedExpr::Min { operands } => extremum(operands, inputs, budget, Ord::min),
        DerivedExpr::Ceil { operand } => evaluate(operand, inputs, budget)?
            .ceil()
            .map_err(|error| EvaluationError::from_money("derived Ceil", &error)),
        DerivedExpr::Floor { operand } => evaluate(operand, inputs, budget)?
            .floor()
            .map_err(|error| EvaluationError::from_money("derived Floor", &error)),
        DerivedExpr::Round {
            operand,
            scale,
            mode,
        } => round_at(&evaluate(operand, inputs, budget)?, *scale, *mode),
    }
}

/// `value` rounded exactly to `scale` fractional digits by a declared mode, as `rust_decimal`'s
/// strategies apply them: `HalfEven` and `HalfUp` to the nearer step, a tie to the even step or
/// away from zero; `Up` away from zero and `Down` toward zero.
///
/// # Errors
///
/// `precision_overflow` when the money crate's budget refuses the result.
pub fn round_at(
    value: &ExactAmount,
    scale: u32,
    mode: RoundMode,
) -> Result<ExactAmount, EvaluationError> {
    let (numerator, denominator) = (value.numer(), value.denom());
    let step = BigInt::from(10_u8).pow(scale);
    let scaled = numerator * &step;
    // Truncating division: the quotient rounds toward zero and the remainder keeps the sign.
    let quotient = &scaled / denominator;
    let remainder = &scaled % denominator;
    let away = !remainder.is_zero() && {
        let twice = remainder.abs() * BigInt::from(2_u8);
        match mode {
            RoundMode::Down => false,
            RoundMode::Up => true,
            RoundMode::HalfUp => &twice >= denominator,
            RoundMode::HalfEven => {
                &twice > denominator || (&twice == denominator && !(&quotient % 2_u8).is_zero())
            }
        }
    };
    let rounded = if away {
        quotient + numerator.signum()
    } else {
        quotient
    };
    ExactAmount::from_parts(rounded, step)
        .map_err(|error| EvaluationError::from_money("derived rounding", &error))
}

/// The largest coefficient of a `rust_decimal::Decimal`, `2^96 − 1`.
const DECIMAL_MAX_COEFFICIENT: &str = "79228162514264337593543950335";

/// Whether `value` is exactly a `rust_decimal::Decimal`: it terminates within 28 fractional digits
/// and its coefficient at the smallest such scale fits 96 bits. `Decimal` holds every such value
/// without rounding, including a 29-digit coefficient up to `2^96 − 1`.
#[must_use]
pub fn fits_decimal(value: &ExactAmount) -> bool {
    coefficient_digits(value).is_some_and(|digits| {
        digits.len() < DECIMAL_MAX_COEFFICIENT.len()
            || (digits.len() == DECIMAL_MAX_COEFFICIENT.len()
                && digits.as_str() <= DECIMAL_MAX_COEFFICIENT)
    })
}

/// Whether `value` is decimal text of at most 28 significant and 28 fractional digits: the exact
/// form Orders stores (Orders D-216), narrower than `Decimal`'s range.
#[must_use]
pub fn fits_decimal_text(value: &ExactAmount) -> bool {
    coefficient_digits(value).is_some_and(|digits| digits.len() <= usize::from(DECIMAL_DIGITS))
}

/// The digits of `value`'s coefficient at the smallest scale (at most 28) it terminates at,
/// without sign or leading zeros; none when it does not terminate within 28 places. A reduced
/// fraction terminates exactly when its denominator is `2^a · 5^b`, at scale `max(a, b)`.
fn coefficient_digits(value: &ExactAmount) -> Option<String> {
    let (numerator, denominator) = (value.numer(), value.denom());
    let (two, five) = (BigInt::from(2_u8), BigInt::from(5_u8));
    let (mut rest, mut twos, mut fives) = (denominator.clone(), 0_u32, 0_u32);
    while (&rest % &two).is_zero() {
        rest /= &two;
        twos += 1;
    }
    while (&rest % &five).is_zero() {
        rest /= &five;
        fives += 1;
    }
    let scale = twos.max(fives);
    if !rest.is_one() || scale > u32::from(DECIMAL_DIGITS) {
        return None;
    }
    let coefficient = numerator.abs() * (BigInt::from(10_u8).pow(scale) / denominator);
    Some(
        coefficient
            .to_str_radix(10)
            .trim_start_matches('0')
            .to_owned(),
    )
}

/// An arithmetic step's result, refused when `Decimal` would round it.
fn exact_step(
    op: &str,
    result: Result<ExactAmount, ExactError>,
) -> Result<ExactAmount, EvaluationError> {
    let value = result.map_err(|error| EvaluationError::from_money(op, &error))?;
    if fits_decimal(&value) {
        return Ok(value);
    }
    Err(EvaluationError::DerivedFormulaInexact {
        detail: format!(
            "the {op} result {value} is not exactly a rust_decimal Decimal (a 96-bit coefficient \
             at a scale of at most {DECIMAL_DIGITS}), so Products would round it before the \
             declared rounding"
        ),
    })
}

/// The largest or smallest operand.
fn extremum(
    operands: &[DerivedExpr],
    inputs: &BTreeMap<&str, ExactAmount>,
    budget: ArithmeticBudget,
    pick: fn(ExactAmount, ExactAmount) -> ExactAmount,
) -> Result<ExactAmount, EvaluationError> {
    let mut best: Option<ExactAmount> = None;
    for operand in operands {
        let value = evaluate(operand, inputs, budget)?;
        best = Some(match best {
            None => value,
            Some(best) => pick(best, value),
        });
    }
    best.ok_or_else(|| EvaluationError::DerivedEvaluationFailed {
        detail: "an extremum without operands".into(),
    })
}

// @cpt-end:cpt-cf-bss-rating-algo-core-model:p1:inst-mod-exactness

#[cfg(test)]
#[path = "exactness_tests.rs"]
mod tests;
