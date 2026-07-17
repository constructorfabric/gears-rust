//! http-json mapping tests.

use bss_ledger_sdk::RateProviderError;
use serde_json::json;

use super::{json_lookup, map_json_document};
use crate::config::Mapping;

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
    let rates = map_json_document(&body, &mapping()).unwrap();
    assert_eq!(rates.len(), 2);
    let eur = rates.iter().find(|r| r.quote == "EUR").unwrap();
    assert_eq!(eur.base, "USD");
    assert_eq!(eur.rate_micro, 920_000);
}

#[test]
fn zero_mappable_entries_is_internal_error() {
    let body = json!({ "date": "2026-07-21T00:00:00Z", "rates": {} });
    assert!(matches!(
        map_json_document(&body, &mapping()),
        Err(RateProviderError::Internal(_))
    ));
}

#[test]
fn unmappable_entry_is_skipped_not_fatal() {
    let body = json!({
        "date": "2026-07-21T00:00:00Z",
        "rates": { "EUR": {"value": "0.92"}, "BAD": {"nope": 1} }
    });
    let rates = map_json_document(&body, &mapping()).unwrap();
    assert_eq!(rates.len(), 1);
}

#[test]
fn all_entries_present_but_all_unmappable_is_internal_error() {
    // Distinct from `zero_mappable_entries_is_internal_error` (an empty `rates`
    // object): here `rates` is non-empty, but every entry individually fails
    // to map. The zero-successfully-mapped check must key off "how many
    // mapped," not "was the object non-empty."
    let body = json!({
        "date": "2026-07-21T00:00:00Z",
        "rates": { "EUR": {"nope": 1}, "GBP": {"value": "not-a-number"} }
    });
    assert!(matches!(
        map_json_document(&body, &mapping()),
        Err(RateProviderError::Internal(_))
    ));
}
