use super::*;
use serde_json::json;

/// Unix timestamp for 9999-12-31T23:59:59Z — max representable date in `time` crate default range.
const MAX_UNIX_TIMESTAMP: i64 = 253_402_300_799;
/// Unix timestamp for -9999-01-01T00:00:00Z — min representable date in `time` crate default range.
const MIN_UNIX_TIMESTAMP: i64 = -377_705_116_800;

#[test]
fn test_valid_claims_pass() {
    let now = time::OffsetDateTime::now_utc();
    let claims = json!({
        "iss": "https://test.example.com",
        "aud": "api",
        "exp": (now + time::Duration::hours(1)).unix_timestamp(),
    });
    let config = ValidationConfig {
        allowed_issuers: vec!["https://test.example.com".to_owned()],
        allowed_audiences: vec!["api".to_owned()],
        ..Default::default()
    };
    assert!(validate_claims(&claims, &config).is_ok());
}

#[test]
fn test_invalid_issuer_fails() {
    let claims = json!({ "iss": "https://wrong.example.com" });
    let config = ValidationConfig {
        allowed_issuers: vec!["https://expected.example.com".to_owned()],
        ..Default::default()
    };
    let err = validate_claims(&claims, &config).unwrap_err();
    match err {
        ClaimsError::InvalidIssuer { expected, actual } => {
            assert_eq!(expected, vec!["https://expected.example.com"]);
            assert_eq!(actual, "https://wrong.example.com");
        }
        other => panic!("expected InvalidIssuer, got {other:?}"),
    }
}

#[test]
fn test_missing_issuer_fails_when_required() {
    let claims = json!({ "sub": "user-1" });
    let config = ValidationConfig {
        allowed_issuers: vec!["https://expected.example.com".to_owned()],
        ..Default::default()
    };
    let err = validate_claims(&claims, &config).unwrap_err();
    match err {
        ClaimsError::MissingClaim(claim) => assert_eq!(claim, StandardClaim::ISS),
        other => panic!("expected MissingClaim(iss), got {other:?}"),
    }
}

#[test]
fn test_invalid_audience_fails() {
    let claims = json!({ "aud": "wrong-api" });
    let config = ValidationConfig {
        allowed_audiences: vec!["expected-api".to_owned()],
        ..Default::default()
    };
    let err = validate_claims(&claims, &config).unwrap_err();
    match err {
        ClaimsError::InvalidAudience { expected, actual } => {
            assert_eq!(expected, vec!["expected-api"]);
            assert_eq!(actual, vec!["wrong-api"]);
        }
        other => panic!("expected InvalidAudience, got {other:?}"),
    }
}

#[test]
fn test_missing_audience_fails_when_required() {
    let claims = json!({ "sub": "user-1" });
    let config = ValidationConfig {
        allowed_audiences: vec!["api".to_owned()],
        ..Default::default()
    };
    let err = validate_claims(&claims, &config).unwrap_err();
    match err {
        ClaimsError::MissingClaim(claim) => assert_eq!(claim, StandardClaim::AUD),
        other => panic!("expected MissingClaim(aud), got {other:?}"),
    }
}

#[test]
fn test_expired_token_fails() {
    let now = time::OffsetDateTime::now_utc();
    let claims = json!({
        "exp": (now - time::Duration::hours(1)).unix_timestamp(),
    });
    let config = ValidationConfig::default();
    assert!(matches!(
        validate_claims(&claims, &config),
        Err(ClaimsError::Expired)
    ));
}

#[test]
fn test_not_yet_valid_fails() {
    let now = time::OffsetDateTime::now_utc();
    let claims = json!({
        "exp": (now + time::Duration::hours(2)).unix_timestamp(),
        "nbf": (now + time::Duration::hours(1)).unix_timestamp(),
    });
    let config = ValidationConfig::default();
    assert!(matches!(
        validate_claims(&claims, &config),
        Err(ClaimsError::NotYetValid)
    ));
}

#[test]
fn test_leeway_allows_slightly_expired() {
    let now = time::OffsetDateTime::now_utc();
    let claims = json!({
        "exp": (now - time::Duration::seconds(30)).unix_timestamp(),
    });
    let config = ValidationConfig {
        leeway_seconds: 60,
        ..Default::default()
    };
    assert!(validate_claims(&claims, &config).is_ok());
}

#[test]
fn test_default_config_accepts_valid_claims_with_exp() {
    let now = time::OffsetDateTime::now_utc();
    let claims = json!({
        "sub": "anyone",
        "iss": "any-issuer",
        "exp": (now + time::Duration::hours(1)).unix_timestamp(),
    });
    let config = ValidationConfig::default();
    assert!(validate_claims(&claims, &config).is_ok());
}

#[test]
fn test_missing_exp_fails() {
    let claims = json!({ "sub": "user-1" });
    let config = ValidationConfig::default();
    let err = validate_claims(&claims, &config).unwrap_err();
    match err {
        ClaimsError::MissingClaim(claim) => assert_eq!(claim, StandardClaim::EXP),
        other => panic!("expected MissingClaim(exp), got {other:?}"),
    }
}

#[test]
fn test_missing_exp_allowed_when_not_required() {
    let claims = json!({ "sub": "service-token", "iss": "internal" });
    let config = ValidationConfig {
        require_exp: false,
        ..Default::default()
    };
    assert!(validate_claims(&claims, &config).is_ok());
}

