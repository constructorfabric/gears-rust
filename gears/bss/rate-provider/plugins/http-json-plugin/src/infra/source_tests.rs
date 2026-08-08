//! http-json mapping tests.

use bss_ledger_sdk::RateProviderError;
use serde_json::json;

use super::{json_lookup, map_json_document};
use crate::config::Mapping;

/// The `provider_id` the mapper stamps onto every rate under test.
const PROVIDER: &str = "http-json";

fn mapping() -> Mapping {
    Mapping {
        base: "USD".to_owned(),
        rates: "rates".to_owned(),
        rate: "value".to_owned(),
        as_of: "date".to_owned(),
    }
}

#[test]
fn dotted_lookup_walks_objects() {
    let value = json!({"a": {"b": {"c": 1}}});
    assert_eq!(json_lookup(&value, "a.b.c"), Some(&json!(1)));
    assert_eq!(json_lookup(&value, "a.x"), None);
}

#[test]
fn maps_entries_to_provider_rates() {
    let body = json!({
        "date": "2026-07-21T00:00:00Z",
        "rates": { "EUR": {"value": "0.92"}, "GBP": {"value": "0.78"} }
    });
    let rates = map_json_document(&body, &mapping(), PROVIDER).unwrap();
    assert_eq!(rates.len(), 2);
    let eur = rates.iter().find(|r| r.quote == "EUR").unwrap();
    assert_eq!(eur.base, "USD");
    assert_eq!(eur.rate_micro, 920_000);
}

#[test]
fn every_rate_is_stamped_with_the_serving_provider_id() {
    // Provenance is per-rate, so the ledger records the true upstream on every
    // stored row instead of reading back a racy `provider_id()`.
    let body = json!({
        "date": "2026-07-21T00:00:00Z",
        "rates": { "EUR": {"value": "0.92"} }
    });
    let rates = map_json_document(&body, &mapping(), "bank-x").unwrap();
    assert!(rates.iter().all(|r| r.provider == "bank-x"));
}

#[test]
fn zero_mappable_entries_is_internal_error() {
    let body = json!({ "date": "2026-07-21T00:00:00Z", "rates": {} });
    assert!(matches!(
        map_json_document(&body, &mapping(), PROVIDER),
        Err(RateProviderError::Internal(_))
    ));
}

#[test]
fn unmappable_entry_is_skipped_not_fatal() {
    let body = json!({
        "date": "2026-07-21T00:00:00Z",
        "rates": { "EUR": {"value": "0.92"}, "BAD": {"nope": 1} }
    });
    let rates = map_json_document(&body, &mapping(), PROVIDER).unwrap();
    assert_eq!(rates.len(), 1);
}

#[test]
fn all_entries_present_but_all_unmappable_is_internal_error() {
    // `rates` is non-empty but every entry fails to map, so the check must key off
    // "how many mapped", not "was the object non-empty".
    let body = json!({
        "date": "2026-07-21T00:00:00Z",
        "rates": { "EUR": {"nope": 1}, "GBP": {"value": "not-a-number"} }
    });
    assert!(matches!(
        map_json_document(&body, &mapping(), PROVIDER),
        Err(RateProviderError::Internal(_))
    ));
}

#[test]
fn numeric_rate_is_accepted_as_well_as_string() {
    let body = json!({
        "date": "2026-07-21T00:00:00Z",
        "rates": { "EUR": {"value": 0.92} }
    });
    let rates = map_json_document(&body, &mapping(), PROVIDER).unwrap();
    assert_eq!(rates[0].rate_micro, 920_000);
}

#[test]
fn a_rate_that_is_neither_string_nor_number_is_skipped() {
    // A distinct case from "no rate field": the field is present, so the mapping
    // still matches the feed's shape, but the value has a JSON type no rate can be
    // read out of. Skipping (not erroring) keeps one malformed currency from
    // costing the whole document.
    let body = json!({
        "date": "2026-07-21T00:00:00Z",
        "rates": {
            "EUR": {"value": "0.92"},
            "GBP": {"value": true},
            "CHF": {"value": {"nested": 1}},
            "JPY": {"value": ["160.85"]}
        }
    });
    let rates = map_json_document(&body, &mapping(), PROVIDER).unwrap();
    assert_eq!(rates.len(), 1, "only the readable rate survives");
    assert_eq!(rates[0].quote, "EUR");
}

