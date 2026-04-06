use super::*;
use crate::domain::model::SharingMode;

fn make_config() -> CorsConfig {
    CorsConfig {
        sharing: SharingMode::Private,
        enabled: true,
        allowed_origins: vec!["https://example.com".to_string()],
        allowed_methods: vec![CorsHttpMethod::Get, CorsHttpMethod::Post],
        expose_headers: vec!["x-request-id".to_string()],
        allow_credentials: false,
    }
}

// -- validate_cors_config --

#[test]
fn test_validate_valid_config_accepted() {
    let config = make_config();
    assert!(validate_cors_config(&config).is_ok());
}

#[test]
fn test_validate_credentials_with_wildcard_rejected() {
    let config = CorsConfig {
        allow_credentials: true,
        allowed_origins: vec!["*".to_string()],
        ..make_config()
    };
    let err = validate_cors_config(&config).unwrap_err();
    assert!(matches!(err, DomainError::Validation { .. }));
}

#[test]
fn test_validate_invalid_origin_rejected() {
    let config = CorsConfig {
        allowed_origins: vec!["not-a-url".to_string()],
        ..make_config()
    };
    assert!(validate_cors_config(&config).is_err());
}

#[test]
fn test_validate_wildcard_origin_accepted() {
    let config = CorsConfig {
        allowed_origins: vec!["*".to_string()],
        ..make_config()
    };
    assert!(validate_cors_config(&config).is_ok());
}

// -- apply_cors_headers --

#[test]
fn test_actual_request_cors_headers() {
    let config = make_config();
    let headers = apply_cors_headers(&config, "https://example.com");
    assert!(!headers.is_empty());

    let origin = headers
        .iter()
        .find(|(k, _)| k == "access-control-allow-origin")
        .unwrap();
    assert_eq!(origin.1, "https://example.com");

    let expose = headers
        .iter()
        .find(|(k, _)| k == "access-control-expose-headers")
        .unwrap();
    assert_eq!(expose.1, "x-request-id");
}

#[test]
fn test_actual_request_disallowed_origin_no_headers() {
    let config = make_config();
    let headers = apply_cors_headers(&config, "https://evil.com");
    assert!(headers.is_empty());
}

// -- Origin matching --

#[test]
fn test_wildcard_origin_allows_any() {
    let config = CorsConfig {
        allowed_origins: vec!["*".to_string()],
        ..make_config()
    };
    let headers = apply_cors_headers(&config, "https://anything.com");
    let origin = headers
        .iter()
        .find(|(k, _)| k == "access-control-allow-origin")
        .unwrap();
    assert_eq!(origin.1, "*");
}

#[test]
fn test_origin_matching_port_sensitive() {
    let config = CorsConfig {
        allowed_origins: vec!["https://example.com".to_string()],
        ..make_config()
    };
    // Different port should not match.
    assert!(apply_cors_headers(&config, "https://example.com:8443").is_empty());
}

#[test]
fn test_origin_matching_protocol_sensitive() {
    let config = CorsConfig {
        allowed_origins: vec!["https://example.com".to_string()],
        ..make_config()
    };
    // Different protocol should not match.
    assert!(apply_cors_headers(&config, "http://example.com").is_empty());
}

// -- is_valid_origin --

#[test]
fn test_valid_origins() {
    assert!(is_valid_origin("https://example.com"));
    assert!(is_valid_origin("http://localhost"));
    assert!(is_valid_origin("https://example.com:8443"));
    assert!(is_valid_origin("http://127.0.0.1:3000"));
}

#[test]
fn test_invalid_origins() {
    assert!(!is_valid_origin("example.com"));
    assert!(!is_valid_origin("ftp://example.com"));
    assert!(!is_valid_origin("https://"));
    assert!(!is_valid_origin("https://example.com:notaport"));
    assert!(!is_valid_origin(""));
}

#[test]
fn test_valid_ipv6_origins() {
    assert!(is_valid_origin("http://[::1]"));
    assert!(is_valid_origin("http://[::1]:8080"));
    assert!(is_valid_origin("https://[::1]:443"));
    assert!(is_valid_origin("http://[2001:db8::1]"));
    assert!(is_valid_origin("http://[2001:db8::1]:3000"));
    assert!(is_valid_origin("http://[0:0:0:0:0:0:0:1]"));
}

#[test]
fn test_invalid_ipv6_origins() {
    assert!(!is_valid_origin("http://[::1]:notaport"));
    assert!(!is_valid_origin("http://[::1]:"));
    assert!(!is_valid_origin("http://[not-ipv6]"));
    assert!(!is_valid_origin("http://[::1"));
    assert!(!is_valid_origin("http://::1"));
    assert!(!is_valid_origin("http://[]"));
    assert!(!is_valid_origin("http://[::1]:99999"));
}

#[test]
fn test_validate_config_with_ipv6_origin() {
    let config = CorsConfig {
        allowed_origins: vec!["http://[::1]:8080".to_string()],
        ..make_config()
    };
    assert!(validate_cors_config(&config).is_ok());
}
