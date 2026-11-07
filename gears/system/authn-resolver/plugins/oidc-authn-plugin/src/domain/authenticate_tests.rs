use std::sync::Arc;
use toolkit_gts::gts_id;

use async_trait::async_trait;
use authn_resolver_sdk::ClientCredentialsRequest;
use jsonwebtoken::jwk::JwkSet;
use reqwest::Url;

use super::*;
use crate::config::INSTANCE_SUFFIX;
use crate::domain::metrics::test_harness::MetricsHarness;
use crate::domain::ports::{ClientCredentialsExchanger, JwksProvider};

use crate::test_support::test_fixtures::{
    TEST_ISSUER, TEST_KID, future_exp, sign_jwt, test_jwk_json,
};

const TEST_S2S_DEFAULT_SUBJECT_TYPE: &str = gts_id!("cf.core.security.subject_user.v1~");

fn test_issuer_trust() -> IssuerTrustConfig {
    IssuerTrustConfig::from_exact_issuers(vec![TEST_ISSUER.to_owned()])
        .expect("trust config should build")
}

fn base_jwt_validation_config() -> JwtValidationConfig {
    JwtValidationConfig {
        supported_algorithms: vec![
            jsonwebtoken::Algorithm::RS256,
            jsonwebtoken::Algorithm::ES256,
        ],
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

struct StaticJwksProvider {
    jwks: Option<Arc<JwkSet>>,
}

impl StaticJwksProvider {
    fn available() -> Self {
        let jwks: JwkSet = serde_json::from_str(test_jwk_json()).expect("test JWKS should parse");
        Self {
            jwks: Some(Arc::new(jwks)),
        }
    }

    fn unavailable() -> Self {
        Self { jwks: None }
    }
}

#[async_trait]
impl JwksProvider for StaticJwksProvider {
    async fn get_jwks(
        &self,
        _issuer: &str,
        _discovery_base: &Url,
    ) -> Result<Arc<JwkSet>, AuthNError> {
        self.jwks
            .as_ref()
            .map(Arc::clone)
            .ok_or(AuthNError::IdpUnreachable)
    }

    async fn force_refresh(
        &self,
        issuer: &str,
        discovery_base: &Url,
    ) -> Result<Arc<JwkSet>, AuthNError> {
        self.get_jwks(issuer, discovery_base).await
    }
}

struct DisabledTokenExchanger;

#[async_trait]
impl ClientCredentialsExchanger for DisabledTokenExchanger {
    async fn exchange(
        &self,
        _request: &ClientCredentialsRequest,
        _issuer_trust: &IssuerTrustConfig,
    ) -> Result<crate::domain::ports::ExchangedToken, AuthNError> {
        Err(AuthNError::TokenEndpointNotConfigured)
    }
}

fn make_plugin() -> OidcAuthNPlugin {
    OidcAuthNPluginBuilder::new(
        base_jwt_validation_config(),
        test_issuer_trust(),
        TEST_S2S_DEFAULT_SUBJECT_TYPE.to_owned(),
    )
    .build(
        Arc::new(StaticJwksProvider::available()),
        Arc::new(DisabledTokenExchanger),
        MetricsHarness::new().metrics(),
    )
}

fn sign_valid_jwt() -> String {
    let claims = serde_json::json!({
        "sub": "550e8400-e29b-41d4-a716-446655440000",
        "iss": TEST_ISSUER,
        "exp": future_exp(),
        "tenant_id": "550e8400-e29b-41d4-a716-446655440111",
    });
    sign_jwt(&claims, Some(TEST_KID))
}

fn jwt_plugin_with_jwks_provider(
    provider: StaticJwksProvider,
) -> (OidcAuthNPlugin, MetricsHarness) {
    let jwt_config = base_jwt_validation_config();
    let metrics = MetricsHarness::new();

    (
        OidcAuthNPluginBuilder::new(
            jwt_config,
            test_issuer_trust(),
            TEST_S2S_DEFAULT_SUBJECT_TYPE.to_owned(),
        )
        .build(
            Arc::new(provider),
            Arc::new(DisabledTokenExchanger),
            metrics.metrics(),
        ),
        metrics,
    )
}

#[tokio::test]
async fn test_opaque_token_rejected_without_network() {
    let result = make_plugin().authenticate("opaque-no-dots").await;
    assert!(
        matches!(result, Err(AuthNError::UnsupportedTokenFormat)),
        "opaque token -> UnsupportedTokenFormat immediately: {result:?}"
    );
}

#[tokio::test]
async fn test_authenticate_validates_with_jwks_provider() {
    let (plugin, _) = jwt_plugin_with_jwks_provider(StaticJwksProvider::available());
    let token = sign_valid_jwt();
    let result =
        <OidcAuthNPlugin as AuthNResolverPluginClient>::authenticate(&plugin, &request(&token))
            .await;

    assert!(
        result.is_ok(),
        "provided JWKS should allow local JWT validation: {result:?}"
    );
}

#[tokio::test]
async fn test_jwks_provider_unavailable_returns_service_unavailable() {
    let (plugin, harness) = jwt_plugin_with_jwks_provider(StaticJwksProvider::unavailable());
    let token = sign_valid_jwt();

    let result =
        <OidcAuthNPlugin as AuthNResolverPluginClient>::authenticate(&plugin, &request(&token))
            .await;

    assert!(
        matches!(
            result,
            Err(AuthNResolverError::ServiceUnavailable(ref msg))
            if msg == "identity provider unreachable"
        ),
        "cold cache with IdP unreachable should fail closed with ServiceUnavailable: {result:?}"
    );

    harness.force_flush();
    let failures = harness.counter_value(
        crate::domain::metrics::AUTHN_REQUEST_FAILURES_TOTAL,
        &[("reason", "service_unavailable")],
    );
    assert!(failures >= 1, "request failure counter should increase");
}

#[tokio::test]
async fn metrics_increment_for_successful_jwt_validation() {
    let (plugin, harness) = jwt_plugin_with_jwks_provider(StaticJwksProvider::available());
    let token = sign_valid_jwt();
    let result =
        <OidcAuthNPlugin as AuthNResolverPluginClient>::authenticate(&plugin, &request(&token))
            .await;
    assert!(result.is_ok(), "JWT path should succeed: {result:?}");

    harness.force_flush();

    let request_duration_samples = harness.histogram_count(
        crate::domain::metrics::AUTHN_REQUEST_SUCCESS_DURATION_SECONDS,
        &[],
    );
    let after_hist = harness.histogram_count(
        crate::domain::metrics::AUTHN_JWT_VALIDATION_DURATION_SECONDS,
        &[],
    );

    assert!(
        request_duration_samples >= 1,
        "successful request duration histogram should record at least one sample"
    );
    assert!(
        after_hist >= 1,
        "JWT validation histogram should record at least one sample"
    );
}

#[test]
fn register_uses_canonical_instance_scope() {
    let hub = ClientHub::new();
    let plugin = Arc::new(make_plugin());
    let registered_scope = plugin.register(&hub).expect("registration should succeed");
    let instance_id = AuthNResolverPluginSpecV1::gts_make_instance_id(INSTANCE_SUFFIX);
    let expected_scope = ClientScope::gts_id(instance_id.as_ref());
    assert_eq!(
        registered_scope.as_str(),
        expected_scope.as_str(),
        "registration scope should be canonical plugin instance ID"
    );
    assert!(
        hub.try_get_scoped::<dyn AuthNResolverPluginClient>(&registered_scope)
            .is_some(),
        "registered scope should resolve a plugin client from ClientHub"
    );
}

#[test]
fn jwt_claims_to_map_includes_all_populated_fields() {
    let jwt_claims = JwtClaims {
        sub: "sub-val".to_owned(),
        iss: "iss-val".to_owned(),
        exp: 12345,
        iat: Some(67890),
        aud: Some(serde_json::Value::String("aud-val".to_owned())),
        azp: Some("azp-val".to_owned()),
        client_id: Some("client-val".to_owned()),
        tenant_id: Some("tenant-val".to_owned()),
        user_type: Some("type-val".to_owned()),
        scope: Some("scope-val".to_owned()),
        extra: serde_json::Map::new(),
    };
    let map = super::jwt_claims_to_map(jwt_claims);
    assert_eq!(
        map.len(),
        10,
        "all JwtClaims fields must be present when set; \
         if you added a field to JwtClaims, update this assertion"
    );
}

#[test]
fn jwt_claims_to_map_produces_correct_entries() {
    let jwt_claims = JwtClaims {
        sub: "550e8400-e29b-41d4-a716-446655440000".to_owned(),
        iss: "https://oidc.example.com/realms/platform".to_owned(),
        exp: 9_999_999_999,
        iat: None,
        aud: None,
        azp: Some("cyber-fabric-portal".to_owned()),
        client_id: None,
        tenant_id: Some("tenant-abc".to_owned()),
        user_type: None,
        scope: Some("read write".to_owned()),
        extra: serde_json::Map::new(),
    };
    let map = super::jwt_claims_to_map(jwt_claims);
    assert_eq!(
        map.get("sub").and_then(|v| v.as_str()),
        Some("550e8400-e29b-41d4-a716-446655440000")
    );
    assert_eq!(
        map.get("iss").and_then(|v| v.as_str()),
        Some("https://oidc.example.com/realms/platform")
    );
    assert_eq!(
        map.get("exp").and_then(serde_json::Value::as_u64),
        Some(9_999_999_999)
    );
    assert_eq!(
        map.get("azp").and_then(|v| v.as_str()),
        Some("cyber-fabric-portal")
    );
    assert_eq!(
        map.get("tenant_id").and_then(|v| v.as_str()),
        Some("tenant-abc")
    );
    assert_eq!(
        map.get("scope").and_then(|v| v.as_str()),
        Some("read write")
    );
    assert!(map.get("aud").is_none(), "None fields should be omitted");
    assert!(
        map.get("client_id").is_none(),
        "None fields should be omitted"
    );
    assert!(
        map.get("user_type").is_none(),
        "None fields should be omitted"
    );
}

#[test]
fn non_standard_claims_survive_into_map() {
    // Cap tokens carry claims under names the struct does not declare
    // (`subject_tenant`, `scopes`, `context_tenant`). They must be preserved via
    // `extra` so claim mapping can be configured to read them.
    let payload = serde_json::json!({
        "sub": "550e8400-e29b-41d4-a716-446655440000",
        "iss": "https://core.example/issuers/cap",
        "exp": 9_999_999_999u64,
        "subject_tenant": "11111111-1111-1111-1111-111111111111",
        "scopes": "rms.read rms.write",
        "context_tenant": "22222222-2222-2222-2222-222222222222"
    });
    let jwt_claims: JwtClaims =
        serde_json::from_value(payload).expect("cap claims should deserialize");
    let map = super::jwt_claims_to_map(jwt_claims);
    assert_eq!(
        map.get("subject_tenant").and_then(|v| v.as_str()),
        Some("11111111-1111-1111-1111-111111111111"),
        "non-standard claim must survive the JwtClaims round-trip"
    );
    assert_eq!(
        map.get("scopes").and_then(|v| v.as_str()),
        Some("rms.read rms.write")
    );
    assert_eq!(
        map.get("context_tenant").and_then(|v| v.as_str()),
        Some("22222222-2222-2222-2222-222222222222")
    );
    // Standard fields still decode into their own slots, not `extra`.
    assert_eq!(
        map.get("exp").and_then(serde_json::Value::as_u64),
        Some(9_999_999_999)
    );
}

#[tokio::test]
async fn concurrent_authenticate_calls_all_succeed() {
    let (plugin, _) = jwt_plugin_with_jwks_provider(StaticJwksProvider::available());
    let plugin = Arc::new(plugin);
    let mut handles = Vec::new();
    for _ in 0..20 {
        let p = Arc::clone(&plugin);
        let token = sign_valid_jwt();
        handles.push(tokio::spawn(async move {
            <OidcAuthNPlugin as AuthNResolverPluginClient>::authenticate(
                p.as_ref(),
                &request(&token),
            )
            .await
        }));
    }

    for handle in handles {
        let result = handle.await.expect("task should not panic");
        assert!(
            result.is_ok(),
            "concurrent authentication should succeed: {result:?}"
        );
    }
}

#[tokio::test]
async fn es256_token_authenticates_with_ec_jwk() {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use toolkit_security::crypto::signature::EcdsaP256KeyPair;
    let bytes = EcdsaP256KeyPair::generate_pkcs8().unwrap();
    let pair = EcdsaP256KeyPair::from_pkcs8(&bytes).unwrap();
    let point = pair.public_key();
    let jwks: JwkSet = serde_json::from_value(serde_json::json!({"keys": [{
        "kty": "EC", "crv": "P-256", "alg": "ES256", "use": "sig", "kid": TEST_KID,
        "x": URL_SAFE_NO_PAD.encode(&point[1..33]), "y": URL_SAFE_NO_PAD.encode(&point[33..]),
    }]}))
    .unwrap();
    let (plugin, _) = jwt_plugin_with_jwks_provider(StaticJwksProvider {
        jwks: Some(Arc::new(jwks)),
    });
    let mut header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::ES256);
    header.kid = Some(TEST_KID.to_owned());
    let claims = serde_json::json!({
        "sub": "550e8400-e29b-41d4-a716-446655440000", "iss": TEST_ISSUER,
        "exp": future_exp(), "tenant_id": "550e8400-e29b-41d4-a716-446655440111",
    });
    // Independent AWS-LC JWT signing oracle; production verification uses the facade.
    let token = jsonwebtoken::encode(
        &header,
        &claims,
        &jsonwebtoken::EncodingKey::from_ec_der(&bytes),
    )
    .unwrap();
    assert!(
        <OidcAuthNPlugin as AuthNResolverPluginClient>::authenticate(&plugin, &request(&token))
            .await
            .is_ok()
    );
    let (message, signature) = token.rsplit_once('.').unwrap();
    let mut signature = URL_SAFE_NO_PAD.decode(signature).unwrap();
    signature[0] ^= 1;
    let tampered = format!("{message}.{}", URL_SAFE_NO_PAD.encode(signature));
    assert!(
        <OidcAuthNPlugin as AuthNResolverPluginClient>::authenticate(&plugin, &request(&tampered))
            .await
            .is_err()
    );
}

struct FixedTokenExchanger(crate::domain::ports::ExchangedToken);

#[async_trait]
impl ClientCredentialsExchanger for FixedTokenExchanger {
    async fn exchange(
        &self,
        _request: &ClientCredentialsRequest,
        _issuer_trust: &IssuerTrustConfig,
    ) -> Result<crate::domain::ports::ExchangedToken, AuthNError> {
        Ok(self.0.clone())
    }
}

#[tokio::test]
async fn exchanged_context_uses_earliest_original_expiry_and_finite_reuse_policy() {
    use crate::domain::ports::ExchangedToken;
    use time::OffsetDateTime;

    let now = OffsetDateTime::now_utc();
    let jwt_expiry =
        OffsetDateTime::from_unix_timestamp(i64::try_from(future_exp()).unwrap()).unwrap();
    let claims = serde_json::json!({
        "sub": "550e8400-e29b-41d4-a716-446655440000",
        "iss": TEST_ISSUER,
        "exp": jwt_expiry.unix_timestamp(),
        "tenant_id": "550e8400-e29b-41d4-a716-446655440111",
    });
    let token = sign_jwt(&claims, Some(TEST_KID));
    let request = ClientCredentialsRequest::new("svc", secrecy::SecretString::from("secret"));
    for oauth_expiry in [
        None,
        // Already elapsed: no reuse even when the lower cache allows it.
        Some(now - std::time::Duration::from_secs(1)),
        Some(now + std::time::Duration::from_secs(10)),
        Some(jwt_expiry + std::time::Duration::from_secs(10)),
    ] {
        for cache_until in [
            now - std::time::Duration::from_secs(1),
            now + std::time::Duration::from_secs(100),
        ] {
            let plugin = OidcAuthNPluginBuilder::new(
                base_jwt_validation_config(),
                test_issuer_trust(),
                TEST_S2S_DEFAULT_SUBJECT_TYPE.to_owned(),
            )
            .build(
                Arc::new(StaticJwksProvider::available()),
                Arc::new(FixedTokenExchanger(ExchangedToken {
                    access_token: token.clone().into(),
                    expires_at: oauth_expiry,
                    cache_until,
                })),
                MetricsHarness::new().metrics(),
            );
            let result = plugin.exchange_client_credentials(&request).await.unwrap();
            let expected_expiry = oauth_expiry.map_or(jwt_expiry, |expiry| expiry.min(jwt_expiry));
            assert_eq!(result.security_context.expires_at(), Some(expected_expiry));
            let reuse_until = cache_until.min(expected_expiry);
            assert_eq!(
                result.cache_control.cache_until,
                (reuse_until > now).then_some(reuse_until),
                "oauth {oauth_expiry:?}, lower cache until {cache_until}"
            );
        }
    }
}

#[tokio::test]
async fn bearer_context_propagates_validated_jwt_exp_without_opt_in_to_reuse() {
    let expiry = future_exp();
    let token = sign_jwt(
        &serde_json::json!({
            "sub": "550e8400-e29b-41d4-a716-446655440000",
            "iss": TEST_ISSUER,
            "exp": expiry,
            "tenant_id": "550e8400-e29b-41d4-a716-446655440111",
        }),
        Some(TEST_KID),
    );
    let result = AuthNResolverPluginClient::authenticate(&make_plugin(), &request(&token))
        .await
        .unwrap();
    assert_eq!(
        result
            .security_context
            .expires_at()
            .unwrap()
            .unix_timestamp(),
        i64::try_from(expiry).unwrap()
    );
    assert!(result.cache_control.is_no_store());
}

#[tokio::test]
async fn bearer_token_with_an_unrepresentable_exp_is_rejected_after_validation() {
    // Valid JWT, but year 10000 exceeds OffsetDateTime's range.
    // Reject during mapping rather than publish an unknown expiry.
    let token = sign_jwt(
        &serde_json::json!({
            "sub": "550e8400-e29b-41d4-a716-446655440000",
            "iss": TEST_ISSUER,
            "exp": 253_402_300_800_u64,
            "tenant_id": "550e8400-e29b-41d4-a716-446655440111",
        }),
        Some(TEST_KID),
    );

    let err = AuthNResolverPluginClient::authenticate(&make_plugin(), &request(&token))
        .await
        .unwrap_err();

    assert!(
        matches!(&err, AuthNResolverError::Unauthorized(msg) if msg == "invalid credential expiry"),
        "{err:?}"
    );
}

// --- caller reuse policy for bearer-token results ---------------------------

/// Serves the test JWKS after `delay`, to model slow validation.
struct DelayedJwksProvider {
    inner: StaticJwksProvider,
    delay: std::time::Duration,
}

#[async_trait]
impl JwksProvider for DelayedJwksProvider {
    async fn get_jwks(&self, issuer: &str, base: &Url) -> Result<Arc<JwkSet>, AuthNError> {
        tokio::time::sleep(self.delay).await;
        self.inner.get_jwks(issuer, base).await
    }

    async fn force_refresh(&self, issuer: &str, base: &Url) -> Result<Arc<JwkSet>, AuthNError> {
        self.get_jwks(issuer, base).await
    }
}

fn plugin_with_result_ttl(
    ttl: std::time::Duration,
    jwks: Arc<dyn JwksProvider>,
) -> OidcAuthNPlugin {
    OidcAuthNPluginBuilder::new(
        base_jwt_validation_config(),
        test_issuer_trust(),
        TEST_S2S_DEFAULT_SUBJECT_TYPE.to_owned(),
    )
    .authenticate_result_cache_ttl(ttl)
    .build(
        jwks,
        Arc::new(DisabledTokenExchanger),
        MetricsHarness::new().metrics(),
    )
}

fn jwt_expiring_in(secs: i64) -> String {
    let exp = time::OffsetDateTime::now_utc().unix_timestamp() + secs;
    sign_jwt(
        &serde_json::json!({
            "sub": "550e8400-e29b-41d4-a716-446655440000",
            "iss": TEST_ISSUER,
            "exp": exp,
            "tenant_id": "550e8400-e29b-41d4-a716-446655440111",
        }),
        Some(TEST_KID),
    )
}

async fn authenticate_result(plugin: &OidcAuthNPlugin, token: &str) -> AuthenticationResult {
    <OidcAuthNPlugin as AuthNResolverPluginClient>::authenticate(plugin, &request(token))
        .await
        .expect("token should validate")
}

#[tokio::test]
async fn bearer_results_are_no_store_by_default() {
    let result = authenticate_result(&make_plugin(), &sign_valid_jwt()).await;
    assert!(result.cache_control.is_no_store());
}

#[tokio::test]
async fn a_configured_ttl_allows_reuse_from_validation_start_bounded_by_exp() {
    let ttl = std::time::Duration::from_secs(120);
    let plugin = plugin_with_result_ttl(ttl, Arc::new(StaticJwksProvider::available()));

    let before = time::OffsetDateTime::now_utc();
    let result = authenticate_result(&plugin, &jwt_expiring_in(3600)).await;
    let after = time::OffsetDateTime::now_utc();
    let until = result.cache_control.cache_until.expect("reuse allowed");
    assert!(until >= before + ttl && until <= after + ttl, "{until}");

    let short = authenticate_result(&plugin, &jwt_expiring_in(30)).await;
    assert_eq!(
        short.cache_control.cache_until,
        short.security_context.expires_at()
    );
}

#[tokio::test]
async fn slow_validation_does_not_extend_reuse_past_its_start_plus_ttl() {
    let jwks = DelayedJwksProvider {
        inner: StaticJwksProvider::available(),
        delay: std::time::Duration::from_millis(300),
    };
    let plugin = plugin_with_result_ttl(std::time::Duration::from_millis(200), Arc::new(jwks));
    let result = authenticate_result(&plugin, &jwt_expiring_in(3600)).await;
    assert!(
        result.cache_control.is_no_store(),
        "the window closed during validation: {:?}",
        result.cache_control
    );
}

#[test]
fn the_reuse_policy_requires_a_positive_ttl_and_a_future_known_expiry() {
    let start = time::OffsetDateTime::from_unix_timestamp(1_900_000_000).unwrap();
    let ttl = std::time::Duration::from_secs(60);
    let secs = |n: i64| start + time::Duration::seconds(n);
    let policy = |ttl, exp, now| authenticate_cache_control(ttl, start, exp, now).cache_until;

    assert_eq!(
        policy(std::time::Duration::ZERO, Some(secs(600)), start),
        None,
        "default"
    );
    assert_eq!(policy(ttl, None, start), None, "unknown expiry");
    assert_eq!(
        policy(ttl, Some(secs(-1)), start),
        None,
        "accepted within leeway"
    );
    assert_eq!(
        policy(ttl, Some(secs(600)), secs(60)),
        None,
        "window closed at completion"
    );
    assert_eq!(
        policy(ttl, Some(secs(600)), secs(1)),
        Some(secs(60)),
        "anchored at start"
    );
    assert_eq!(
        policy(ttl, Some(secs(30)), secs(1)),
        Some(secs(30)),
        "bounded by exp"
    );
}

#[test]
fn an_unrepresentable_reuse_deadline_is_no_store() {
    let start = time::OffsetDateTime::from_unix_timestamp(1_900_000_000).unwrap();
    let far = Some(time::OffsetDateTime::new_utc(
        time::Date::MAX,
        time::Time::MIDNIGHT,
    ));
    assert_eq!(
        authenticate_cache_control(Duration::MAX, start, far, start).cache_until,
        None
    );
}

#[tokio::test]
async fn a_programmatic_ttl_is_capped_at_the_policy_bound() {
    let plugin = plugin_with_result_ttl(Duration::MAX, Arc::new(StaticJwksProvider::available()));
    let before = time::OffsetDateTime::now_utc();
    let result = authenticate_result(&plugin, &jwt_expiring_in(3600)).await;
    let until = result.cache_control.cache_until.expect("reuse allowed");
    let after = time::OffsetDateTime::now_utc();
    assert!(until >= before + MAX_AUTHENTICATE_RESULT_CACHE_TTL);
    assert!(
        until <= after + MAX_AUTHENTICATE_RESULT_CACHE_TTL,
        "{until}"
    );
}

/// A plugin request for `token`; these plugins have no authenticate cache, so
/// the cache control is irrelevant.
fn request(token: &str) -> authn_resolver_sdk::AuthenticationRequest {
    authn_resolver_sdk::AuthenticationRequest::new(token)
}
