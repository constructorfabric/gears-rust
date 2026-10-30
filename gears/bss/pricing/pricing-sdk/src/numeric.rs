//! Numeric limits for pricing values, as a thin layer over [`bss_money`].
//!
//! Three kinds of value carry a limit:
//!
//! - A **rate** (`amount`, `rate`, `package_price`) is not negative and has at most
//!   [`RATE_FRACTIONAL_DIGITS`] fractional places.
//! - A **threshold** (`up_to`) is not negative and has at most [`QUANTITY_FRACTIONAL_DIGITS`]
//!   fractional places.
//! - A **positive quantity** (`package_size`, `NewSaleQuery.quantity`) is above zero and has at
//!   most [`QUANTITY_FRACTIONAL_DIGITS`] fractional places.
//!
//! All values are bound by `bss_money`: a normalized coefficient of at most 28 digits. A
//! parser reads the one plain decimal text format of `bss_money::parse_decimal`. A validator
//! checks a [`Decimal`] that the caller already holds. Both return the normalized value, so
//! `0.10` becomes `0.1`. "Fractional places" always means the places of the normalized value.
//! A minimum fee has no fixed limit: it follows the scale of its currency.
use bss_money::{CurrencySpec, MoneyError, parse_decimal, validate_amount};
use rust_decimal::Decimal;

/// Most fractional places a rate (amount, rate or package price) can have.
pub const RATE_FRACTIONAL_DIGITS: u32 = 12;
/// Most fractional places a threshold or a quantity can have: all the places a [`Decimal`] has.
pub const QUANTITY_FRACTIONAL_DIGITS: u32 = 28;

/// The most fractional places `bss_money::parse_decimal` reads from text.
const TEXT_PLACES: usize = QUANTITY_FRACTIONAL_DIGITS as usize;

/// Why a pricing numeric value was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum NumericError {
    /// The text is not a plain decimal. Exponents, a plus sign, spaces, `_`, leading zeros,
    /// negative zero, a missing digit around the point and text over 64 bytes are all refused.
    #[error("invalid decimal text")]
    InvalidText,
    /// The value is too big or too precise: more than 28 digits after normalization, or more
    /// fractional places than the value allows. A 29th fractional place in text is this error,
    /// not [`NumericError::InvalidText`], also when the text is longer than 64 bytes.
    #[error("decimal out of range")]
    OutOfRange,
    /// The value is below zero.
    #[error("negative value")]
    Negative,
    /// The value is zero where a value above zero is required.
    #[error("value must be positive")]
    NotPositive,
    /// The minimum fee has more fractional places than its currency allows.
    #[error("invalid minimum fee")]
    InvalidMinimumFee,
}

/// Parse a rate (`amount`, `rate` or `package_price`): not negative, at most
/// [`RATE_FRACTIONAL_DIGITS`] places.
///
/// # Errors
/// [`NumericError::InvalidText`] for text that is not a plain decimal,
/// [`NumericError::Negative`] below zero, [`NumericError::OutOfRange`] for too many digits or
/// more than 12 places.
pub fn parse_rate(text: &str) -> Result<Decimal, NumericError> {
    validate_rate(parse_text(text)?)
}

/// Parse a threshold (`up_to`): not negative, at most [`QUANTITY_FRACTIONAL_DIGITS`] places.
///
/// # Errors
/// [`NumericError::InvalidText`] for text that is not a plain decimal,
/// [`NumericError::Negative`] below zero, [`NumericError::OutOfRange`] for too many digits or
/// more than 28 places.
pub fn parse_threshold(text: &str) -> Result<Decimal, NumericError> {
    validate_threshold(parse_text(text)?)
}

/// Parse a positive quantity (`package_size`): above zero, at most
/// [`QUANTITY_FRACTIONAL_DIGITS`] places.
///
/// # Errors
/// [`NumericError::InvalidText`] for text that is not a plain decimal,
/// [`NumericError::Negative`] below zero, [`NumericError::NotPositive`] for zero,
/// [`NumericError::OutOfRange`] for too many digits or more than 28 places.
pub fn parse_positive_quantity(text: &str) -> Result<Decimal, NumericError> {
    validate_positive_quantity(parse_text(text)?)
}

