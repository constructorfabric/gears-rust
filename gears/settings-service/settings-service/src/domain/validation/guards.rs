// Created: 2026-09-06 by Constructor Tech
// @cpt-dod:cpt-cf-settings-service-dod-typed-value-validation-size-cap:p1
// @cpt-dod:cpt-cf-settings-service-dod-typed-value-validation-canonicality:p1
//! The two guards every value passes before its type is consulted.
//!
//! Both exist for reasons that are easy to miss and expensive to discover
//! later. The size cap keeps the hot read cache, audit pre-images and
//! post-images, and validate-before-set report payloads bounded — a settings
//! value is a configuration datum, not a blob. The canonicality check rejects
//! numbers that a round trip through IEEE-754 binary64 does not return unchanged
//! in value, because the canonical encoding used downstream cannot carry them; a
//! setting needing more range or precision declares a string type instead.
//!
//! # Two entry points, one rule
//!
//! [`check`] inspects a parsed value. A parsed number has already been read into
//! a `u64`, an `i64` or an `f64`, so what remains detectable there is an
//! **integer** beyond the range a double represents exactly — the decimal
//! `0.30000000000000004440892098500626` has become `0.30000000000000004` before
//! this code sees it. [`check_text`] inspects the JSON **text** instead and sees
//! every literal as the caller wrote it; the write path runs it on the request
//! body before parsing, which is what closes that gap.

use serde_json::Value;

use super::FieldViolation;
use crate::field;

/// The largest serialized value the service stores.
pub const MAX_SERIALIZED_BYTES: usize = 64 * 1024;

/// The largest integer a binary64 double represents exactly, `2^53`.
const MAX_EXACT_INTEGER: u64 = 1 << 53;

/// Guard a parsed value: the size cap, then every number at every depth.
///
/// # Errors
/// The first fault found, since a value that fails a guard never reaches the
/// schema.
pub fn check(value: &Value) -> Result<(), FieldViolation> {
    // @cpt-begin:cpt-cf-settings-service-algo-typed-value-validation-guards:p1:inst-tvv-guard-1
    // @cpt-begin:cpt-cf-settings-service-algo-typed-value-validation-guards:p1:inst-tvv-guard-2
    let serialized = serde_json::to_vec(value).map_err(|e| FieldViolation {
        field: "value".to_owned(),
        code: field::VALIDATION,
        message: format!("value does not serialize: {e}"),
    })?;
    check_size(serialized.len())?;
    // @cpt-end:cpt-cf-settings-service-algo-typed-value-validation-guards:p1:inst-tvv-guard-2
    // @cpt-end:cpt-cf-settings-service-algo-typed-value-validation-guards:p1:inst-tvv-guard-1

    // @cpt-begin:cpt-cf-settings-service-algo-typed-value-validation-guards:p1:inst-tvv-guard-3
    // @cpt-begin:cpt-cf-settings-service-algo-typed-value-validation-guards:p1:inst-tvv-guard-4
    // @cpt-begin:cpt-cf-settings-service-algo-typed-value-validation-guards:p1:inst-tvv-guard-5
    check_numbers(value, "value")?;
    // @cpt-end:cpt-cf-settings-service-algo-typed-value-validation-guards:p1:inst-tvv-guard-5
    // @cpt-end:cpt-cf-settings-service-algo-typed-value-validation-guards:p1:inst-tvv-guard-4
    // @cpt-end:cpt-cf-settings-service-algo-typed-value-validation-guards:p1:inst-tvv-guard-3

    // @cpt-begin:cpt-cf-settings-service-algo-typed-value-validation-guards:p1:inst-tvv-guard-6
    Ok(())
    // @cpt-end:cpt-cf-settings-service-algo-typed-value-validation-guards:p1:inst-tvv-guard-6
}

/// Guard the JSON text of a value before it is parsed.
///
/// Runs the size cap on the text itself and the round-trip check on every
/// numeric literal as written, which is the only place a decimal finer than a
/// double resolves is still visible.
///
/// # Errors
/// The first fault found; malformed JSON is reported as a validation fault
/// rather than parsed leniently.
pub fn check_text(text: &str) -> Result<(), FieldViolation> {
    check_size(text.len())?;
    for (offset, literal) in number_literals(text) {
        if !round_trips(literal) {
            return Err(FieldViolation {
                field: "value".to_owned(),
                code: field::VALUE_NOT_CANONICAL,
                message: format!(
                    "number `{literal}` at byte {offset} does not survive a round trip through \
                     IEEE-754 binary64; declare a string type for wider range or finer precision"
                ),
            });
        }
    }
    Ok(())
}

fn check_size(bytes: usize) -> Result<(), FieldViolation> {
    if bytes > MAX_SERIALIZED_BYTES {
        return Err(FieldViolation {
            field: "value".to_owned(),
            code: field::VALUE_TOO_LARGE,
            message: format!(
                "serialized value is {bytes} bytes; the cap is {MAX_SERIALIZED_BYTES} — a settings \
                 value is a configuration datum, not a blob"
            ),
        });
    }
    Ok(())
}

