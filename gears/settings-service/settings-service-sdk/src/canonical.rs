// @cpt-dod:cpt-cf-settings-service-dod-consumer-activation-canonical:p1
// @cpt-dod:cpt-cf-settings-service-dod-consumer-activation-canonical-sdk:p1
//! The expected value's encoding: RFC 8785 canonical JSON, and the salted hash
//! of a `secret`-trait value.
//!
//! One implementation, shared by both sides: the service encodes the expected
//! value with it when a change commits, and the SDK encodes a consumer's
//! `applied_value` with it before reporting, so a report is compared with the
//! expectation byte for byte. It never normalizes a
//! number: the Type Validator keeps every number inside the binary64-lossless
//! domain on write, and a number outside it reaches the encoder only through a
//! defect, where it is refused rather than rounded.

use std::cmp::Ordering;

use secrecy::{ExposeSecret, SecretString};
use serde_json::{Number, Value};
use toolkit_security::crypto::hash::Sha256;
use zeroize::Zeroizing;

/// The literal that leads the array a secret's hash is taken over, so a hash
/// of this scheme cannot collide with another use of SHA-256 over a value.
///
/// Service-side helper, not part of the consumer contract.
#[doc(hidden)]
pub const SECRET_HASH_DOMAIN: &str = "cf.settings.activation.secret.v1";

/// A value the canonical encoding cannot carry, and the request field it came
/// from: [`Self::APPLIED_VALUE`] or [`Self::TENANT`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct EncodingError {
    field: &'static str,
    message: &'static str,
}

impl EncodingError {
    /// The field of a value the encoder refused.
    pub const APPLIED_VALUE: &'static str = "applied_value";
    /// The field of a tenant that is not a UUID.
    pub const TENANT: &'static str = "tenant";

    /// An error on `field` with `message`.
    #[must_use]
    pub const fn new(field: &'static str, message: &'static str) -> Self {
        Self { field, message }
    }

    /// The request field the error names.
    #[must_use]
    pub const fn field(&self) -> &'static str {
        self.field
    }

    /// The explanation, free of any value.
    #[must_use]
    pub const fn message(&self) -> &'static str {
        self.message
    }

    /// An applied value the encoder refused.
    const fn value(message: &'static str) -> Self {
        Self::new(Self::APPLIED_VALUE, message)
    }
}

/// The encoded expected value a row stores: canonical JSON text, or the
/// lowercase hex of a secret's hash.
///
/// Service-side helper, not part of the consumer contract.
#[doc(hidden)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpectedValue(String);

impl ExpectedValue {
    /// The text the row stores.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Into the text the row stores.
    #[must_use]
    pub fn into_string(self) -> String {
        self.0
    }

    /// The encoding of a non-secret value: its canonical JSON.
    ///
    /// # Errors
    /// [`EncodingError`] for a number outside the binary64-lossless domain.
    pub fn of_value(value: &Value) -> Result<Self, EncodingError> {
        // @cpt-begin:cpt-cf-settings-service-algo-consumer-activation-expected:p1:inst-ca-expected-1
        // @cpt-begin:cpt-cf-settings-service-algo-consumer-activation-expected:p1:inst-ca-expected-3
        // A `file-reference` value is an ordinary structured value: it carries a
        // version-pinned reference, never the file, and needs no hash.
        canonical_json(value).map(Self)
        // @cpt-end:cpt-cf-settings-service-algo-consumer-activation-expected:p1:inst-ca-expected-3
        // @cpt-end:cpt-cf-settings-service-algo-consumer-activation-expected:p1:inst-ca-expected-1
    }

    /// The encoding of a `secret`-trait value at one tenant: the salted hash of
    /// its plaintext.
    ///
    /// # Errors
    /// [`EncodingError`] only if the array cannot be encoded, which a string
    /// plaintext never causes.
    pub fn of_secret(
        key: &str,
        tenant: &str,
        plaintext: &SecretString,
    ) -> Result<Self, EncodingError> {
        secret_hash(key, tenant, plaintext).map(Self)
    }
}