/// Check a rate that the caller already holds as a [`Decimal`].
///
/// # Errors
/// [`NumericError::OutOfRange`] for more than 28 digits or more than 12 places,
/// [`NumericError::Negative`] below zero.
pub fn validate_rate(value: Decimal) -> Result<Decimal, NumericError> {
    let value = non_negative(normalize(value)?)?;
    with_places_up_to(value, RATE_FRACTIONAL_DIGITS)
}

/// Check a threshold that the caller already holds as a [`Decimal`].
///
/// # Errors
/// [`NumericError::OutOfRange`] for more than 28 digits or more than 28 places,
/// [`NumericError::Negative`] below zero.
pub fn validate_threshold(value: Decimal) -> Result<Decimal, NumericError> {
    let value = non_negative(normalize(value)?)?;
    with_places_up_to(value, QUANTITY_FRACTIONAL_DIGITS)
}

/// Check a quantity that the caller already holds as a [`Decimal`]; it must be above zero.
///
/// # Errors
/// [`NumericError::OutOfRange`] for more than 28 digits or more than 28 places,
/// [`NumericError::Negative`] below zero, [`NumericError::NotPositive`] for zero.
pub fn validate_positive_quantity(value: Decimal) -> Result<Decimal, NumericError> {
    let value = non_negative(normalize(value)?)?;
    if value.is_zero() {
        return Err(NumericError::NotPositive);
    }
    with_places_up_to(value, QUANTITY_FRACTIONAL_DIGITS)
}

/// Check a minimum fee: not negative, and no more places than the currency scale.
///
/// # Errors
/// [`NumericError::OutOfRange`] for more than 28 digits, [`NumericError::Negative`] below zero,
/// [`NumericError::InvalidMinimumFee`] for more places than `currency.scale()`.
pub fn validate_minimum_fee(
    value: Decimal,
    currency: &CurrencySpec,
) -> Result<Decimal, NumericError> {
    let value = non_negative(normalize(value)?)?;
    if value.scale() > u32::from(currency.scale()) {
        return Err(NumericError::InvalidMinimumFee);
    }
    Ok(value)
}

/// Read plain decimal text and map the `bss_money` refusals to [`NumericError`].
fn parse_text(text: &str) -> Result<Decimal, NumericError> {
    match parse_decimal(text) {
        Ok(value) => Ok(value),
        Err(MoneyError::AmountOutOfRange) => Err(NumericError::OutOfRange),
        Err(_) if has_excess_places(text) => Err(NumericError::OutOfRange),
        Err(_) => Err(NumericError::InvalidText),
    }
}

/// True when `text` is valid plain decimal text except for fractional places beyond the 28th.
///
/// `bss_money::parse_decimal` reports such text as a syntax error. Here it is a size error.
fn has_excess_places(text: &str) -> bool {
    // Trailing zeros are not places: `1.5000` has one.
    let trimmed = text.trim_end_matches('0');
    let Some((whole, fraction)) = trimmed.split_once('.') else {
        return false;
    };
    if fraction.len() <= TEXT_PLACES || !fraction.bytes().all(|byte| byte.is_ascii_digit()) {
        return false;
    }
    // Cut the fraction to 28 places and end it with a nonzero digit. The rest of the syntax
    // is then valid exactly when `parse_decimal` does not call it invalid. The nonzero digit
    // keeps a text such as `-0.000...1` from looking like the refused negative zero.
    let probe = format!("{whole}.{}1", &fraction[..TEXT_PLACES - 1]);
    matches!(
        parse_decimal(&probe),
        Ok(_) | Err(MoneyError::AmountOutOfRange)
    )
}

/// Normalize the value and enforce the 28-digit coefficient limit of `bss_money`.
fn normalize(value: Decimal) -> Result<Decimal, NumericError> {
    validate_amount(value).map_err(|error| match error {
        MoneyError::AmountOutOfRange => NumericError::OutOfRange,
        _ => NumericError::InvalidText,
    })
}

/// Refuse a value below zero. The value is normalized, so a negative zero is already zero.
fn non_negative(value: Decimal) -> Result<Decimal, NumericError> {
    if value.is_sign_negative() {
        return Err(NumericError::Negative);
    }
    Ok(value)
}

/// Refuse a normalized value with more than `max` fractional places.
fn with_places_up_to(value: Decimal, max: u32) -> Result<Decimal, NumericError> {
    if value.scale() > max {
        return Err(NumericError::OutOfRange);
    }
    Ok(value)
}

#[cfg(test)]
#[path = "numeric_tests.rs"]
mod tests;
