use super::*;
use crate::config::{
    IssuerTrustConfig, JwtValidationConfig, TrustedIssuerEntry, TrustedIssuerInput,
};
use crate::domain::metrics::{
    TOKEN_REJECTION_REASON_EXPIRED, TOKEN_REJECTION_REASON_UNTRUSTED_ISSUER,
    test_harness::MetricsHarness,
};
use crate::domain::ports::JwksProvider;

/// Build `IssuerTrustConfig` in exact mode from a list of literal issuers.
fn make_trust(issuers: &[&str]) -> IssuerTrustConfig {
    let issuers = issuers
        .iter()
        .map(|issuer| (*issuer).to_owned())
        .collect::<Vec<_>>();
    IssuerTrustConfig::from_exact_issuers(issuers).unwrap()
}

fn make_regex_trust(patterns: &[&str]) -> IssuerTrustConfig {
    let inputs = patterns
        .iter()
        .map(|pattern| TrustedIssuerInput {
            entry: TrustedIssuerEntry::IssuerPattern((*pattern).to_owned()),
            discovery_url: None,
        })
        .collect::<Vec<_>>();
    IssuerTrustConfig::from_inputs(inputs).unwrap()
}

fn base_jwt_validation_config() -> JwtValidationConfig {
    JwtValidationConfig {
        supported_algorithms: vec![Algorithm::RS256, Algorithm::ES256],
        clock_skew_leeway_secs: 60,
        require_audience: false,
        expected_audience: Vec::new(),
        jwks_cache_ttl_secs: 3600,
        jwks_stale_ttl_secs: 86_400,
        jwks_max_entries: 64,
        jwks_refresh_on_unknown_kid: true,
        jwks_refresh_min_interval_secs: 30,
        discovery_cache_ttl_secs: 3600,
        discovery_max_entries: 64,
    }
}

fn jwt_validation_config_with_audience(
    patterns: &[&str],
    require_audience: bool,
) -> JwtValidationConfig {
    let mut config = base_jwt_validation_config();
    config.require_audience = require_audience;
    config.expected_audience = patterns
        .iter()
        .enumerate()
        .map(|(index, pattern)| {
            MatcherCompiled::from_wildcard_pattern(pattern, index)
                .expect("audience matcher pattern should compile")
        })
        .collect();
    config
}

#[test]
fn test_validate_algorithm_rs256_allowed() {
    let header = Header::new(Algorithm::RS256);
    assert!(JwtValidator::validate_algorithm(&header, &base_jwt_validation_config()).is_ok());
}

#[test]
fn test_validate_algorithm_hs256_rejected() {
    let header = Header::new(Algorithm::HS256);
    assert!(matches!(
        JwtValidator::validate_algorithm(&header, &base_jwt_validation_config()),
        Err(AuthNError::UnsupportedAlgorithm)
    ));
}

#[test]
fn test_validate_algorithm_disabled_by_config_rejected() {
    let header = Header::new(Algorithm::ES256);
    let mut config = base_jwt_validation_config();
    config.supported_algorithms = vec![Algorithm::RS256];
    assert!(matches!(
        JwtValidator::validate_algorithm(&header, &config),
        Err(AuthNError::UnsupportedAlgorithm)
    ));
}

#[test]
fn test_validate_issuer_trusted() {
    let trust = make_trust(&["https://oidc/realms/platform"]);
    let (validator, _) = make_validator_with_test_key();
    assert!(
        validator
            .validate_issuer("https://oidc/realms/platform", &trust)
            .is_ok()
    );
}

#[test]
fn test_validate_issuer_untrusted() {
    let trust = make_trust(&["https://oidc/realms/platform"]);
    let (validator, _) = make_validator_with_test_key();
    assert!(matches!(
        validator.validate_issuer("https://evil.example.com", &trust),
        Err(AuthNError::UntrustedIssuer)
    ));
}

