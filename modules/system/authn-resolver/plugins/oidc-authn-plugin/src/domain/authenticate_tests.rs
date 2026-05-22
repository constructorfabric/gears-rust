use std::sync::Arc;

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

const TEST_S2S_DEFAULT_SUBJECT_TYPE: &str = "gts.cf.core.security.subject_user.v1~";

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
    ) -> Result<String, AuthNError> {
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
        <OidcAuthNPlugin as AuthNResolverPluginClient>::authenticate(&plugin, &token).await;

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
        <OidcAuthNPlugin as AuthNResolverPluginClient>::authenticate(&plugin, &token).await;

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
        <OidcAuthNPlugin as AuthNResolverPluginClient>::authenticate(&plugin, &token).await;
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

#[tokio::test]
async fn concurrent_authenticate_calls_all_succeed() {
    let (plugin, _) = jwt_plugin_with_jwks_provider(StaticJwksProvider::available());
    let plugin = Arc::new(plugin);
    let mut handles = Vec::new();
    for _ in 0..20 {
        let p = Arc::clone(&plugin);
        let token = sign_valid_jwt();
        handles.push(tokio::spawn(async move {
            <OidcAuthNPlugin as AuthNResolverPluginClient>::authenticate(p.as_ref(), &token).await
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
