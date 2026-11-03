//! Request-limit configuration (DESIGN §4.5).

use super::{LimitsConfig, LlmGatewayConfig};

#[test]
fn request_limit_defaults_are_sensible() {
    let limits = LimitsConfig::default();
    assert_eq!(limits.max_inflight_sync, 256);
    assert_eq!(limits.max_input_items, 1_000);
    assert_eq!(limits.max_tools, 128);
    assert_eq!(limits.max_schema_bytes, 262_144);
}

#[test]
fn partial_limits_keep_the_other_defaults() {
    let cfg: LlmGatewayConfig =
        serde_json::from_value(serde_json::json!({ "limits": { "max_tools": 3 } }))
            .expect("partial limits deserialize");
    assert_eq!(cfg.limits.max_tools, 3);
    assert_eq!(cfg.limits.max_input_items, 1_000);
    assert_eq!(cfg.limits.max_schema_bytes, 262_144);
}

#[test]
fn unknown_limit_keys_are_rejected() {
    let result: Result<LlmGatewayConfig, _> =
        serde_json::from_value(serde_json::json!({ "limits": { "max_toolz": 3 } }));
    assert!(result.is_err());
}
