//! The arithmetic budget of generation 1 (rating DESIGN §3.3, "Intermediate arithmetic").
//!
//! The budget is part of the generation, so the same input fails the same way on replay. It
//! records the limits of the exact arithmetic it computes with (`bss_money::exact`: a reduced
//! integer of at most 256 decimal digits, scratch products of at most 512) and is checked
//! **before** each multiply and divide: an operand beyond it fails `precision_overflow` before any
//! work is done. The input carries the budget its pipeline dispatched with; another budget is
//! another engine.

use bss_money::exact::{BigInt, ExactAmount};
use serde::{Deserialize, Serialize};

use crate::error::EvaluationError;

/// The per-operand limits checked before every multiply and divide.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArithmeticBudget {
    /// The most decimal digits of an operand's reduced numerator or denominator.
    pub reduced_digits: u32,
    /// The most decimal digits of the cross products of two operands.
    pub scratch_digits: u32,
}

impl ArithmeticBudget {
    /// Generation 1's budget: the limits of `bss_money::exact`.
    pub const GEN_1: Self = Self {
        reduced_digits: 256,
        scratch_digits: 512,
    };

    /// Checks two operands before they are multiplied or divided at `site`.
    ///
    /// # Errors
    ///
    /// `precision_overflow` when a numerator or denominator has more than `reduced_digits`
    /// digits, or when a cross product of the two could have more than `scratch_digits`.
    pub fn check_operands(
        self,
        site: &str,
        left: &ExactAmount,
        right: &ExactAmount,
    ) -> Result<(), EvaluationError> {
        let (left_num, left_den) = (digit_count(left.numer()), digit_count(left.denom()));
        let (right_num, right_den) = (digit_count(right.numer()), digit_count(right.denom()));
        let widest = left_num.max(left_den).max(right_num).max(right_den);
        // `a/b × c/d` forms `a·c` and `b·d`; `a/b ÷ c/d` forms `a·d` and `b·c`. Each product has at
        // most the sum of its factors' digits, so this bounds every cross product of the pair.
        let scratch =
            (left_num + right_num.max(right_den)).max(left_den + right_num.max(right_den));
        if widest > self.reduced_digits || scratch > self.scratch_digits {
            return Err(EvaluationError::PrecisionOverflow {
                detail: format!(
                    "{site}: an operand of {widest} digits or a product of {scratch} digits \
                     leaves the arithmetic budget ({} reduced, {} scratch)",
                    self.reduced_digits, self.scratch_digits
                ),
            });
        }
        Ok(())
    }
}

/// The number of decimal digits of `value`'s magnitude; zero has one.
fn digit_count(value: &BigInt) -> u32 {
    u32::try_from(value.magnitude().to_str_radix(10).len()).unwrap_or(u32::MAX)
}

#[cfg(test)]
#[path = "budget_tests.rs"]
mod tests;
