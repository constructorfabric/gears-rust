use std::time::Duration;

use event_broker_sdk::backend::BackendError;
use serde_json::json;

use crate::settings::MemorySettings;

fn parse(value: serde_json::Value) -> Result<MemorySettings, BackendError> {
    let serde_json::Value::Object(map) = value else {
        panic!("settings fixture must be an object");
    };
    MemorySettings::parse(&map)
}

fn invalid_config_detail(result: Result<MemorySettings, BackendError>) -> String {
    match result {
        Err(BackendError::InvalidConfig { detail }) => detail,
        other => panic!("expected InvalidConfig, got {other:?}"),
    }
}

#[test]
fn empty_settings_take_the_defaults() {
    assert_eq!(
        parse(json!({})).unwrap(),
        MemorySettings {
            store: "default".to_owned(),
            retention_interval: Duration::from_secs(1),
        }
    );
}

#[test]
fn every_setting_is_read() {
    assert_eq!(
        parse(json!({ "store": "orders", "retention_interval_ms": 250 })).unwrap(),
        MemorySettings {
            store: "orders".to_owned(),
            retention_interval: Duration::from_millis(250),
        }
    );
}

#[test]
fn an_unknown_setting_is_named() {
    assert_eq!(
        invalid_config_detail(parse(json!({ "path": "/tmp/x" }))),
        "`path` is not a memory backend setting"
    );
}

#[test]
fn a_bad_value_is_refused_without_quoting_it() {
    assert_eq!(
        invalid_config_detail(parse(json!({ "store": "" }))),
        "`store` must be a non-empty string of at most 255 bytes"
    );
    assert_eq!(
        invalid_config_detail(parse(json!({ "retention_interval_ms": "secret-ish" }))),
        "`retention_interval_ms` must be a positive integer"
    );
    assert_eq!(
        invalid_config_detail(parse(json!({ "retention_interval_ms": 0 }))),
        "`retention_interval_ms` must be a positive integer"
    );
}
