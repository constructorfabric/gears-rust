//! Validated decimal money in major currency units, independent of REST and storage.

use rust_decimal::Decimal;
use thiserror::Error;

const COEFFICIENT_LIMIT: i128 = 10_i128.pow(28);
const MAX_DECIMAL_BYTES: usize = 64;

/// A failure to validate decimal money or combine its metadata.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum MoneyError {
    /// The input is not an exact plain decimal in the accepted text format.
    #[error("invalid decimal")]
    InvalidDecimal,
    /// Currency codes require 1–16 uppercase ASCII letters or digits.
    #[error("invalid currency")]
    InvalidCurrency,
    /// Currency scale must be between 0 and 28 inclusive.
    #[error("currency scale out of range")]
    ScaleOutOfRange,
    /// The normalized decimal coefficient has more than 28 digits.
    #[error("amount out of range")]
    AmountOutOfRange,
    /// The amount is not a multiple of the currency's posting increment.
    #[error("invalid posting increment")]
    InvalidPostingIncrement,
    /// The monetary values have different currencies.
    #[error("currency mismatch")]
    CurrencyMismatch,
    /// The monetary values have different stored currency scales.
    #[error("currency scale mismatch")]
    ScaleMismatch,
}

/// Currency code and stored posting scale, validated without consulting a registry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CurrencySpec {
    code: String,
    scale: u8,
}

impl CurrencySpec {
    /// Validate the code's shape and supported scale.
    ///
    /// The posting service must separately check the authoritative currency registry.
    /// Stored values can therefore be restored after registry changes.
    ///
    /// # Errors
    /// Returns [`MoneyError::InvalidCurrency`] for invalid codes or
    /// [`MoneyError::ScaleOutOfRange`] for scales above 28.
    pub fn try_new(code: String, scale: u8) -> Result<Self, MoneyError> {
        if code.is_empty()
            || code.len() > 16
            || !code
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
        {
            return Err(MoneyError::InvalidCurrency);
        }
        if scale > 28 {
            return Err(MoneyError::ScaleOutOfRange);
        }
        Ok(Self { code, scale })
    }

    /// Return the validated currency code.
    #[must_use]
    pub fn code(&self) -> &str {
        &self.code
    }

    /// Return the stored posting scale.
    #[must_use]
    pub fn scale(&self) -> u8 {
        self.scale
    }

    /// Require the same currency and scale: the one rule every gear applies
    /// before comparing or combining two amounts.
    ///
    /// # Errors
    /// [`MoneyError::CurrencyMismatch`] for a different code, otherwise
    /// [`MoneyError::ScaleMismatch`] for a different scale.
    pub fn ensure_same(&self, other: &Self) -> Result<(), MoneyError> {
        if self.code != other.code {
            return Err(MoneyError::CurrencyMismatch);
        }
        if self.scale != other.scale {
            return Err(MoneyError::ScaleMismatch);
        }
        Ok(())
    }
}

/// A bounded decimal posting amount with its currency and stored scale.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PostedMoney {
    amount: Decimal,
    currency: CurrencySpec,
}

impl PostedMoney {
    /// Validate a posting in major units without rounding or rescaling.
    ///
    /// # Errors
    /// Returns [`MoneyError::AmountOutOfRange`] when the normalized coefficient
    /// has more than 28 digits, or [`MoneyError::InvalidPostingIncrement`] when
    /// the amount has fractional digits beyond the currency scale.
    pub fn try_new(amount: Decimal, currency: CurrencySpec) -> Result<Self, MoneyError> {
        let amount = validate_amount(amount)?;
        if amount.scale() > u32::from(currency.scale()) {
            return Err(MoneyError::InvalidPostingIncrement);
        }
        Ok(Self { amount, currency })
    }

    /// Return the normalized amount in major units.
    #[must_use]
    pub fn amount(&self) -> Decimal {
        self.amount
    }

    /// Return the posting's currency and stored scale.
    #[must_use]
    pub fn currency(&self) -> &CurrencySpec {
        &self.currency
    }
}

impl std::fmt::Display for PostedMoney {
    /// Canonical decimal text and currency code, for messages: `12.34 EUR`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} {}",
            canonical_decimal(self.amount),
            self.currency.code
        )
    }
}

/// Parse at most 64 bytes of plain decimal text exactly and normalize it.
///
/// Fractional trailing zeros are accepted. Exponents, whitespace, `+`, leading
/// integer zeros and negative zero are rejected.
///
/// # Errors
/// Returns [`MoneyError::InvalidDecimal`] for invalid syntax, excessive input
/// length or more than 28 fractional places. Returns
/// [`MoneyError::AmountOutOfRange`] for a normalized coefficient with more than
/// 28 digits, whether or not the carrier could hold it.
pub fn parse_decimal(text: &str) -> Result<Decimal, MoneyError> {
    if text.is_empty() || text.len() > MAX_DECIMAL_BYTES {
        return Err(MoneyError::InvalidDecimal);
    }
    let unsigned = text.strip_prefix('-').unwrap_or(text);
    let (integer, fraction) = unsigned
        .split_once('.')
        .map_or((unsigned, None), |(i, f)| (i, Some(f)));
    if integer.is_empty()
        || !integer.bytes().all(|byte| byte.is_ascii_digit())
        || (integer.len() > 1 && integer.starts_with('0'))
        || fraction.is_some_and(|f| f.is_empty() || !f.bytes().all(|byte| byte.is_ascii_digit()))
    {
        return Err(MoneyError::InvalidDecimal);
    }
    // Strip only fractional zeros before exact parsing so harmless zero suffixes
    // do not exceed the carrier's scale or coefficient capacity.
    let exact_text = if fraction.is_some() {
        text.trim_end_matches('0').trim_end_matches('.')
    } else {
        text
    };
    let amount = Decimal::from_str_exact(exact_text).map_err(|_| {
        // The syntax is already valid here, so the carrier refused it for size.
        // Precision beyond 28 fractional places is not representable; anything
        // else is a coefficient with more than 28 digits, which is out of range.
        let fraction_digits = exact_text.split_once('.').map_or(0, |(_, f)| f.len());
        if fraction_digits > 28 {
            MoneyError::InvalidDecimal
        } else {
            MoneyError::AmountOutOfRange
        }
    })?;
    if text.starts_with('-') && amount.is_zero() {
        return Err(MoneyError::InvalidDecimal);
    }
    validate_amount(amount)
}

/// Format a decimal without exponents, fractional trailing zeros or negative zero.
#[must_use]
pub fn canonical_decimal(value: Decimal) -> String {
    value.normalize().to_string()
}

/// Normalize the decimal and enforce the 28-digit coefficient limit.
///
/// The bound check for a value already held as a [`Decimal`], without a
/// format-and-reparse round trip through [`parse_decimal`].
///
/// # Errors
/// Returns [`MoneyError::AmountOutOfRange`] when the normalized coefficient has
/// more than 28 digits.
pub fn validate_amount(value: Decimal) -> Result<Decimal, MoneyError> {
    let amount = value.normalize();
    if amount.mantissa().abs() >= COEFFICIENT_LIMIT {
        return Err(MoneyError::AmountOutOfRange);
    }
    Ok(amount)
}

#[cfg(test)]
#[path = "money_tests.rs"]
mod money_tests;
