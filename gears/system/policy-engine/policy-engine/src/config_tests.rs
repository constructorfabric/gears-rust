#![allow(clippy::unwrap_used, clippy::expect_used)]

use serde_json::json;

use super::*;

#[test]
fn defaults_are_valid() {
    PolicyEngineConfig::default().validate().unwrap();
}

#[test]
fn empty_and_partial_configs_take_defaults() {
    let config: PolicyEngineConfig = serde_json::from_value(json!({})).unwrap();
    assert_eq!(config, PolicyEngineConfig::default());
    let config: PolicyEngineConfig =
        serde_json::from_value(json!({ "compile_cache_capacity": 8 })).unwrap();
    assert_eq!(config.compile_cache_capacity, 8);
    assert_eq!(config.evaluation_timeout_ms, 5);
}

#[test]
fn unknown_keys_are_rejected() {
    for value in [
        json!({ "per_document_timeout_ms": 2 }),
        json!({ "engine_plugin": { "vendor": "v", "weight": 1 } }),
    ] {
        assert!(serde_json::from_value::<PolicyEngineConfig>(value).is_err());
    }
}

fn rejected(mutate: impl FnOnce(&mut PolicyEngineConfig)) -> ConfigError {
    let mut config = PolicyEngineConfig::default();
    mutate(&mut config);
    config.validate().unwrap_err()
}

fn not_positive(field: &'static str) -> ConfigError {
    ConfigError::NotPositive { field }
}

#[test]
fn zero_bounds_and_a_blank_vendor_are_rejected() {
    assert_eq!(
        rejected(|c| c.evaluation_timeout_ms = 0),
        not_positive("evaluation_timeout_ms")
    );
    assert_eq!(
        rejected(|c| c.hierarchy_timeout_ms = 0),
        not_positive("hierarchy_timeout_ms")
    );
    assert_eq!(
        rejected(|c| c.registry_timeout_ms = 0),
        not_positive("registry_timeout_ms")
    );
    assert_eq!(
        rejected(|c| c.compile_cache_capacity = 0),
        not_positive("compile_cache_capacity")
    );
    assert_eq!(
        rejected(|c| c.max_documents_per_version = 0),
        not_positive("max_documents_per_version")
    );
    assert_eq!(
        rejected(|c| c.max_document_bytes = 0),
        not_positive("max_document_bytes")
    );
    assert_eq!(
        rejected(|c| c.engine_plugin.vendor = "  ".to_owned()),
        ConfigError::EnginePluginVendorBlank
    );
}
