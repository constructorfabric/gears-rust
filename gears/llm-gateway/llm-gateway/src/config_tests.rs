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

// ── slice 5a: timeouts, quota, client credentials ───────────────────────────

mod quota {
    use std::time::Duration;

    use super::LlmGatewayConfig;
    use crate::config::{QuotaConfig, TimeoutsConfig};

    #[test]
    fn timeouts_and_quota_defaults_are_sensible() {
        let t = TimeoutsConfig::default();
        assert_eq!(t.provider_sync, Duration::from_mins(10));
        assert_eq!(t.stream_max_duration, Duration::from_mins(30));
        let q = QuotaConfig::default();
        assert_eq!(
            q.metric,
            "gts.cf.core.qe.metric_type.v1~cf.llmgw.gateway.ai_credits.v1"
        );
        assert_eq!(q.units_per_credit, 1_000_000);
        assert_eq!(q.lease_ttl_margin, Duration::from_secs(60));
        assert_eq!(q.fallback_max_output_tokens, 4_096);
        assert_eq!(q.settle_retry_backoff, Duration::from_secs(1));
        assert!(LlmGatewayConfig::default().client_credentials.is_none());
    }

    #[test]
    fn durations_deserialize_from_humantime_strings_and_keep_other_defaults() {
        let cfg: LlmGatewayConfig = serde_json::from_value(serde_json::json!({
            "timeouts": { "provider_sync": "30s" },
            "quota": { "lease_ttl_margin": "2m", "units_per_credit": 1000 },
            "client_credentials": { "client_id": "gw", "client_secret": "s", "scopes": ["a"] }
        }))
        .expect("deserializes");
        assert_eq!(cfg.timeouts.provider_sync, Duration::from_secs(30));
        assert_eq!(cfg.timeouts.stream_max_duration, Duration::from_mins(30));
        assert_eq!(cfg.quota.lease_ttl_margin, Duration::from_secs(120));
        assert_eq!(cfg.quota.units_per_credit, 1000);
        assert_eq!(cfg.quota.fallback_max_output_tokens, 4_096);
        let creds = cfg.client_credentials.expect("credentials");
        assert_eq!(creds.client_id, "gw");
        assert_eq!(creds.scopes, ["a"]);
    }

    #[test]
    fn unknown_keys_are_rejected_in_every_new_section() {
        for value in [
            serde_json::json!({ "timeouts": { "provider_syn": "1s" } }),
            serde_json::json!({ "quota": { "metrc": "x" } }),
            serde_json::json!({ "client_credentials": { "client_id": "a", "client_secret": "b", "x": 1 } }),
        ] {
            assert!(serde_json::from_value::<LlmGatewayConfig>(value).is_err());
        }
    }

    #[test]
    fn the_client_secret_is_redacted_in_debug() {
        let cfg: LlmGatewayConfig = serde_json::from_value(serde_json::json!({
            "client_credentials": { "client_id": "gw", "client_secret": "topsecret" }
        }))
        .expect("deserializes");
        assert!(!format!("{cfg:?}").contains("topsecret"));
    }
}

// ── validate() ──────────────────────────────────────────────────────────────

fn cfg(value: serde_json::Value) -> LlmGatewayConfig {
    serde_json::from_value(value).expect("config deserializes")
}

fn creds(extra: &serde_json::Value) -> serde_json::Value {
    let mut base = serde_json::json!({ "client_id": "gw", "client_secret": "s" });
    for (key, value) in extra.as_object().expect("object") {
        base[key] = value.clone();
    }
    serde_json::json!({ "client_credentials": base })
}

#[test]
fn the_defaults_with_credentials_are_valid() {
    cfg(creds(&serde_json::json!({})))
        .validate()
        .expect("valid");
    LlmGatewayConfig::default()
        .validate()
        .expect("defaults without credentials are valid here (module requires them)");
}

#[test]
fn exchange_defaults_are_one_second_interval_and_ten_second_timeout() {
    let cfg = cfg(creds(&serde_json::json!({})));
    let credentials = cfg.client_credentials.expect("credentials");
    assert_eq!(
        credentials.min_exchange_interval,
        std::time::Duration::from_secs(1)
    );
    assert_eq!(
        credentials.exchange_timeout,
        std::time::Duration::from_secs(10)
    );
}

#[test]
fn invalid_values_are_rejected_naming_the_key() {
    let cases = [
        (
            serde_json::json!({ "quota": { "units_per_credit": 0 } }),
            "quota.units_per_credit",
        ),
        (
            serde_json::json!({ "quota": { "settle_retry_backoff": "0s" } }),
            "quota.settle_retry_backoff",
        ),
        (
            serde_json::json!({ "quota": { "metric": " " } }),
            "quota.metric",
        ),
        (
            serde_json::json!({ "timeouts": { "provider_sync": "0s" } }),
            "timeouts.provider_sync",
        ),
        (
            serde_json::json!({ "timeouts": { "stream_max_duration": "0s" } }),
            "timeouts.stream_max_duration",
        ),
        (
            creds(&serde_json::json!({ "client_id": "" })),
            "client_credentials.client_id",
        ),
        (
            creds(&serde_json::json!({ "refresh_interval": "0s" })),
            "client_credentials.refresh_interval",
        ),
        (
            creds(&serde_json::json!({ "min_exchange_interval": "0s" })),
            "client_credentials.min_exchange_interval",
        ),
        (
            creds(&serde_json::json!({ "exchange_timeout": "0s" })),
            "client_credentials.exchange_timeout",
        ),
    ];
    for (value, key) in cases {
        let err = cfg(value).validate().expect_err(key);
        assert!(err.contains(key), "{err} should name {key}");
    }
}
