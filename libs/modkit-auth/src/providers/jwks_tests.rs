use super::*;
use httpmock::prelude::*;

/// Create a test provider with insecure HTTP allowed (for httpmock) and no retries
fn test_provider_with_http(uri: &str) -> JwksKeyProvider {
    let client = modkit_http::HttpClient::builder()
        .timeout(Duration::from_secs(5))
        .retry(None)
        .build()
        .expect("failed to create test HTTP client");

    JwksKeyProvider {
        jwks_uri: uri.to_owned(),
        keys: Arc::new(ArcSwap::from_pointee(HashMap::new())),
        refresh_state: Arc::new(RwLock::new(RefreshState::default())),
        client,
        refresh_interval: Duration::from_secs(300),
        max_backoff: Duration::from_secs(3600),
        on_demand_refresh_cooldown: Duration::from_secs(60),
        header_extras_handler: None,
    }
}

/// Create a basic test provider (HTTPS only, for non-network tests)
fn test_provider(uri: &str) -> JwksKeyProvider {
    JwksKeyProvider::new(uri).expect("failed to create test provider")
}

/// Valid JWKS JSON response with a single RSA key
fn valid_jwks_json() -> &'static str {
    r#"{
        "keys": [{
            "kty": "RSA",
            "kid": "test-key-1",
            "use": "sig",
            "n": "0vx7agoebGcQSuuPiLJXZptN9nndrQmbXEps2aiAFbWhM78LhWx4cbbfAAtVT86zwu1RK7aPFFxuhDR1L6tSoc_BJECPebWKRXjBZCiFV4n3oknjhMstn64tZ_2W-5JsGY4Hc5n9yBXArwl93lqt7_RN5w6Cf0h4QyQ5v-65YGjQR0_FDW2QvzqY368QQMicAtaSqzs8KJZgnYb9c7d0zgdAZHzu6qMQvRL5hajrn1n91CbOpbISD08qNLyrdkt-bFTWhAI4vMQFh6WeZu0fM4lFd2NcRwr3XPksINHaQ-G_xBniIqbw0Ls1jF44-csFCur-kEgU8awapJzKnqDKgw",
            "e": "AQAB",
            "alg": "RS256"
        }]
    }"#
}

#[tokio::test]
async fn test_calculate_backoff() {
    let provider = test_provider("https://example.com/jwks");

    assert_eq!(provider.calculate_backoff(0), Duration::from_secs(60));
    assert_eq!(provider.calculate_backoff(1), Duration::from_secs(120));
    assert_eq!(provider.calculate_backoff(2), Duration::from_secs(240));
    assert_eq!(provider.calculate_backoff(3), Duration::from_secs(480));

    // Should cap at max_backoff
    assert_eq!(provider.calculate_backoff(100), provider.max_backoff);
}

#[tokio::test]
async fn test_should_refresh_on_first_call() {
    let provider = test_provider("https://example.com/jwks");
    assert!(provider.should_refresh().await);
}

#[tokio::test]
async fn test_key_storage() {
    let provider = test_provider("https://example.com/jwks");

    // Initially empty
    assert!(provider.get_key("test-kid").is_none());

    // Store a dummy key
    let mut keys = HashMap::new();
    keys.insert("test-kid".to_owned(), DecodingKey::from_secret(b"secret"));
    provider.keys.store(Arc::new(keys));

    // Should be retrievable
    assert!(provider.get_key("test-kid").is_some());
}

