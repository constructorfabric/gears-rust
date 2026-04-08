#![allow(clippy::unwrap_used, clippy::expect_used)]

use modkit_auth::{ClaimsError, JwksConfig};

#[test]
fn test_jwks_config_serialization_roundtrip() {
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
fn test_claims_error_types() {
    let err = ClaimsError::InvalidIssuer {
        expected: vec!["https://expected.com".to_owned()],
        actual: "https://actual.com".to_owned(),
    };
    assert!(matches!(
        err,
        ClaimsError::InvalidIssuer { ref expected, ref actual }
            if expected == &vec!["https://expected.com".to_owned()]
                && actual == "https://actual.com"
    ));

    let err = ClaimsError::Expired;
    assert!(matches!(err, ClaimsError::Expired));

    let err = ClaimsError::MissingClaim("sub".to_owned());
    assert!(matches!(err, ClaimsError::MissingClaim(ref c) if c == "sub"));

    let err = ClaimsError::UnknownKidAfterRefresh;
    assert!(matches!(err, ClaimsError::UnknownKidAfterRefresh));

    let err = ClaimsError::InvalidClaimFormat {
        field: "typ".to_owned(),
        reason: "expected string, got number".to_owned(),
    };
    assert!(matches!(
        err,
        ClaimsError::InvalidClaimFormat { ref field, ref reason }
            if field == "typ" && reason == "expected string, got number"
    ));

    let err = ClaimsError::InvalidAudience {
        expected: vec!["https://api.example.com".to_owned()],
        actual: vec!["https://other.example.com".to_owned()],
    };
    assert!(matches!(
        err,
        ClaimsError::InvalidAudience { ref expected, ref actual }
            if expected == &vec!["https://api.example.com".to_owned()]
                && actual == &vec!["https://other.example.com".to_owned()]
    ));
}
