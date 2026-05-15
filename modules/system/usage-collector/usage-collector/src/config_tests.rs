use std::time::Duration;

use usage_collector_sdk::UsageKind;

use super::{CircuitBreakerConfig, UsageCollectorConfig};

#[test]
fn test_validate_rejects_plugin_timeout_zero() {
    let cfg = UsageCollectorConfig {
        plugin_timeout: Duration::ZERO,
        ..UsageCollectorConfig::default()
    };
    let err = cfg.validate().unwrap_err();
    assert!(
        err.to_string().contains("plugin_timeout"),
        "error must mention plugin_timeout, got: {err}"
    );
}

#[test]
fn test_validate_rejects_plugin_timeout_above_30s() {
    let cfg = UsageCollectorConfig {
        plugin_timeout: Duration::from_secs(31),
        ..UsageCollectorConfig::default()
    };
    let err = cfg.validate().unwrap_err();
    assert!(
        err.to_string().contains("plugin_timeout"),
        "error must mention plugin_timeout, got: {err}"
    );
}

#[test]
fn test_validate_rejects_circuit_breaker_failure_threshold_zero() {
    let cfg = UsageCollectorConfig {
        circuit_breaker: CircuitBreakerConfig {
            failure_threshold: 0,
            ..CircuitBreakerConfig::default()
        },
        ..UsageCollectorConfig::default()
    };
    let err = cfg.validate().unwrap_err();
    assert!(
        err.to_string()
            .contains("circuit_breaker.failure_threshold"),
        "error must mention circuit_breaker.failure_threshold, got: {err}"
    );
}

#[test]
fn test_validate_rejects_circuit_breaker_failure_threshold_above_100() {
    let cfg = UsageCollectorConfig {
        circuit_breaker: CircuitBreakerConfig {
            failure_threshold: 101,
            ..CircuitBreakerConfig::default()
        },
        ..UsageCollectorConfig::default()
    };
    let err = cfg.validate().unwrap_err();
    assert!(
        err.to_string()
            .contains("circuit_breaker.failure_threshold"),
        "error must mention circuit_breaker.failure_threshold, got: {err}"
    );
}

#[test]
fn test_validate_rejects_plugin_timeout_below_100ms() {
    let cfg = UsageCollectorConfig {
        plugin_timeout: Duration::from_millis(50),
        ..UsageCollectorConfig::default()
    };
    let err = cfg.validate().unwrap_err();
    assert!(
        err.to_string().contains("plugin_timeout"),
        "error must mention plugin_timeout, got: {err}"
    );
}

#[test]
fn test_validate_rejects_circuit_breaker_window_below_100ms() {
    let cfg = UsageCollectorConfig {
        circuit_breaker: CircuitBreakerConfig {
            window: Duration::ZERO,
            ..CircuitBreakerConfig::default()
        },
        ..UsageCollectorConfig::default()
    };
    let err = cfg.validate().unwrap_err();
    assert!(
        err.to_string().contains("circuit_breaker.window"),
        "error must mention circuit_breaker.window, got: {err}"
    );
}

#[test]
fn test_validate_rejects_circuit_breaker_recovery_timeout_zero() {
    let cfg = UsageCollectorConfig {
        circuit_breaker: CircuitBreakerConfig {
            recovery_timeout: Duration::ZERO,
            ..CircuitBreakerConfig::default()
        },
        ..UsageCollectorConfig::default()
    };
    let err = cfg.validate().unwrap_err();
    assert!(
        err.to_string().contains("circuit_breaker.recovery_timeout"),
        "error must mention circuit_breaker.recovery_timeout, got: {err}"
    );
}

#[test]
fn test_validate_rejects_circuit_breaker_recovery_timeout_above_5min() {
    let cfg = UsageCollectorConfig {
        circuit_breaker: CircuitBreakerConfig {
            recovery_timeout: Duration::from_secs(301),
            ..CircuitBreakerConfig::default()
        },
        ..UsageCollectorConfig::default()
    };
    let err = cfg.validate().unwrap_err();
    assert!(
        err.to_string().contains("circuit_breaker.recovery_timeout"),
        "error must mention circuit_breaker.recovery_timeout, got: {err}"
    );
}