#[tokio::test]
async fn test_on_demand_refresh_returns_ok_when_key_exists() {
    let provider = test_provider("https://example.com/jwks");

    // Pre-populate with a key
    let mut keys = HashMap::new();
    keys.insert(
        "existing-kid".to_owned(),
        DecodingKey::from_secret(b"secret"),
    );
    provider.keys.store(Arc::new(keys));

    // Should return Ok immediately without any refresh
    let result = provider.on_demand_refresh("existing-kid").await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_try_new_returns_result() {
    // Valid URL should work
    let result = JwksKeyProvider::try_new("https://example.com/jwks");
    assert!(result.is_ok());
}

// ==================== httpmock-based tests ====================

#[tokio::test]
async fn test_fetch_jwks_success_with_valid_json() {
    let server = MockServer::start();

    let mock = server.mock(|when, then| {
        when.method(GET).path("/jwks");
        then.status(200)
            .header("content-type", "application/json")
            .body(valid_jwks_json());
    });

    let jwks_url = server.url("/jwks");
    let provider = test_provider_with_http(&jwks_url);

    let result = provider.perform_refresh().await;
    assert!(result.is_ok(), "Expected success, got: {result:?}");

    // Verify key was stored
    assert!(
        provider.get_key("test-key-1").is_some(),
        "Expected key 'test-key-1' to be stored"
    );

    mock.assert();
}

#[tokio::test]
async fn test_fetch_jwks_http_404_error_mapping() {
    let server = MockServer::start();

    let mock = server.mock(|when, then| {
        when.method(GET).path("/jwks");
        then.status(404).body("Not Found");
    });

    let jwks_url = server.url("/jwks");
    let provider = test_provider_with_http(&jwks_url);

    let result = provider.perform_refresh().await;
    assert!(result.is_err());

    let err = result.unwrap_err();
    let err_msg = err.to_string();
    assert!(
        err_msg.contains("JWKS HTTP 404"),
        "Expected error to contain 'JWKS HTTP 404', got: {err_msg}"
    );
    // Must NOT say "parse"
    assert!(
        !err_msg.to_lowercase().contains("parse"),
        "HTTP status error should not mention 'parse', got: {err_msg}"
    );

    mock.assert();
}

#[tokio::test]
async fn test_fetch_jwks_http_500_error_mapping() {
    let server = MockServer::start();

    let mock = server.mock(|when, then| {
        when.method(GET).path("/jwks");
        then.status(500).body("Internal Server Error");
    });

    let jwks_url = server.url("/jwks");
    let provider = test_provider_with_http(&jwks_url);

    let result = provider.perform_refresh().await;
    assert!(result.is_err());

    let err = result.unwrap_err();
    let err_msg = err.to_string();
    assert!(
        err_msg.contains("JWKS HTTP 500"),
        "Expected error to contain 'JWKS HTTP 500', got: {err_msg}"
    );

    mock.assert();
}

#[tokio::test]
async fn test_fetch_jwks_invalid_json_error_mapping() {
    let server = MockServer::start();

    let mock = server.mock(|when, then| {
        when.method(GET).path("/jwks");
        then.status(200)
            .header("content-type", "application/json")
            .body("this is not valid json");
    });

    let jwks_url = server.url("/jwks");
    let provider = test_provider_with_http(&jwks_url);

    let result = provider.perform_refresh().await;
    assert!(result.is_err());

    let err = result.unwrap_err();
    let err_msg = err.to_string();
    assert!(
        err_msg.contains("JWKS JSON parse failed"),
        "Expected error to contain 'JWKS JSON parse failed', got: {err_msg}"
    );

    mock.assert();
}

#[tokio::test]
async fn test_fetch_jwks_empty_keys_error() {
    let server = MockServer::start();

    let mock = server.mock(|when, then| {
        when.method(GET).path("/jwks");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"keys": []}"#);
    });

    let jwks_url = server.url("/jwks");
    let provider = test_provider_with_http(&jwks_url);

    let result = provider.perform_refresh().await;
    assert!(result.is_err());

    let err = result.unwrap_err();
    let err_msg = err.to_string();
    assert!(
        err_msg.contains("No valid RSA keys"),
        "Expected error about no RSA keys, got: {err_msg}"
    );

    mock.assert();
}

