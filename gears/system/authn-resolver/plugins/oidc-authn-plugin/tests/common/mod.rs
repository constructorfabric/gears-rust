//! Integration-test helpers for the oidc-authn-plugin crate.

use std::collections::HashMap;
use std::sync::OnceLock;
use std::sync::{Arc, Mutex};

use httpmock::prelude::{GET, HttpMockRequest, HttpMockResponse, MockServer, POST};
use jsonwebtoken::jwk::{Jwk, JwkSet, PublicKeyUse};
use jsonwebtoken::{Algorithm, EncodingKey};
use oidc_authn_plugin::authenticate::OidcAuthNPlugin;
use oidc_authn_plugin::claim_mapper;
use oidc_authn_plugin::config::{
    CircuitBreakerConfig, IssuerTrustConfig, JwtValidationConfig, OidcPluginConfig,
    RetryPolicyConfig, S2sConfig, TrustedIssuerEntry, TrustedIssuerInput,
};
use oidc_authn_plugin::domain::metrics::AuthNMetrics;
use oidc_authn_plugin::infra::runtime::build_oidc_authn_plugin_allowing_insecure_http_for_tests;
use serde_json::json;

struct TestKeyMaterial {
    encoding_key: EncodingKey,
    jwks_json: String,
}

static KEY_MATERIAL: OnceLock<TestKeyMaterial> = OnceLock::new();

fn key_material() -> &'static TestKeyMaterial {
    KEY_MATERIAL.get_or_init(build_key_material)
}

fn build_key_material() -> TestKeyMaterial {
    let signing_key = rcgen::KeyPair::generate_for(&rcgen::PKCS_RSA_SHA256)
        .unwrap_or_else(|error| panic!("test RSA key should generate: {error}"));
    let encoding_key = EncodingKey::from_rsa_pem(signing_key.serialize_pem().as_bytes())
        .unwrap_or_else(|error| panic!("generated RSA key should encode JWTs: {error}"));
    let mut jwk = Jwk::from_encoding_key(&encoding_key, Algorithm::RS256)
        .unwrap_or_else(|error| panic!("generated RSA key should derive a public JWK: {error}"));
    jwk.common.key_id = Some(TEST_KID.to_owned());
    jwk.common.public_key_use = Some(PublicKeyUse::Signature);
    let jwks_json = serde_json::to_string(&JwkSet { keys: vec![jwk] })
        .unwrap_or_else(|error| panic!("generated JWKS should encode: {error}"));

    TestKeyMaterial {
        encoding_key,
        jwks_json,
    }
}

#[must_use]
fn test_jwk_json() -> &'static str {
    &key_material().jwks_json
}

/// Key ID embedded in [`test_jwk_json`].
pub const TEST_KID: &str = "test-key-1";

/// Valid test client credentials accepted by the mock token endpoint.
pub const TEST_S2S_CLIENT_ID: &str = "svc-test";
/// Valid test client secret accepted by the mock token endpoint.
pub const TEST_S2S_CLIENT_SECRET: &str = "test-secret-value";
/// Default S2S subject type used by integration-test plugin fixtures.
pub const TEST_S2S_DEFAULT_SUBJECT_TYPE: &str = "gts.cf.core.security.subject_user.v1~";

type RequestCounters = Arc<Mutex<HashMap<String, usize>>>;

/// Return a Unix timestamp 1 hour in the future (for `exp` claims).
#[must_use]
pub fn future_exp() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};

    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(9_999_999_999, |d| d.as_secs() + 3600)
}

/// Sign a JWT with the test RS256 private key.
#[must_use]
pub fn sign_jwt(claims: &serde_json::Value, kid: Option<&str>) -> String {
    use jsonwebtoken::{Header, encode};

    let mut header = Header::new(Algorithm::RS256);
    header.kid = kid.map(str::to_owned);
    encode(&header, claims, &key_material().encoding_key).unwrap_or_default()
}

/// Build a [`claim_mapper::Claims`] map from a JSON object.
///
/// # Panics
///
/// Panics when `value` is not a JSON object.
#[must_use]
pub fn claims(value: serde_json::Value) -> claim_mapper::Claims {
    let serde_json::Value::Object(claims) = value else {
        panic!("claims helper expects a JSON object");
    };
    claims
}

