//! The OAuth2 client-credentials plugin with `private_key_jwt` client
//! authentication (RFC 7523 §2.2) through the whole gateway: upstream
//! writes, the token request the identity provider receives, the bearer
//! token the upstream receives, the token cache, credential isolation
//! between subjects (scenarios 9.7 and 9.10) and what errors and logs show.
//!
//! The mock upstream also serves the token endpoint; the test reads the
//! token request it recorded and verifies the assertion with the public key.

use std::collections::HashMap;
use std::io::Write;
use std::sync::{Arc, Mutex, PoisonError};

use http::{Method, StatusCode};
use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use oagw_sdk::{
    AuthConfig, Body, CreateRouteRequest, CreateUpstreamRequest, Endpoint, HTTP_PROTOCOL_ID,
    HttpMatch, HttpMethod, MatchRules, PathSuffixMode, Scheme, Server, SharingMode,
};
use serde_json::{Value, json};
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;
use toolkit_security::crypto::signature::{EcdsaP256KeyPair, RsaKeyPair};
use tower::ServiceExt;
use uuid::Uuid;

use crate::domain::gts_helpers::OAUTH2_CLIENT_CRED_PRIVATE_KEY_JWT_AUTH_PLUGIN_ID;
use crate::infra::proxy::credstore_spy::{CredStoreSpy, SpySecret};
use crate::test_support::{AppHarness, MockBody, MockGuard, MockResponse, RecordedRequest};

/// Test-only keys, generated for these tests (`openssl genpkey`).
const P256_PEM: &str = include_str!("testdata/p256-private.pem");
const P256_PEM_2: &str = include_str!("testdata/p256-private-2.pem");
const RSA_PEM: &str = include_str!("testdata/rsa-private.pem");

const TOKEN: &str = "/oauth/token";
const API: &str = "/api/resource";
const JWT_BEARER: &str = "urn:ietf:params:oauth:client-assertion-type:jwt-bearer";

/// Every event of every crate, at every level, while the guard lives, on
/// the current thread (a `#[tokio::test]` runtime runs its tasks there).
#[derive(Clone, Default)]
struct Logs(Arc<Mutex<Vec<u8>>>);

impl Write for Logs {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Logs {
    fn capture() -> (Self, tracing::subscriber::DefaultGuard) {
        let logs = Self::default();
        let writer = logs.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish();
        (logs, tracing::subscriber::set_default(subscriber))
    }

    /// The events captured since the last call.
    fn take(&self) -> String {
        let bytes = std::mem::take(&mut *self.0.lock().unwrap_or_else(PoisonError::into_inner));
        String::from_utf8_lossy(&bytes).into_owned()
    }
}

/// The base64 lines of a PEM: what a leak of the key would show.
fn key_lines(pem: &str) -> impl Iterator<Item = &str> {
    pem.lines().filter(|l| !l.starts_with("-----"))
}

/// The public key that verifies assertions signed with `pem` under `alg`.
fn verifier(alg: Algorithm, pem: &str) -> DecodingKey {
    match alg {
        Algorithm::ES256 => {
            DecodingKey::from_ec_der(EcdsaP256KeyPair::from_pkcs8_pem(pem).unwrap().public_key())
        }
        _ => {
            let key = RsaKeyPair::from_pkcs8_pem(pem).unwrap();
            DecodingKey::from_rsa_raw_components(key.modulus(), key.public_exponent())
        }
    }
}

fn form_field(body: &[u8], name: &str) -> Option<String> {
    url::form_urlencoded::parse(body)
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.into_owned())
}

