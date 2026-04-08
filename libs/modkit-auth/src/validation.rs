use crate::claims_error::ClaimsError;
use crate::standard_claims::StandardClaim;
use time::OffsetDateTime;
use uuid::Uuid;

/// Configuration for common validation
#[derive(Debug, Clone)]
pub struct ValidationConfig {
    /// Allowed issuers (if empty, any issuer is accepted)
    pub allowed_issuers: Vec<String>,

    /// Allowed audiences (if empty, any audience is accepted)
    pub allowed_audiences: Vec<String>,

    /// Leeway in seconds for time-based validations (exp, nbf)
    pub leeway_seconds: i64,

    /// Whether the `exp` claim is required (default: `true`).
    /// Set to `false` to allow tokens without an expiration claim.
    pub require_exp: bool,
}

impl Default for ValidationConfig {
    fn default() -> Self {
        Self {
            allowed_issuers: vec![],
            allowed_audiences: vec![],
            leeway_seconds: 60,
            require_exp: true,
        }
    }
}

/// Validate standard JWT claims in raw JSON against the given configuration.
///
/// Checks performed:
/// 1. **Issuer** (`iss`) — must match one of `config.allowed_issuers` (skipped if empty)
/// 2. **Audience** (`aud`) — at least one must match `config.allowed_audiences` (skipped if empty)
/// 3. **Expiration** (`exp`) — required by default; must not be in the past (with leeway).
///    Set `require_exp = false` to accept tokens without an `exp` claim.
/// 4. **Not Before** (`nbf`) — must not be in the future (with leeway)
///
/// # Errors
/// Returns `ClaimsError` if any validation check fails.
pub fn validate_claims(
    raw: &serde_json::Value,
    config: &ValidationConfig,
) -> Result<(), ClaimsError> {
    // 0. Reject non-object payloads early
    if !raw.is_object() {
        return Err(ClaimsError::InvalidClaimFormat {
            field: "claims".to_owned(),
            reason: "must be a JSON object".to_owned(),
        });
    }

    // 1. Validate issuer
    if !config.allowed_issuers.is_empty() {
        if let Some(iss_value) = raw.get(StandardClaim::ISS) {
            let iss = iss_value
                .as_str()
                .ok_or_else(|| ClaimsError::InvalidClaimFormat {
                    field: StandardClaim::ISS.to_owned(),
                    reason: "must be a string".to_owned(),
                })?;
            if !config.allowed_issuers.iter().any(|a| a == iss) {
                return Err(ClaimsError::InvalidIssuer {
                    expected: config.allowed_issuers.clone(),
                    actual: iss.to_owned(),
                });
            }
        } else {
            return Err(ClaimsError::MissingClaim(StandardClaim::ISS.to_owned()));
        }
    }

    // 2. Validate audience (at least one must match)
    if !config.allowed_audiences.is_empty() {
        if let Some(aud_value) = raw.get(StandardClaim::AUD) {
            let audiences = extract_audiences(aud_value)?;
            let has_match = audiences
                .iter()
                .any(|a| config.allowed_audiences.contains(a));
            if !has_match {
                return Err(ClaimsError::InvalidAudience {
                    expected: config.allowed_audiences.clone(),
                    actual: audiences,
                });
            }
        } else {
            return Err(ClaimsError::MissingClaim(StandardClaim::AUD.to_owned()));
        }
    }

    let now = OffsetDateTime::now_utc();
    let leeway = time::Duration::seconds(config.leeway_seconds);

    // 3. Validate expiration with leeway
    if let Some(exp_value) = raw.get(StandardClaim::EXP) {
        let exp = parse_timestamp(exp_value, StandardClaim::EXP)?;
        let exp_with_leeway =
            exp.checked_add(leeway)
                .ok_or_else(|| ClaimsError::InvalidClaimFormat {
                    field: StandardClaim::EXP.to_owned(),
                    reason: "timestamp with leeway is out of range".to_owned(),
                })?;
        if now > exp_with_leeway {
            return Err(ClaimsError::Expired);
        }
    } else if config.require_exp {
        return Err(ClaimsError::MissingClaim(StandardClaim::EXP.to_owned()));
    }

    // 4. Validate not-before with leeway
    if let Some(nbf_value) = raw.get(StandardClaim::NBF) {
        let nbf = parse_timestamp(nbf_value, StandardClaim::NBF)?;
        let nbf_with_leeway =
            nbf.checked_sub(leeway)
                .ok_or_else(|| ClaimsError::InvalidClaimFormat {
                    field: StandardClaim::NBF.to_owned(),
                    reason: "timestamp with leeway is out of range".to_owned(),
                })?;
        if now < nbf_with_leeway {
            return Err(ClaimsError::NotYetValid);
        }
    }

    Ok(())
}