#[test]
fn test_validate_accepts_defaults() {
    let cfg = UsageCollectorConfig::default();
    assert!(cfg.validate().is_ok());
}

#[test]
fn vendor_can_be_overridden_via_serde() {
    let json = r#"{"vendor": "acme"}"#;
    let cfg: UsageCollectorConfig = serde_json::from_str(json).unwrap();
    assert_eq!(cfg.vendor, "acme");
}

#[test]
fn serde_default_applies_default_vendor() {
    let cfg: UsageCollectorConfig = serde_json::from_str("{}").unwrap();
    assert_eq!(
        cfg.vendor, "cyberfabric",
        "serde(default) must use Default impl"
    );
}

#[test]
fn plugin_timeout_can_be_overridden_via_serde() {
    let json = r#"{"plugin_timeout": "10s"}"#;
    let cfg: UsageCollectorConfig = serde_json::from_str(json).unwrap();
    assert_eq!(cfg.plugin_timeout, Duration::from_secs(10));
}

#[test]
fn serde_default_applies_default_plugin_timeout() {
    let cfg: UsageCollectorConfig = serde_json::from_str("{}").unwrap();
    assert_eq!(
        cfg.plugin_timeout,
        Duration::from_secs(5),
        "serde(default) must use Default impl"
    );
}

#[test]
fn rejects_unknown_fields() {
    let json = r#"{"vendor": "x", "unexpected": true}"#;
    assert!(serde_json::from_str::<UsageCollectorConfig>(json).is_err());
}

#[test]
fn metrics_config_parses_with_modules() {
    let json = r#"{"metrics": {"cpu.usage": {"kind": "gauge", "modules": ["mod-a"]}}}"#;
    let cfg: UsageCollectorConfig = serde_json::from_str(json).unwrap();
    let m = &cfg.metrics["cpu.usage"];
    assert!(matches!(m.kind, UsageKind::Gauge));
    assert_eq!(m.modules.as_deref(), Some(["mod-a".to_owned()].as_slice()));
}

#[test]
fn metrics_config_parses_without_modules() {
    let json = r#"{"metrics": {"req.count": {"kind": "counter"}}}"#;
    let cfg: UsageCollectorConfig = serde_json::from_str(json).unwrap();
    let m = &cfg.metrics["req.count"];
    assert!(matches!(m.kind, UsageKind::Counter));
    assert!(m.modules.is_none());
}

#[test]
fn omitted_max_metadata_bytes_deserializes_to_default() {
    // Exercises the `#[serde(default)]` wiring on `UsageCollectorConfig`:
    // deployments that omit `max_metadata_bytes` must inherit the Default impl
    // rather than failing to parse or silently picking up serde's `0`.
    let cfg: UsageCollectorConfig = serde_json::from_str("{}").unwrap();
    assert_eq!(
        cfg.max_metadata_bytes,
        UsageCollectorConfig::default().max_metadata_bytes,
        "missing `max_metadata_bytes` should fall back to the struct default"
    );
    // And the deserialized config must still validate, so the default
    // remains within the bounds enforced by `validate()`.
    cfg.validate()
        .expect("default config must satisfy validate()");
}

#[test]
fn test_validate_accepts_max_metadata_bytes_boundary_values() {
    for value in [0u32, 1, 8192, 1_048_576] {
        let cfg = UsageCollectorConfig {
            max_metadata_bytes: value,
            ..UsageCollectorConfig::default()
        };
        assert!(
            cfg.validate().is_ok(),
            "validate() must accept max_metadata_bytes = {value}"
        );
    }
}

#[test]
fn test_validate_rejects_max_metadata_bytes_above_1_mib() {
    let cfg = UsageCollectorConfig {
        max_metadata_bytes: 1_048_577,
        ..UsageCollectorConfig::default()
    };
    let err = cfg.validate().unwrap_err();
    assert_eq!(
        err.to_string(),
        "max_metadata_bytes must not exceed 1 MiB (1_048_576 bytes)"
    );
}
