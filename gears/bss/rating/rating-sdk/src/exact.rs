//! Exact money and quantities as canonical text (DESIGN §3.3, T-D-66, T-D-80; money ADR rules 1, 6).
//!
//! Every numerator, denominator and quantity crosses the SDK serde, REST and event boundaries as a
//! canonical string: `-` only for a negative value and never `+`; no leading zeros except the `0`
//! before a decimal point; a decimal point only when a fractional part remains; no trailing
//! fractional zeros; no exponent or separators; every zero is `0`. Metadata integers such as
//! `currency_scale` stay JSON integers.
//!
//! Deserializers reject a JSON number in these fields with "the field requires a string", and reject
//! exponents, a leading `+`, leading zeros and `-0`. Fractional trailing zeros of a quantity are
//! accepted on input and dropped (`10.000` reads as `10`), as `bss_money::parse_decimal` does.
//!
//! The bounds are the delivery bounds of DESIGN §3.3: `abs(numerator) < 10^84`,
//! `1 <= denominator < 10^64`, and a quantity of at most 60 integer and 28 fractional digits. A value
//! beyond them is refused by name, never truncated. A fraction must be reduced, so one amount has
//! one text and one digest: `2/4` is refused, never rewritten to `1/2`. These types carry the wire
//! form only; exact arithmetic is `rating-core`'s (feature 02), which produces reduced fractions.

use std::fmt;

use bigdecimal::num_bigint::BigUint;
use bigdecimal::num_traits::{One, Zero};
use bss_money::{CurrencySpec, MoneyError};
use serde::de::{self, Deserializer, Visitor};
use serde::{Deserialize, Serialize, Serializer};

/// The most digits of a numerator: `abs(numerator) < 10^84`.
pub const MAX_NUMERATOR_DIGITS: usize = 84;
/// The most digits of a denominator: `denominator < 10^64`.
pub const MAX_DENOMINATOR_DIGITS: usize = 64;
/// The most integer digits of an `ExactQuantity` (`NUMERIC(88,28)`).
pub const MAX_QUANTITY_INTEGER_DIGITS: usize = 60;
/// The most fractional digits of an `ExactQuantity`.
pub const MAX_QUANTITY_FRACTION_DIGITS: usize = 28;

/// The longest input text any parser here looks at; longer input is refused before it is scanned.
const MAX_INPUT_LEN: usize = 256;

/// Why a value is not a canonical exact number of the Rating contract.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ExactNumberError {
    /// The text is not in canonical form (sign, leading zeros, exponent, separators, `-0`).
    #[error("{field}: not a canonical decimal string")]
    NotCanonical { field: &'static str },
    /// The value is outside the contract bound.
    #[error("{field}: outside the contract bound ({bound})")]
    OutOfRange {
        field: &'static str,
        bound: &'static str,
    },
    /// A fraction with a zero or negative denominator, a zero that is not `0/1`, or a fraction
    /// that is not reduced.
    #[error("{field}: not a fraction in lowest sign form ({reason})")]
    InvalidFraction {
        field: &'static str,
        reason: &'static str,
    },
    /// The currency code or scale is not one `bss_money::CurrencySpec` accepts.
    #[error("currency: {reason}")]
    InvalidCurrency { reason: &'static str },
}

impl ExactNumberError {
    /// The field the error is about, as it is named on the wire.
    #[must_use]
    pub fn field(&self) -> &'static str {
        match self {
            Self::NotCanonical { field }
            | Self::OutOfRange { field, .. }
            | Self::InvalidFraction { field, .. } => field,
            Self::InvalidCurrency { .. } => "currency",
        }
    }
}

/// A reduced fraction in **major** currency units with its currency and posting scale.
///
/// Wire form: `{"numerator": "1", "denominator": "200", "currency": "EUR", "currency_scale": 2}`
/// (`1/200` EUR). `currency_scale` is the posting increment Billing rounds to, never a limit on the
/// digits of a rate: `0.047` EUR is a valid amount at scale 2.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ExactMoney {
    numerator: String,
    denominator: String,
    currency: String,
    currency_scale: u8,
}

