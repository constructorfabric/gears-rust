//! Tests for the ISO-4217 shape gate applied to feed-supplied currency codes.

use super::{is_iso4217_shaped, normalize_currency};

#[test]
fn accepts_canonical_three_letter_codes() {
    assert!(is_iso4217_shaped("USD"));
    assert!(is_iso4217_shaped("eur"));
    assert_eq!(normalize_currency("USD").as_deref(), Some("USD"));
}

#[test]
fn normalizes_case_so_the_store_never_holds_two_rows_per_currency() {
    assert_eq!(normalize_currency("usd").as_deref(), Some("USD"));
    assert_eq!(normalize_currency("uSd").as_deref(), Some("USD"));
}

#[test]
fn rejects_wrong_length() {
    for code in ["", "U", "US", "USDX"] {
        assert!(!is_iso4217_shaped(code), "{code} must be rejected");
        assert!(
            normalize_currency(code).is_none(),
            "{code} must be rejected"
        );
    }
}

#[test]
fn rejects_non_alphabetic_and_injection_shaped_input() {
    // The point of the gate: a compromised feed must not get arbitrary strings
    // into a tenant's persistent FX store as a currency identifier.
    for code in ["US1", "U$D", "US ", " US", "US\n", "US;", "'--"] {
        assert!(!is_iso4217_shaped(code), "{code:?} must be rejected");
        assert!(
            normalize_currency(code).is_none(),
            "{code:?} must be rejected"
        );
    }
}

#[test]
fn rejects_multibyte_input_that_is_three_bytes_but_not_three_letters() {
    // U+00E9 ("e" with acute) is 2 bytes in UTF-8, so this is 3 BYTES but only 2
    // chars — the byte-length check must not let it pass as a 3-letter code.
    // Escapes rather than literals: the workspace denies non-ASCII literals.
    assert!(!is_iso4217_shaped("\u{e9}a"));
    // U+20AC (euro sign) is 3 bytes, 1 char.
    assert!(!is_iso4217_shaped("\u{20ac}"));
}
