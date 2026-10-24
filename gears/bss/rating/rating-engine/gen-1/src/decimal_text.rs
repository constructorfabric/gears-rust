//! Serde for Pricing's decimals as canonical text (rating DESIGN §3.3, "Canonical text").
//!
//! The same rule as [`crate::exact::ExactQuantity`]: written as `bss_money`'s canonical decimal; read
//! only from a string, refusing a JSON number, an exponent, a leading `+`, leading zeros and `-0`,
//! and accepting fractional trailing zeros, which it drops (`0.0470` reads as `0.047`), as
//! `bss_money::parse_decimal` does. A value with more than 28 significant digits is refused both
//! ways, so every input the engine reads has a canonical text.

use std::fmt;

use rating_gen1_money::{canonical_decimal, parse_decimal, validate_amount};
use rust_decimal::Decimal;
use serde::Serializer;
use serde::de::{self, Deserializer, Visitor};

use crate::exact::REQUIRES_STRING;

/// Writes canonical decimal text.
///
/// # Errors
///
/// A serialization error for a value beyond 28 significant digits.
pub fn serialize<S: Serializer>(value: &Decimal, serializer: S) -> Result<S::Ok, S::Error> {
    let value = validate_amount(*value).map_err(serde::ser::Error::custom)?;
    serializer.serialize_str(&canonical_decimal(value))
}

/// Reads decimal text, dropping fractional trailing zeros.
///
/// # Errors
///
/// A deserialization error for a non-string, malformed text or a value beyond the bound.
pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Decimal, D::Error> {
    let text = deserializer.deserialize_any(TextOnly)?;
    parse_decimal(&text).map_err(de::Error::custom)
}

/// The same for an optional decimal: null, or decimal text.
pub mod option {
    use rust_decimal::Decimal;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    #[derive(Serialize, Deserialize)]
    struct Text(#[serde(with = "super")] Decimal);

    /// Writes null or canonical decimal text.
    ///
    /// # Errors
    ///
    /// As [`super::serialize`].
    #[expect(
        clippy::ref_option,
        reason = "`#[serde(with)]` requires the `&Option<T>` signature"
    )]
    pub fn serialize<S: Serializer>(
        value: &Option<Decimal>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        value.map(Text).serialize(serializer)
    }

    /// Reads null or decimal text.
    ///
    /// # Errors
    ///
    /// As [`super::deserialize`].
    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<Decimal>, D::Error> {
        Ok(Option::<Text>::deserialize(deserializer)?.map(|text| text.0))
    }
}

/// Accepts a string and refuses every other JSON type, a number in particular.
struct TextOnly;

impl Visitor<'_> for TextOnly {
    type Value = String;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(REQUIRES_STRING)
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<String, E> {
        Ok(value.to_owned())
    }

    fn visit_string<E: de::Error>(self, value: String) -> Result<String, E> {
        Ok(value)
    }
}

#[cfg(test)]
#[path = "decimal_text_tests.rs"]
mod tests;
