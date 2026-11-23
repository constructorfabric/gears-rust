//! The AWS SigV4 auth plugin through the pipeline: the in-process SDK client
//! against the shared mock upstream, with the built-in plugin on a fixed
//! clock. The upstream's request is checked by a verifier written from the
//! SigV4 spec, as AWS checks it: from the request line, the headers the
//! `Authorization` header names and the body.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use credstore_sdk::test_util::MockCredStoreClient;
use http::StatusCode;
use oagw_sdk::{
    AuthConfig, Body, CircuitBreakerConfig, CreateRouteRequest, CreateUpstreamRequest,
    CreateUpstreamRequestBuilder, Endpoint, HTTP_PROTOCOL_ID, HeadersConfig, HttpMatch, HttpMethod,
    MatchRules, PassthroughMode, PathSuffixMode, PluginBinding, PluginsConfig, RequestHeaderRules,
    Scheme, Server, ServiceGatewayError, SharingMode,
};
use serde_json::json;
use time::OffsetDateTime;
use toolkit_security::SecurityContext;
use toolkit_security::crypto::sigv4::{CredentialScope, SigningKey, sha256_hex};
use uuid::Uuid;

use crate::domain::gts_helpers::{AWS_SIGV4_AUTH_PLUGIN_ID, REQUEST_ID_TRANSFORM_PLUGIN_ID};
use crate::domain::plugin::aws_sigv4::AwsSigV4AuthConfig;
use crate::domain::plugin::{PluginError, TransformPlugin, TransformRequestContext};
use crate::infra::plugin::aws_sigv4_auth::{
    AwsCredentialSource, AwsCredentials, AwsSigV4AuthPlugin, CredStoreCredentialSource,
    SigningClock,
};
use crate::test_support::{AppHarness, MockBody, MockGuard, MockResponse, RecordedRequest};

const SIGN: &str = "/v1/sign";
const STREAM: &str = "/v1/stream";
const AKID: &str = "AKIDEXAMPLE";
const SECRET: &str = "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY";
const TOKEN: &str = "FwoGZXIvYXdzEXAMPLETOKEN+/=";
/// `kSigning` for 20150830/us-east-1/bedrock, as hex.
const SIGNING_KEY_HEX: &str = "90e694a0eeb93605dc6a1e798b98fe0e790776a911d2494317b06246c599ba53";
/// 2015-08-30T12:36:00Z.
const NOW: i64 = 1_440_938_160;

struct FixedClock;

impl SigningClock for FixedClock {
    fn now(&self) -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(NOW).unwrap()
    }
}

/// Adds `t=x y+z` to the query, so the signer must re-encode what a
/// transform added.
struct AddQuery;

#[async_trait::async_trait]
impl TransformPlugin for AddQuery {
    async fn on_request(&self, ctx: &mut TransformRequestContext) -> Result<(), PluginError> {
        ctx.query.push(("t".into(), "x y+z".into()));
        Ok(())
    }
}

/// The plugin's config with `extra` entries; an empty value removes a key.
fn auth_config(extra: &[(&str, &str)]) -> HashMap<String, String> {
    let mut config: HashMap<String, String> = [
        ("region", "us-east-1"),
        ("service", "bedrock"),
        ("access_key_id_ref", "cred://aws-akid"),
        ("secret_access_key_ref", "cred://aws-secret"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v.to_owned()))
    .collect();
    for (k, v) in extra {
        if v.is_empty() {
            config.remove(*k);
        } else {
            config.insert((*k).to_owned(), (*v).to_owned());
        }
    }
    config
}

fn allowlist(names: &[&str]) -> RequestHeaderRules {
    RequestHeaderRules {
        passthrough: PassthroughMode::Allowlist,
        passthrough_allowlist: names.iter().map(|n| (*n).to_owned()).collect(),
        ..Default::default()
    }
}

fn passthrough_all() -> RequestHeaderRules {
    RequestHeaderRules {
        passthrough: PassthroughMode::All,
        ..Default::default()
    }
}

fn upstream_request(
    port: u16,
    alias: &str,
    config: HashMap<String, String>,
    headers: RequestHeaderRules,
) -> CreateUpstreamRequest {
    upstream_builder(port, alias, config, headers).build()
}

