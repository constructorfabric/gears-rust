//! Unit tests for [`PiiMinimizer`] (pure prohibited-field scanner). No database.

use super::PiiMinimizer;

/// A clean, internal-id-only payload flags NOTHING.
#[test]
fn clean_internal_payload_flags_nothing() {
    let v = serde_json::json!({
        "payer_tenant_id": "018f-…",
        "invoice_id": "inv-123",
        "amount_minor": 1000,
        "currency": "USD",
        "account_id": "018f-…",
    });
    assert!(
        PiiMinimizer::prohibited_fields(&v).is_empty(),
        "an internal-id-only payload must flag no prohibited fields"
    );
}

/// Each prohibited category is flagged by at least one common key spelling.
#[test]
fn flags_each_category_by_a_common_spelling() {
    assert_eq!(
        PiiMinimizer::prohibited_fields(&serde_json::json!({ "customer_name": "Ada" })),
        vec!["name"]
    );
    assert_eq!(
        PiiMinimizer::prohibited_fields(&serde_json::json!({ "email_address": "a@b.co" })),
        vec!["email"]
    );
    assert_eq!(
        PiiMinimizer::prohibited_fields(&serde_json::json!({ "msisdn": "+1555" })),
        vec!["phone"]
    );
    assert_eq!(
        PiiMinimizer::prohibited_fields(&serde_json::json!({ "pan": "4111…" })),
        vec!["payment_instrument"]
    );
    assert_eq!(
        PiiMinimizer::prohibited_fields(&serde_json::json!({ "iban": "DE89…" })),
        vec!["payment_instrument"]
    );
    assert_eq!(
        PiiMinimizer::prohibited_fields(&serde_json::json!({ "address_line1": "1 Main St" })),
        vec!["street_address"]
    );
}

/// Distinct categories are returned once each, in canonical order, regardless of
/// the input key order.
#[test]
fn distinct_categories_in_canonical_order() {
    let v = serde_json::json!({
        "street": "1 Main St",
        "card_number": "4111…",
        "phone": "+1555",
        "email": "a@b.co",
        "name": "Ada",
        // a second name-spelling must not duplicate the `name` category
        "full_name": "Ada L.",
    });
    assert_eq!(
        PiiMinimizer::prohibited_fields(&v),
        vec![
            "name",
            "email",
            "phone",
            "payment_instrument",
            "street_address"
        ],
        "categories de-duplicated and returned in canonical order"
    );
}

/// Keys are matched case-insensitively and at any nesting depth (nested object
/// AND array element).
#[test]
fn matches_nested_and_array_and_case_insensitively() {
    let v = serde_json::json!({
        "customer": { "Email": "a@b.co" },
        "contacts": [ { "Phone_Number": "+1555" } ],
        "internal_id": "ok",
    });
    assert_eq!(
        PiiMinimizer::prohibited_fields(&v),
        vec!["email", "phone"],
        "nested object + array element keys are flagged, case-insensitively"
    );
}

/// Only KEYS are matched, never values: a benign key whose value happens to be a
/// string like an email is NOT flagged.
#[test]
fn matches_keys_not_values() {
    let v = serde_json::json!({ "note": "contact a@b.co or call +1555" });
    assert!(
        PiiMinimizer::prohibited_fields(&v).is_empty(),
        "a benign key carrying PII-looking text in its VALUE is not flagged"
    );
}

// --- prohibited_in_values: VALUE-level free-text scan ---

/// A free-text string value carrying an email is flagged (the value scan sees
/// what the key-only scan cannot).
#[test]
fn value_scan_flags_email_in_free_text() {
    let v = serde_json::json!("paid by John Smith, john.smith@example.com, thanks");
    assert_eq!(PiiMinimizer::prohibited_in_values(&v), vec!["email"]);
}

/// A long digit run (phone / card / account) in a value is flagged as a payment
/// instrument, with separators (spaces / dashes / parentheses / `+`) ignored.
#[test]
fn value_scan_flags_long_number() {
    for s in [
        "call +1 415 555 2671 today", // 11 digits across separators
        "card 4111-1111-1111-1111",   // 16-digit card
        "iban-ish 12345678901",       // 11 bare digits
        "call 415-555-2671 asap",     // 10-digit national phone (dashed)
        "ring (212) 555-0147 pls",    // 10-digit national phone (parens)
        "num 4155552671 noted",       // 10 bare digits
    ] {
        let v = serde_json::json!(s);
        assert_eq!(
            PiiMinimizer::prohibited_in_values(&v),
            vec!["payment_instrument"],
            "{s:?} should flag a numeric PII run"
        );
    }
}

/// Benign free text — including a date and a short reference number — is NOT
/// flagged (the 10-digit floor keeps false positives low; ≤ 9-digit runs pass).
#[test]
fn value_scan_ignores_dates_and_short_numbers() {
    for s in [
        "reconciled on 2026-06-25",   // date = 8 digits
        "order PO-1234567 shipped",   // 7-digit reference
        "adjusted opening balance",   // no numbers at all
        "ratio 3.14 over 2 quarters", // tiny numbers
    ] {
        let v = serde_json::json!(s);
        assert!(
            PiiMinimizer::prohibited_in_values(&v).is_empty(),
            "{s:?} must NOT be flagged as PII"
        );
    }
}

/// The value scan descends nested objects and array elements, returning distinct
/// categories in canonical order (`email` before `payment_instrument`).
#[test]
fn value_scan_nested_and_distinct_order() {
    let v = serde_json::json!({
        "note": "reach me at ada@example.org",
        "history": ["ok", "fallback card 4111 1111 1111 1111"],
        "count": 3,
    });
    assert_eq!(
        PiiMinimizer::prohibited_in_values(&v),
        vec!["email", "payment_instrument"]
    );
}
