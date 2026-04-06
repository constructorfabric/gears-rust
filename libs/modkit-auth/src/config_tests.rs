use super::*;

#[test]
fn test_default_config() {
    let config = AuthConfig::default();
    assert_eq!(config.leeway_seconds, 60);
    assert!(config.issuers.is_empty());
    assert!(config.audiences.is_empty());
    assert!(config.require_exp);
    assert!(config.jwks.is_none());
}

#[test]
fn test_auth_config_serialization() {
    let config = AuthConfig {
        leeway_seconds: 120,
        issuers: vec!["https://auth.example.com".to_owned()],
        audiences: vec!["api".to_owned()],
        require_exp: true,
        jwks: Some(JwksConfig {
            uri: "https://auth.example.com/.well-known/jwks.json".to_owned(),
            refresh_interval_seconds: 300,
            max_backoff_seconds: 3600,
        }),
    };

    let json = serde_json::to_string_pretty(&config).unwrap();
    let deserialized: AuthConfig = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized.leeway_seconds, 120);
    assert_eq!(deserialized.issuers, vec!["https://auth.example.com"]);
    assert_eq!(deserialized.audiences, vec!["api"]);
    assert!(deserialized.require_exp);
    let jwks = deserialized.jwks.expect("jwks should be present");
    assert_eq!(jwks.uri, "https://auth.example.com/.well-known/jwks.json");
    assert_eq!(jwks.refresh_interval_seconds, 300);
    assert_eq!(jwks.max_backoff_seconds, 3600);
}

#[test]
fn test_jwks_config_serialization() {
    let config = JwksConfig {
        uri: "https://auth.example.com/.well-known/jwks.json".to_owned(),
        refresh_interval_seconds: 300,
        max_backoff_seconds: 3600,
    };

    let json = serde_json::to_string_pretty(&config).unwrap();
    let deserialized: JwksConfig = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized.uri, config.uri);
    assert_eq!(
        deserialized.refresh_interval_seconds,
        config.refresh_interval_seconds
    );
    assert_eq!(deserialized.max_backoff_seconds, config.max_backoff_seconds);
}

#[test]
fn test_auth_config_to_validation_config() {
    let auth_config = AuthConfig {
        leeway_seconds: 30,
        issuers: vec!["https://auth.example.com".to_owned()],
        audiences: vec!["api".to_owned()],
        require_exp: true,
        jwks: None,
    };
    let validation_config = ValidationConfig::from(&auth_config);
    assert_eq!(validation_config.allowed_issuers, auth_config.issuers);
    assert_eq!(validation_config.allowed_audiences, auth_config.audiences);
    assert_eq!(validation_config.leeway_seconds, auth_config.leeway_seconds);
    assert!(validation_config.require_exp);
}

#[test]
fn test_require_exp_defaults_true_when_omitted() {
    let json = r#"{"leeway_seconds": 60}"#;
    let config: AuthConfig = serde_json::from_str(json).unwrap();
    assert!(config.require_exp);
}

#[test]
fn test_require_exp_false_propagates_to_validation_config() {
    let auth_config = AuthConfig {
        require_exp: false,
        ..Default::default()
    };
    let validation_config = ValidationConfig::from(&auth_config);
    assert!(!validation_config.require_exp);
}

#[test]
fn test_jwks_config_defaults() {
    let json = r#"{"uri": "https://example.com/jwks"}"#;
    let config: JwksConfig = serde_json::from_str(json).unwrap();
    assert_eq!(config.refresh_interval_seconds, 300);
    assert_eq!(config.max_backoff_seconds, 3600);
}