/// The text of a secret value as the Credential Store holds it: a string as
/// itself, anything else as its JSON text. Both sides of a report hash this
/// text, so an inline plaintext, a fetched one and the consumer's agree.
///
/// A string is copied once at its exact length, straight into the redacting,
/// zeroizing wrapper. The request's own `Value` it is taken from, and the JSON
/// text a non-string value is written as (built by `serde_json` with its own
/// growth), are outside this function's reach and stay unwiped.
///
/// Service-side helper, not part of the consumer contract.
#[doc(hidden)]
#[must_use]
pub fn plaintext_text(value: &Value) -> SecretString {
    match value {
        Value::String(text) => SecretString::from(text.clone()),
        other => SecretString::from(other.to_string()),
    }
}

/// The salted hash of a secret value at one tenant: SHA-256, lowercase hex,
/// over the canonical encoding of the array of [`SECRET_HASH_DOMAIN`], the key,
/// the tenant as a bare lowercase UUID string and the plaintext.
///
/// The salt is the key and the tenant only, never anything of the change, so
/// one value hashes equal across the changes that replace one another on a row.
///
/// # Errors
/// Never in practice: an array of four strings always encodes. The `Result` is
/// kept so the signature stays that of the encoder it agrees with.
#[allow(clippy::unnecessary_wraps)] // the encoder's signature, which an array of strings never fails
pub fn secret_hash(
    key: &str,
    tenant: &str,
    plaintext: &SecretString,
) -> Result<String, EncodingError> {
    // @cpt-begin:cpt-cf-settings-service-algo-consumer-activation-expected:p1:inst-ca-expected-4
    // @cpt-begin:cpt-cf-settings-service-algo-consumer-activation-expected:p1:inst-ca-expected-5
    // The array is written straight through the string encoder, which is what
    // `canonical_json` does for an array of four strings, so the plaintext is
    // never held in a `serde_json::Value`; the buffer is sized for the worst
    // case (six bytes per escaped byte) so it never reallocates, leaving no
    // unwiped copy behind, and is wiped when it drops.
    let secret = plaintext.expose_secret();
    let worst = 6 * (SECRET_HASH_DOMAIN.len() + key.len() + tenant.len() + secret.len()) + 16;
    let mut canonical = Zeroizing::new(String::with_capacity(worst));
    canonical.push('[');
    for (index, part) in [SECRET_HASH_DOMAIN, key, tenant, secret]
        .into_iter()
        .enumerate()
    {
        if index > 0 {
            canonical.push(',');
        }
        encode_string(part, &mut canonical);
    }
    canonical.push(']');
    Ok(hex::encode(Sha256::digest(canonical.as_bytes())))
    // @cpt-end:cpt-cf-settings-service-algo-consumer-activation-expected:p1:inst-ca-expected-5
    // @cpt-end:cpt-cf-settings-service-algo-consumer-activation-expected:p1:inst-ca-expected-4
}

/// The RFC 8785 (JCS) encoding of `value`.
///
/// # Errors
/// [`EncodingError`] for an integer beyond 2^53 in magnitude or a number that
/// is not finite: neither survives a round trip through binary64.
pub fn canonical_json(value: &Value) -> Result<String, EncodingError> {
    let mut out = String::new();
    encode(value, &mut out)?;
    Ok(out)
}

fn encode(value: &Value, out: &mut String) -> Result<(), EncodingError> {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Number(number) => out.push_str(&number_text(number)?),
        Value::String(text) => encode_string(text, out),
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                encode(item, out)?;
            }
            out.push(']');
        }
        Value::Object(members) => {
            // Members sorted by the UTF-16 code units of their names, which is
            // not the byte order of the UTF-8 text for names outside the BMP.
            let mut sorted: Vec<(&String, &Value)> = members.iter().collect();
            sorted.sort_by(|(a, _), (b, _)| utf16_order(a, b));
            out.push('{');
            for (index, (name, member)) in sorted.into_iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                encode_string(name, out);
                out.push(':');
                encode(member, out)?;
            }
            out.push('}');
        }
    }
    Ok(())
}

/// `value` as `width` lowercase hex digits, appended to `out`.
fn push_hex(out: &mut String, value: u32, width: u32) {
    for shift in (0..width).rev() {
        let digit = (value >> (shift * 4)) & 0xf;
        out.push(char::from_digit(digit, 16).unwrap_or('0'));
    }
}

fn utf16_order(a: &str, b: &str) -> Ordering {
    a.encode_utf16().cmp(b.encode_utf16())
}

