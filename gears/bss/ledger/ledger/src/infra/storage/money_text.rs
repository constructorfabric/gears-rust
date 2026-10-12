//! Canonical text storage for validated decimal money on both databases.

use bss_ledger_sdk::{CurrencySpec, PostedMoney, canonical_decimal, parse_decimal};

use crate::domain::model::RepoError;

/// Encode a validated posting in canonical decimal text.
#[must_use]
pub fn encode_amount(value: &PostedMoney) -> String {
    canonical_decimal(value.amount())
}

/// Restore stored money, rejecting corrupt text rather than normalizing it.
///
/// # Errors
/// Returns [`RepoError::InvalidStoredMoney`] for malformed, noncanonical or
/// out-of-contract amounts. Its diagnostic belongs only in server-side errors.
pub fn decode_amount(text: &str, currency: CurrencySpec) -> Result<PostedMoney, RepoError> {
    let amount = parse_decimal(text)
        .map_err(|error| RepoError::InvalidStoredMoney(format!("amount {text:?}: {error}")))?;
    if canonical_decimal(amount) != text {
        return Err(RepoError::InvalidStoredMoney(format!(
            "noncanonical amount {text:?}"
        )));
    }
    PostedMoney::try_new(amount, currency)
        .map_err(|error| RepoError::InvalidStoredMoney(format!("amount {text:?}: {error}")))
}

/// Restore currency metadata without consulting the live registry.
///
/// # Errors
/// Returns [`RepoError::InvalidStoredMoney`] for invalid codes or scales.
pub fn decode_currency(code: &str, scale: i16) -> Result<CurrencySpec, RepoError> {
    let scale = u8::try_from(scale)
        .map_err(|_| RepoError::InvalidStoredMoney(format!("invalid stored scale {scale}")))?;
    CurrencySpec::try_new(code.to_owned(), scale)
        .map_err(|error| RepoError::InvalidStoredMoney(format!("currency metadata: {error}")))
}

/// Restore a stored amount and its immutable currency metadata.
///
/// # Errors
/// Returns [`RepoError::InvalidStoredMoney`] for corrupt metadata or amount text.
pub fn decode_money(text: &str, code: &str, scale: i16) -> Result<PostedMoney, RepoError> {
    decode_amount(text, decode_currency(code, scale)?)
}

/// Restore an optional functional amount; all three columns must agree on nullability.
///
/// # Errors
/// Returns [`RepoError::InvalidStoredMoney`] for an incomplete triple or invalid money.
pub fn decode_optional_money(
    amount: Option<&str>,
    code: Option<&str>,
    scale: Option<i16>,
) -> Result<Option<PostedMoney>, RepoError> {
    match (amount, code, scale) {
        (None, None, None) => Ok(None),
        (Some(amount), Some(code), Some(scale)) => decode_money(amount, code, scale).map(Some),
        _ => Err(RepoError::InvalidStoredMoney(
            "incomplete functional money triple".to_owned(),
        )),
    }
}

/// Restore a positive canonical rate without interpreting currency scale as precision.
///
/// # Errors
/// Returns [`RepoError::InvalidStoredMoney`] for noncanonical, nonpositive or invalid text.
pub fn decode_rate(text: &str) -> Result<rust_decimal::Decimal, RepoError> {
    let amount = parse_decimal(text)
        .map_err(|error| RepoError::InvalidStoredMoney(format!("rate {text:?}: {error}")))?;
    if canonical_decimal(amount) != text || amount <= rust_decimal::Decimal::ZERO {
        return Err(RepoError::InvalidStoredMoney(format!(
            "rate {text:?}: noncanonical or not positive"
        )));
    }
    Ok(amount)
}

/// Money inside a persisted JSON payload (approval intents, queued operations,
/// threshold snapshots): canonical decimal text with its currency and stored
/// scale. The JSON shape matches the REST money object, so stored rows and their
/// dedup hashes are the same bytes, but storage no longer depends on the API layer.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StoredMoney {
    /// Canonical decimal text in major units.
    pub amount: String,
    /// Currency code.
    pub currency: String,
    /// Stored posting scale, 0 to 28.
    pub currency_scale: u8,
}

impl From<&PostedMoney> for StoredMoney {
    fn from(value: &PostedMoney) -> Self {
        Self {
            amount: canonical_decimal(value.amount()),
            currency: value.currency().code().to_owned(),
            currency_scale: value.currency().scale(),
        }
    }
}

impl TryFrom<StoredMoney> for PostedMoney {
    type Error = bss_ledger_sdk::MoneyError;

    /// Parse and validate without rounding.
    ///
    /// # Errors
    /// A money validation error for invalid text, metadata or increments.
    fn try_from(value: StoredMoney) -> Result<Self, Self::Error> {
        let currency = CurrencySpec::try_new(value.currency, value.currency_scale)?;
        Self::try_new(parse_decimal(&value.amount)?, currency)
    }
}

#[cfg(test)]
#[path = "money_text_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "money_text_roundtrip_tests.rs"]
mod roundtrip_tests;
