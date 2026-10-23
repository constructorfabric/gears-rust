//! The numerator and denominator of a vendored `ExactAmount`, until R-35 lands.
//!
//! `bss_money::exact::ExactAmount` keeps its reduced fraction private. R-35
//! (`…-upreq-bss-money-exact-api`, rating `UPSTREAM_REQS` §2.8) asks the crate for `numer()` /
//! `denom()`, `from_parts` and a constructor from a long decimal; generation 1 is frozen only with
//! the crate release that has them vendored, and this module then goes. Until then it derives the
//! same values from the crate's public, exact API, so the arithmetic stays the crate's (money ADR
//! rule 2: one crate, no Rating copy):
//!
//! - [`parts`] reads the reduced fraction back from `Display`, which writes a value that
//!   terminates within 28 places as decimal text and any other as `numerator/denominator`; both
//!   forms are exact, and the decimal form is reduced here;
//! - [`from_integer`] builds an integer of any length from `i128` chunks with the crate's checked
//!   multiply and add, so the crate's own budget applies to every step.

use bigdecimal::num_bigint::{BigInt, Sign};
use bigdecimal::num_traits::{One, Signed, Zero};
use rating_gen1_money::exact::ExactAmount;

use crate::error::EvaluationError;

/// The most decimal digits one `i128` chunk carries (`10^36 < i128::MAX`).
const CHUNK_DIGITS: usize = 36;

/// The reduced `(numerator, denominator)` of `value`; the denominator is positive and the sign is
/// on the numerator, zero is `0/1`.
///
/// # Errors
///
/// `money_contract_violation` if the crate's text is not one of its two documented forms.
pub fn parts(value: &ExactAmount) -> Result<(BigInt, BigInt), EvaluationError> {
    let text = value.to_string();
    let unparsable = || EvaluationError::MoneyContractViolation {
        detail: format!("exact amount text {text:?} is neither a decimal nor a fraction"),
    };
    let (negative, unsigned) = text
        .strip_prefix('-')
        .map_or((false, text.as_str()), |rest| (true, rest));
    let (mut numerator, denominator) = if let Some((top, bottom)) = unsigned.split_once('/') {
        (
            digits(top).ok_or_else(unparsable)?,
            digits(bottom).ok_or_else(unparsable)?,
        )
    } else {
        let (whole, fractional) = unsigned.split_once('.').unwrap_or((unsigned, ""));
        let numerator = digits(&format!("{whole}{fractional}")).ok_or_else(unparsable)?;
        let exponent = u32::try_from(fractional.len()).map_err(|_| unparsable())?;
        reduce(numerator, BigInt::from(10_u8).pow(exponent))
    };
    if denominator.is_zero() {
        return Err(unparsable());
    }
    if negative {
        numerator = -numerator;
    }
    Ok((numerator, denominator))
}

/// The decimal digit counts of the reduced numerator (without its sign) and denominator.
///
/// # Errors
///
/// As [`parts`].
pub fn digit_counts(value: &ExactAmount) -> Result<(u32, u32), EvaluationError> {
    let (numerator, denominator) = parts(value)?;
    Ok((digit_count(&numerator), digit_count(&denominator)))
}

/// The number of decimal digits of `value`'s magnitude; zero has one.
#[must_use]
pub fn digit_count(value: &BigInt) -> u32 {
    u32::try_from(value.magnitude().to_str_radix(10).len()).unwrap_or(u32::MAX)
}

/// The exact integer `value`, built from `i128` chunks with checked arithmetic.
///
/// # Errors
///
/// `precision_overflow` when the crate's budget refuses a step.
pub fn from_integer(value: &BigInt) -> Result<ExactAmount, EvaluationError> {
    let text = value.magnitude().to_str_radix(10);
    let mut total = ExactAmount::from_coefficient(0, 0);
    // `total = total × 10^width + chunk`, chunk by chunk from the most significant digits.
    for bytes in text.as_bytes().chunks(CHUNK_DIGITS) {
        let (Ok(width), Some(chunk)) = (
            u32::try_from(bytes.len()),
            std::str::from_utf8(bytes)
                .ok()
                .and_then(|digits| digits.parse::<i128>().ok()),
        ) else {
            return Err(EvaluationError::MoneyContractViolation {
                detail: format!("integer text {text:?} is not decimal digits"),
            });
        };
        let shift = ExactAmount::from_coefficient(10_i128.pow(width), 0);
        total = total
            .checked_mul(&shift)
            .and_then(|scaled| scaled.checked_add(&ExactAmount::from_coefficient(chunk, 0)))
            .map_err(|error| EvaluationError::from_money("integer", &error))?;
    }
    if value.sign() == Sign::Minus {
        total = ExactAmount::from_coefficient(0, 0)
            .checked_sub(&total)
            .map_err(|error| EvaluationError::from_money("integer", &error))?;
    }
    Ok(total)
}

/// The exact fraction `numerator / denominator`, reduced by the crate.
///
/// # Errors
///
/// `precision_overflow` when the crate's budget refuses a step, `money_contract_violation` for a
/// zero denominator (every caller checks it first).
pub fn from_parts(
    numerator: &BigInt,
    denominator: &BigInt,
) -> Result<ExactAmount, EvaluationError> {
    from_integer(numerator)?
        .checked_div(&from_integer(denominator)?)
        .map_err(|error| EvaluationError::from_money("fraction", &error))
}

/// The exact value of the decimal `digits × 10^-scale`.
///
/// # Errors
///
/// As [`from_integer`].
pub fn from_scaled(digits: &BigInt, scale: u8) -> Result<ExactAmount, EvaluationError> {
    from_integer(digits)?
        .checked_mul(&ExactAmount::from_coefficient(1, scale))
        .map_err(|error| EvaluationError::from_money("decimal", &error))
}

/// Parses unsigned decimal digits.
fn digits(text: &str) -> Option<BigInt> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    BigInt::parse_bytes(text.as_bytes(), 10)
}

/// Divides both by their greatest common divisor (Euclid).
fn reduce(numerator: BigInt, denominator: BigInt) -> (BigInt, BigInt) {
    if numerator.is_zero() {
        return (BigInt::zero(), BigInt::one());
    }
    let (mut a, mut b) = (numerator.abs(), denominator.abs());
    while !b.is_zero() {
        let rest = &a % &b;
        a = b;
        b = rest;
    }
    (numerator / &a, denominator / a)
}

#[cfg(test)]
#[path = "fraction_tests.rs"]
mod tests;
