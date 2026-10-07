//! Tenant dimension vocabulary validation.
use super::RuleError;
use std::collections::BTreeSet;

fn key_code(value: &str) -> bool {
    value.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}
fn value_code(value: &str) -> bool {
    value
        .as_bytes()
        .first()
        .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}
/// Whether `text` is spelled as a dimension value may be (lowercase letters, digits, `_`, `-`),
/// whether or not a registry holds it today: a resolve pin names a value that may have been
/// removed since (D-419).
#[must_use]
pub fn is_value(text: &str) -> bool {
    value_code(text)
}
/// The key every tenant's registry is seeded with (spec decision 4): declared, not yet valued.
pub const SEED_KEY: &str = "region";
/// Validate a registry entry. Removal of a used value is a door-level rule. A key may be
/// declared with no values yet; one value alone is not a dimension (`DIM_VALUES_FEW`).
#[must_use]
pub fn validate(key: &str, values: &[String]) -> Vec<RuleError> {
    let values: Vec<_> = values
        .iter()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .collect();
    let mut errors = Vec::new();
    if !key_code(key.trim()) {
        errors.push(RuleError::new("DIM_KEY_INVALID"));
    }
    if values.len() == 1 {
        errors.push(RuleError::new("DIM_VALUES_FEW"));
    }
    let mut seen = BTreeSet::new();
    for value in values {
        if !value_code(value) {
            errors.push(RuleError::new("DIM_VALUE_INVALID"));
        }
        if !seen.insert(value) {
            errors.push(RuleError::new("DIM_VALUE_DUPLICATE"));
        }
    }
    errors
}
#[cfg(test)]
#[path = "dimension_tests.rs"]
mod tests;
