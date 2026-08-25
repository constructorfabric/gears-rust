//! ISO-4217 currency-code shape validation at the feed boundary. Shared by every
//! rate-provider source plugin.
//!
//! A remote feed's currency identifiers end up as `ledger_fx_rate.base_currency` /
//! `.quote_currency` in every tenant's store, so a compromised or misconfigured
//! upstream must not be able to inject arbitrary strings there. The feed response
//! is a system boundary: rates and timestamps are already validated, and these
//! helpers close the same gap for the codes themselves.
//!
//! Shape-only, deliberately: this checks the ISO-4217 *form* (three ASCII
//! letters), not membership of the current ISO-4217 register. Validating against
//! a hardcoded code list would reject legitimately-published new or exotic codes
//! the moment the register moves ahead of our copy of it, which for a fetch-only
//! adapter is a worse failure than passing an unknown-but-well-formed code
//! through to the ledger.

/// True when `code` has the ISO-4217 shape: exactly three ASCII letters.
#[must_use]
pub fn is_iso4217_shaped(code: &str) -> bool {
    code.len() == 3 && code.bytes().all(|b| b.is_ascii_alphabetic())
}

/// Normalize a feed-supplied currency code to canonical uppercase ISO-4217,
/// or `None` when it is not ISO-4217-shaped.
///
/// Uppercasing keeps the stored code canonical regardless of how the upstream
/// cased it, so the ledger never ends up with both `"usd"` and `"USD"` rows for
/// the same currency.
#[must_use]
pub fn normalize_currency(code: &str) -> Option<String> {
    is_iso4217_shaped(code).then(|| code.to_ascii_uppercase())
}

#[cfg(test)]
#[path = "currency_tests.rs"]
mod tests;