fn upstream_builder(
    port: u16,
    alias: &str,
    config: HashMap<String, String>,
    headers: RequestHeaderRules,
) -> CreateUpstreamRequestBuilder {
    CreateUpstreamRequest::builder(
        Server {
            endpoints: vec![Endpoint {
                scheme: Scheme::Http,
                host: "127.0.0.1".into(),
                port,
            }],
        },
        HTTP_PROTOCOL_ID,
    )
    .alias(alias.to_owned())
    .auth(AuthConfig {
        plugin_type: AWS_SIGV4_AUTH_PLUGIN_ID.into(),
        sharing: SharingMode::Private,
        config: Some(config),
    })
    .headers(HeadersConfig {
        request: Some(headers),
        response: None,
    })
    .plugins(PluginsConfig {
        sharing: SharingMode::Private,
        items: vec![PluginBinding {
            plugin_ref: REQUEST_ID_TRANSFORM_PLUGIN_ID.into(),
            config: Default::default(),
        }],
    })
}

/// The CredStore with the test credentials.
fn store() -> Arc<MockCredStoreClient> {
    Arc::new(MockCredStoreClient::with_secrets(vec![
        ("aws-akid".into(), AKID.into()),
        ("aws-secret".into(), SECRET.into()),
        ("aws-token".into(), TOKEN.into()),
    ]))
}

async fn harness() -> AppHarness {
    harness_with(Duration::ZERO, None).await
}

/// The harness with the plugin's credentials read after `read_delay` and
/// the given request timeout; the write check reads the store directly.
async fn harness_with(read_delay: Duration, request_timeout: Option<Duration>) -> AppHarness {
    let store = store();
    let plugin = AwsSigV4AuthPlugin::with_sources(
        Arc::new(SlowCredentials {
            inner: CredStoreCredentialSource::new(store.clone()),
            delay: read_delay,
        }),
        Arc::new(FixedClock),
    );
    let mut builder = AppHarness::builder()
        .with_credstore(store)
        .with_auth_plugin(AWS_SIGV4_AUTH_PLUGIN_ID, Arc::new(plugin))
        .with_transform_plugin(REQUEST_ID_TRANSFORM_PLUGIN_ID, Arc::new(AddQuery));
    if let Some(timeout) = request_timeout {
        builder = builder.with_request_timeout(timeout);
    }
    builder.build().await
}

/// A credential source that answers after `delay`, as a slow CredStore.
struct SlowCredentials {
    inner: CredStoreCredentialSource,
    delay: Duration,
}

#[async_trait::async_trait]
impl AwsCredentialSource for SlowCredentials {
    async fn credentials(
        &self,
        config: &AwsSigV4AuthConfig,
        security_context: &SecurityContext,
    ) -> Result<AwsCredentials, PluginError> {
        if !self.delay.is_zero() {
            tokio::time::sleep(self.delay).await;
        }
        self.inner.credentials(config, security_context).await
    }
}

struct Fx {
    h: AppHarness,
    guard: MockGuard,
    alias: String,
}

impl Fx {
    /// An upstream on the mock signing with [`auth_config`] plus `extra`,
    /// [`AddQuery`] bound; routes on [`SIGN`] (query `q`, `e`), [`STREAM`]
    /// (an SSE response) and the WebSocket echo.
    async fn new(extra: &[(&str, &str)], headers: RequestHeaderRules) -> Self {
        Self::with(harness().await, extra, headers, None).await
    }

    /// [`Fx::new`] on `h`, with `circuit_breaker` on the upstream.
    async fn with(
        h: AppHarness,
        extra: &[(&str, &str)],
        headers: RequestHeaderRules,
        circuit_breaker: Option<CircuitBreakerConfig>,
    ) -> Self {
        let mut guard = MockGuard::new();
        guard.mock(
            "POST",
            SIGN,
            MockResponse {
                status: 200,
                headers: vec![("content-type".into(), "application/json".into())],
                body: MockBody::Json(json!({"ok": true})),
            },
        );
        guard.mock(
            "POST",
            STREAM,
            MockResponse {
                status: 200,
                headers: vec![("content-type".into(), "text/event-stream".into())],
                body: MockBody::Sse(vec!["one".into(), "two".into()]),
            },
        );
        let alias = format!("sigv4-{}", Uuid::new_v4().as_simple());
        let ctx = h.security_context().clone();
        let upstream = h
            .facade()
            .create_upstream(
                ctx.clone(),
                match circuit_breaker {
                    Some(cb) => {
                        upstream_builder(h.mock_port(), &alias, auth_config(extra), headers)
                            .circuit_breaker(cb)
                            .build()
                    }
                    None => upstream_request(h.mock_port(), &alias, auth_config(extra), headers),
                },
            )
            .await
            .unwrap();
        for (method, path, query_allowlist) in [
            (
                HttpMethod::Post,
                guard.path(SIGN),
                vec!["q".to_owned(), "e".to_owned()],
            ),
            (HttpMethod::Post, guard.path(STREAM), vec![]),
            (HttpMethod::Get, "/ws/echo".to_owned(), vec![]),
        ] {
            h.facade()
                .create_route(
                    ctx.clone(),
                    CreateRouteRequest::builder(
                        upstream.id,
                        MatchRules {
                            http: Some(HttpMatch {
                                methods: vec![method],
                                path,
                                query_allowlist,
                                path_suffix_mode: PathSuffixMode::Disabled,
                            }),
                            grpc: None,
                        },
                    )
                    .build(),
                )
                .await
                .unwrap();
        }
        Self { h, guard, alias }
    }