/// Verify the assertion of a recorded token request: signature, header and
/// the RFC 7523 claims, with `aud` the expected audience. Returns the claims.
fn verify_assertion(
    token_request: &RecordedRequest,
    alg: Algorithm,
    pem: &str,
    client_id: &str,
    aud: &str,
    kid: Option<&str>,
) -> Value {
    assert_eq!(
        form_field(&token_request.body, "grant_type").as_deref(),
        Some("client_credentials")
    );
    assert_eq!(
        form_field(&token_request.body, "client_assertion_type").as_deref(),
        Some(JWT_BEARER)
    );
    assert!(
        form_field(&token_request.body, "client_secret").is_none(),
        "no client secret is sent"
    );
    assert!(
        !token_request
            .headers
            .iter()
            .any(|(k, _)| k.eq_ignore_ascii_case("authorization")),
        "no Basic client authentication"
    );
    let assertion = form_field(&token_request.body, "client_assertion").expect("client_assertion");
    let mut validation = Validation::new(alg);
    validation.set_issuer(&[client_id]);
    validation.set_audience(&[aud]);
    validation.sub = Some(client_id.to_owned());
    validation.set_required_spec_claims(&["iss", "sub", "aud", "exp", "iat"]);
    let data = toolkit_security::jwt::decode::<Value>(&assertion, &verifier(alg, pem), &validation)
        .expect("the assertion verifies with the public key");
    assert_eq!(data.header.alg, alg);
    assert_eq!(data.header.kid.as_deref(), kid);
    let claims = data.claims;
    assert!(
        claims["jti"].as_str().is_some_and(|j| !j.is_empty()),
        "{claims}"
    );
    let (iat, exp) = (
        claims["iat"].as_i64().unwrap(),
        claims["exp"].as_i64().unwrap(),
    );
    assert!(exp > iat && exp - iat <= 300, "{claims}");
    claims
}

fn mock_token(guard: &mut MockGuard, access_token: &str) {
    guard.mock(
        "POST",
        TOKEN,
        MockResponse {
            status: 200,
            headers: vec![("content-type".into(), "application/json".into())],
            body: MockBody::Json(
                json!({"access_token": access_token, "expires_in": 3600, "token_type": "Bearer"}),
            ),
        },
    );
}

fn mock_api(guard: &mut MockGuard) {
    guard.mock(
        "GET",
        API,
        MockResponse {
            status: 200,
            headers: vec![("content-type".into(), "application/json".into())],
            body: MockBody::Json(json!({"ok": true})),
        },
    );
}

fn token_url(h: &AppHarness, guard: &MockGuard) -> String {
    format!("http://127.0.0.1:{}{}", h.mock_port(), guard.path(TOKEN))
}

/// A `private_key_jwt` auth config on the mock token endpoint.
fn auth_config(
    h: &AppHarness,
    guard: &MockGuard,
    alg: &str,
    key_ref: &str,
    kid: Option<&str>,
) -> HashMap<String, String> {
    let mut config: HashMap<String, String> = [
        ("token_endpoint", token_url(h, guard)),
        ("client_id_ref", "cred://pkjwt-client-id".to_owned()),
        ("private_key_ref", key_ref.to_owned()),
        ("signing_alg", alg.to_owned()),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v))
    .collect();
    if let Some(kid) = kid {
        config.insert("kid".into(), kid.into());
    }
    config
}

fn upstream_request(
    h: &AppHarness,
    alias: &str,
    config: HashMap<String, String>,
) -> CreateUpstreamRequest {
    CreateUpstreamRequest::builder(
        Server {
            endpoints: vec![Endpoint {
                scheme: Scheme::Http,
                host: "127.0.0.1".into(),
                port: h.mock_port(),
            }],
        },
        HTTP_PROTOCOL_ID,
    )
    .alias(alias)
    .auth(AuthConfig {
        plugin_type: OAUTH2_CLIENT_CRED_PRIVATE_KEY_JWT_AUTH_PLUGIN_ID.into(),
        sharing: SharingMode::Private,
        config: Some(config),
    })
    .build()
}

/// Create an upstream with `config` and a `GET` route to the mock API.
async fn upstream(h: &AppHarness, guard: &MockGuard, alias: &str, config: HashMap<String, String>) {
    let ctx = h.security_context().clone();
    let upstream = h
        .facade()
        .create_upstream(ctx.clone(), upstream_request(h, alias, config))
        .await
        .expect("upstream created");
    h.facade()
        .create_route(
            ctx,
            CreateRouteRequest::builder(
                upstream.id,
                MatchRules {
                    http: Some(HttpMatch {
                        methods: vec![HttpMethod::Get],
                        path: guard.path(API),
                        query_allowlist: vec![],
                        path_suffix_mode: PathSuffixMode::Disabled,
                    }),
                    grpc: None,
                },
            )
            .build(),
        )
        .await
        .expect("route created");
}

fn proxy_request(guard: &MockGuard, alias: &str) -> http::Request<Body> {
    http::Request::builder()
        .method(Method::GET)
        .uri(format!("/{alias}{}", guard.path(API)))
        .body(Body::Empty)
        .unwrap()
}

