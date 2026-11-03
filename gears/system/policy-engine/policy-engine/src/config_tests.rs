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
    assert_eq!(config.evaluation_timeout, Duration::from_millis(5));
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
        rejected(|c| c.evaluation_timeout = Duration::ZERO),
        not_positive("evaluation_timeout")
    );
    assert_eq!(
        rejected(|c| c.hierarchy_timeout = Duration::ZERO),
        not_positive("hierarchy_timeout")
    );
    assert_eq!(
        rejected(|c| c.management_hierarchy_timeout = Duration::ZERO),
        not_positive("management_hierarchy_timeout")
    );
    assert_eq!(
        rejected(|c| c.registry_timeout = Duration::ZERO),
        not_positive("registry_timeout")
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

#[test]
fn builtin_policies_parse_from_yaml_and_become_named_documents() {
    let config: PolicyEngineConfig = serde_saphyr::from_str(
        r#"
builtin_policies:
  - id: "reserved-name-prefix"
    description: "Reserved names cannot be created"
    resource_types: ["gts.cf.core.*"]
    actions: ["create"]
    content: |
      package builtin
      deny if startswith(input.properties.name, "cf-")
  - id: "all-widgets"
    resource_types: ["gts.cf.core.example.widget.v1~"]
    content: "package p\ndeny := false"
"#,
    )
    .unwrap();
    config.validate().unwrap();
    let documents = config.builtin_documents();
    let names: Vec<_> = documents.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(names, ["reserved-name-prefix", "all-widgets"]);
    assert_eq!(documents[0].actions, ["create"]);
    assert_eq!(documents[0].resource_types, ["gts.cf.core.*"]);
    assert!(
        documents[1].actions.is_empty(),
        "actions default to every action"
    );
}

#[test]
fn builtin_policies_reject_unknown_keys_and_blank_ids() {
    let unknown = json!({ "builtin_policies": [
        { "id": "p", "resource_types": ["gts.cf.core.*"], "content": "", "priority": 1 }
    ] });
    assert!(serde_json::from_value::<PolicyEngineConfig>(unknown).is_err());

    let config: PolicyEngineConfig = serde_json::from_value(json!({ "builtin_policies": [
        { "id": "p", "resource_types": ["gts.cf.core.*"], "content": "" },
        { "id": " ", "resource_types": ["gts.cf.core.*"], "content": "" },
    ] }))
    .unwrap();
    assert_eq!(
        config.validate().unwrap_err(),
        ConfigError::BuiltinPolicyIdBlank { index: 1 }
    );
}

#[test]
fn oversized_bounds_are_rejected() {
    assert_eq!(
        rejected(|c| c.evaluation_timeout = Duration::from_secs(2)),
        ConfigError::TooLarge {
            field: "evaluation_timeout",
            max: "1s".to_owned()
        }
    );
    assert_eq!(
        rejected(|c| c.management_hierarchy_timeout = Duration::from_secs(61)),
        ConfigError::TooLarge {
            field: "management_hierarchy_timeout",
            max: "60s".to_owned()
        }
    );
    // Its own bound: the decision path's stays where it was.
    assert_eq!(
        rejected(|c| c.hierarchy_timeout = Duration::from_secs(11)),
        ConfigError::TooLarge {
            field: "hierarchy_timeout",
            max: "10s".to_owned()
        }
    );
    assert_eq!(
        rejected(|c| c.store_timeout = Duration::from_secs(11)),
        ConfigError::TooLarge {
            field: "store_timeout",
            max: "10s".to_owned()
        }
    );
    assert_eq!(
        rejected(|c| c.max_document_bytes = 2 * 1_048_576),
        ConfigError::TooLarge {
            field: "max_document_bytes",
            max: "1048576".to_owned()
        }
    );
    assert_eq!(
        rejected(|c| c.store_timeout = Duration::ZERO),
        not_positive("store_timeout")
    );
}
