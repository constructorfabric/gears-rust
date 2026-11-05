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

// ── slice 5b: usage delivery and shutdown ───────────────────────────────────

mod usage {
    use std::time::Duration;

    use super::{LlmGatewayConfig, cfg};
    use crate::config::{ShutdownConfig, UsageConfig};

    #[test]
    fn usage_and_shutdown_defaults_are_sensible() {
        let u = UsageConfig::default();
        assert_eq!(
            u.usage_type,
            "gts.cf.core.uc.usage_record.v1~cf.llmgw.gateway.ai_credits.v1"
        );
        assert_eq!(u.buffer_capacity, 10_000);
        assert_eq!(u.batch_size, 100);
        assert_eq!(u.flush_interval, Duration::from_secs(1));
        assert_eq!(u.retry_backoff, Duration::from_secs(1));
        assert_eq!(u.max_internal_retries, 3);
        assert_eq!(
            ShutdownConfig::default().usage_drain_reserve,
            Duration::from_secs(5)
        );
        LlmGatewayConfig::default()
            .validate()
            .expect("the defaults are valid");
    }

    #[test]
    fn usage_settings_deserialize_and_keep_other_defaults() {
        let c = cfg(serde_json::json!({
            "usage": { "batch_size": 10, "flush_interval": "250ms", "buffer_capacity": 50 },
            "shutdown": { "usage_drain_reserve": "8s" }
        }));
        assert_eq!(c.usage.batch_size, 10);
        assert_eq!(c.usage.flush_interval, Duration::from_millis(250));
        assert_eq!(c.usage.buffer_capacity, 50);
        assert_eq!(c.usage.retry_backoff, Duration::from_secs(1));
        assert_eq!(c.shutdown.usage_drain_reserve, Duration::from_secs(8));
        c.validate().expect("valid");
    }

    #[test]
    fn zero_internal_retries_is_allowed_and_means_no_retry() {
        let c = cfg(serde_json::json!({ "usage": { "max_internal_retries": 0 } }));
        assert_eq!(c.usage.max_internal_retries, 0);
        c.validate().expect("0 disables internal retries");
        let c = cfg(serde_json::json!({ "usage": { "max_internal_retries": 7 } }));
        assert_eq!(c.usage.max_internal_retries, 7);
        c.validate().expect("valid");
    }

    #[test]
    fn unknown_usage_and_shutdown_keys_are_rejected() {
        for value in [
            serde_json::json!({ "usage": { "batch_sizee": 1 } }),
            serde_json::json!({ "shutdown": { "usage_drain": "1s" } }),
        ] {
            assert!(serde_json::from_value::<LlmGatewayConfig>(value).is_err());
        }
    }

    #[test]
    fn invalid_usage_values_are_rejected_naming_the_key() {
        let cases = [
            (
                serde_json::json!({ "usage": { "usage_type": " " } }),
                "usage.usage_type",
            ),
            (
                // A quota metric id is not a usage record derived id.
                serde_json::json!({ "usage": {
                    "usage_type": "gts.cf.core.qe.metric_type.v1~cf.llmgw.gateway.ai_credits.v1"
                } }),
                "usage.usage_type",
            ),
            (
                serde_json::json!({ "usage": { "usage_type": "not a gts id" } }),
                "usage.usage_type",
            ),
            (
                serde_json::json!({ "usage": { "buffer_capacity": 0 } }),
                "usage.buffer_capacity",
            ),
            (
                serde_json::json!({ "usage": { "batch_size": 0 } }),
                "usage.batch_size",
            ),
            (
                // Usage Collector caps one batch at 100 records.
                serde_json::json!({ "usage": { "batch_size": 101 } }),
                "usage.batch_size",
            ),
            (
                serde_json::json!({ "usage": { "flush_interval": "0s" } }),
                "usage.flush_interval",
            ),
            (
                serde_json::json!({ "usage": { "retry_backoff": "0s" } }),
                "usage.retry_backoff",
            ),
            (
                serde_json::json!({ "shutdown": { "usage_drain_reserve": "0s" } }),
                "shutdown.usage_drain_reserve",
            ),
            (
                // Leaves in-flight requests less than the stop timeout minus
                // the margin and the maximum reserve.
                serde_json::json!({ "shutdown": { "usage_drain_reserve": "21s" } }),
                "shutdown.usage_drain_reserve",
            ),
        ];
        for (value, key) in cases {
            let err = cfg(value).validate().expect_err(key);
            assert!(err.contains(key), "{err} should name {key}");
        }
    }

    #[test]
    fn the_largest_drain_reserve_leaves_a_quiescence_budget_inside_the_stop_timeout() {
        let c = cfg(serde_json::json!({ "shutdown": { "usage_drain_reserve": "20s" } }));
        c.validate().expect("20s is the largest valid reserve");
        let budget = c.shutdown.quiescence_budget();
        assert_eq!(budget, Duration::from_secs(7));
        assert!(budget + c.shutdown.usage_drain_reserve < crate::module::GEAR_STOP_TIMEOUT);
        assert_eq!(
            ShutdownConfig::default().quiescence_budget(),
            Duration::from_secs(22)
        );
    }