async fn proxy(
    h: &AppHarness,
    ctx: SecurityContext,
    guard: &MockGuard,
    alias: &str,
) -> Result<StatusCode, CanonicalError> {
    h.facade()
        .proxy_request(ctx, proxy_request(guard, alias))
        .await
        .map(|r| r.status())
}

/// The token requests and the upstream API requests the mock recorded.
async fn recorded(guard: &MockGuard) -> (Vec<RecordedRequest>, Vec<RecordedRequest>) {
    let all = guard.recorded_requests().await;
    let token = all
        .iter()
        .filter(|r| r.uri.contains(TOKEN))
        .cloned()
        .collect();
    let api = all
        .iter()
        .filter(|r| r.uri.contains(API))
        .cloned()
        .collect();
    (token, api)
}

fn authorization(r: &RecordedRequest) -> Option<&str> {
    r.headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("authorization"))
        .map(|(_, v)| v.as_str())
}

fn store(key_pem: &str) -> Arc<CredStoreSpy> {
    Arc::new(CredStoreSpy::with_entries(vec![
        SpySecret::new("pkjwt-client-id", "pkjwt-client"),
        SpySecret::new("pkjwt-signing-key", key_pem),
    ]))
}

async fn harness(store: &Arc<CredStoreSpy>) -> AppHarness {
    AppHarness::builder()
        .with_credstore(store.clone())
        .build()
        .await
}

/// Each algorithm: the token request carries a verifiable assertion and no
/// secret, the upstream gets the bearer token, and nothing logged shows the
/// key.
#[tokio::test]
async fn each_algorithm_injects_a_token_fetched_with_a_signed_assertion() {
    let (logs, _guard) = Logs::capture();
    for (alg, pem, kid) in [
        (Algorithm::ES256, P256_PEM, Some("p256-key")),
        (Algorithm::RS256, RSA_PEM, None),
        (Algorithm::PS256, RSA_PEM, Some("rsa-key")),
    ] {
        let alg_name = format!("{alg:?}");
        let store = store(pem);
        let h = harness(&store).await;
        let mut guard = MockGuard::new();
        mock_token(&mut guard, &format!("tok-{alg_name}"));
        mock_api(&mut guard);
        let alias = format!("pkjwt-{}", alg_name.to_lowercase());
        upstream(
            &h,
            &guard,
            &alias,
            auth_config(&h, &guard, &alg_name, "cred://pkjwt-signing-key", kid),
        )
        .await;

        let status = proxy(&h, h.security_context().clone(), &guard, &alias).await;
        assert_eq!(status.unwrap(), StatusCode::OK, "{alg_name}");

        let (token, api) = recorded(&guard).await;
        assert_eq!(token.len(), 1, "{alg_name}");
        verify_assertion(
            &token[0],
            alg,
            pem,
            "pkjwt-client",
            &token_url(&h, &guard),
            kid,
        );
        assert_eq!(api.len(), 1);
        assert_eq!(
            authorization(&api[0]),
            Some(format!("Bearer tok-{alg_name}").as_str())
        );
    }
    let logs = logs.take();
    assert!(
        logs.contains("executing auth plugin"),
        "the capture sees the gateway's events"
    );
    for line in [P256_PEM, RSA_PEM].into_iter().flat_map(key_lines) {
        assert!(!logs.contains(line), "a log line shows the key");
    }
}

/// A second request reuses the cached token: one token request, two
/// upstream requests.
#[tokio::test]
async fn second_request_hits_the_token_cache() {
    let store = store(P256_PEM);
    let h = harness(&store).await;
    let mut guard = MockGuard::new();
    mock_token(&mut guard, "tok-cached");
    mock_api(&mut guard);
    upstream(
        &h,
        &guard,
        "pkjwt-cache",
        auth_config(&h, &guard, "ES256", "cred://pkjwt-signing-key", None),
    )
    .await;

    for _ in 0..2 {
        let status = proxy(&h, h.security_context().clone(), &guard, "pkjwt-cache").await;
        assert_eq!(status.unwrap(), StatusCode::OK);
    }
    let (token, api) = recorded(&guard).await;
    assert_eq!(token.len(), 1, "one token request");
    assert_eq!(api.len(), 2, "two upstream requests");
    assert!(
        api.iter()
            .all(|r| authorization(r) == Some("Bearer tok-cached"))
    );
}