#[tokio::test]
async fn test_on_demand_refresh_respects_cooldown() {
    let server = MockServer::start();

    // First request will return 404
    let mock = server.mock(|when, then| {
        when.method(GET).path("/jwks");
        then.status(404).body("Not Found");
    });

    let jwks_url = server.url("/jwks");
    let provider =
        test_provider_with_http(&jwks_url).with_on_demand_refresh_cooldown(Duration::from_secs(60));

    // First attempt - should try to refresh and fail
    let result1 = provider.on_demand_refresh("test-kid").await;
    assert!(result1.is_err());

    // Immediate second attempt - should be throttled (no network call)
    let result2 = provider.on_demand_refresh("test-kid").await;
    assert!(result2.is_err());

    // Should return UnknownKeyId due to cooldown
    match result2.unwrap_err() {
        ClaimsError::UnknownKeyId(_) => {}
        other => panic!("Expected UnknownKeyId during cooldown, got: {other:?}"),
    }

    // Only one request should have been made (first attempt)
    mock.assert_calls(1);
}

#[tokio::test]
async fn test_on_demand_refresh_tracks_failed_kids() {
    let server = MockServer::start();

    server.mock(|when, then| {
        when.method(GET).path("/jwks");
        then.status(404).body("Not Found");
    });

    let jwks_url = server.url("/jwks");
    let provider = test_provider_with_http(&jwks_url)
        .with_on_demand_refresh_cooldown(Duration::from_millis(100));

    // Attempt refresh - will fail and track the kid
    let result = provider.on_demand_refresh("failed-kid").await;
    assert!(result.is_err());

    // Check that failed_kids contains the kid
    let state = provider.refresh_state.read().await;
    assert!(state.failed_kids.contains("failed-kid"));
}

#[tokio::test]
async fn test_perform_refresh_updates_state_on_failure() {
    let server = MockServer::start();

    server.mock(|when, then| {
        when.method(GET).path("/jwks");
        then.status(500).body("Server Error");
    });

    let jwks_url = server.url("/jwks");
    let provider = test_provider_with_http(&jwks_url);

    // Mark as previously failed
    {
        let mut state = provider.refresh_state.write().await;
        state.consecutive_failures = 3;
        state.last_error = Some("Previous error".to_owned());
    }

    // This will fail
    _ = provider.perform_refresh().await;

    // Check that consecutive_failures increased
    let state = provider.refresh_state.read().await;
    assert_eq!(state.consecutive_failures, 4);
    assert!(state.last_error.is_some());
}

#[tokio::test]
async fn test_perform_refresh_resets_state_on_success() {
    let server = MockServer::start();

    server.mock(|when, then| {
        when.method(GET).path("/jwks");
        then.status(200)
            .header("content-type", "application/json")
            .body(valid_jwks_json());
    });

    let jwks_url = server.url("/jwks");
    let provider = test_provider_with_http(&jwks_url);

    // Mark as previously failed
    {
        let mut state = provider.refresh_state.write().await;
        state.consecutive_failures = 5;
        state.last_error = Some("Previous error".to_owned());
    }

    // This should succeed
    let result = provider.perform_refresh().await;
    assert!(result.is_ok());

    // Check that state was reset
    let state = provider.refresh_state.read().await;
    assert_eq!(state.consecutive_failures, 0);
    assert!(state.last_error.is_none());
}

#[tokio::test]
async fn test_validate_and_decode_with_missing_kid() {
    let server = MockServer::start();

    // Return valid JWKS but without the requested kid
    server.mock(|when, then| {
        when.method(GET).path("/jwks");
        then.status(200)
            .header("content-type", "application/json")
            .body(valid_jwks_json());
    });

    let jwks_url = server.url("/jwks");
    let provider = test_provider_with_http(&jwks_url)
        .with_on_demand_refresh_cooldown(Duration::from_millis(100));

    // Create a minimal JWT with a kid that doesn't exist in JWKS
    // Header: {"alg":"RS256","kid":"nonexistent-kid"}
    let token = "eyJhbGciOiJSUzI1NiIsImtpZCI6Im5vbmV4aXN0ZW50LWtpZCJ9.\
                 eyJzdWIiOiIxMjM0NTY3ODkwIn0.invalid";

    // Should attempt on-demand refresh but kid still won't exist
    let result = provider.validate_and_decode(token).await;
    assert!(result.is_err());

    match result.unwrap_err() {
        ClaimsError::UnknownKeyId(kid) => {
            assert_eq!(kid, "nonexistent-kid");
        }
        other => panic!("Expected UnknownKeyId, got: {other:?}"),
    }
}

