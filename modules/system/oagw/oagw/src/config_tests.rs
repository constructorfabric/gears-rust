use super::*;

#[test]
fn debug_shows_timeout_and_body_size() {
    let config = OagwConfig::default();
    let debug_output = format!("{config:?}");
    assert!(debug_output.contains("proxy_timeout_secs"));
    assert!(debug_output.contains("max_body_size_bytes"));
}

#[test]
fn token_cache_ttl_defaults_to_300() {
    let config = OagwConfig::default();
    assert_eq!(config.token_cache_ttl_secs, 300);
}

#[test]
fn token_cache_capacity_defaults_to_10000() {
    let config = OagwConfig::default();
    assert_eq!(config.token_cache_capacity, 10_000);
}

#[test]
fn validate_rejects_zero_idle_timeout() {
    let config = OagwConfig {
        websocket_idle_timeout_secs: 0,
        ..Default::default()
    };
    assert!(config.validate().is_err());
}

#[test]
fn validate_rejects_zero_close_timeout() {
    let config = OagwConfig {
        websocket_close_timeout_secs: 0,
        ..Default::default()
    };
    assert!(config.validate().is_err());
}

#[test]
fn validate_accepts_nonzero_timeouts() {
    let config = OagwConfig::default();
    assert!(config.validate().is_ok());
}

#[test]
fn protocol_cache_ttl_defaults_to_3600() {
    let config = OagwConfig::default();
    assert_eq!(config.protocol_cache_ttl_secs, 3600);
}

#[test]
fn streaming_idle_timeout_defaults_to_300() {
    let config = OagwConfig::default();
    assert_eq!(config.streaming_idle_timeout_secs, 300);
}

#[test]
fn validate_rejects_zero_streaming_idle_timeout() {
    let config = OagwConfig {
        streaming_idle_timeout_secs: 0,
        ..Default::default()
    };
    assert!(config.validate().is_err());
}

#[test]
fn validate_accepts_zero_protocol_cache_ttl() {
    let config = OagwConfig {
        protocol_cache_ttl_secs: 0,
        ..Default::default()
    };
    assert!(config.validate().is_ok());
}