/// Upstreams whose configs differ only in the `kid`, only in the key
/// reference, or only in the `audience` never share a cached token; a
/// configured `audience` is the assertion's `aud`.
#[tokio::test]
async fn upstreams_with_different_keys_do_not_share_a_token() {
    let store = store(P256_PEM);
    store.add_secret(SpySecret::new("pkjwt-signing-key-2", P256_PEM_2));
    let h = harness(&store).await;
    let mut guard = MockGuard::new();
    mock_token(&mut guard, "tok");
    mock_api(&mut guard);
    let key_1 = "cred://pkjwt-signing-key";
    let key_2 = "cred://pkjwt-signing-key-2";
    let issuer = "https://idp.example.com/issuer";
    for (alias, key_ref, kid, audience) in [
        ("pkjwt-kid-a", key_1, "kid-a", None),
        ("pkjwt-kid-b", key_1, "kid-b", None),
        ("pkjwt-key-2", key_2, "kid-a", None),
        ("pkjwt-aud", key_1, "kid-a", Some(issuer)),
    ] {
        let mut config = auth_config(&h, &guard, "ES256", key_ref, Some(kid));
        if let Some(audience) = audience {
            config.insert("audience".into(), audience.into());
        }
        upstream(&h, &guard, alias, config).await;
        let status = proxy(&h, h.security_context().clone(), &guard, alias).await;
        assert_eq!(status.unwrap(), StatusCode::OK, "{alias}");
    }
    let (token, _) = recorded(&guard).await;
    assert_eq!(
        token.len(),
        4,
        "each key, kid and audience fetches its own token"
    );
    let url = token_url(&h, &guard);
    verify_assertion(
        &token[0],
        Algorithm::ES256,
        P256_PEM,
        "pkjwt-client",
        &url,
        Some("kid-a"),
    );
    verify_assertion(
        &token[1],
        Algorithm::ES256,
        P256_PEM,
        "pkjwt-client",
        &url,
        Some("kid-b"),
    );
    verify_assertion(
        &token[2],
        Algorithm::ES256,
        P256_PEM_2,
        "pkjwt-client",
        &url,
        Some("kid-a"),
    );
    let claims = verify_assertion(
        &token[3],
        Algorithm::ES256,
        P256_PEM,
        "pkjwt-client",
        issuer,
        Some("kid-a"),
    );
    assert_eq!(claims["aud"], issuer, "a single string, not the endpoint");
}

/// One RSA key reference under `RS256` on one upstream and `PS256` on
/// another: two token requests, each assertion signed with its algorithm.
#[tokio::test]
async fn one_rsa_key_under_two_algorithms_fetches_two_tokens() {
    let store = store(RSA_PEM);
    let h = harness(&store).await;
    let mut guard = MockGuard::new();
    mock_token(&mut guard, "tok");
    mock_api(&mut guard);
    let algs = [Algorithm::RS256, Algorithm::PS256];
    for alg in algs {
        let alias = format!("pkjwt-same-key-{alg:?}").to_lowercase();
        upstream(
            &h,
            &guard,
            &alias,
            auth_config(
                &h,
                &guard,
                &format!("{alg:?}"),
                "cred://pkjwt-signing-key",
                None,
            ),
        )
        .await;
        let status = proxy(&h, h.security_context().clone(), &guard, &alias).await;
        assert_eq!(status.unwrap(), StatusCode::OK, "{alg:?}");
    }
    let (token, api) = recorded(&guard).await;
    assert_eq!(token.len(), 2, "one token request per algorithm");
    assert_eq!(api.len(), 2);
    let url = token_url(&h, &guard);
    for (request, alg) in token.iter().zip(algs) {
        verify_assertion(request, alg, RSA_PEM, "pkjwt-client", &url, None);
    }
}

