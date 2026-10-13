//! Decimal-string money DTOs, independent of decimal Serde representations.

use bss_ledger_sdk::{CurrencySpec, MoneyError, PostedMoney, canonical_decimal, parse_decimal};

/// Money in major currency units with explicit currency and posting scale.
#[derive(Debug, Clone, PartialEq, Eq)]
#[toolkit_macros::api_dto(request, response)]
pub struct MoneyDto {
    /// Plain decimal text; requests may include fractional trailing zeros.
    #[schema(example = "12.34", max_length = 64)]
    pub amount: String,
    /// Currency code carried with every monetary value.
    pub currency: String,
    /// Stored posting scale, between 0 and 28 inclusive.
    pub currency_scale: u8,
}

impl TryFrom<MoneyDto> for PostedMoney {
    type Error = MoneyError;

    /// Parse and validate without rounding the input posting.
    ///
    /// # Errors
    /// Returns a money validation error for invalid text, metadata or increments.
    fn try_from(value: MoneyDto) -> Result<Self, Self::Error> {
        let currency = CurrencySpec::try_new(value.currency, value.currency_scale)?;
        Self::try_new(parse_decimal(&value.amount)?, currency)
    }
}

impl From<&PostedMoney> for MoneyDto {
    /// Emit canonical decimal text and preserve the stored currency metadata.
    fn from(value: &PostedMoney) -> Self {
        Self {
            amount: canonical_decimal(value.amount()),
            currency: value.currency().code().to_owned(),
            currency_scale: value.currency().scale(),
        }
    }
}

#[cfg(test)]
#[path = "money_sweep_tests.rs"]
mod sweep_tests;