#[test]
fn test_decode_header_with_handler_coerces_non_string_extras() {
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};

    // Header with non-standard fields: integer, string, and array
    let header_json = r#"{"alg":"RS256","eap":1,"iri":"some-string-id","irn":["role_a"],"kid":"kid-1","typ":"at+jwt"}"#;
    let header_b64 = URL_SAFE_NO_PAD.encode(header_json.as_bytes());
    let payload_b64 = URL_SAFE_NO_PAD.encode(b"{}");
    let token = format!("{header_b64}.{payload_b64}.fake");

    let header = decode_header_with_handler(&token, &|_key, value| Some(value.to_string()))
        .expect("should handle non-standard header fields");

    assert_eq!(header.alg, jsonwebtoken::Algorithm::RS256);
    assert_eq!(header.kid.as_deref(), Some("kid-1"));
    assert_eq!(header.typ.as_deref(), Some("at+jwt"));

    // Non-string extras coerced to JSON text
    assert_eq!(header.extras.get("eap").map(String::as_str), Some("1"));
    assert_eq!(
        header.extras.get("irn").map(String::as_str),
        Some(r#"["role_a"]"#)
    );
    // String extras preserved as-is
    assert_eq!(
        header.extras.get("iri").map(String::as_str),
        Some("some-string-id")
    );
}

#[test]
fn test_decode_header_with_handler_can_drop_fields() {
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};

    let header_json = r#"{"alg":"RS256","eap":1,"iri":"keep-me","kid":"kid-1","typ":"JWT"}"#;
    let header_b64 = URL_SAFE_NO_PAD.encode(header_json.as_bytes());
    let token = format!("{header_b64}.e30.fake");

    let header = decode_header_with_handler(&token, &|_key, _value| None)
        .expect("should succeed when handler drops non-string fields");

    assert_eq!(header.alg, jsonwebtoken::Algorithm::RS256);
    assert!(!header.extras.contains_key("eap"));
    assert_eq!(
        header.extras.get("iri").map(String::as_str),
        Some("keep-me")
    );
}

#[tokio::test]
async fn test_with_header_extras_stringified_coerces_non_string_extras() {
    let server = MockServer::start();

    server.mock(|when, then| {
        when.method(GET).path("/jwks");
        then.status(200)
            .header("content-type", "application/json")
            .body(valid_jwks_json());
    });

    let jwks_url = server.url("/jwks");
    let provider = test_provider_with_http(&jwks_url).with_header_extras_stringified();

    // Header with non-string extras: integer and array
    let header_json = r#"{"alg":"RS256","kid":"test-key-1","typ":"JWT","eap":1,"irn":["role_a"]}"#;
    let header_b64 = URL_SAFE_NO_PAD.encode(header_json.as_bytes());
    let payload_b64 = URL_SAFE_NO_PAD.encode(b"{}");
    let token = format!("{header_b64}.{payload_b64}.AAAA");

    let result = provider.validate_and_decode(&token).await;

    // The handler lets header decode succeed; error must come from signature
    // validation, not from header parsing.
    let err = result.expect_err("fake signature should fail validation");
    assert!(
        matches!(
            &err,
            ClaimsError::InvalidSignature | ClaimsError::DecodeFailed(_)
        ),
        "Expected signature-related error, got: {err:?}"
    );
}

