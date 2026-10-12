//! Serde adapters for a `Decimal` carried as canonical decimal text.
//!
//! `#[serde(with = "bss_money::serde_text")]` writes [`canonical_decimal`] and
//! accepts only text that parses back to the same canonical form, so a stored or
//! wire value has exactly one spelling. `serde_text::option` does the same for
//! `Option<Decimal>`.

use rust_decimal::Decimal;
use serde::{Deserialize, Deserializer, Serializer, de::Error as _};

use crate::{canonical_decimal, parse_decimal};

/// Serialize as canonical decimal text.
///
/// # Errors
/// A serialization error for a coefficient of more than 28 digits, which the
/// reader would refuse; otherwise the serializer's error.
pub fn serialize<S: Serializer>(value: &Decimal, serializer: S) -> Result<S::Ok, S::Error> {
    // Refuse what `deserialize` would refuse, so every written value reads back.
    crate::money::validate_amount(*value).map_err(serde::ser::Error::custom)?;
    serializer.serialize_str(&canonical_decimal(*value))
}

/// Deserialize canonical decimal text; a noncanonical spelling is an error.
///
/// # Errors
/// Returns a deserialization error for invalid, out-of-contract or noncanonical text.
pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Decimal, D::Error> {
    let text = String::deserialize(deserializer)?;
    let value = parse_decimal(&text).map_err(D::Error::custom)?;
    if canonical_decimal(value) != text {
        return Err(D::Error::custom("noncanonical exact decimal"));
    }
    Ok(value)
}

/// The same adapter for an optional decimal.
pub mod option {
    use rust_decimal::Decimal;
    use serde::{Deserialize, Deserializer, Serializer};

    #[derive(serde::Serialize, serde::Deserialize)]
    struct Exact(#[serde(with = "super")] Decimal);

    /// Serialize `None` as null, `Some` as canonical decimal text.
    ///
    /// # Errors
    /// Propagates the serializer's error.
    #[expect(
        clippy::ref_option,
        reason = "`#[serde(with)]` requires the `&Option<T>` signature"
    )]
    pub fn serialize<S: Serializer>(
        value: &Option<Decimal>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serde::Serialize::serialize(&value.map(Exact), serializer)
    }

    /// Deserialize null or canonical decimal text.
    ///
    /// # Errors
    /// Returns a deserialization error for invalid or noncanonical text.
    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<Decimal>, D::Error> {
        Ok(Option::<Exact>::deserialize(deserializer)?.map(|exact| exact.0))
    }
}

#[cfg(test)]
#[path = "serde_text_tests.rs"]
mod serde_text_tests;