    /// `POST` on `path` with repeated query names, a space, a literal plus
    /// and an empty value.
    fn post(&self, path: &str) -> http::request::Builder {
        http::Request::post(format!(
            "/{}{}?q=a+b&q=c%2Bd&e=",
            self.alias,
            self.guard.path(path)
        ))
        .header("content-type", "application/json")
    }

    async fn send(
        &self,
        req: http::Request<Body>,
    ) -> Result<(StatusCode, Bytes), toolkit_canonical_errors::CanonicalError> {
        let resp = self
            .h
            .facade()
            .proxy_request(self.h.security_context().clone(), req)
            .await?;
        let status = resp.status();
        let body = resp.into_body().into_bytes().await.unwrap_or_default();
        Ok((status, body))
    }
}

fn json_body() -> Body {
    Body::Bytes(Bytes::from_static(br#"{"prompt":"hi"}"#))
}

fn header_values(r: &RecordedRequest, name: &str) -> Vec<String> {
    r.headers
        .iter()
        .filter(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.clone())
        .collect()
}

// ---------------------------------------------------------------------------
// Verifier
// ---------------------------------------------------------------------------

fn encode(bytes: &[u8], keep_slash: bool) -> String {
    let mut out = String::new();
    for &b in bytes {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) || (keep_slash && b == b'/') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn decode(s: &str) -> Vec<u8> {
    percent_encoding::percent_decode_str(s).collect()
}

/// Check the SigV4 signature of what the upstream received, with `secret`,
/// for a service that double-encodes the path (its paths here have no dot
/// segments). The payload hash is the header's, and must be the body's
/// unless it is `UNSIGNED-PAYLOAD`. Returns the signed header names.
fn verify(r: &RecordedRequest, secret: &str) -> Result<String, String> {
    verify_encoded(r, secret, true)
}

/// [`verify`] for a service that double-encodes the path (`double`) or,
/// as S3 does, decodes the path as received and encodes it once.
fn verify_encoded(r: &RecordedRequest, secret: &str, double: bool) -> Result<String, String> {
    let auth = header_values(r, "authorization");
    let [auth] = auth.as_slice() else {
        return Err(format!("expected one authorization header, got {auth:?}"));
    };
    let rest = auth
        .strip_prefix("AWS4-HMAC-SHA256 ")
        .ok_or("not AWS4-HMAC-SHA256")?;
    let mut parts = HashMap::new();
    for part in rest.split(", ") {
        let (k, v) = part.split_once('=').ok_or("malformed authorization")?;
        parts.insert(k, v);
    }
    let credential = parts.get("Credential").ok_or("no Credential")?;
    let signed_headers = parts.get("SignedHeaders").ok_or("no SignedHeaders")?;
    let signature = parts.get("Signature").ok_or("no Signature")?;
    let scope: Vec<&str> = credential.split('/').collect();
    let [akid, date, region, service, "aws4_request"] = scope.as_slice() else {
        return Err(format!("bad credential scope {credential}"));
    };
    if *akid != AKID {
        return Err(format!("unknown access key {akid}"));
    }

    let (path, query) = r.uri.split_once('?').unwrap_or((&r.uri, ""));
    let mut pairs: Vec<(String, String)> = query
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            (encode(&decode(k), false), encode(&decode(v), false))
        })
        .collect();
    pairs.sort();
    let canonical_query = pairs
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&");

    let mut canonical_headers = String::new();
    for name in signed_headers.split(';') {
        let values = header_values(r, name);
        if values.is_empty() {
            return Err(format!("signed header {name} was not received"));
        }
        let joined = values
            .iter()
            .map(|v| v.split_whitespace().collect::<Vec<_>>().join(" "))
            .collect::<Vec<_>>()
            .join(",");
        canonical_headers.push_str(&format!("{name}:{joined}\n"));
    }

    let body_hash = sha256_hex(&r.body).unwrap();
    let payload_hash = header_values(r, "x-amz-content-sha256")
        .pop()
        .unwrap_or_else(|| body_hash.clone());
    if payload_hash != "UNSIGNED-PAYLOAD" && payload_hash != body_hash {
        return Err("x-amz-content-sha256 is not the body's hash".into());
    }
    let canonical = format!(
        "{}\n{}\n{canonical_query}\n{canonical_headers}\n{signed_headers}\n{payload_hash}",
        r.method,
        if double {
            encode(path.as_bytes(), true)
        } else {
            encode(&decode(path), true)
        }
    );
    let amz_date = header_values(r, "x-amz-date")
        .pop()
        .ok_or("no x-amz-date")?;
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{date}/{region}/{service}/aws4_request\n{}",
        sha256_hex(canonical.as_bytes()).unwrap()
    );
    let expected = SigningKey::derive(
        secret.as_bytes(),
        &CredentialScope::new(date, region, service).unwrap(),
    )
    .unwrap()
    .signature(string_to_sign.as_bytes())
    .unwrap();
    if expected == *signature {
        Ok((*signed_headers).to_owned())
    } else {
        Err("signature does not match".into())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// A buffered JSON request is signed as the upstream receives it: the query
/// after a transform added to it, re-encoded for SigV4 (a space as `%20`, a
/// literal plus as `%2B`), repeated names and an empty value kept, `Host`
/// with the non-default port. Allowlisted `x-amz-*` headers are signed, a
/// repeated one with every line; `user-agent` and a repeated unrelated
/// header are not. The keys appear in no log line.
#[tokio::test]
#[tracing_test::traced_test]
async fn signed_request_verifies_at_the_upstream() {
    let fx = Fx::new(
        &[],
        allowlist(&["x-amz-target", "x-amz-meta-tag", "x-multi", "user-agent"]),
    )
    .await;
    let req = fx
        .post(SIGN)
        .header("x-amz-target", "BedrockRuntime.InvokeModel")
        .header("x-amz-meta-tag", "one")
        .header("x-amz-meta-tag", "two  words")
        .header("x-multi", "a")
        .header("x-multi", "b")
        .header("user-agent", "sdk/1.0")
        .body(json_body())
        .unwrap();
    let (status, _) = fx.send(req).await.unwrap();
    assert_eq!(status, StatusCode::OK);

    let rec = fx.guard.recorded_requests().await;
    assert_eq!(rec.len(), 1);
    let r = &rec[0];
    assert_eq!(
        r.uri,
        format!("{}?q=a%20b&q=c%2Bd&e=&t=x%20y%2Bz", fx.guard.path(SIGN))
    );
    assert_eq!(
        header_values(r, "host"),
        [format!("127.0.0.1:{}", fx.h.mock_port())]
    );
    assert_eq!(header_values(r, "x-amz-date"), ["20150830T123600Z"]);
    assert_eq!(
        header_values(r, "x-amz-content-sha256"),
        [sha256_hex(br#"{"prompt":"hi"}"#).unwrap()]
    );
    assert_eq!(header_values(r, "x-amz-meta-tag"), ["one", "two  words"]);
    assert_eq!(
        verify(r, SECRET).unwrap(),
        "content-type;host;x-amz-content-sha256;x-amz-date;x-amz-meta-tag;x-amz-target"
    );
    assert!(verify(r, "wrong-secret-0123456789").is_err());

    for secret in [SECRET, SIGNING_KEY_HEX, TOKEN] {
        assert!(!logs_contain(secret));
    }
}

/// Without a transform the gateway forwards the client's raw query, and the
/// signer rewrites what it sends: `+` read as a space, `%2B` kept, `%7E` as
/// the unreserved `~`, `*` as `%2A`, the order kept. A query with a literal
/// `;`, which the gateway re-encodes (`%3B`), is signed as re-encoded. Each request
/// verifies at the upstream.
#[tokio::test]
async fn raw_query_is_signed_as_sent() {
    const RAW: &str = "/v1/raw";
    let h = harness().await;
    let mut guard = MockGuard::new();
    guard.mock(
        "GET",
        RAW,
        MockResponse {
            status: 200,
            headers: vec![("content-type".into(), "application/json".into())],
            body: MockBody::Json(json!({"ok": true})),
        },
    );
    let alias = format!("sigv4-{}", Uuid::new_v4().as_simple());
    let ctx = h.security_context().clone();
    let upstream = h
        .facade()
        .create_upstream(
            ctx.clone(),
            upstream_builder(h.mock_port(), &alias, auth_config(&[]), allowlist(&[]))
                .plugins(PluginsConfig {
                    sharing: SharingMode::Private,
                    items: vec![],
                })
                .build(),
        )
        .await
        .unwrap();
    h.facade()
        .create_route(
            ctx.clone(),
            CreateRouteRequest::builder(
                upstream.id,
                MatchRules {
                    http: Some(HttpMatch {
                        methods: vec![HttpMethod::Get],
                        path: guard.path(RAW),
                        query_allowlist: vec!["b".to_owned(), "a".to_owned()],
                        path_suffix_mode: PathSuffixMode::Disabled,
                    }),
                    grpc: None,
                },
            )
            .build(),
        )
        .await
        .unwrap();

    // The query the client sends, and the one the upstream receives.
    const CASES: [(&str, &str); 2] = [
        ("b=x+y*z&a=%7E1&b=c%2Bd", "b=x%20y%2Az&a=~1&b=c%2Bd"),
        ("b=x;admin=1", "b=x%3Badmin%3D1"),
    ];
    for (query, _) in CASES {
        let req = http::Request::get(format!("/{alias}{}?{query}", guard.path(RAW)))
            .body(Body::Empty)
            .unwrap();
        let resp = h.facade().proxy_request(ctx.clone(), req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "{query}");
    }
    let rec = guard.recorded_requests().await;
    assert_eq!(rec.len(), 2);
    for (r, (_, sent)) in rec.iter().zip(CASES) {
        assert_eq!(r.uri, format!("{}?{sent}", guard.path(RAW)));
        verify(r, SECRET).unwrap();
    }
}

/// A signed request's streamed (SSE) response comes back to the caller.
#[tokio::test]
async fn response_streams_back_from_a_signed_request() {
    let fx = Fx::new(&[], allowlist(&[])).await;
    let req = http::Request::post(format!("/{}{}", fx.alias, fx.guard.path(STREAM)))
        .header("content-type", "application/json")
        .body(json_body())
        .unwrap();
    let (status, body) = fx.send(req).await.unwrap();
    assert_eq!(status, StatusCode::OK);
    let body = String::from_utf8(body.to_vec()).unwrap();
    assert!(body.contains("one") && body.contains("two"), "{body}");
    let rec = fx.guard.recorded_requests().await;
    assert_eq!(rec.len(), 1);
    verify(&rec[0], SECRET).unwrap();
}

/// Under `passthrough: all`, a client's `authorization`, `x-amz-date`,
/// `x-amz-content-sha256` and `x-amz-security-token` do not reach the
/// upstream: each is sent once, with the plugin's value. Its other
/// `x-amz-*` headers are kept and signed.
#[tokio::test]
async fn client_amz_headers_are_not_trusted() {
    let fx = Fx::new(
        &[("session_token_ref", "cred://aws-token")],
        passthrough_all(),
    )
    .await;
    let req = fx
        .post(SIGN)
        .header("authorization", "AWS4-HMAC-SHA256 Credential=forged")
        .header("x-amz-date", "19700101T000000Z")
        .header("x-amz-content-sha256", "forged")
        .header("x-amz-security-token", "forged")
        .header("x-amz-target", "Svc.Op")
        .body(json_body())
        .unwrap();
    assert_eq!(fx.send(req).await.unwrap().0, StatusCode::OK);

    let rec = fx.guard.recorded_requests().await;
    assert_eq!(rec.len(), 1);
    let r = &rec[0];
    assert_eq!(header_values(r, "x-amz-date"), ["20150830T123600Z"]);
    assert_eq!(header_values(r, "x-amz-security-token"), [TOKEN]);
    assert_eq!(
        header_values(r, "x-amz-content-sha256"),
        [sha256_hex(br#"{"prompt":"hi"}"#).unwrap()]
    );
    assert_eq!(header_values(r, "x-amz-target"), ["Svc.Op"]);
    assert_eq!(
        verify(r, SECRET).unwrap(),
        "content-type;host;x-amz-content-sha256;x-amz-date;x-amz-security-token;x-amz-target"
    );
}

/// Without a configured session token the plugin cannot remove a client's
/// `x-amz-security-token`, so it refuses the request (400) and nothing is
/// sent; a `headers.request.remove` rule lets the request through.
#[tokio::test]
async fn client_security_token_without_session_token() {
    let fx = Fx::new(&[], passthrough_all()).await;
    let req = fx
        .post(SIGN)
        .header("x-amz-security-token", "client-token")
        .body(json_body())
        .unwrap();
    let err = fx.send(req).await.unwrap_err();
    assert_eq!(err.status_code(), 400);
    match ServiceGatewayError::from(err) {
        ServiceGatewayError::Validation {
            field,
            reason,
            detail,
        } => {
            assert_eq!(field, "x-amz-security-token");
            assert_eq!(reason, oagw_sdk::field::VALIDATION);
            assert!(detail.contains("x-amz-security-token"), "{detail}");
            assert!(!detail.contains("client-token"), "{detail}");
        }
        other => panic!("{other:?}"),
    }
    assert!(fx.guard.recorded_requests().await.is_empty());

    let mut rules = passthrough_all();
    rules.remove = vec!["x-amz-security-token".into()];
    let fx = Fx::new(&[], rules).await;
    let req = fx
        .post(SIGN)
        .header("x-amz-security-token", "client-token")
        .body(json_body())
        .unwrap();
    assert_eq!(fx.send(req).await.unwrap().0, StatusCode::OK);
    let rec = fx.guard.recorded_requests().await;
    assert!(header_values(&rec[0], "x-amz-security-token").is_empty());
    verify(&rec[0], SECRET).unwrap();
}

fn streamed() -> Body {
    let chunks = futures_util::stream::iter([
        Ok::<_, oagw_sdk::body::BoxError>(Bytes::from_static(b"{\"k\":")),
        Ok(Bytes::from_static(b"2}")),
    ]);
    Body::Stream(Box::pin(chunks))
}

/// A streamed body from an in-process caller is refused (400) before
/// anything is sent; with `unsigned_payload` it is sent as
/// `UNSIGNED-PAYLOAD` and the rest of the request is signed.
#[tokio::test]
async fn streamed_body_is_refused_unless_unsigned_payload() {
    let fx = Fx::new(&[], allowlist(&[])).await;
    let err = fx
        .send(fx.post(SIGN).body(streamed()).unwrap())
        .await
        .unwrap_err();
    assert_eq!(err.status_code(), 400);
    match ServiceGatewayError::from(err) {
        ServiceGatewayError::Validation {
            field,
            reason,
            detail,
        } => {
            assert_eq!(field, "body");
            assert_eq!(reason, oagw_sdk::field::VALIDATION);
            assert!(detail.contains("auth.config.unsigned_payload"), "{detail}");
        }
        other => panic!("{other:?}"),
    }
    assert!(fx.guard.recorded_requests().await.is_empty());

    let fx = Fx::new(&[("unsigned_payload", "true")], allowlist(&[])).await;
    let (status, _) = fx
        .send(fx.post(SIGN).body(streamed()).unwrap())
        .await
        .unwrap();
    assert_eq!(status, StatusCode::OK);
    let rec = fx.guard.recorded_requests().await;
    assert_eq!(rec.len(), 1);
    assert_eq!(rec[0].body, br#"{"k":2}"#);
    assert_eq!(
        header_values(&rec[0], "x-amz-content-sha256"),
        ["UNSIGNED-PAYLOAD"]
    );
    verify(&rec[0], SECRET).unwrap();
}

/// An upstream signing for `service`, with `PUT` routes on `/bucket`
/// (any suffix, also `GET`) and on `/dir/` (exactly).
async fn object_store(h: &AppHarness, guard: &MockGuard, service: &str) -> String {
    let alias = format!("sigv4-{}", Uuid::new_v4().as_simple());
    let ctx = h.security_context().clone();
    let upstream = h
        .facade()
        .create_upstream(
            ctx.clone(),
            upstream_request(
                h.mock_port(),
                &alias,
                auth_config(&[("service", service)]),
                allowlist(&[]),
            ),
        )
        .await
        .unwrap();
    for (path, methods, path_suffix_mode) in [
        (
            guard.path("/bucket"),
            vec![HttpMethod::Put, HttpMethod::Get],
            PathSuffixMode::Append,
        ),
        (
            guard.path("/dir/"),
            vec![HttpMethod::Put],
            PathSuffixMode::Disabled,
        ),
    ] {
        h.facade()
            .create_route(
                ctx.clone(),
                CreateRouteRequest::builder(
                    upstream.id,
                    MatchRules {
                        http: Some(HttpMatch {
                            methods,
                            path,
                            query_allowlist: vec![],
                            path_suffix_mode,
                        }),
                        grpc: None,
                    },
                )
                .build(),
            )
            .await
            .unwrap();
    }
    alias
}

/// The path the upstream received; the bound [`AddQuery`] adds a query.
fn uri_path(r: &RecordedRequest) -> &str {
    r.uri
        .split_once('?')
        .map_or(r.uri.as_str(), |(path, _)| path)
}

async fn put(
    h: &AppHarness,
    uri: String,
) -> Result<StatusCode, toolkit_canonical_errors::CanonicalError> {
    let req = http::Request::put(uri)
        .header("content-type", "application/json")
        .body(json_body())
        .unwrap();
    let resp = h
        .facade()
        .proxy_request(h.security_context().clone(), req)
        .await?;
    Ok(resp.status())
}

/// S3 keys are not normalized: a path the gateway changed (`//`, `.`,
/// `..`, a trailing `/` removed, or added by a route path that ends with
/// one) would sign and write another object, so it is refused (400 on
/// `path`) and nothing is sent; a WebSocket handshake too. A path the
/// gateway kept, an encoded `/` included, is signed as sent, and so is a
/// trailing `/` the route path gives back.
#[tokio::test]
async fn s3_refuses_a_path_the_gateway_rewrote() {
    let h = harness().await;
    let guard = MockGuard::new();
    let alias = object_store(&h, &guard, "s3").await;
    for suffix in [
        "/bucket/a//b",
        "/bucket//a",
        "/bucket/a/./b",
        "/bucket/a/../b",
        "/bucket/folder/",
        "/dir",
    ] {
        let err = put(&h, format!("/{alias}{}", guard.path(suffix)))
            .await
            .unwrap_err();
        assert_eq!(err.status_code(), 400, "{suffix}");
        match ServiceGatewayError::from(err) {
            ServiceGatewayError::Validation { field, detail, .. } => {
                assert_eq!(field, "path", "{suffix}");
                assert!(detail.contains("another object"), "{detail}");
            }
            other => panic!("{suffix}: {other:?}"),
        }
    }
    let upgrade = http::Request::get(format!("/{alias}{}", guard.path("/bucket/a//b")))
        .header("upgrade", "websocket")
        .header("connection", "Upgrade")
        .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==")
        .header("sec-websocket-version", "13")
        .body(Body::Empty)
        .unwrap();
    let err = h
        .facade()
        .proxy_request(h.security_context().clone(), upgrade)
        .await
        .unwrap_err();
    assert_eq!(err.status_code(), 400);
    assert!(guard.recorded_requests().await.is_empty());

    // Characters a client sends unencoded go out as sent and are signed
    // encoded, as S3 checks them.
    let sent = [
        "/bucket/a/b",
        "/bucket/a%2Fb",
        "/dir/",
        "/bucket/a:b",
        "/bucket/photo(1).jpg",
        "/bucket/x!y*z",
        "/bucket/k=v@w,u",
    ];
    for suffix in sent {
        put(&h, format!("/{alias}{}", guard.path(suffix)))
            .await
            .unwrap();
    }
    let rec = guard.recorded_requests().await;
    let paths: Vec<&str> = rec.iter().map(uri_path).collect();
    let expected: Vec<String> = sent.iter().map(|s| guard.path(s)).collect();
    assert_eq!(paths, expected);
    for r in &rec {
        verify_encoded(r, SECRET, false).unwrap();
    }
}

/// With `double` encoding the service normalizes the path as the gateway
/// does, so a path the gateway rewrote is signed and sent.
#[tokio::test]
async fn double_encoding_signs_a_path_the_gateway_rewrote() {
    let h = harness().await;
    let guard = MockGuard::new();
    let alias = object_store(&h, &guard, "bedrock").await;
    put(&h, format!("/{alias}{}", guard.path("/bucket/a//b/")))
        .await
        .unwrap();
    let rec = guard.recorded_requests().await;
    assert_eq!(rec.len(), 1);
    assert_eq!(uri_path(&rec[0]), guard.path("/bucket/a/b"));
    verify(&rec[0], SECRET).unwrap();
}

/// A WebSocket handshake is signed with an empty payload; the handshake
/// headers are not signed.
#[tokio::test]
async fn websocket_handshake_is_signed() {
    let fx = Fx::new(&[], allowlist(&["x-marker"])).await;
    let marker = Uuid::new_v4().to_string();
    let req = http::Request::get(format!("/{}/ws/echo", fx.alias))
        .header("upgrade", "websocket")
        .header("connection", "Upgrade")
        .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==")
        .header("sec-websocket-version", "13")
        .header("x-marker", &marker)
        .body(Body::Empty)
        .unwrap();
    assert_eq!(
        fx.send(req).await.unwrap().0,
        StatusCode::SWITCHING_PROTOCOLS
    );
    let rec: Vec<RecordedRequest> = fx
        .guard
        .all_recorded_requests()
        .await
        .into_iter()
        .filter(|r| header_values(r, "x-marker") == [marker.as_str()])
        .collect();
    assert_eq!(rec.len(), 1);
    assert_eq!(rec[0].uri, "/ws/echo?t=x%20y%2Bz");
    assert_eq!(
        verify(&rec[0], SECRET).unwrap(),
        "host;x-amz-content-sha256;x-amz-date"
    );
}

/// Writes check the plugin's config: a missing region, service or key
/// reference is a 400 naming the key, and every reference, the optional
/// session token included, must resolve for the caller.
#[tokio::test]
async fn writes_check_the_config_and_the_references() {
    let h = harness().await;
    let ctx = h.security_context().clone();
    let create = |extra: &'static [(&'static str, &'static str)]| {
        let h = &h;
        let ctx = ctx.clone();
        async move {
            h.facade()
                .create_upstream(
                    ctx,
                    upstream_request(
                        h.mock_port(),
                        &format!("sigv4-w-{}", Uuid::new_v4().as_simple()),
                        auth_config(extra),
                        allowlist(&[]),
                    ),
                )
                .await
        }
    };

    for (key, field) in [
        ("region", "auth.config.region"),
        ("service", "auth.config.service"),
        ("access_key_id_ref", "auth.config.access_key_id_ref"),
        ("secret_access_key_ref", "auth.config.secret_access_key_ref"),
    ] {
        let extra: &'static [(&'static str, &'static str)] = match key {
            "region" => &[("region", "")],
            "service" => &[("service", "")],
            "access_key_id_ref" => &[("access_key_id_ref", "")],
            _ => &[("secret_access_key_ref", "")],
        };
        let err = create(extra).await.unwrap_err();
        match ServiceGatewayError::from(err) {
            ServiceGatewayError::Validation {
                field: f, reason, ..
            } => {
                assert_eq!(f, field, "{key}");
                assert_eq!(reason, oagw_sdk::field::REQUIRED, "{key}");
            }
            other => panic!("{key}: {other:?}"),
        }
    }

    for (extra, subject) in [
        (
            &[("secret_access_key_ref", "cred://aws-missing")][..],
            oagw_sdk::precondition::SECRET_ACCESS_KEY_REF,
        ),
        (
            &[("session_token_ref", "cred://aws-missing")][..],
            oagw_sdk::precondition::SESSION_TOKEN_REF,
        ),
    ] {
        let extra: &'static [(&'static str, &'static str)] = extra;
        let err = create(extra).await.unwrap_err();
        match ServiceGatewayError::from(err) {
            ServiceGatewayError::FailedPrecondition {
                precondition_type,
                subject: s,
                detail,
            } => {
                assert_eq!(precondition_type, oagw_sdk::precondition::STATE);
                assert_eq!(s, subject);
                assert!(!detail.contains("aws-missing"), "{detail}");
            }
            other => panic!("{subject}: {other:?}"),
        }
    }

    create(&[("session_token_ref", "cred://aws-token")])
        .await
        .unwrap();
}

/// The signer's credential read is the gateway's time, not the upstream's
/// (ADR-0005, *Full budget*). A deadline hit in the exchange after a read
/// that used more than a tenth of the request timeout is not counted, so
/// the circuit stays closed and the next request reaches the upstream.
#[tokio::test]
async fn circuit_breaker_ignores_a_deadline_the_credential_read_used_up() {
    assert_breaker_after_deadline_hit(Duration::from_secs(3), 504, 2).await;
}

/// With a fast read the same deadline hit counts and opens the circuit.
#[tokio::test]
async fn circuit_breaker_counts_a_deadline_after_a_fast_credential_read() {
    assert_breaker_after_deadline_hit(Duration::ZERO, 503, 1).await;
}

/// Two requests the upstream holds past the deadline, on a breaker that
/// opens on the first counted failure: the second one's status and how many
/// reached the upstream.
///
/// Real time: the mock is reached over TCP, and paused time would jump to
/// the deadline before the exchange. The gateway's share of the 10 s
/// timeout is 1 s: the fast case's steps before the exchange must fit in
/// it, the slow read (3 s) is well past it.
async fn assert_breaker_after_deadline_hit(read: Duration, second_status: u16, reached: usize) {
    const TIMEOUT: Duration = Duration::from_secs(10);
    let open_on_first_failure = CircuitBreakerConfig {
        failure_threshold: 1,
        timeout_seconds: 60,
        ..CircuitBreakerConfig::default()
    };
    let mut fx = Fx::with(
        harness_with(read, Some(TIMEOUT)).await,
        &[],
        allowlist(&[]),
        Some(open_on_first_failure),
    )
    .await;
    let mut statuses = Vec::new();
    for _ in 0..2 {
        // The upstream holds its answer past the deadline.
        let _hold = fx.guard.mock_gated(
            "POST",
            SIGN,
            MockResponse {
                status: 200,
                headers: vec![],
                body: MockBody::Json(json!({})),
            },
        );
        let req = fx.post(SIGN).body(json_body()).unwrap();
        statuses.push(fx.send(req).await.unwrap_err().status_code());
    }
    assert_eq!(statuses, [504, second_status], "read {read:?}");
    assert_eq!(
        fx.guard.recorded_requests().await.len(),
        reached,
        "read {read:?}"
    );
}