#[tokio::test]
async fn test_validate_and_decode_uses_header_extras_handler() {
    let server = MockServer::start();

    server.mock(|when, then| {
        when.method(GET).path("/jwks");
        then.status(200)
            .header("content-type", "application/json")
            .body(valid_jwks_json());
    });

    let jwks_url = server.url("/jwks");
    let provider = test_provider_with_http(&jwks_url)
        .with_header_extras_handler(|_key, value| Some(value.to_string()));

    // Header with a non-string extra ("eap":1) that would reject without handler
    let header_json = r#"{"alg":"RS256","kid":"test-key-1","typ":"JWT","eap":1}"#;
    let header_b64 = URL_SAFE_NO_PAD.encode(header_json.as_bytes());
    let payload_b64 = URL_SAFE_NO_PAD.encode(b"{}");
    let token = format!("{header_b64}.{payload_b64}.AAAA");

    let result = provider.validate_and_decode(&token).await;

    // Handler lets header decode succeed → error must come from signature
    // validation, not from header parsing.
    let err = result.expect_err("fake signature should fail validation");
    assert!(
        matches!(
            &err,
            ClaimsError::InvalidSignature | ClaimsError::DecodeFailed(_)
        ),
        "Expected signature-related error, got: {err:?}"
    );
}

/// RSA private key (PKCS#8 PEM) used to sign test JWTs.
/// The matching public-key components (n, e) are served by `signed_jwks_json()`.
const TEST_RSA_PRIVATE_PEM: &[u8] = b"-----BEGIN PRIVATE KEY-----
MIIEvgIBADANBgkqhkiG9w0BAQEFAASCBKgwggSkAgEAAoIBAQCohcw9B9YK7ULF
KgrGNJKAH0BH9CpJB03wIkQl6ECCJ/BfmBsNSWwZdnG0cWwwGhsSSSj32AKB+t6W
44/vi9hv+PHusIRCMNqM/AJ/zA7xau9mNsxS8U8J3olm74vLFtF05hTRmJuefMmz
mOt4kMP44UeVg0nyFlToa0SmhMxIeFgz2VgktHjHDe/rr/FdrjMwxesz3ezj+Y4k
YPPrQfMZJTyEd68M+pPkjyg6AkakNSUJp+dZibnRLKcj6Ehz1W3lSGkaQ4YFSXVX
UCaHWNmPsJHejwKrUA/fbkYi3sLO7cW/4h+b2laWsL9qC4P2RJMbZBzklJoL+WoH
Lo5zUvo7AgMBAAECggEACrynlBXdOcn/EI/KqvErilUzY8I3NXrtKMkOHXosLf68
bmLDCngslny45t25HmFzaxlVLmFJW52vs95gy8rVqeCrDWGas5roOcZOpHTMWO5O
vWztXLV6Ky9OAsxtVC2qf6+vEOGPvKvHsBUkn4RdsAwuYuS//9gTZdF7yL46Q72o
pJ8bLUZBpqmVNyLxyfbFn8u9j71zMUweB9vOMYAIAv1cYRa/0bVYLIZumcotY822
B0ny1fLru1gDJt2p1DL9fQTg16pBYr1V0nhoiktS8Lx5PFLMI+NhmalBerqtPN+u
qqauu9jolmXtydfOP7pTN2sqGFAKlcx55KZlVLK2YQKBgQDaiRxPXnFCPY4yYBxS
POFJe8UcvoM3d5HGwQfbJ5PHq+YN8NW0ACaox6QQkQYmE9OHriHrVmp4af6erN2K
zbjmL41E5C4MzEau2ipZWY4GA+lLXomEiHsUD0cfqfL+7Fs6ufiG2nXrWIBXggz8
8mTdP/LHMPybY0wxoZI5Xij+2wKBgQDFacPh+PhT0U8wu7nSgvQ85ozJN7TWq0KD
TgWuZ0W6L5OlAAVernYuvvRH/Uy9JqVfX4KLHbcEcdUx8t5usKMf8S3kQyMM8xK+
KaEYZNOMdA6E9PAJVD8crDQT/QD6/+oHrTTFFKxW7jWLY1ggWXVHk4CxLXBlDnKQ
xIA5DuhgIQKBgQCA5Km77loi1aeO8r0BjELcUpH52CwQhQeIEMYPbpJtDGhOBKQm
3IfwuH99/euAfeUfe4cqBPgbOXkiIZcxjRDnQ1ixL1wx1DJEYwzjUjzAM4JgH8xA
TTc6p6AtftGBpepRAusgrq0qODLKajw63MS88kDBV5VGGRURmNhj2bOYTQKBgHPr
hiVj/9Wf+6M/KH9vfCFis9rYBi1jxRu7LeTaKXyJwWXLHFwbj7QlVuYK3AvZ7JOT
TuGHoldOzISW+3v95tuz0GHP9n39Ic1ePoVHd11rLLdv6J9hw+l/SNlP4EqDCZZW
Y70yRXyKRhDCVhYw0YglGhVv/CarFCTj7fMTSOphAoGBAJcM4H4qmCFLdR9FRQgT
YJPGcyjWPmm9tlb8M6rSJGPlfpAhKjRVGWwpHPiUnvrW296QKr9+5q43HRcK3qa5
GU5n8VxYiniVFVMSEpLJgvu7hGq5fmMiRTTot1pOTSXZ1LY6rDQvjsTeGQumb/Eo
F8gvjIeiwVfp4nDnO2JFexiy
-----END PRIVATE KEY-----";

