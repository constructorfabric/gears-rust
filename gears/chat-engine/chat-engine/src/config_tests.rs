use super::*;

#[test]
fn defaults_validate() {
    let cfg = ChatEngineConfig::default();
    cfg.validate().expect("defaults must validate");
    assert!(cfg.plugin_deadline_secs > 0);
    assert!(cfg.ndjson_buffer_size > 0);
    assert!(cfg.summary_buffer_size > 0);
    assert!(cfg.retention_cleanup_interval_hours > 0);
}

#[test]
fn zero_buffer_rejected() {
    let cfg = ChatEngineConfig {
        ndjson_buffer_size: 0,
        ..Default::default()
    };
    let err = cfg.validate().unwrap_err();
    assert!(matches!(
        err,
        ConfigError::ZeroBufferSize {
            field: "ndjson_buffer_size"
        }
    ));
}

#[test]
fn zero_summary_buffer_rejected() {
    let cfg = ChatEngineConfig {
        summary_buffer_size: 0,
        ..Default::default()
    };
    let err = cfg.validate().unwrap_err();
    assert!(matches!(
        err,
        ConfigError::ZeroBufferSize {
            field: "summary_buffer_size"
        }
    ));
}

#[test]
fn zero_retention_interval_rejected() {
    let cfg = ChatEngineConfig {
        retention_cleanup_interval_hours: 0,
        ..Default::default()
    };
    let err = cfg.validate().unwrap_err();
    assert!(matches!(err, ConfigError::ZeroRetentionInterval));
}

#[test]
fn deserialise_empty_table_uses_defaults() {
    let cfg: ChatEngineConfig = serde_json::from_value(serde_json::json!({})).unwrap();
    cfg.validate().expect("empty config must use defaults");
}