/// Walk every number in a parsed value, naming the position of the first fault.
fn check_numbers(value: &Value, path: &str) -> Result<(), FieldViolation> {
    match value {
        Value::Number(n) => {
            let exact = n.as_u64().map_or_else(
                || {
                    n.as_i64()
                        .is_none_or(|i| i.unsigned_abs() <= MAX_EXACT_INTEGER)
                },
                |u| u <= MAX_EXACT_INTEGER,
            );
            if exact {
                Ok(())
            } else {
                Err(FieldViolation {
                    field: path.to_owned(),
                    code: field::VALUE_NOT_CANONICAL,
                    message: format!(
                        "integer {n} lies beyond ±2^53 and does not survive a round trip through \
                         IEEE-754 binary64; declare a string type for wider range"
                    ),
                })
            }
        }
        Value::Array(items) => items
            .iter()
            .enumerate()
            .try_for_each(|(i, item)| check_numbers(item, &format!("{path}/{i}"))),
        Value::Object(map) => map
            .iter()
            .try_for_each(|(k, v)| check_numbers(v, &format!("{path}/{k}"))),
        Value::Null | Value::Bool(_) | Value::String(_) => Ok(()),
    }
}

/// Whether a numeric literal, as written, denotes exactly the double it parses
/// to.
///
/// Integers are compared as integers against `2^53`. Decimals are parsed to a
/// double, printed back through the shortest-representation formatter, and the
/// two are compared as decimal numbers: `0.1` prints as `0.1` and passes, while
/// a literal carrying digits the double cannot hold prints shorter and fails.
fn round_trips(literal: &str) -> bool {
    let is_integer = !literal.bytes().any(|b| matches!(b, b'.' | b'e' | b'E'));
    if is_integer {
        let digits = literal.strip_prefix('-').unwrap_or(literal);
        return digits
            .parse::<u64>()
            .is_ok_and(|magnitude| magnitude <= MAX_EXACT_INTEGER);
    }
    let Ok(parsed) = literal.parse::<f64>() else {
        return false;
    };
    if !parsed.is_finite() {
        return false;
    }
    // The shortest string that parses back to `parsed`. Comparing the two as
    // decimal numbers rather than as strings tolerates spelling differences —
    // `1.0` against `1`, `1e2` against `100` — while still catching digits the
    // double lost.
    same_decimal_value(literal, &parsed.to_string())
}

/// Compare two decimal literals numerically, without going through a double.
fn same_decimal_value(a: &str, b: &str) -> bool {
    normalize_decimal(a) == normalize_decimal(b)
}

/// `(negative, integer digits, fraction digits)` with the exponent applied and
/// leading/trailing zeros removed, so equal values normalize identically.
fn normalize_decimal(literal: &str) -> Option<(bool, String, String)> {
    let (negative, rest) = match literal.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, literal),
    };
    let (mantissa, exponent) = match rest.find(['e', 'E']) {
        Some(i) => (&rest[..i], rest[i + 1..].parse::<i32>().ok()?),
        None => (rest, 0),
    };
    let (int_part, frac_part) = match mantissa.find('.') {
        Some(i) => (&mantissa[..i], &mantissa[i + 1..]),
        None => (mantissa, ""),
    };
    if !int_part.bytes().all(|b| b.is_ascii_digit())
        || !frac_part.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let mut digits = format!("{int_part}{frac_part}");
    let mut point = i64::try_from(int_part.len()).ok()? + i64::from(exponent);
    // Shift the decimal point by the exponent, padding with zeros as needed.
    if point < 0 {
        let pad = usize::try_from(point.unsigned_abs()).ok()?;
        digits = format!("{}{digits}", "0".repeat(pad));
        point = 0;
    }
    let point = usize::try_from(point).ok()?;
    while digits.len() < point {
        digits.push('0');
    }
    let (int_digits, frac_digits) = digits.split_at(point);
    let int_digits = int_digits.trim_start_matches('0');
    let frac_digits = frac_digits.trim_end_matches('0');
    let negative = negative && !(int_digits.is_empty() && frac_digits.is_empty());
    Some((negative, int_digits.to_owned(), frac_digits.to_owned()))
}

/// Every numeric literal in a JSON text with its byte offset, skipping strings.
fn number_literals(text: &str) -> Vec<(usize, &str)> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    let mut in_string = false;
    let mut escaped = false;
    while i < bytes.len() {
        let b = bytes[i];
        if in_string {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        if b == b'"' {
            in_string = true;
            i += 1;
            continue;
        }
        if b == b'-' || b.is_ascii_digit() {
            let start = i;
            i += 1;
            while i < bytes.len()
                && matches!(bytes[i], b'0'..=b'9' | b'.' | b'e' | b'E' | b'+' | b'-')
            {
                i += 1;
            }
            out.push((start, &text[start..i]));
            continue;
        }
        i += 1;
    }
    out
}

#[cfg(test)]
#[path = "guards_tests.rs"]
mod guards_tests;