/// JWKS JSON whose public key matches `TEST_RSA_PRIVATE_PEM`.
fn signed_jwks_json() -> &'static str {
    r#"{
        "keys": [{
            "kty": "RSA",
            "kid": "sign-key-1",
            "use": "sig",
            "n": "qIXMPQfWCu1CxSoKxjSSgB9AR_QqSQdN8CJEJehAgifwX5gbDUlsGXZxtHFsMBobEkko99gCgfreluOP74vYb_jx7rCEQjDajPwCf8wO8WrvZjbMUvFPCd6JZu-LyxbRdOYU0ZibnnzJs5jreJDD-OFHlYNJ8hZU6GtEpoTMSHhYM9lYJLR4xw3v66_xXa4zMMXrM93s4_mOJGDz60HzGSU8hHevDPqT5I8oOgJGpDUlCafnWYm50SynI-hIc9Vt5UhpGkOGBUl1V1Amh1jZj7CR3o8Cq1AP325GIt7Czu3Fv-Ifm9pWlrC_aguD9kSTG2Qc5JSaC_lqBy6Oc1L6Ow",
            "e": "AQAB",
            "alg": "RS256"
        }]
    }"#
}

/// Build a properly-signed RS256 JWT for testing.
fn build_signed_jwt(kid: &str, claims: &serde_json::Value) -> String {
    let encoding_key = jsonwebtoken::EncodingKey::from_rsa_pem(TEST_RSA_PRIVATE_PEM)
        .expect("test RSA PEM should be valid");
    let mut header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256);
    header.kid = Some(kid.to_owned());
    jsonwebtoken::encode(&header, claims, &encoding_key).expect("JWT signing should succeed")
}

#[tokio::test]
async fn test_validate_and_decode_happy_path() {
    let server = MockServer::start();

    server.mock(|when, then| {
        when.method(GET).path("/jwks");
        then.status(200)
            .header("content-type", "application/json")
            .body(signed_jwks_json());
    });

    let jwks_url = server.url("/jwks");
    let provider = test_provider_with_http(&jwks_url);

    let claims = serde_json::json!({
        "sub": "user-42",
        "name": "Test User",
        "iat": 1_700_000_000u64
    });
    let token = build_signed_jwt("sign-key-1", &claims);

    let (header, decoded_claims) = provider
        .validate_and_decode(&token)
        .await
        .expect("validate_and_decode should succeed for a properly signed token");

    assert_eq!(header.alg, jsonwebtoken::Algorithm::RS256);
    assert_eq!(header.kid.as_deref(), Some("sign-key-1"));
    assert_eq!(decoded_claims["sub"], "user-42");
    assert_eq!(decoded_claims["name"], "Test User");
}

