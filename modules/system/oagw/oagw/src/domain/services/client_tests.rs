use super::*;
use std::collections::HashMap;

#[test]
fn auth_config_hashmap_round_trips() {
    let mut config = HashMap::new();
    config.insert("header".into(), "authorization".into());
    config.insert("prefix".into(), "Bearer ".into());
    let sdk_auth = oagw_sdk::AuthConfig {
        plugin_type: "test-plugin".into(),
        sharing: oagw_sdk::SharingMode::Private,
        config: Some(config.clone()),
    };
    let domain_auth = auth_config_to_domain(sdk_auth);
    assert_eq!(domain_auth.plugin_type, "test-plugin");
    assert_eq!(domain_auth.sharing, model::SharingMode::Private);
    assert_eq!(domain_auth.config.unwrap(), config);
}

#[test]
fn auth_config_none_stays_none() {
    let sdk_auth = oagw_sdk::AuthConfig {
        plugin_type: "noop".into(),
        sharing: oagw_sdk::SharingMode::Inherit,
        config: None,
    };
    let domain_auth = auth_config_to_domain(sdk_auth);
    assert!(domain_auth.config.is_none());
    assert_eq!(domain_auth.sharing, model::SharingMode::Inherit);
}

#[test]
fn upstream_to_sdk_converts_auth_config_back() {
    let mut config = HashMap::new();
    config.insert("header".into(), "x-api-key".into());
    config.insert("secret_ref".into(), "cred://key".into());

    let domain_upstream = model::Upstream {
        id: Uuid::nil(),
        tenant_id: Uuid::nil(),
        alias: "test".into(),
        server: model::Server {
            endpoints: vec![model::Endpoint {
                scheme: model::Scheme::Https,
                host: "example.com".into(),
                port: 443,
            }],
        },
        protocol: "http".into(),
        enabled: true,
        auth: Some(model::AuthConfig {
            plugin_type: "apikey".into(),
            sharing: model::SharingMode::Private,
            config: Some(config),
        }),
        headers: None,
        plugins: None,
        rate_limit: None,
        cors: None,
        tags: vec![],
    };

    let sdk = upstream_to_sdk(domain_upstream);
    let auth = sdk.auth.unwrap();
    assert_eq!(auth.plugin_type, "apikey");
    let config = auth.config.unwrap();
    assert_eq!(config.get("header").unwrap(), "x-api-key");
    assert_eq!(config.get("secret_ref").unwrap(), "cred://key");
}

#[test]
fn domain_err_not_found_maps_to_sdk() {
    let err = DomainError::NotFound {
        entity: "upstream",
        id: Uuid::nil(),
    };
    let sdk_err = domain_err_to_sdk(err);
    assert!(matches!(
        sdk_err,
        ServiceGatewayError::NotFound { ref entity, .. } if entity == "upstream"
    ));
}

#[test]
fn domain_err_validation_maps_to_sdk() {
    let err = DomainError::Validation {
        detail: "bad input".into(),
        instance: "/test".into(),
    };
    let sdk_err = domain_err_to_sdk(err);
    assert!(matches!(
        sdk_err,
        ServiceGatewayError::ValidationError { .. }
    ));
}

#[test]
fn domain_err_rate_limit_maps_to_sdk() {
    let err = DomainError::RateLimitExceeded {
        detail: "too fast".into(),
        instance: "/api".into(),
        retry_after_secs: Some(30),
    };
    let sdk_err = domain_err_to_sdk(err);
    match sdk_err {
        ServiceGatewayError::RateLimitExceeded {
            retry_after_secs, ..
        } => assert_eq!(retry_after_secs, Some(30)),
        _ => panic!("expected RateLimitExceeded"),
    }
}

#[test]
fn domain_err_timeout_maps_to_sdk() {
    let err = DomainError::RequestTimeout {
        detail: "timed out".into(),
        instance: "/slow".into(),
    };
    let sdk_err = domain_err_to_sdk(err);
    assert!(matches!(
        sdk_err,
        ServiceGatewayError::RequestTimeout { .. }
    ));
}

#[test]
fn sharing_mode_round_trip() {
    for (sdk_val, expected_domain) in [
        (oagw_sdk::SharingMode::Private, model::SharingMode::Private),
        (oagw_sdk::SharingMode::Inherit, model::SharingMode::Inherit),
        (oagw_sdk::SharingMode::Enforce, model::SharingMode::Enforce),
    ] {
        let domain = sharing_mode_to_domain(sdk_val);
        assert_eq!(domain, expected_domain);
        let back = sharing_mode_to_sdk(domain);
        assert_eq!(back, sdk_val);
    }
}

#[test]
fn scheme_round_trip() {
    for (sdk_val, expected_domain) in [
        (oagw_sdk::Scheme::Http, model::Scheme::Http),
        (oagw_sdk::Scheme::Https, model::Scheme::Https),
        (oagw_sdk::Scheme::Wss, model::Scheme::Wss),
        (oagw_sdk::Scheme::Wt, model::Scheme::Wt),
        (oagw_sdk::Scheme::Grpc, model::Scheme::Grpc),
    ] {
        let domain = scheme_to_domain(sdk_val);
        assert_eq!(domain, expected_domain);
        let back = scheme_to_sdk(domain);
        assert_eq!(back, sdk_val);
    }
}