    #[test]
    fn the_gear_lifecycle_uses_the_stop_timeout_constant() {
        assert_eq!(
            crate::module::LlmGatewayModule::default().stop_timeout(),
            crate::module::GEAR_STOP_TIMEOUT
        );
        assert_eq!(crate::module::GEAR_STOP_TIMEOUT, Duration::from_secs(30));
    }

    #[test]
    fn a_batch_of_exactly_the_collector_cap_is_valid() {
        cfg(serde_json::json!({ "usage": { "batch_size": 100 } }))
            .validate()
            .expect("100 is the cap, not over it");
    }
}

// ── media (DESIGN §4.5, `media.*`, `limits.max_inline_media_bytes`) ─────────

use super::MediaConfig;
use std::time::Duration;

const MIB: usize = 1024 * 1024;

#[allow(clippy::needless_pass_by_value)] // call sites build the value inline with `json!`
fn media_cfg(media: serde_json::Value) -> LlmGatewayConfig {
    serde_json::from_value(serde_json::json!({ "media": media })).expect("media config parses")
}

#[test]
fn media_defaults_are_sensible() {
    let media = MediaConfig::default();
    assert_eq!(media.fetch_timeout, Duration::from_secs(10));
    assert_eq!(media.total_timeout, Duration::from_secs(30));
    assert_eq!(media.max_bytes, 20 * MIB);
    assert_eq!(LimitsConfig::default().max_media_fetches, 16);
    assert_eq!(
        media.allowed_image_types,
        ["image/png", "image/jpeg", "image/gif", "image/webp"]
    );
    assert_eq!(media.allowed_file_types, ["application/pdf", "text/*"]);
    assert_eq!(LimitsConfig::default().max_inline_media_bytes, 20 * MIB);
    assert_eq!(LlmGatewayConfig::default().media.max_bytes, 20 * MIB);
    LlmGatewayConfig::default()
        .validate()
        .expect("defaults validate");
}

#[test]
fn media_settings_deserialize_and_keep_other_defaults() {
    let cfg: LlmGatewayConfig = serde_json::from_value(serde_json::json!({
        "limits": { "max_inline_media_bytes": 1024, "max_media_fetches": 4 },
        "media": {
            "fetch_timeout": "3s",
            "total_timeout": "45s",
            "max_bytes": 2048,
            "allowed_image_types": ["image/png"]
        }
    }))
    .expect("media settings deserialize");
    assert_eq!(cfg.limits.max_inline_media_bytes, 1024);
    assert_eq!(cfg.limits.max_media_fetches, 4);
    assert_eq!(cfg.media.total_timeout, Duration::from_secs(45));
    assert_eq!(cfg.limits.max_tools, 128);
    assert_eq!(cfg.media.fetch_timeout, Duration::from_secs(3));
    assert_eq!(cfg.media.max_bytes, 2048);
    assert_eq!(cfg.media.allowed_image_types, ["image/png"]);
    assert_eq!(
        cfg.media.allowed_file_types,
        MediaConfig::default().allowed_file_types
    );
    cfg.validate().expect("valid");
}

#[test]
fn unknown_media_keys_are_rejected() {
    let result: Result<LlmGatewayConfig, _> =
        serde_json::from_value(serde_json::json!({ "media": { "max_byte": 3 } }));
    assert!(result.is_err());
}

#[test]
fn invalid_media_values_are_rejected_naming_the_key() {
    let cases = [
        (
            serde_json::json!({ "media": { "fetch_timeout": "0s" } }),
            "media.fetch_timeout",
        ),
        (
            serde_json::json!({ "media": { "max_bytes": 0 } }),
            "media.max_bytes",
        ),
        (
            serde_json::json!({ "media": { "total_timeout": "0s" } }),
            "media.total_timeout",
        ),
        (
            serde_json::json!({ "media": { "fetch_timeout": "20s", "total_timeout": "19s" } }),
            "media.total_timeout",
        ),
        (
            serde_json::json!({ "limits": { "max_media_fetches": 0 } }),
            "limits.max_media_fetches",
        ),
        (
            serde_json::json!({ "media": { "allowed_image_types": [] } }),
            "media.allowed_image_types",
        ),
        (
            serde_json::json!({ "media": { "allowed_file_types": [] } }),
            "media.allowed_file_types",
        ),
        (
            serde_json::json!({ "media": { "allowed_image_types": ["png"] } }),
            "media.allowed_image_types",
        ),
        (
            serde_json::json!({ "media": { "allowed_image_types": ["image/"] } }),
            "media.allowed_image_types",
        ),
        (
            serde_json::json!({ "media": { "allowed_image_types": ["*/*"] } }),
            "media.allowed_image_types",
        ),
        (
            serde_json::json!({ "media": { "allowed_image_types": ["Image/PNG"] } }),
            "media.allowed_image_types",
        ),
        (
            serde_json::json!({ "media": { "allowed_image_types": ["image/png; q=1"] } }),
            "media.allowed_image_types",
        ),
        (
            serde_json::json!({ "media": { "allowed_file_types": [""] } }),
            "media.allowed_file_types",
        ),
        (
            serde_json::json!({ "limits": { "max_inline_media_bytes": 0 } }),
            "limits.max_inline_media_bytes",
        ),
    ];
    for (value, key) in cases {
        let cfg: LlmGatewayConfig = serde_json::from_value(value.clone()).expect("parses");
        let err = cfg.validate().expect_err(&format!("{value} must fail"));
        assert!(err.contains(key), "{value}: {err}");
    }
}