/// The string literal of RFC 8785 §3.2.2.2: only `"`, `\` and the control
/// characters are escaped — the short forms where JSON has one, `\u00xx` in
/// lowercase hex otherwise — and everything else is the character itself.
fn encode_string(text: &str, out: &mut String) {
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{08}' => out.push_str("\\b"),
            '\u{09}' => out.push_str("\\t"),
            '\u{0a}' => out.push_str("\\n"),
            '\u{0c}' => out.push_str("\\f"),
            '\u{0d}' => out.push_str("\\r"),
            c if u32::from(c) < 0x20 => {
                out.push_str("\\u");
                push_hex(out, u32::from(c), 4);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

fn number_text(number: &Number) -> Result<String, EncodingError> {
    // @cpt-begin:cpt-cf-settings-service-algo-consumer-activation-expected:p1:inst-ca-expected-2
    // The encoder never normalizes: an integer binary64 cannot hold exactly is
    // refused, not rounded to the nearest double.
    // Exactness, not magnitude: `1e16` and `10000000000000000` are the same
    // double and encode alike, whichever spelling the store kept.
    let exact = number
        .as_u64()
        .map(survives_binary64_u64)
        .or_else(|| number.as_i64().map(survives_binary64_i64))
        .unwrap_or(true);
    if !exact {
        return Err(EncodingError::value(
            "an integer binary64 does not hold exactly is not canonical",
        ));
    }
    let Some(double) = number.as_f64().filter(|d| d.is_finite()) else {
        return Err(EncodingError::value(
            "a number that is not finite is not canonical",
        ));
    };
    // @cpt-end:cpt-cf-settings-service-algo-consumer-activation-expected:p1:inst-ca-expected-2
    Ok(es6_number(double))
}

/// Whether an unsigned integer is the double it converts to. The float-to-int
/// cast saturates, so the top of the range (which rounds up to 2^64) is
/// excluded explicitly.
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
fn survives_binary64_u64(n: u64) -> bool {
    let double = n as f64;
    double as u128 == u128::from(n)
}

/// Whether a signed integer is the double it converts to.
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
fn survives_binary64_i64(n: i64) -> bool {
    let double = n as f64;
    double as i128 == i128::from(n)
}

/// A double as ECMAScript's `Number::toString` writes it, which is what RFC
/// 8785 §3.2.2.3 prescribes: the shortest digits that round-trip (from `ryu`),
/// laid out in fixed or exponential form by the exponent.
fn es6_number(double: f64) -> String {
    if double == 0.0 {
        // Both zeroes: `-0` is written `0`.
        return "0".to_owned();
    }
    let mut buffer = ryu::Buffer::new();
    let shortest = buffer.format_finite(double.abs());
    let (mantissa, exponent) = shortest
        .split_once('e')
        .map_or((shortest, 0_i32), |(m, e)| (m, e.parse().unwrap_or(0)));
    let (integral, fractional) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let mut digits = format!("{integral}{fractional}");
    // The decimal point sits after `point` digits of `digits`.
    let mut point = i32::try_from(integral.len()).unwrap_or(i32::MAX) + exponent;
    while digits.len() > 1 && digits.starts_with('0') {
        digits.remove(0);
        point -= 1;
    }
    while digits.len() > 1 && digits.ends_with('0') {
        digits.pop();
    }
    let k = i32::try_from(digits.len()).unwrap_or(i32::MAX);
    let n = point;
    let mut text = String::new();
    if double < 0.0 {
        text.push('-');
    }
    if k <= n && n <= 21 {
        text.push_str(&digits);
        text.extend(std::iter::repeat_n(
            '0',
            usize::try_from(n - k).unwrap_or(0),
        ));
    } else if 0 < n && n <= 21 {
        let at = usize::try_from(n).unwrap_or(0);
        text.push_str(&digits[..at]);
        text.push('.');
        text.push_str(&digits[at..]);
    } else if -6 < n && n <= 0 {
        text.push_str("0.");
        text.extend(std::iter::repeat_n('0', usize::try_from(-n).unwrap_or(0)));
        text.push_str(&digits);
    } else {
        let exp = n - 1;
        let sign = if exp < 0 { '-' } else { '+' };
        text.push_str(&digits[..1]);
        if digits.len() > 1 {
            text.push('.');
            text.push_str(&digits[1..]);
        }
        text.push('e');
        text.push(sign);
        text.push_str(&exp.abs().to_string());
    }
    text
}

#[cfg(test)]
#[path = "canonical_tests.rs"]
mod canonical_tests;