impl ExactMoney {
    /// Validates the canonical parts of an amount.
    ///
    /// # Errors
    ///
    /// [`ExactNumberError`] when the numerator or denominator is not a canonical integer string,
    /// leaves its bound, the denominator is not positive, a zero is not `0/1`, the fraction is not
    /// reduced, or the currency code or scale is invalid.
    pub fn new(
        numerator: &str,
        denominator: &str,
        currency: &str,
        currency_scale: u8,
    ) -> Result<Self, ExactNumberError> {
        let numerator_digits = canonical_integer("numerator", numerator)?;
        if numerator_digits > MAX_NUMERATOR_DIGITS {
            return Err(ExactNumberError::OutOfRange {
                field: "numerator",
                bound: "abs(numerator) < 10^84",
            });
        }
        if denominator.starts_with('-') || denominator == "0" {
            return Err(ExactNumberError::InvalidFraction {
                field: "denominator",
                reason: "the denominator must be positive",
            });
        }
        let denominator_digits = canonical_integer("denominator", denominator)?;
        if denominator_digits > MAX_DENOMINATOR_DIGITS {
            return Err(ExactNumberError::OutOfRange {
                field: "denominator",
                bound: "denominator < 10^64",
            });
        }
        if numerator == "0" && denominator != "1" {
            return Err(ExactNumberError::InvalidFraction {
                field: "denominator",
                reason: "zero is 0/1",
            });
        }
        if !is_reduced(numerator, denominator) {
            return Err(ExactNumberError::InvalidFraction {
                field: "denominator",
                reason: "the fraction is not reduced",
            });
        }
        validate_currency(currency, currency_scale)?;
        Ok(Self {
            numerator: numerator.to_owned(),
            denominator: denominator.to_owned(),
            currency: currency.to_owned(),
            currency_scale,
        })
    }

    /// The canonical numerator; the sign of the amount is here.
    #[must_use]
    pub fn numerator(&self) -> &str {
        &self.numerator
    }

    /// The canonical, positive denominator.
    #[must_use]
    pub fn denominator(&self) -> &str {
        &self.denominator
    }

    /// The currency code of the amount.
    #[must_use]
    pub fn currency(&self) -> &str {
        &self.currency
    }

    /// The posting scale of the binding of record the amount belongs to.
    #[must_use]
    pub fn currency_scale(&self) -> u8 {
        self.currency_scale
    }
}

/// A finite exact decimal quantity in canonical text, at most 60 integer and 28 fractional digits.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ExactQuantity(String);

impl ExactQuantity {
    /// Parses a decimal quantity and returns it in canonical form.
    ///
    /// Fractional trailing zeros are accepted and dropped; everything else that is not canonical
    /// (a leading `+`, leading zeros, an exponent, separators, `-0`) is refused.
    ///
    /// # Errors
    ///
    /// [`ExactNumberError::NotCanonical`] for malformed text and
    /// [`ExactNumberError::OutOfRange`] beyond 60 integer or 28 fractional digits.
    pub fn parse(text: &str) -> Result<Self, ExactNumberError> {
        const FIELD: &str = "quantity";
        if text.is_empty() || text.len() > MAX_INPUT_LEN {
            return Err(ExactNumberError::NotCanonical { field: FIELD });
        }
        let (negative, unsigned) = text
            .strip_prefix('-')
            .map_or((false, text), |rest| (true, rest));
        let (integer, fraction) = unsigned
            .split_once('.')
            .map_or((unsigned, None), |(i, f)| (i, Some(f)));
        if !is_canonical_magnitude(integer)
            || fraction.is_some_and(|f| f.is_empty() || !f.bytes().all(|b| b.is_ascii_digit()))
        {
            return Err(ExactNumberError::NotCanonical { field: FIELD });
        }
        let fraction = fraction.map_or("", |f| f.trim_end_matches('0'));
        if integer.len() > MAX_QUANTITY_INTEGER_DIGITS {
            return Err(ExactNumberError::OutOfRange {
                field: FIELD,
                bound: "at most 60 integer digits",
            });
        }
        if fraction.len() > MAX_QUANTITY_FRACTION_DIGITS {
            return Err(ExactNumberError::OutOfRange {
                field: FIELD,
                bound: "at most 28 fractional digits",
            });
        }
        if negative && integer == "0" && fraction.is_empty() {
            return Err(ExactNumberError::NotCanonical { field: FIELD });
        }
        let mut canonical = String::with_capacity(text.len());
        if negative {
            canonical.push('-');
        }
        canonical.push_str(integer);
        if !fraction.is_empty() {
            canonical.push('.');
            canonical.push_str(fraction);
        }
        Ok(Self(canonical))
    }

    /// The canonical text, which is also what every Rating digest hashes.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ExactQuantity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for ExactQuantity {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for ExactQuantity {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = deserializer.deserialize_any(TextOnly)?;
        Self::parse(&text).map_err(de::Error::custom)
    }
}

