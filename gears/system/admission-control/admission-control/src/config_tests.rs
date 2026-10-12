#![allow(clippy::unwrap_used, clippy::expect_used)]

use serde_json::json;

use super::*;

/// Sets one numeric key of a config to zero.
type Zero = fn(&mut AdmissionControlConfig);

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
}

#[test]
fn zero_numeric_keys_are_rejected_naming_the_key() {
    assert!(AdmissionControlConfig::default().validate().is_ok());
    let zeroed: [(&str, Zero); 4] = [
        ("engine_timeout_ms", |c| c.engine_timeout_ms = 0),
        ("max_properties", |c| c.max_properties = 0),
        ("max_context_bytes", |c| c.max_context_bytes = 0),
        ("event_queue_capacity", |c| c.event_queue_capacity = 0),
    ];
    for (key, zero) in zeroed {
        let mut config = AdmissionControlConfig::default();
        zero(&mut config);
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains(key), "{key}: {err}");
    }
}