/// A key that does not parse, or is not of the type `signing_alg` needs, is
/// a plugin misconfiguration: 500, no token request, no upstream request,
/// and neither the response nor the logs show the key.
#[tokio::test]
async fn an_unusable_key_is_a_500_without_key_material() {
    let (logs, _guard) = Logs::capture();
    for (alg, pem) in [
        ("ES256", RSA_PEM),
        ("RS256", P256_PEM),
        ("ES256", "garbage"),
    ] {
        let store = store(pem);
        let h = harness(&store).await;
        let mut guard = MockGuard::new();
        mock_token(&mut guard, "never");
        mock_api(&mut guard);
        let alias = format!("pkjwt-bad-{}", Uuid::new_v4().simple());
        upstream(
            &h,
            &guard,
            &alias,
            auth_config(&h, &guard, alg, "cred://pkjwt-signing-key", None),
        )
        .await;

        let err = proxy(&h, h.security_context().clone(), &guard, &alias)
            .await
            .expect_err("an unusable key fails the request");
        assert!(matches!(err, CanonicalError::Internal { .. }), "{err:?}");
        let sdk_text = format!("{err:?}");

        let mut req = proxy_request(&guard, &alias);
        *req.uri_mut() = format!("/oagw/v1/proxy{}", req.uri()).parse().unwrap();
        let (parts, _) = req.into_parts();
        let resp = h
            .router()
            .clone()
            .oneshot(http::Request::from_parts(parts, axum::body::Body::empty()))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR, "{alg}");
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let body = String::from_utf8_lossy(&body);

        let logs = logs.take();
        for line in [P256_PEM, RSA_PEM].into_iter().flat_map(key_lines) {
            assert!(!body.contains(line), "{alg}: the response shows the key");
            assert!(!sdk_text.contains(line), "{alg}: the error shows the key");
            assert!(!logs.contains(line), "{alg}: a log line shows the key");
        }
        assert!(
            logs.contains("auth plugin misconfigured")
                && logs.contains("auth.config.private_key_ref"),
            "{alg}: the log names the config key: {logs}"
        );
        let (token, api) = recorded(&guard).await;
        assert!(token.is_empty() && api.is_empty(), "{alg}: nothing sent");
    }
}

/// Write-time checks: a config with both a secret and a key reference, no
/// key reference, no algorithm or an unknown one, neither or both of
/// `token_endpoint` and `issuer_url`, or an empty `audience` is a 400 field
/// violation; a key
/// reference the store does not hold is a 400 precondition naming the key
/// (scenario 9.7-B). None of them quote the reference.
#[tokio::test]
async fn invalid_configs_are_refused_on_write() {
    let store = store(P256_PEM);
    let h = harness(&store).await;
    let guard = MockGuard::new();
    let base = || auth_config(&h, &guard, "ES256", "cred://pkjwt-signing-key", None);
    let edit = |f: fn(&mut HashMap<String, String>)| {
        let mut c = base();
        f(&mut c);
        c
    };
    for (config, field, reason) in [
        (
            edit(|c| {
                c.insert("client_secret_ref".into(), "cred://pkjwt-client-id".into());
            }),
            "auth.config.client_secret_ref",
            oagw_sdk::field::VALIDATION,
        ),
        (
            edit(|c| {
                c.remove("private_key_ref");
            }),
            "auth.config.private_key_ref",
            oagw_sdk::field::REQUIRED,
        ),
        (
            edit(|c| {
                c.insert("signing_alg".into(), "ES384".into());
            }),
            "auth.config.signing_alg",
            oagw_sdk::field::INVALID_VALUE,
        ),
        (
            edit(|c| {
                c.remove("signing_alg");
            }),
            "auth.config.signing_alg",
            oagw_sdk::field::REQUIRED,
        ),
        (
            edit(|c| {
                c.remove("token_endpoint");
            }),
            "auth.config.token_endpoint",
            oagw_sdk::field::REQUIRED,
        ),
        (
            edit(|c| {
                c.insert("issuer_url".into(), "https://idp.example.com".into());
            }),
            "auth.config.issuer_url",
            oagw_sdk::field::VALIDATION,
        ),
        (
            edit(|c| {
                c.insert("audience".into(), String::new());
            }),
            "auth.config.audience",
            oagw_sdk::field::INVALID_VALUE,
        ),
    ] {
        let err = h
            .facade()
            .create_upstream(
                h.security_context().clone(),
                upstream_request(&h, &format!("pkjwt-{}", Uuid::new_v4().simple()), config),
            )
            .await
            .expect_err("refused");
        match err {
            CanonicalError::InvalidArgument {
                ctx: toolkit_canonical_errors::InvalidArgument::FieldViolations { field_violations },
                ..
            } => {
                assert_eq!(field_violations.len(), 1, "{field_violations:?}");
                assert_eq!(
                    (
                        field_violations[0].field.as_str(),
                        field_violations[0].reason.as_str()
                    ),
                    (field, reason)
                );
                assert!(!field_violations[0].description.contains("pkjwt-"));
            }
            other => panic!("{field}: expected a field violation, got {other:?}"),
        }
    }

    let err = h
        .facade()
        .create_upstream(
            h.security_context().clone(),
            upstream_request(
                &h,
                "pkjwt-missing-key",
                auth_config(&h, &guard, "ES256", "cred://pkjwt-missing-key", None),
            ),
        )
        .await
        .expect_err("a key reference the store does not hold is refused");
    match err {
        CanonicalError::FailedPrecondition { ctx, detail, .. } => {
            let subjects: Vec<&str> = ctx.violations.iter().map(|v| v.subject.as_str()).collect();
            assert_eq!(subjects, [oagw_sdk::precondition::PRIVATE_KEY_REF]);
            assert!(!detail.contains("pkjwt-missing-key"), "{detail}");
        }
        other => panic!("expected FailedPrecondition, got {other:?}"),
    }
}