/// The wire shape of [`ExactMoney`] as it is read. Every field is required, and a member a later
/// V1 release adds is ignored: consumers of a Rating V1 contract ignore unknown fields (DESIGN
/// §3.3, T-D-71).
#[derive(Deserialize)]
struct ExactMoneyWire {
    #[serde(deserialize_with = "text_only")]
    numerator: String,
    #[serde(deserialize_with = "text_only")]
    denominator: String,
    currency: String,
    currency_scale: u8,
}

/// The wire shape of [`ExactMoney`] as it is written, borrowing the amount's parts.
#[derive(Serialize)]
struct ExactMoneyWireRef<'a> {
    numerator: &'a str,
    denominator: &'a str,
    currency: &'a str,
    currency_scale: u8,
}

impl Serialize for ExactMoney {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        ExactMoneyWireRef {
            numerator: &self.numerator,
            denominator: &self.denominator,
            currency: &self.currency,
            currency_scale: self.currency_scale,
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ExactMoney {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = ExactMoneyWire::deserialize(deserializer)?;
        Self::new(
            &wire.numerator,
            &wire.denominator,
            &wire.currency,
            wire.currency_scale,
        )
        .map_err(de::Error::custom)
    }
}

/// The message a JSON number in a money or quantity field is refused with.
pub const REQUIRES_STRING: &str = "the field requires a string";

fn text_only<'de, D: Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    deserializer.deserialize_any(TextOnly)
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

    fn visit_i64<E: de::Error>(self, _: i64) -> Result<String, E> {
        Err(E::custom(REQUIRES_STRING))
    }

    fn visit_u64<E: de::Error>(self, _: u64) -> Result<String, E> {
        Err(E::custom(REQUIRES_STRING))
    }

    fn visit_i128<E: de::Error>(self, _: i128) -> Result<String, E> {
        Err(E::custom(REQUIRES_STRING))
    }

    fn visit_u128<E: de::Error>(self, _: u128) -> Result<String, E> {
        Err(E::custom(REQUIRES_STRING))
    }

    fn visit_f64<E: de::Error>(self, _: f64) -> Result<String, E> {
        Err(E::custom(REQUIRES_STRING))
    }
}

/// Checks a canonical integer (`0` or `-?[1-9][0-9]*`) and returns its number of digits.
fn canonical_integer(field: &'static str, text: &str) -> Result<usize, ExactNumberError> {
    if text.is_empty() || text.len() > MAX_INPUT_LEN {
        return Err(ExactNumberError::NotCanonical { field });
    }
    let unsigned = text.strip_prefix('-').unwrap_or(text);
    if !is_canonical_magnitude(unsigned) || (unsigned == "0" && unsigned.len() != text.len()) {
        return Err(ExactNumberError::NotCanonical { field });
    }
    Ok(unsigned.len())
}

/// Whether `gcd(abs(numerator), denominator) == 1`, for canonical integers within their bounds.
fn is_reduced(numerator: &str, denominator: &str) -> bool {
    let magnitude = |digits: &str| BigUint::parse_bytes(digits.as_bytes(), 10);
    let (Some(mut a), Some(mut b)) = (
        magnitude(numerator.strip_prefix('-').unwrap_or(numerator)),
        magnitude(denominator),
    ) else {
        return false;
    };
    while !b.is_zero() {
        let rest = &a % &b;
        a = b;
        b = rest;
    }
    a.is_one()
}

/// `0` or a run of digits without a leading zero.
fn is_canonical_magnitude(digits: &str) -> bool {
    !digits.is_empty()
        && digits.bytes().all(|b| b.is_ascii_digit())
        && (digits == "0" || !digits.starts_with('0'))
}

/// The currency rules are `bss_money::CurrencySpec`'s, the one BSS money representation.
fn validate_currency(code: &str, scale: u8) -> Result<(), ExactNumberError> {
    if code.len() > MAX_INPUT_LEN {
        return Err(currency_refusal(&MoneyError::InvalidCurrency));
    }
    CurrencySpec::try_new(code.to_owned(), scale)
        .map(drop)
        .map_err(|err| currency_refusal(&err))
}

fn currency_refusal(err: &MoneyError) -> ExactNumberError {
    ExactNumberError::InvalidCurrency {
        reason: match err {
            MoneyError::ScaleOutOfRange => "the scale is out of range",
            _ => "the code is not a valid currency code",
        },
    }
}

#[cfg(test)]
#[path = "exact_tests.rs"]
mod tests;