#[tokio::test]
async fn test_validate_and_decode_with_bearer_prefix() {
    let server = MockServer::start();

    server.mock(|when, then| {
        when.method(GET).path("/jwks");
        then.status(200)
            .header("content-type", "application/json")
            .body(signed_jwks_json());
    });

    let jwks_url = server.url("/jwks");
    let provider = test_provider_with_http(&jwks_url);

    let claims = serde_json::json!({"sub": "user-99"});
    let token = format!("Bearer {}", build_signed_jwt("sign-key-1", &claims));

    let (_, decoded_claims) = provider
        .validate_and_decode(&token)
        .await
        .expect("should strip Bearer prefix and succeed");

    assert_eq!(decoded_claims["sub"], "user-99");
}

#[tokio::test]
async fn test_validate_and_decode_rejects_tampered_payload() {
    let server = MockServer::start();

    server.mock(|when, then| {
        when.method(GET).path("/jwks");
        then.status(200)
            .header("content-type", "application/json")
            .body(signed_jwks_json());
    });

    let jwks_url = server.url("/jwks");
    let provider = test_provider_with_http(&jwks_url);

    let claims = serde_json::json!({"sub": "legit"});
    let token = build_signed_jwt("sign-key-1", &claims);

    // Tamper with the payload segment
    let parts: Vec<&str> = token.splitn(3, '.').collect();
    let tampered_payload = URL_SAFE_NO_PAD.encode(br#"{"sub":"evil"}"#);
    let tampered_token = format!("{}.{}.{}", parts[0], tampered_payload, parts[2]);

    let err = provider
        .validate_and_decode(&tampered_token)
        .await
        .expect_err("tampered token should fail signature verification");

    assert!(
        matches!(err, ClaimsError::InvalidSignature),
        "Expected InvalidSignature, got: {err:?}"
    );
}

/// Build a JWT with a custom header JSON (for non-string extras), properly signed.
fn build_signed_jwt_custom_header(header_json: &str, claims: &serde_json::Value) -> String {
    let encoding_key = jsonwebtoken::EncodingKey::from_rsa_pem(TEST_RSA_PRIVATE_PEM)
        .expect("test RSA PEM should be valid");
    let header_b64 = URL_SAFE_NO_PAD.encode(header_json.as_bytes());
    let payload_b64 = URL_SAFE_NO_PAD.encode(serde_json::to_vec(claims).unwrap());
    let message = format!("{header_b64}.{payload_b64}");
    let signature = jsonwebtoken::crypto::sign(
        message.as_bytes(),
        &encoding_key,
        jsonwebtoken::Algorithm::RS256,
    )
    .expect("signing should succeed");
    format!("{message}.{signature}")
}

#[tokio::test]
async fn test_validate_and_decode_with_non_string_header_extras() {
    let server = MockServer::start();

    server.mock(|when, then| {
        when.method(GET).path("/jwks");
        then.status(200)
            .header("content-type", "application/json")
            .body(signed_jwks_json());
    });

    let jwks_url = server.url("/jwks");
    let provider = test_provider_with_http(&jwks_url).with_header_extras_stringified();

    let claims = serde_json::json!({"sub": "user-extras"});
    let header_json = r#"{"alg":"RS256","kid":"sign-key-1","typ":"JWT","eap":1}"#;
    let token = build_signed_jwt_custom_header(header_json, &claims);

    let (header, decoded_claims) = provider
        .validate_and_decode(&token)
        .await
        .expect("should decode JWT with non-string header extras when handler is set");

    assert_eq!(header.alg, jsonwebtoken::Algorithm::RS256);
    assert_eq!(header.kid.as_deref(), Some("sign-key-1"));
    assert_eq!(header.extras.get("eap").map(String::as_str), Some("1"));
    assert_eq!(decoded_claims["sub"], "user-extras");
}

#[test]
fn test_decode_header_without_handler_rejects_non_string_extras() {
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};

    let header_json = r#"{"alg":"RS256","eap":1,"kid":"kid-1","typ":"JWT"}"#;
    let header_b64 = URL_SAFE_NO_PAD.encode(header_json.as_bytes());
    let token = format!("{header_b64}.e30.fake");

    let result = decode_header(&token);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("invalid type: integer"),
        "expected type error, got: {err}"
    );
}