#[test]
fn well_formed_media_type_lists_are_accepted() {
    let cfg = media_cfg(serde_json::json!({
        "allowed_image_types": ["image/*", "image/svg+xml"],
        "allowed_file_types": ["application/pdf", "application/vnd.ms-excel", "text/*"]
    }));
    cfg.validate().expect("valid lists");
}

// ── slice 7: stream idle, dependency timeouts, drain deadline ───────────────

mod slice7 {
    use std::time::Duration;

    use super::{LlmGatewayConfig, cfg};
    use crate::config::{DependencyTimeouts, ShutdownConfig};

    const NAMES: [&str; 5] = [
        "authz_resolver",
        "model_registry",
        "types_registry",
        "quota_enforcement",
        "usage_collector",
    ];

    #[test]
    fn stream_idle_defaults_to_two_minutes_and_max_duration_stays_thirty_minutes() {
        let c = LlmGatewayConfig::default();
        assert_eq!(c.timeouts.stream_idle, Duration::from_secs(120));
        assert_eq!(c.timeouts.stream_max_duration, Duration::from_mins(30));
    }

    #[test]
    fn every_dependency_timeout_defaults_to_five_seconds() {
        let d = LlmGatewayConfig::default().timeouts.dependency;
        for (name, timeout) in d.named() {
            assert_eq!(timeout, Duration::from_secs(5), "{name}");
        }
        assert_eq!(d.named().map(|(name, _)| name), NAMES);
        assert_eq!(
            DependencyTimeouts::default().authz_resolver,
            Duration::from_secs(5)
        );
    }

    #[test]
    fn the_new_timeouts_deserialize_from_humantime_and_keep_the_other_defaults() {
        let c = cfg(serde_json::json!({
            "timeouts": {
                "stream_idle": "45s",
                "dependency": { "model_registry": "1500ms", "usage_collector": "2s" }
            }
        }));
        assert_eq!(c.timeouts.stream_idle, Duration::from_secs(45));
        let d = &c.timeouts.dependency;
        assert_eq!(d.model_registry, Duration::from_millis(1500));
        assert_eq!(d.usage_collector, Duration::from_secs(2));
        assert_eq!(d.authz_resolver, Duration::from_secs(5));
        assert_eq!(d.quota_enforcement, Duration::from_secs(5));
        assert_eq!(c.timeouts.provider_sync, Duration::from_secs(600));
        c.validate().expect("valid");
    }

    #[test]
    fn unknown_dependency_timeout_keys_are_rejected() {
        let err = serde_json::from_value::<LlmGatewayConfig>(
            serde_json::json!({ "timeouts": { "dependency": { "credstore": "1s" } } }),
        );
        assert!(err.is_err(), "credstore has no timeout while keyless");
    }

    #[test]
    fn zero_values_are_rejected_naming_the_key() {
        let mut cases = vec![(
            serde_json::json!({ "timeouts": { "stream_idle": "0s" } }),
            "timeouts.stream_idle".to_owned(),
        )];
        for name in NAMES {
            cases.push((
                serde_json::json!({ "timeouts": { "dependency": { (name): "0s" } } }),
                format!("timeouts.dependency.{name}"),
            ));
        }
        for (value, key) in cases {
            let err = cfg(value).validate().expect_err(&key);
            assert!(err.contains(&key), "{err} should name {key}");
        }
    }

    #[test]
    fn stream_idle_above_the_maximum_duration_is_rejected_naming_the_key() {
        let err = cfg(serde_json::json!({
            "timeouts": { "stream_idle": "10m", "stream_max_duration": "5m" }
        }))
        .validate()
        .expect_err("idle above max");
        assert!(err.contains("timeouts.stream_idle"), "{err}");
        // Equal is fine.
        cfg(serde_json::json!({
            "timeouts": { "stream_idle": "5m", "stream_max_duration": "5m" }
        }))
        .validate()
        .expect("equal bounds");
    }

    #[test]
    fn the_drain_deadline_precedes_the_end_of_the_quiescence_budget() {
        // The cut requests need time to settle and hand their usage over
        // before the Usage Reporter stops waiting.
        let defaults = ShutdownConfig::default();
        assert_eq!(defaults.quiescence_budget(), Duration::from_secs(22));
        assert_eq!(defaults.cut_after(), Duration::from_secs(20));
        let widest = cfg(serde_json::json!({ "shutdown": { "usage_drain_reserve": "20s" } }));
        assert!(widest.shutdown.cut_after() < widest.shutdown.quiescence_budget());
        assert_eq!(widest.shutdown.cut_after(), Duration::from_secs(5));
    }
}