#[test]
fn test_validate_issuer_regex_trusted() {
    let trust = make_regex_trust(&[r"https://oidc\..*\.example\.com/realms/platform"]);
    let (validator, _) = make_validator_with_test_key();
    assert!(
        validator
            .validate_issuer("https://oidc.eu.example.com/realms/platform", &trust)
            .is_ok()
    );
}

#[test]
fn test_validate_issuer_regex_untrusted() {
    let trust = make_regex_trust(&[r"https://oidc\..*\.example\.com/realms/platform"]);
    let (validator, _) = make_validator_with_test_key();
    assert!(matches!(
        validator.validate_issuer("https://evil.example.com/realms/platform", &trust),
        Err(AuthNError::UntrustedIssuer)
    ));
}

#[test]
fn test_decode_header_malformed() {
    assert!(matches!(
        JwtValidator::decode_header("not-a-jwt"),
        Err(AuthNError::SignatureInvalid)
    ));
}

#[test]
fn test_peek_issuer_missing_iss_claim() {
    // Valid JWT structure but no `iss` claim
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(r#"{"sub":"user-1","exp":9999999999}"#);
    let header =
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(r#"{"alg":"RS256","typ":"JWT"}"#);
    let token = format!("{header}.{payload}.fakesig");

    let result = JwtValidator::peek_issuer(&token);
    assert!(
        matches!(result, Err(AuthNError::MissingClaim(ref c)) if c == "iss"),
        "missing iss should return MissingClaim: {result:?}"
    );
}

#[test]
fn test_peek_issuer_too_few_segments() {
    let result = JwtValidator::peek_issuer("single-segment");
    assert!(
        matches!(result, Err(AuthNError::SignatureInvalid)),
        "single-segment token should return SignatureInvalid: {result:?}"
    );
}

use crate::test_support::test_fixtures::{
    TEST_ISSUER, TEST_KID, future_exp, past_exp, sign_jwt, test_jwk_json,
};

struct TestJwksProvider {
    jwks: Arc<JwkSet>,
    refresh_fails: bool,
}

#[async_trait::async_trait]
impl JwksProvider for TestJwksProvider {
    async fn get_jwks(
        &self,
        _issuer: &str,
        _discovery_base: &Url,
    ) -> Result<Arc<JwkSet>, AuthNError> {
        Ok(Arc::clone(&self.jwks))
    }

    async fn force_refresh(
        &self,
        _issuer: &str,
        _discovery_base: &Url,
    ) -> Result<Arc<JwkSet>, AuthNError> {
        if self.refresh_fails {
            Err(AuthNError::IdpUnreachable)
        } else {
            Ok(Arc::clone(&self.jwks))
        }
    }
}

/// Build a `JwtValidator` with the test public key pre-loaded in the JWKS cache.
fn make_validator_with_test_key() -> (JwtValidator, MetricsHarness) {
    let harness = MetricsHarness::new();
    let metrics = harness.metrics();
    let jwks: JwkSet = serde_json::from_str(test_jwk_json()).expect("test JWK should parse");
    let provider = Arc::new(TestJwksProvider {
        jwks: Arc::new(jwks),
        refresh_fails: true,
    });

    (JwtValidator::new(provider, metrics), harness)
}

#[tokio::test]
async fn test_valid_jwt_validates_successfully() {
    let (validator, _) = make_validator_with_test_key();
    let trust = make_trust(&[TEST_ISSUER]);

    let claims = serde_json::json!({
        "sub": "550e8400-e29b-41d4-a716-446655440000",
        "iss": TEST_ISSUER,
        "exp": future_exp(),
        "tenant_id": "tenant-abc",
    });
    let token = sign_jwt(&claims, Some(TEST_KID));

    let result = validator
        .validate(&token, &base_jwt_validation_config(), &trust)
        .await;
    assert!(
        result.is_ok(),
        "valid JWT should validate: {:?}",
        result.err()
    );
    let jwt_claims = result.unwrap();
    assert_eq!(jwt_claims.sub, "550e8400-e29b-41d4-a716-446655440000");
    assert_eq!(jwt_claims.iss, TEST_ISSUER);
}

#[tokio::test]
async fn test_expired_token_rejected() {
    let (validator, _) = make_validator_with_test_key();
    let trust = make_trust(&[TEST_ISSUER]);

    let claims = serde_json::json!({
        "sub": "550e8400-e29b-41d4-a716-446655440000",
        "iss": TEST_ISSUER,
        "exp": past_exp(),
    });
    let token = sign_jwt(&claims, Some(TEST_KID));

    let result = validator
        .validate(&token, &base_jwt_validation_config(), &trust)
        .await;
    assert!(
        matches!(result, Err(AuthNError::TokenExpired)),
        "expired token should return TokenExpired, got: {result:?}"
    );
}

#[tokio::test]
async fn test_not_before_token_rejected() {
    let (validator, _) = make_validator_with_test_key();
    let trust = make_trust(&[TEST_ISSUER]);

    let claims = serde_json::json!({
        "sub": "550e8400-e29b-41d4-a716-446655440000",
        "iss": TEST_ISSUER,
        "exp": future_exp(),
        "nbf": future_exp(),
    });
    let token = sign_jwt(&claims, Some(TEST_KID));

    let result = validator
        .validate(&token, &base_jwt_validation_config(), &trust)
        .await;
    assert!(
        matches!(result, Err(AuthNError::SignatureInvalid)),
        "token with future nbf should be rejected, got: {result:?}"
    );
}

#[tokio::test]
async fn test_future_issued_at_beyond_leeway_rejected() {
    let (validator, _) = make_validator_with_test_key();
    let trust = make_trust(&[TEST_ISSUER]);
    let now = current_unix_timestamp();

    let claims = serde_json::json!({
        "sub": "550e8400-e29b-41d4-a716-446655440000",
        "iss": TEST_ISSUER,
        "exp": now + 7200,
        "iat": now + 3600,
    });
    let token = sign_jwt(&claims, Some(TEST_KID));

    let result = validator
        .validate(&token, &base_jwt_validation_config(), &trust)
        .await;
    assert!(
        matches!(result, Err(AuthNError::SignatureInvalid)),
        "token issued after now + leeway should be rejected, got: {result:?}"
    );
}

#[tokio::test]
async fn test_future_issued_at_within_leeway_accepted() {
    let (validator, _) = make_validator_with_test_key();
    let trust = make_trust(&[TEST_ISSUER]);
    let config = base_jwt_validation_config();
    let now = current_unix_timestamp();
    let iat = now + config.clock_skew_leeway_secs;

    let claims = serde_json::json!({
        "sub": "550e8400-e29b-41d4-a716-446655440000",
        "iss": TEST_ISSUER,
        "exp": now + 7200,
        "iat": iat,
    });
    let token = sign_jwt(&claims, Some(TEST_KID));

    let result = validator.validate(&token, &config, &trust).await;
    assert!(
        result.is_ok(),
        "token issued within leeway should be accepted: {result:?}"
    );
    assert_eq!(result.unwrap().iat, Some(iat));
}

#[tokio::test]
async fn test_untrusted_issuer_rejected() {
    let (validator, _) = make_validator_with_test_key();
    let trust = make_trust(&["https://legit-oidc.example.com/realms/platform"]);

    let claims = serde_json::json!({
        "sub": "550e8400-e29b-41d4-a716-446655440000",
        "iss": TEST_ISSUER,   // iss does NOT match trusted list
        "exp": future_exp(),
    });
    let token = sign_jwt(&claims, Some(TEST_KID));

    let result = validator
        .validate(&token, &base_jwt_validation_config(), &trust)
        .await;
    assert!(
        matches!(result, Err(AuthNError::UntrustedIssuer)),
        "untrusted issuer should be rejected: {result:?}"
    );
}

#[tokio::test]
async fn test_hs256_token_rejected() {
    use jsonwebtoken::{EncodingKey, Header, encode};

    let (validator, _) = make_validator_with_test_key();
    let trust = make_trust(&[TEST_ISSUER]);

    // Build an HS256 token manually (not RS256)
    let header = Header::new(Algorithm::HS256);
    let claims = serde_json::json!({
        "sub": "550e8400-e29b-41d4-a716-446655440000",
        "iss": TEST_ISSUER,
        "exp": future_exp(),
    });
    let key = EncodingKey::from_secret(b"some-hmac-secret");
    let token = encode(&header, &claims, &key).expect("HS256 token should encode");

    let result = validator
        .validate(&token, &base_jwt_validation_config(), &trust)
        .await;
    assert!(
        matches!(result, Err(AuthNError::UnsupportedAlgorithm)),
        "HS256 token should be rejected with UnsupportedAlgorithm: {result:?}"
    );
}

#[tokio::test]
async fn test_unknown_kid_after_refresh_returns_kid_not_found() {
    let (validator, _) = make_validator_with_test_key();
    let trust = make_trust(&[TEST_ISSUER]);

    let claims = serde_json::json!({
        "sub": "550e8400-e29b-41d4-a716-446655440000",
        "iss": TEST_ISSUER,
        "exp": future_exp(),
    });
    let token = sign_jwt(&claims, Some("unknown-kid-xyz"));

    let result = validator
        .validate(&token, &base_jwt_validation_config(), &trust)
        .await;
    assert!(
        matches!(
            result,
            Err(AuthNError::KidNotFound | AuthNError::IdpUnreachable)
        ),
        "unknown kid should result in KidNotFound or IdpUnreachable: {result:?}"
    );
}

#[test]
fn test_find_key_without_kid_accepts_single_signing_key() {
    let jwks: JwkSet = serde_json::from_str(test_jwk_json()).expect("test JWKS should parse");

    let result = JwtValidator::find_key_in_jwks(&jwks, None, Algorithm::RS256);

    assert!(
        result.is_some(),
        "single signing key should allow missing kid"
    );
}

#[test]
fn test_find_key_without_kid_rejects_multiple_signing_keys() {
    let mut jwks: JwkSet = serde_json::from_str(test_jwk_json()).expect("test JWKS should parse");
    let mut second_key = jwks.keys[0].clone();
    second_key.common.key_id = Some("test-key-2".to_owned());
    jwks.keys.push(second_key);

    let result = JwtValidator::find_key_in_jwks(&jwks, None, Algorithm::RS256);

    assert!(
        result.is_none(),
        "missing kid should be rejected when multiple signing keys are present"
    );
}

#[test]
fn test_find_key_with_kid_accepts_matching_key_from_multiple_signing_keys() {
    let mut jwks: JwkSet = serde_json::from_str(test_jwk_json()).expect("test JWKS should parse");
    let mut second_key = jwks.keys[0].clone();
    second_key.common.key_id = Some("test-key-2".to_owned());
    jwks.keys.push(second_key);

    let result = JwtValidator::find_key_in_jwks(&jwks, Some(TEST_KID), Algorithm::RS256);

    assert!(
        result.is_some(),
        "matching kid should select a key even when multiple signing keys are present"
    );
}

#[tokio::test]
async fn test_audience_mismatch_rejected_when_required() {
    let (validator, _) = make_validator_with_test_key();
    let config = jwt_validation_config_with_audience(&["cyber-fabric-api"], true);
    let trust = make_trust(&[TEST_ISSUER]);

    let claims = serde_json::json!({
        "sub": "550e8400-e29b-41d4-a716-446655440000",
        "iss": TEST_ISSUER,
        "exp": future_exp(),
        "aud": "wrong-audience",
    });
    let token = sign_jwt(&claims, Some(TEST_KID));

    let result = validator.validate(&token, &config, &trust).await;
    assert!(
        matches!(result, Err(AuthNError::InvalidAudience)),
        "audience mismatch should be rejected: {result:?}"
    );
}

#[tokio::test]
async fn test_audience_exact_match_accepted() {
    let (validator, _) = make_validator_with_test_key();
    let config = jwt_validation_config_with_audience(&["cyber-fabric-api"], true);
    let trust = make_trust(&[TEST_ISSUER]);

    let claims = serde_json::json!({
        "sub": "550e8400-e29b-41d4-a716-446655440000",
        "iss": TEST_ISSUER,
        "exp": future_exp(),
        "aud": "cyber-fabric-api",
    });
    let token = sign_jwt(&claims, Some(TEST_KID));

    let result = validator.validate(&token, &config, &trust).await;
    assert!(
        result.is_ok(),
        "exact audience should be accepted: {result:?}"
    );
}

#[tokio::test]
async fn test_audience_wildcard_match_accepted_from_array_claim() {
    let (validator, _) = make_validator_with_test_key();
    let config = jwt_validation_config_with_audience(&["https://*.example.com/api"], true);
    let trust = make_trust(&[TEST_ISSUER]);

    let claims = serde_json::json!({
        "sub": "550e8400-e29b-41d4-a716-446655440000",
        "iss": TEST_ISSUER,
        "exp": future_exp(),
        "aud": ["other-audience", "https://tenant.example.com/api"],
    });
    let token = sign_jwt(&claims, Some(TEST_KID));

    let result = validator.validate(&token, &config, &trust).await;
    assert!(
        result.is_ok(),
        "wildcard audience in array should be accepted: {result:?}"
    );
}

#[tokio::test]
async fn test_missing_required_audience_rejected() {
    let (validator, _) = make_validator_with_test_key();
    let config = jwt_validation_config_with_audience(&["cyber-fabric-api"], true);
    let trust = make_trust(&[TEST_ISSUER]);

    let claims = serde_json::json!({
        "sub": "550e8400-e29b-41d4-a716-446655440000",
        "iss": TEST_ISSUER,
        "exp": future_exp(),
    });
    let token = sign_jwt(&claims, Some(TEST_KID));

    let result = validator.validate(&token, &config, &trust).await;
    assert!(
        matches!(result, Err(AuthNError::MissingClaim(ref claim)) if claim == "aud"),
        "missing audience should return MissingClaim(aud): {result:?}"
    );
}

#[tokio::test]
async fn test_required_audience_without_expected_patterns_accepts_present_audience() {
    let (validator, _) = make_validator_with_test_key();
    let config = jwt_validation_config_with_audience(&[], true);
    let trust = make_trust(&[TEST_ISSUER]);

    let claims = serde_json::json!({
        "sub": "550e8400-e29b-41d4-a716-446655440000",
        "iss": TEST_ISSUER,
        "exp": future_exp(),
        "aud": "any-present-audience",
    });
    let token = sign_jwt(&claims, Some(TEST_KID));

    let result = validator.validate(&token, &config, &trust).await;
    assert!(
        result.is_ok(),
        "present audience should satisfy require_audience when no expected patterns are configured: {result:?}"
    );
}

#[tokio::test]
async fn test_missing_optional_audience_accepted_when_expected_audience_configured() {
    let (validator, _) = make_validator_with_test_key();
    let config = jwt_validation_config_with_audience(&["cyber-fabric-api"], false);
    let trust = make_trust(&[TEST_ISSUER]);

    let claims = serde_json::json!({
        "sub": "550e8400-e29b-41d4-a716-446655440000",
        "iss": TEST_ISSUER,
        "exp": future_exp(),
    });
    let token = sign_jwt(&claims, Some(TEST_KID));

    let result = validator.validate(&token, &config, &trust).await;
    assert!(
        result.is_ok(),
        "missing optional audience should be accepted: {result:?}"
    );
}

#[tokio::test]
async fn test_optional_audience_mismatch_rejected_when_claim_present() {
    let (validator, _) = make_validator_with_test_key();
    let config = jwt_validation_config_with_audience(&["cyber-fabric-api"], false);
    let trust = make_trust(&[TEST_ISSUER]);

    let claims = serde_json::json!({
        "sub": "550e8400-e29b-41d4-a716-446655440000",
        "iss": TEST_ISSUER,
        "exp": future_exp(),
        "aud": "other-api",
    });
    let token = sign_jwt(&claims, Some(TEST_KID));

    let result = validator.validate(&token, &config, &trust).await;
    assert!(
        matches!(result, Err(AuthNError::InvalidAudience)),
        "present optional audience mismatch should be rejected when expected_audience is configured: {result:?}"
    );
}

#[tokio::test]
async fn test_bad_signature_rejected() {
    let (validator, _) = make_validator_with_test_key();
    let trust = make_trust(&[TEST_ISSUER]);

    let claims = serde_json::json!({
        "sub": "550e8400-e29b-41d4-a716-446655440000",
        "iss": TEST_ISSUER,
        "exp": future_exp(),
    });
    let token = sign_jwt(&claims, Some(TEST_KID));
    let parts: Vec<&str> = token.rsplitn(2, '.').collect();
    let corrupt_token = format!("{}.AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA", parts[1]);

    let result = validator
        .validate(&corrupt_token, &base_jwt_validation_config(), &trust)
        .await;
    assert!(
        matches!(result, Err(AuthNError::SignatureInvalid)),
        "corrupted signature should be rejected: {result:?}"
    );
}

#[tokio::test]
async fn metrics_increment_for_expired_token_rejection() {
    let (validator, harness) = make_validator_with_test_key();
    let trust = make_trust(&[TEST_ISSUER]);
    let claims = serde_json::json!({
        "sub": "550e8400-e29b-41d4-a716-446655440000",
        "iss": TEST_ISSUER,
        "exp": past_exp(),
    });
    let token = sign_jwt(&claims, Some(TEST_KID));

    let result = validator
        .validate(&token, &base_jwt_validation_config(), &trust)
        .await;
    assert!(
        matches!(result, Err(AuthNError::TokenExpired)),
        "expired token should be rejected: {result:?}"
    );

    harness.force_flush();
    let after = harness.counter_value(
        crate::domain::metrics::AUTHN_TOKEN_REJECTED_TOTAL,
        &[("reason", TOKEN_REJECTION_REASON_EXPIRED)],
    );
    assert!(after >= 1, "expired rejection counter should increase");
}

#[tokio::test]
async fn metrics_increment_for_untrusted_issuer_rejection() {
    let (validator, harness) = make_validator_with_test_key();
    let trust = make_trust(&["https://trusted.example.com/realms/platform"]);
    let claims = serde_json::json!({
        "sub": "550e8400-e29b-41d4-a716-446655440000",
        "iss": TEST_ISSUER,
        "exp": future_exp(),
    });
    let token = sign_jwt(&claims, Some(TEST_KID));

    let result = validator
        .validate(&token, &base_jwt_validation_config(), &trust)
        .await;
    assert!(
        matches!(result, Err(AuthNError::UntrustedIssuer)),
        "untrusted issuer should be rejected: {result:?}"
    );

    harness.force_flush();
    let after_reason = harness.counter_value(
        crate::domain::metrics::AUTHN_TOKEN_REJECTED_TOTAL,
        &[("reason", TOKEN_REJECTION_REASON_UNTRUSTED_ISSUER)],
    );

    assert!(
        after_reason >= 1,
        "untrusted issuer reason counter should increase"
    );
}

#[tokio::test]
async fn metrics_increment_for_jwks_refresh_failure() {
    let (validator, harness) = make_validator_with_test_key();
    let trust = make_trust(&[TEST_ISSUER]);
    let claims = serde_json::json!({
        "sub": "550e8400-e29b-41d4-a716-446655440000",
        "iss": TEST_ISSUER,
        "exp": future_exp(),
    });
    let token = sign_jwt(&claims, Some("rotated-kid-not-in-cache"));

    let result = validator
        .validate(&token, &base_jwt_validation_config(), &trust)
        .await;
    assert!(
        matches!(
            result,
            Err(AuthNError::KidNotFound | AuthNError::IdpUnreachable)
        ),
        "unknown kid path should fail closed: {result:?}"
    );

    harness.force_flush();
    let after = harness.counter_value(
        crate::domain::metrics::AUTHN_JWKS_REFRESH_FAILURES_TOTAL,
        &[],
    );
    assert!(after >= 1, "JWKS refresh failure counter should increase");
}
