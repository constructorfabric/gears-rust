//! Request-limit configuration (DESIGN §4.5).

use super::{LimitsConfig, LlmGatewayConfig, StreamConfig};

#[test]
fn request_limit_defaults_are_sensible() {
    let limits = LimitsConfig::default();
    assert_eq!(limits.max_inflight_sync, 256);
    assert_eq!(limits.max_input_items, 1_000);
    assert_eq!(limits.max_tools, 128);
    assert_eq!(limits.max_schema_bytes, 262_144);
    assert_eq!(limits.max_embedding_inputs, 2_048);
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

#[test]
fn stream_defaults_are_sensible() {
    assert_eq!(LimitsConfig::default().max_inflight_streams, 1_024);
    assert_eq!(StreamConfig::default().channel_capacity, 64);
    assert_eq!(LlmGatewayConfig::default().stream.channel_capacity, 64);
}

#[test]
fn stream_settings_deserialize_and_keep_other_defaults() {
    let cfg: LlmGatewayConfig = serde_json::from_value(serde_json::json!({
        "limits": { "max_inflight_streams": 5 },
        "stream": { "channel_capacity": 8 }
    }))
    .expect("stream settings deserialize");
    assert_eq!(cfg.limits.max_inflight_streams, 5);
    assert_eq!(cfg.limits.max_inflight_sync, 256);
    assert_eq!(cfg.stream.channel_capacity, 8);
}

#[test]
fn unknown_stream_keys_are_rejected() {
    let result: Result<LlmGatewayConfig, _> =
        serde_json::from_value(serde_json::json!({ "stream": { "channel_capacty": 3 } }));
    assert!(result.is_err());
}

#[test]
fn max_embedding_inputs_deserializes_and_keeps_other_defaults() {
    let cfg: LlmGatewayConfig =
        serde_json::from_value(serde_json::json!({ "limits": { "max_embedding_inputs": 16 } }))
            .expect("deserializes");
    assert_eq!(cfg.limits.max_embedding_inputs, 16);
    assert_eq!(cfg.limits.max_inflight_sync, 256);
}
