// Created: 2026-09-06 by Constructor Tech
//! The sample catalogue is well-formed before it ever reaches the reconciler.

use std::collections::HashSet;

use settings_service_sdk::catalogue::CATALOGUE;

fn declarations() -> Vec<settings_service_sdk::models::ContributedDeclaration> {
    super::declarations().expect("demo keys parse")
}

#[test]
fn every_declaration_has_a_distinct_key_under_the_demo_namespace() {
    let all = declarations();
    let keys: HashSet<String> = all.iter().map(|d| d.key.to_string()).collect();
    assert_eq!(
        keys.len(),
        all.len(),
        "duplicate keys in the demo catalogue"
    );
    for d in &all {
        assert!(
            d.key.as_str().contains("cf.settings_demo."),
            "{} is outside the demo namespace",
            d.key
        );
        assert_eq!(d.key.major(), 1);
    }
}

#[test]
fn every_value_type_is_one_the_sdk_ships() {
    let shipped: HashSet<&str> = CATALOGUE.iter().map(|t| t.id).collect();
    for d in declarations() {
        assert!(
            shipped.contains(d.value_type_id.as_str()),
            "{} names {} which the SDK does not ship",
            d.key,
            d.value_type_id
        );
    }
}

#[test]
fn the_demo_covers_the_whole_shipped_catalogue() {
    // One declaration per value type, so a running example server exercises
    // every validator branch there is.
    let used: HashSet<String> = declarations()
        .into_iter()
        .map(|d| d.value_type_id)
        .collect();
    for t in CATALOGUE {
        assert!(used.contains(t.id), "no demo declaration uses {}", t.id);
    }
}

#[test]
fn the_secret_default_is_the_empty_placeholder() {
    let token = declarations()
        .into_iter()
        .find(|d| d.key.leaf_slug() == "api_token")
        .expect("api_token is declared");
    assert_eq!(token.default_value, serde_json::json!(""));
}

#[test]
fn the_catalogue_size_is_pinned() {
    // The README states the number, and nothing else checks it: a declaration
    // added without updating the prose is the drift this pins.
    assert_eq!(
        declarations().len(),
        16,
        "the catalogue changed size; update README.md with it"
    );
}