/// Scenario 9.10-A with a key reference: the client id and the key are
/// private to each subject. Each subject's token request is signed with its
/// own key and names its own client; neither gets the other's cached token.
/// A subject with a client id but no key of its own gets 401
/// `AUTH_CREDENTIAL_NOT_FOUND` (scenario 9.7-A), not the token cached for
/// another subject; the server log names `private_key_ref`.
#[tokio::test]
async fn private_keys_stay_with_their_subject() {
    let (logs, _guard) = Logs::capture();
    let store = Arc::new(CredStoreSpy::empty());
    let h = harness(&store).await;
    // Subject A is the harness's own subject, which also writes the upstream.
    let tenant = h.security_context().subject_tenant_id();
    let subject = |id: Uuid| {
        SecurityContext::builder()
            .subject_tenant_id(tenant)
            .subject_id(id)
            .build()
            .unwrap()
    };
    let (a, b, c) = (
        h.security_context().subject_id(),
        Uuid::new_v4(),
        Uuid::new_v4(),
    );
    for secret in [
        SpySecret::new("pkjwt-client-id", "client-a").private_to(a),
        SpySecret::new("pkjwt-signing-key", P256_PEM).private_to(a),
        SpySecret::new("pkjwt-client-id", "client-b").private_to(b),
        SpySecret::new("pkjwt-signing-key", P256_PEM_2).private_to(b),
        SpySecret::new("pkjwt-client-id", "client-c").private_to(c),
    ] {
        assert!(store.add_secret(secret));
    }
    let mut guard = MockGuard::new();
    mock_token(&mut guard, "tok");
    mock_api(&mut guard);
    upstream(
        &h,
        &guard,
        "pkjwt-subjects",
        auth_config(&h, &guard, "ES256", "cred://pkjwt-signing-key", None),
    )
    .await;

    for id in [a, b] {
        let status = proxy(&h, subject(id), &guard, "pkjwt-subjects").await;
        assert_eq!(status.unwrap(), StatusCode::OK);
    }
    let err = proxy(&h, subject(c), &guard, "pkjwt-subjects")
        .await
        .expect_err("a subject without the key is refused");
    match err {
        CanonicalError::Unauthenticated { ctx, .. } => {
            assert_eq!(ctx.reason.as_deref(), Some("AUTH_CREDENTIAL_NOT_FOUND"));
        }
        other => panic!("expected Unauthenticated, got {other:?}"),
    }
    // The 401 names no key on the wire (9.7-A); the warning names the one
    // that was missing: the key, since subject C can read the client id.
    let logs = logs.take();
    let warning = logs
        .lines()
        .find(|l| l.contains("auth plugin: credential not accessible"))
        .unwrap_or_else(|| panic!("no credential warning: {logs}"));
    assert!(
        warning.contains("config_key=auth.config.private_key_ref"),
        "{warning}"
    );

    let (token, api) = recorded(&guard).await;
    assert_eq!(token.len(), 2, "one token request per subject with a key");
    assert_eq!(api.len(), 2, "nothing sent for the subject without a key");
    let url = token_url(&h, &guard);
    verify_assertion(
        &token[0],
        Algorithm::ES256,
        P256_PEM,
        "client-a",
        &url,
        None,
    );
    verify_assertion(
        &token[1],
        Algorithm::ES256,
        P256_PEM_2,
        "client-b",
        &url,
        None,
    );
}