#[test]
fn missing_as_of_path_is_internal_error() {
    // A regression that defaulted a missing `as_of` to `now()` would silently
    // mis-timestamp every rate and pass every other test in this file.
    let body = json!({ "rates": { "EUR": {"value": "0.92"} } });
    assert!(matches!(
        map_json_document(&body, &mapping(), PROVIDER),
        Err(RateProviderError::Internal(_))
    ));
}

#[test]
fn non_rfc3339_as_of_is_internal_error() {
    let body = json!({
        "date": "21 July 2026",
        "rates": { "EUR": {"value": "0.92"} }
    });
    assert!(matches!(
        map_json_document(&body, &mapping(), PROVIDER),
        Err(RateProviderError::Internal(_))
    ));
}

#[test]
fn non_string_as_of_is_internal_error() {
    let body = json!({
        "date": 1_753_056_000,
        "rates": { "EUR": {"value": "0.92"} }
    });
    assert!(matches!(
        map_json_document(&body, &mapping(), PROVIDER),
        Err(RateProviderError::Internal(_))
    ));
}

#[test]
fn missing_rates_path_is_internal_error() {
    let body = json!({ "date": "2026-07-21T00:00:00Z" });
    assert!(matches!(
        map_json_document(&body, &mapping(), PROVIDER),
        Err(RateProviderError::Internal(_))
    ));
}

#[test]
fn rates_path_that_is_not_an_object_is_internal_error() {
    let body = json!({ "date": "2026-07-21T00:00:00Z", "rates": ["EUR", "GBP"] });
    assert!(matches!(
        map_json_document(&body, &mapping(), PROVIDER),
        Err(RateProviderError::Internal(_))
    ));
}

#[test]
fn non_iso4217_shaped_quote_is_skipped_not_stored() {
    // The feed is a system boundary: an arbitrary string must never reach a
    // tenant's FX store as a currency identifier.
    let body = json!({
        "date": "2026-07-21T00:00:00Z",
        "rates": {
            "EUR": {"value": "0.92"},
            "NOT-A-CURRENCY": {"value": "1.0"},
            "E$R": {"value": "1.0"},
            "": {"value": "1.0"}
        }
    });
    let rates = map_json_document(&body, &mapping(), PROVIDER).unwrap();
    assert_eq!(rates.len(), 1, "only the well-formed quote survives");
    assert_eq!(rates[0].quote, "EUR");
}

#[test]
fn lowercase_quote_is_normalized_to_uppercase() {
    let body = json!({
        "date": "2026-07-21T00:00:00Z",
        "rates": { "eur": {"value": "0.92"} }
    });
    let rates = map_json_document(&body, &mapping(), PROVIDER).unwrap();
    assert_eq!(rates[0].quote, "EUR");
}

#[test]
fn misconfigured_base_currency_fails_the_whole_document() {
    // The base is operator config, not feed data — a malformed one would taint
    // every rate in the document, so it is fatal rather than per-entry skipped.
    let body = json!({
        "date": "2026-07-21T00:00:00Z",
        "rates": { "EUR": {"value": "0.92"} }
    });
    let bad_base = Mapping {
        base: "US DOLLAR".to_owned(),
        ..mapping()
    };
    assert!(matches!(
        map_json_document(&body, &bad_base, PROVIDER),
        Err(RateProviderError::Internal(_))
    ));
}

#[test]
fn lowercase_configured_base_is_normalized_to_uppercase() {
    let body = json!({
        "date": "2026-07-21T00:00:00Z",
        "rates": { "EUR": {"value": "0.92"} }
    });
    let lower_base = Mapping {
        base: "usd".to_owned(),
        ..mapping()
    };
    let rates = map_json_document(&body, &lower_base, PROVIDER).unwrap();
    assert_eq!(rates[0].base, "USD");
}