/// Helper to parse a UUID from a JSON value.
///
/// # Errors
/// Returns `ClaimsError::InvalidClaimFormat` if the value is not a valid UUID string.
pub fn parse_uuid_from_value(
    value: &serde_json::Value,
    field_name: &str,
) -> Result<Uuid, ClaimsError> {
    value
        .as_str()
        .ok_or_else(|| ClaimsError::InvalidClaimFormat {
            field: field_name.to_owned(),
            reason: "must be a string".to_owned(),
        })
        .and_then(|s| {
            Uuid::parse_str(s).map_err(|_| ClaimsError::InvalidClaimFormat {
                field: field_name.to_owned(),
                reason: "must be a valid UUID".to_owned(),
            })
        })
}

/// Helper to parse an array of UUIDs from a JSON value.
///
/// # Errors
/// Returns `ClaimsError::InvalidClaimFormat` if the value is not an array of valid UUID strings.
pub fn parse_uuid_array_from_value(
    value: &serde_json::Value,
    field_name: &str,
) -> Result<Vec<Uuid>, ClaimsError> {
    value
        .as_array()
        .ok_or_else(|| ClaimsError::InvalidClaimFormat {
            field: field_name.to_owned(),
            reason: "must be an array".to_owned(),
        })?
        .iter()
        .map(|v| parse_uuid_from_value(v, field_name))
        .collect()
}

/// Helper to parse timestamp (seconds since epoch) into `OffsetDateTime`.
///
/// # Errors
/// Returns `ClaimsError::InvalidClaimFormat` if the value is not a valid unix timestamp.
pub fn parse_timestamp(
    value: &serde_json::Value,
    field_name: &str,
) -> Result<OffsetDateTime, ClaimsError> {
    let ts = value
        .as_i64()
        .ok_or_else(|| ClaimsError::InvalidClaimFormat {
            field: field_name.to_owned(),
            reason: "must be a number (unix timestamp)".to_owned(),
        })?;

    OffsetDateTime::from_unix_timestamp(ts).map_err(|_| ClaimsError::InvalidClaimFormat {
        field: field_name.to_owned(),
        reason: "invalid unix timestamp".to_owned(),
    })
}

/// Helper to extract string from JSON value.
///
/// # Errors
/// Returns `ClaimsError::InvalidClaimFormat` if the value is not a string.
pub fn extract_string(value: &serde_json::Value, field_name: &str) -> Result<String, ClaimsError> {
    value
        .as_str()
        .map(ToString::to_string)
        .ok_or_else(|| ClaimsError::InvalidClaimFormat {
            field: field_name.to_owned(),
            reason: "must be a string".to_owned(),
        })
}

/// Extract audiences from a JSON value.
///
/// Accepts a single string or an array of strings. Rejects non-string entries
/// in arrays and non-string/non-array values.
///
/// # Errors
/// Returns `ClaimsError::InvalidClaimFormat` if the value is not a string,
/// not an array of strings, or contains non-string entries.
pub fn extract_audiences(value: &serde_json::Value) -> Result<Vec<String>, ClaimsError> {
    match value {
        serde_json::Value::String(s) => Ok(vec![s.clone()]),
        serde_json::Value::Array(arr) => {
            let mut out = Vec::with_capacity(arr.len());
            for v in arr {
                let s = v.as_str().ok_or_else(|| ClaimsError::InvalidClaimFormat {
                    field: StandardClaim::AUD.to_owned(),
                    reason: "must be a string or array of strings".to_owned(),
                })?;
                out.push(s.to_owned());
            }
            Ok(out)
        }
        _ => Err(ClaimsError::InvalidClaimFormat {
            field: StandardClaim::AUD.to_owned(),
            reason: "must be a string or array of strings".to_owned(),
        }),
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
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
}
