#![allow(clippy::unwrap_used, clippy::expect_used)]

use serde_json::json;
use types_registry_sdk::testing::{MockTypesRegistryClient, make_test_type_schema};

use super::*;

const WIDGET: &str = "gts.cf.core.test.widget.v1~";

fn policy(content: &str) -> BuiltinPolicyConfig {
    BuiltinPolicyConfig {
        id: "p".to_owned(),
        description: None,
        resource_types: vec![WIDGET.to_owned()],
        actions: Vec::new(),
        content: content.to_owned(),
    }
}

fn config_with(policies: Vec<BuiltinPolicyConfig>) -> AdmissionControlConfig {
    AdmissionControlConfig {
        builtin_policies: policies,
        ..Default::default()
    }
}

#[test]
fn defaults_apply_and_unknown_keys_are_rejected() {
    let config: AdmissionControlConfig = serde_json::from_value(json!({})).unwrap();
    assert_eq!(config, AdmissionControlConfig::default());
    assert_eq!(config.engine_timeout_ms, 100);
    for unknown in [
        json!({ "admit_on_failure": true }),
        json!({ "engine": { "vendor": "v", "x": 1 } }),
    ] {
        assert!(serde_json::from_value::<AdmissionControlConfig>(unknown).is_err());
    }
    let nested = json!({ "builtin_policies": [{ "id": "p", "resource_types": [], "content": "", "backend": "x" }] });
    assert!(serde_json::from_value::<AdmissionControlConfig>(nested).is_err());
}

#[test]
fn valid_builtin_compiles() {
    let set = config_with(vec![policy("package a\ndeny := true")])
        .compile_builtins()
        .unwrap();
    assert_eq!(set.len(), 1);
}

#[test]
fn invalid_builtins_fail_startup() {
    let cases = [
        config_with(vec![policy("package a\ndeny := ")]),
        config_with(vec![policy("package a\nallow := true")]),
        config_with(vec![policy("package a\ndeny := time.now_ns() > 0")]),
        config_with(vec![
            policy("package a\ndeny := true"),
            policy("package b\ndeny := true"),
        ]),
        config_with(vec![BuiltinPolicyConfig {
            resource_types: Vec::new(),
            ..policy("package a\ndeny := true")
        }]),
        config_with(vec![BuiltinPolicyConfig {
            resource_types: vec!["not a gts id".to_owned()],
            ..policy("package a\ndeny := true")
        }]),
    ];
    for config in cases {
        assert!(config.compile_builtins().is_err(), "{config:?}");
    }
}

#[tokio::test]
async fn concrete_resource_types_must_resolve() {
    let set = config_with(vec![policy("package a\ndeny := true")])
        .compile_builtins()
        .unwrap();
    let known = MockTypesRegistryClient::new().with_type_schemas([make_test_type_schema(WIDGET)]);
    resolve_resource_types(&set, &known).await.unwrap();
    let empty = MockTypesRegistryClient::new();
    let err = resolve_resource_types(&set, &empty).await.unwrap_err();
    assert!(err.to_string().contains("not registered"), "{err}");

    let wildcard = BuiltinPolicyConfig {
        resource_types: vec!["gts.cf.core.*".to_owned()],
        ..policy("package a\ndeny := true")
    };
    let set = config_with(vec![wildcard]).compile_builtins().unwrap();
    resolve_resource_types(&set, &empty).await.unwrap();
}
