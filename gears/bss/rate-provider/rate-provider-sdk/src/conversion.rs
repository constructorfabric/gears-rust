//! Exact parsing of positive provider quotes in quote-major-units per base-major-unit.
//! Provider lexical forms are normalized exactly before enforcing the `bss-money` bounds.

use bss_ledger_sdk::RateProviderError;
use bss_money::{MoneyError, canonical_decimal, parse_decimal};
use rust_decimal::Decimal;

/// Parse a published quote exactly without scaling or rounding.
///
/// Preserve the previous exact parser's provider lexical forms (including outer
/// whitespace, plus signs, leading zeros and digit separators). The original
/// input is limited to 64 bytes before normalization; normalized values share
/// the ledger SDK's coefficient and scale bounds.
///
/// # Errors
/// Returns [`RateProviderError::Internal`] for invalid or out-of-contract decimal
/// text, or a quote that is zero or negative.
pub fn parse_rate(text: &str) -> Result<Decimal, RateProviderError> {
    if text.len() > 64 {
        return Err(RateProviderError::Internal(
            "provider quote exceeds the 64-byte input limit".to_owned(),
        ));
    }
    let text = text.trim();
    // The shared parser also accepts harmless fractional zero suffixes beyond the
    // carrier's scale. Fall back to the legacy exact parser for provider syntax,
    // then reparse its canonical value through the same bounds.
    let rate = parse_decimal(text)
        .or_else(|_| {
            let parsed = Decimal::from_str_exact(text).map_err(|_| MoneyError::InvalidDecimal)?;
            parse_decimal(&canonical_decimal(parsed))
        })
        .map_err(|error| {
            RateProviderError::Internal(format!(
                "provider quote {text:?} is not a bounded exact decimal: {error}"
            ))
        })?;
    if rate <= Decimal::ZERO {
        return Err(RateProviderError::Internal(format!(
            "provider quote {text:?} must be strictly positive"
        )));
    }
    Ok(rate)
}

#[cfg(test)]
#[path = "conversion_tests.rs"]
mod tests;