#[test]
fn test_audience_array_match() {
    let now = time::OffsetDateTime::now_utc();
    let claims = json!({
        "aud": ["api", "frontend"],
        "exp": (now + time::Duration::hours(1)).unix_timestamp(),
    });
    let config = ValidationConfig {
        allowed_audiences: vec!["api".to_owned()],
        ..Default::default()
    };
    assert!(validate_claims(&claims, &config).is_ok());
}

#[test]
fn test_parse_uuid_from_value() {
    let uuid = Uuid::new_v4();
    let value = json!(uuid.to_string());

    let result = parse_uuid_from_value(&value, "test");
    assert_eq!(result.unwrap(), uuid);
}

#[test]
fn test_parse_uuid_from_value_invalid() {
    let value = json!("not-a-uuid");
    let err = parse_uuid_from_value(&value, "test").unwrap_err();
    match err {
        ClaimsError::InvalidClaimFormat { field, reason } => {
            assert_eq!(field, "test");
            assert_eq!(reason, "must be a valid UUID");
        }
        other => panic!("expected InvalidClaimFormat, got {other:?}"),
    }
}

#[test]
fn test_malformed_audience_array_rejected() {
    let claims = json!({ "aud": ["api", 123] });
    let config = ValidationConfig {
        allowed_audiences: vec!["api".to_owned()],
        ..Default::default()
    };
    let err = validate_claims(&claims, &config).unwrap_err();
    match err {
        ClaimsError::InvalidClaimFormat { field, reason } => {
            assert_eq!(field, StandardClaim::AUD);
            assert_eq!(reason, "must be a string or array of strings");
        }
        other => panic!("expected InvalidClaimFormat for aud, got {other:?}"),
    }
}

#[test]
fn test_malformed_audience_type_rejected() {
    let claims = json!({ "aud": 42 });
    let config = ValidationConfig {
        allowed_audiences: vec!["api".to_owned()],
        ..Default::default()
    };
    let err = validate_claims(&claims, &config).unwrap_err();
    match err {
        ClaimsError::InvalidClaimFormat { field, reason } => {
            assert_eq!(field, StandardClaim::AUD);
            assert_eq!(reason, "must be a string or array of strings");
        }
        other => panic!("expected InvalidClaimFormat for aud, got {other:?}"),
    }
}

#[test]
fn test_extract_audiences_string() {
    let value = json!("api");
    let audiences = extract_audiences(&value).unwrap();
    assert_eq!(audiences, vec!["api"]);
}

#[test]
fn test_extract_audiences_array() {
    let value = json!(["api", "ui"]);
    let audiences = extract_audiences(&value).unwrap();
    assert_eq!(audiences, vec!["api", "ui"]);
}

#[test]
fn test_exp_overflow_returns_error() {
    let claims = json!({ "exp": MAX_UNIX_TIMESTAMP });
    let config = ValidationConfig {
        leeway_seconds: 60,
        ..Default::default()
    };
    let err = validate_claims(&claims, &config).unwrap_err();
    match err {
        ClaimsError::InvalidClaimFormat { field, reason } => {
            assert_eq!(field, StandardClaim::EXP);
            assert_eq!(reason, "timestamp with leeway is out of range");
        }
        other => panic!("expected InvalidClaimFormat for exp overflow, got {other:?}"),
    }
}

#[test]
fn test_nbf_overflow_returns_error() {
    let now = time::OffsetDateTime::now_utc();
    let claims = json!({
        "exp": (now + time::Duration::hours(1)).unix_timestamp(),
        "nbf": MIN_UNIX_TIMESTAMP,
    });
    let config = ValidationConfig {
        leeway_seconds: 60,
        ..Default::default()
    };
    let err = validate_claims(&claims, &config).unwrap_err();
    match err {
        ClaimsError::InvalidClaimFormat { field, reason } => {
            assert_eq!(field, StandardClaim::NBF);
            assert_eq!(reason, "timestamp with leeway is out of range");
        }
        other => panic!("expected InvalidClaimFormat for nbf overflow, got {other:?}"),
    }
}

#[test]
fn test_non_object_payload_rejected() {
    let config = ValidationConfig::default();
    for value in [
        json!("string"),
        json!(42),
        json!(true),
        json!(null),
        json!([1, 2, 3]),
    ] {
        let err = validate_claims(&value, &config).unwrap_err();
        match err {
            ClaimsError::InvalidClaimFormat { field, reason } => {
                assert_eq!(field, "claims");
                assert_eq!(reason, "must be a JSON object");
            }
            other => panic!("expected InvalidClaimFormat for non-object, got {other:?}"),
        }
    }
}

#[test]
fn test_extract_string_valid() {
    let value = json!("hello");
    assert_eq!(extract_string(&value, "field").unwrap(), "hello");
}

#[test]
fn test_extract_string_non_string_returns_invalid_claim_format() {
    for value in [json!(42), json!(true), json!({"a": 1}), json!([1, 2])] {
        let err = extract_string(&value, "my_field").unwrap_err();
        match err {
            ClaimsError::InvalidClaimFormat { field, reason } => {
                assert_eq!(field, "my_field");
                assert_eq!(reason, "must be a string");
            }
            other => panic!("expected InvalidClaimFormat, got {other:?}"),
        }
    }
}