/// Build trust config for a single exact issuer using the public input schema.
///
/// # Errors
///
/// Returns an error when the issuer trust input fails runtime validation.
pub fn exact_issuer_trust(issuer: String) -> anyhow::Result<IssuerTrustConfig> {
    IssuerTrustConfig::from_inputs_allowing_insecure_http_for_tests([TrustedIssuerInput {
        entry: TrustedIssuerEntry::Issuer(issuer),
        discovery_url: None,
        expected_audience: Vec::new(),
        jose_typ: None,
        clock_skew_leeway_secs: None,
    }])
    .map_err(anyhow::Error::msg)
}

/// Build baseline JWT validation config for integration tests.
#[must_use]
pub fn base_jwt_validation_config() -> JwtValidationConfig {
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

/// Create a metrics handle for integration tests.
#[must_use]
pub fn create_test_metrics() -> Arc<AuthNMetrics> {
    Arc::new(AuthNMetrics::new(&opentelemetry::global::meter(
        "oidc-authn-plugin.integration-test",
    )))
}

/// Baseline retry policy used by integration tests.
#[must_use]
pub fn default_retry_policy_config() -> RetryPolicyConfig {
    RetryPolicyConfig {
        max_attempts: 3,
        initial_backoff_ms: 100,
        max_backoff_ms: 2_000,
        jitter: true,
    }
}

/// Baseline circuit-breaker config used by integration tests.
#[must_use]
pub fn default_circuit_breaker_config() -> CircuitBreakerConfig {
    CircuitBreakerConfig {
        failure_threshold: 5,
        reset_timeout_secs: 30,
    }
}

/// Empty S2S runtime config fixture used as a struct-update base.
#[must_use]
pub fn default_s2s_config() -> S2sConfig {
    S2sConfig {
        discovery_url: match reqwest::Url::parse("https://s2s.example.com") {
            Ok(url) => url,
            Err(error) => unreachable!("static S2S test discovery URL is invalid: {error}"),
        },
        token_cache_ttl_secs: 300,
        token_cache_max_entries: 100,
    }
}

/// Baseline plugin config fixture with default mapper/resilience settings.
#[must_use]
pub fn plugin_config() -> OidcPluginConfig {
    OidcPluginConfig {
        vendor: "constructorfabric".to_owned(),
        priority: 100,
        claim_mapper: claim_mapper::default_config(),
        s2s_claim_mapper: claim_mapper::default_config(),
        claim_mapper_options: claim_mapper::ClaimMapperOptions::default(),
        s2s_default_subject_type: TEST_S2S_DEFAULT_SUBJECT_TYPE.to_owned(),
        circuit_breaker: Some(default_circuit_breaker_config()),
        retry_policy: default_retry_policy_config(),
        s2s: default_s2s_config(),
    }
}

/// Build a plugin with production HTTP-backed infrastructure for integration tests.
#[must_use]
pub fn build_test_plugin(
    jwt_config: JwtValidationConfig,
    issuer_trust: IssuerTrustConfig,
    plugin_config: OidcPluginConfig,
    http_client: reqwest::Client,
) -> OidcAuthNPlugin {
    build_oidc_authn_plugin_allowing_insecure_http_for_tests(
        jwt_config,
        issuer_trust,
        plugin_config,
        http_client,
        create_test_metrics(),
    )
}

/// `httpmock` server that serves OIDC Discovery, JWKS, and token endpoints.
pub struct MockOidcServer {
    server: MockServer,
    discovery_requests: RequestCounters,
    jwks_requests: RequestCounters,
}

impl MockOidcServer {
    /// Spawn the mock server on an ephemeral port.
    ///
    /// # Errors
    ///
    /// Returns an error if the generated base URL is unexpectedly invalid.
    pub fn spawn() -> anyhow::Result<Self> {
        let server = MockServer::start();
        reqwest::Url::parse(&server.base_url())?;
        let discovery_requests = Arc::new(Mutex::new(HashMap::new()));
        let jwks_requests = Arc::new(Mutex::new(HashMap::new()));

        install_discovery_mock(&server, Arc::clone(&discovery_requests));
        install_jwks_mock(&server, Arc::clone(&jwks_requests));
        install_token_mock(&server);

        Ok(Self {
            server,
            discovery_requests,
            jwks_requests,
        })
    }

    /// Build the issuer URL for a given realm path (e.g. `"realms/platform"`).
    #[must_use]
    pub fn issuer(&self, realm: &str) -> String {
        let realm = realm.trim_start_matches('/');
        format!("{}/{realm}", self.server.base_url())
    }

    /// Return how many discovery requests were made for the given realm path.
    #[must_use]
    pub fn discovery_request_count(&self, realm: &str) -> usize {
        request_count(&self.discovery_requests, &issuer_path_key(realm))
    }

    /// Return how many JWKS requests were made for the given realm path.
    #[must_use]
    pub fn jwks_request_count(&self, realm: &str) -> usize {
        request_count(&self.jwks_requests, &issuer_path_key(realm))
    }
}

fn install_discovery_mock(server: &MockServer, discovery_requests: RequestCounters) {
    let base_url = server.base_url();
    server.mock(move |when, then| {
        when.method(GET)
            .path_suffix("/.well-known/openid-configuration");
        then.respond_with(move |req: &HttpMockRequest| {
            let uri = req.uri();
            let issuer_path = uri
                .path()
                .strip_suffix("/.well-known/openid-configuration")
                .unwrap_or("");
            increment_request_count(&discovery_requests, issuer_path);
            let issuer = format!("{base_url}{issuer_path}");
            let jwks_uri = format!("{issuer}/protocol/openid-connect/certs");
            let token_endpoint = format!("{issuer}/protocol/openid-connect/token");
            json_response(
                200,
                &json!({
                    "issuer": issuer,
                    "jwks_uri": jwks_uri,
                    "token_endpoint": token_endpoint,
                }),
            )
        });
    });
}

fn install_jwks_mock(server: &MockServer, jwks_requests: RequestCounters) {
    server.mock(move |when, then| {
        when.method(GET)
            .path_suffix("/protocol/openid-connect/certs");
        then.respond_with(move |req: &HttpMockRequest| {
            let uri = req.uri();
            let issuer_path = uri
                .path()
                .strip_suffix("/protocol/openid-connect/certs")
                .unwrap_or("");
            increment_request_count(&jwks_requests, issuer_path);
            HttpMockResponse::builder()
                .status(200)
                .header("content-type", "application/json")
                .body(test_jwk_json())
                .build()
        });
    });
}

fn install_token_mock(server: &MockServer) {
    let base_url = server.base_url();
    server.mock(move |when, then| {
        when.method(POST)
            .path_suffix("/protocol/openid-connect/token");
        then.respond_with(move |req: &HttpMockRequest| {
            let params = form_params(req);
            let client_id = params.get("client_id").map_or("", String::as_str);
            let client_secret = params.get("client_secret").map_or("", String::as_str);

            if client_id != TEST_S2S_CLIENT_ID || client_secret != TEST_S2S_CLIENT_SECRET {
                return json_response(
                    401,
                    &json!({
                        "error": "invalid_client",
                        "error_description": "Invalid client credentials",
                    }),
                );
            }

            let uri = req.uri();
            let issuer_path = uri
                .path()
                .strip_suffix("/protocol/openid-connect/token")
                .unwrap_or("");
            let issuer = format!("{base_url}{issuer_path}");
            let claims = json!({
                "sub": "550e8400-e29b-41d4-a716-446655440333",
                "iss": issuer,
                "exp": future_exp(),
                "client_id": TEST_S2S_CLIENT_ID,
                "tenant_id": "550e8400-e29b-41d4-a716-446655440222",
                "scope": params.get("scope").map_or("", String::as_str),
            });
            let access_token = sign_jwt(&claims, Some(TEST_KID));

            json_response(
                200,
                &json!({
                    "access_token": access_token,
                    "token_type": "Bearer",
                    "expires_in": 300,
                }),
            )
        });
    });
}

fn form_params(req: &HttpMockRequest) -> HashMap<String, String> {
    serde_urlencoded::from_bytes(req.body_ref()).unwrap_or_default()
}

fn issuer_path_key(realm: &str) -> String {
    let realm = realm.trim().trim_matches('/');
    if realm.is_empty() {
        String::new()
    } else {
        format!("/{realm}")
    }
}

fn increment_request_count(counters: &RequestCounters, issuer_path: &str) {
    if let Ok(mut counters) = counters.lock() {
        *counters.entry(issuer_path.to_owned()).or_default() += 1;
    }
}

fn request_count(counters: &RequestCounters, issuer_path: &str) -> usize {
    counters.lock().map_or(0, |counters| {
        counters.get(issuer_path).copied().unwrap_or_default()
    })
}

fn json_response(status: u16, body: &serde_json::Value) -> HttpMockResponse {
    HttpMockResponse::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(body.to_string())
        .build()
}
