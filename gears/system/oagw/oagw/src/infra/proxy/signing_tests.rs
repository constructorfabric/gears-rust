//! The auth plugin's signing phase (ADR-0023) through the pipeline: the
//! in-process SDK client against the shared mock upstream, with a test auth
//! plugin in place of the built-in no-op one and a test transform in place
//! of the request-id one (writes accept only built-in plugin ids).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use http::StatusCode;
use oagw_sdk::reason::auth::FailureReason;
use oagw_sdk::{
    AuthConfig, Body, CreateRouteRequest, CreateUpstreamRequest, Endpoint, HTTP_PROTOCOL_ID,
    HeadersConfig, HttpMatch, HttpMethod, MatchRules, PassthroughMode, PathSuffixMode,
    PluginBinding, PluginsConfig, RequestHeaderRules, Scheme, Server, ServiceGatewayError,
    SharingMode,
};
use opentelemetry::metrics::MeterProvider;
use opentelemetry_sdk::metrics::data::{AggregatedMetrics, MetricData};
use opentelemetry_sdk::metrics::{InMemoryMetricExporter, PeriodicReader, SdkMeterProvider};
use serde_json::json;
use uuid::Uuid;

use crate::domain::gts_helpers::{NOOP_AUTH_PLUGIN_ID, REQUEST_ID_TRANSFORM_PLUGIN_ID};
use crate::domain::plugin::{
    AuthContext, AuthPlugin, PluginError, QueryChange, SigningBody, SigningContext, SigningOutput,
    TransformPlugin, TransformRequestContext,
};
use crate::domain::ports::OagwMetricsPort;
use crate::domain::ports::metric_labels::phase;
use crate::infra::metrics::OagwMetricsMeter;
use crate::test_support::{AppHarness, MockBody, MockGuard, MockResponse, RecordedRequest};

const PATH: &str = "/v1/sign";

/// What the signing phase saw; the caller as its subject id.
#[derive(Debug, Clone)]
struct Seen {
    method: String,
    scheme: String,
    host: String,
    port: u16,
    path: String,
    query: Option<String>,
    headers: Vec<(String, Vec<u8>)>,
    body: SigningBody,
    config: HashMap<String, String>,
    subject: Uuid,
}

impl Seen {
    fn lines(&self, name: &str) -> Vec<&[u8]> {
        self.headers
            .iter()
            .filter(|(k, _)| k == name)
            .map(|(_, v)| v.as_slice())
            .collect()
    }
}

/// An auth plugin that sets `x-auth` and, in its signing phase, records the
/// request and returns `output`, or fails when `output` is `None`, or never
/// answers when `hang` is set. It signs only when `signs` is set.
struct Signer {
    output: Option<SigningOutput>,
    signs: bool,
    hang: bool,
    seen: Mutex<Vec<Seen>>,
}

impl Signer {
    fn new(output: Option<SigningOutput>) -> Arc<Self> {
        Arc::new(Self {
            output,
            signs: true,
            hang: false,
            seen: Mutex::new(Vec::new()),
        })
    }

    fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl AuthPlugin for Signer {
    async fn authenticate(&self, ctx: &mut AuthContext) -> Result<(), PluginError> {
        ctx.set_headers.push(("x-auth".into(), "token".into()));
        Ok(())
    }

    fn signs(&self) -> bool {
        self.signs
    }

    async fn sign(&self, ctx: &SigningContext) -> Result<SigningOutput, PluginError> {
        self.seen.lock().unwrap().push(Seen {
            method: ctx.method.clone(),
            scheme: ctx.scheme.clone(),
            host: ctx.host.clone(),
            port: ctx.port,
            path: ctx.path.clone(),
            query: ctx.query.clone(),
            headers: ctx.headers.clone(),
            body: ctx.body.clone(),
            config: ctx.config.clone(),
            subject: ctx.security_context.subject_id(),
        });
        if self.hang {
            std::future::pending::<()>().await;
        }
        self.output
            .clone()
            .ok_or_else(|| PluginError::AuthFailed("signature refused".into()))
    }
}

/// Appends `added=1` to the query, so the signer must see the query after
/// transforms.
struct AddQuery;

#[async_trait::async_trait]
impl TransformPlugin for AddQuery {
    async fn on_request(&self, ctx: &mut TransformRequestContext) -> Result<(), PluginError> {
        ctx.query.push(("added".into(), "1".into()));
        Ok(())
    }
}

fn signed() -> SigningOutput {
    SigningOutput {
        set_headers: vec![
            ("x-signature".into(), b"sig".to_vec()),
            ("authorization".into(), b"Signed cred".to_vec()),
        ],
        query: QueryChange::Replace("added=1&q=a%20b".into()),
    }
}

fn auth_config() -> HashMap<String, String> {
    HashMap::from([("key_id".to_owned(), "k1".to_owned())])
}

/// How [`Fx`] sets up the upstream.
#[derive(Default)]
struct Setup {
    /// Endpoint port; the mock's when `None`.
    port: Option<u16>,
    /// `All` instead of an allowlist of `x-multi` and `x-marker`.
    passthrough_all: bool,
    request_timeout: Option<Duration>,
    metrics: Option<Arc<dyn OagwMetricsPort>>,
}

struct Fx {
    h: AppHarness,
    guard: MockGuard,
    signer: Arc<Signer>,
    alias: String,
}

impl Fx {
    /// [`Fx::with`] a signer returning `output` and the default setup.
    async fn new(output: Option<SigningOutput>) -> Self {
        Self::with(Signer::new(output), Setup::default()).await
    }

    /// An upstream on the mock with `signer` as its auth plugin (config
    /// [`auth_config`]), `AddQuery` bound, and an allowlist passthrough of
    /// `x-multi` and `x-marker`; a route on [`PATH`] and one on the mock's
    /// WebSocket echo.
    async fn with(signer: Arc<Signer>, setup: Setup) -> Self {
        let mut builder = AppHarness::builder()
            .with_auth_plugin(NOOP_AUTH_PLUGIN_ID, signer.clone())
            .with_transform_plugin(REQUEST_ID_TRANSFORM_PLUGIN_ID, Arc::new(AddQuery));
        if let Some(timeout) = setup.request_timeout {
            builder = builder.with_request_timeout(timeout);
        }
        if let Some(metrics) = setup.metrics {
            builder = builder.with_metrics(metrics);
        }
        let h = builder.build().await;
        let request_headers = if setup.passthrough_all {
            RequestHeaderRules {
                passthrough: PassthroughMode::All,
                ..Default::default()
            }
        } else {
            RequestHeaderRules {
                passthrough: PassthroughMode::Allowlist,
                passthrough_allowlist: vec!["x-multi".into(), "x-marker".into()],
                ..Default::default()
            }
        };
        let mut guard = MockGuard::new();
        guard.mock(
            "POST",
            PATH,
            MockResponse {
                status: 200,
                headers: vec![("content-type".into(), "application/json".into())],
                body: MockBody::Json(json!({"ok": true})),
            },
        );
        let alias = format!("sign-{}", Uuid::new_v4().as_simple());
        let ctx = h.security_context().clone();
        let upstream = h
            .facade()
            .create_upstream(
                ctx.clone(),
                CreateUpstreamRequest::builder(
                    Server {
                        endpoints: vec![Endpoint {
                            scheme: Scheme::Http,
                            host: "127.0.0.1".into(),
                            port: setup.port.unwrap_or_else(|| h.mock_port()),
                        }],
                    },
                    HTTP_PROTOCOL_ID,
                )
                .alias(alias.clone())
                .auth(AuthConfig {
                    plugin_type: NOOP_AUTH_PLUGIN_ID.into(),
                    sharing: SharingMode::Private,
                    config: Some(auth_config()),
                })
                .headers(HeadersConfig {
                    request: Some(request_headers),
                    response: None,
                })
                .plugins(PluginsConfig {
                    sharing: SharingMode::Private,
                    items: vec![PluginBinding {
                        plugin_ref: REQUEST_ID_TRANSFORM_PLUGIN_ID.into(),
                        config: Default::default(),
                    }],
                })
                .build(),
            )
            .await
            .unwrap();
        for (method, path, query_allowlist) in [
            (HttpMethod::Post, guard.path(PATH), vec!["q".to_owned()]),
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
        Self {
            h,
            guard,
            signer,
            alias,
        }
    }

    /// `POST` on [`PATH`] with `?q=a b`, two `x-multi` lines and a header the
    /// allowlist drops.
    fn post(&self, body: Body) -> http::Request<Body> {
        http::Request::post(format!("/{}{}?q=a+b", self.alias, self.guard.path(PATH)))
            .header("content-type", "application/json")
            .header("x-multi", "a")
            .header("x-multi", "b")
            .header("x-dropped", "1")
            .body(body)
            .unwrap()
    }

    /// A WebSocket handshake to the mock's echo handler, tagged with `marker`.
    fn handshake(&self, marker: &str) -> http::Request<Body> {
        http::Request::get(format!("/{}/ws/echo", self.alias))
            .header("upgrade", "websocket")
            .header("connection", "Upgrade")
            .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==")
            .header("sec-websocket-version", "13")
            .header("x-marker", marker)
            .body(Body::Empty)
            .unwrap()
    }

    async fn send(
        &self,
        req: http::Request<Body>,
    ) -> Result<StatusCode, toolkit_canonical_errors::CanonicalError> {
        let resp = self
            .h
            .facade()
            .proxy_request(self.h.security_context().clone(), req)
            .await?;
        let status = resp.status();
        let _ = resp.into_body().into_bytes().await;
        Ok(status)
    }

    /// The handshakes the mock recorded with `marker`.
    async fn handshakes(&self, marker: &str) -> Vec<RecordedRequest> {
        self.guard
            .all_recorded_requests()
            .await
            .into_iter()
            .filter(|r| header_values(r, "x-marker") == [marker])
            .collect()
    }
}

fn header_values(r: &RecordedRequest, name: &str) -> Vec<String> {
    r.headers
        .iter()
        .filter(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.clone())
        .collect()
}

/// The signing phase runs once, on the request as sent: the query after the
/// transform, `Host` with the non-default port, every line of a repeated
/// allowlisted header, the auth header and the buffered body, with the
/// upstream's auth config and the caller's security context. Its headers
/// and its query, byte for byte, reach the upstream.
#[tokio::test]
async fn signing_sees_the_final_request_and_its_output_is_sent() {
    let fx = Fx::new(Some(signed())).await;
    let body = Bytes::from_static(br#"{"k":1}"#);
    assert_eq!(
        fx.send(fx.post(Body::Bytes(body.clone()))).await.unwrap(),
        StatusCode::OK
    );

    let seen = fx.signer.seen();
    assert_eq!(seen.len(), 1);
    let s = &seen[0];
    assert_eq!(s.method, "POST");
    assert_eq!(s.scheme, "http");
    assert_eq!(s.host, "127.0.0.1");
    assert_eq!(s.port, fx.h.mock_port());
    assert_eq!(s.path, fx.guard.path(PATH));
    assert_eq!(s.query.as_deref(), Some("q=a+b&added=1"));
    let host = format!("127.0.0.1:{}", fx.h.mock_port());
    assert_eq!(s.lines("host"), [host.as_bytes()]);
    assert_eq!(s.lines("x-multi"), [&b"a"[..], b"b"]);
    assert_eq!(s.lines("x-auth"), [&b"token"[..]]);
    assert!(s.lines("x-dropped").is_empty());
    assert_eq!(s.body, SigningBody::Buffered(body.clone()));
    assert_eq!(s.config, auth_config());
    assert_eq!(s.subject, fx.h.security_context().subject_id());

    let rec = fx.guard.recorded_requests().await;
    assert_eq!(rec.len(), 1);
    assert_eq!(
        rec[0].uri,
        format!("{}?added=1&q=a%20b", fx.guard.path(PATH))
    );
    assert_eq!(header_values(&rec[0], "x-signature"), ["sig"]);
    assert_eq!(header_values(&rec[0], "authorization"), ["Signed cred"]);
    assert_eq!(header_values(&rec[0], "x-multi"), ["a", "b"]);
    assert_eq!(header_values(&rec[0], "host"), [host]);
    assert_eq!(rec[0].body, body.as_ref());
}

/// A streamed body is not read for signing: the phase gets the marker, and
/// the body still reaches the upstream.
#[tokio::test]
async fn streamed_body_is_signed_as_streamed() {
    let fx = Fx::new(Some(signed())).await;
    let chunks = futures_util::stream::iter([
        Ok::<_, oagw_sdk::body::BoxError>(Bytes::from_static(b"{\"k\":")),
        Ok(Bytes::from_static(b"2}")),
    ]);
    let req = fx.post(Body::Stream(Box::pin(chunks)));
    assert_eq!(fx.send(req).await.unwrap(), StatusCode::OK);

    let seen = fx.signer.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].body, SigningBody::Streamed);
    let rec = fx.guard.recorded_requests().await;
    assert_eq!(rec.len(), 1);
    assert_eq!(rec[0].body, br#"{"k":2}"#);
    assert_eq!(header_values(&rec[0], "x-signature"), ["sig"]);
}

/// A WebSocket handshake is signed with an empty body, the transformed query
/// and the handshake headers it is sent with; the signing headers reach the
/// upstream.
#[tokio::test]
async fn websocket_handshake_is_signed() {
    let fx = Fx::new(Some(SigningOutput {
        set_headers: vec![("x-signature".into(), b"ws-sig".to_vec())],
        query: QueryChange::Keep,
    }))
    .await;
    let marker = Uuid::new_v4().to_string();
    assert_eq!(
        fx.send(fx.handshake(&marker)).await.unwrap(),
        StatusCode::SWITCHING_PROTOCOLS
    );

    let seen = fx.signer.seen();
    assert_eq!(seen.len(), 1);
    let s = &seen[0];
    assert_eq!(s.method, "GET");
    assert_eq!(s.path, "/ws/echo");
    assert_eq!(s.query.as_deref(), Some("added=1"));
    assert_eq!(s.body, SigningBody::Buffered(Bytes::new()));
    assert_eq!(s.lines("connection"), [&b"upgrade"[..]]);
    assert_eq!(s.lines("upgrade"), [&b"websocket"[..]]);
    assert_eq!(s.lines("x-marker"), [marker.as_bytes()]);

    let rec = fx.handshakes(&marker).await;
    assert_eq!(rec.len(), 1);
    assert_eq!(rec[0].uri, "/ws/echo?added=1");
    assert_eq!(header_values(&rec[0], "x-signature"), ["ws-sig"]);
}

/// A signing failure fails the request as an auth plugin failure (401),
/// on both paths, and nothing reaches the upstream.
#[tokio::test]
async fn signing_failure_stops_the_request() {
    let fx = Fx::new(None).await;
    let marker = Uuid::new_v4().to_string();
    for req in [
        fx.post(Body::Bytes(Bytes::from_static(b"{}"))),
        fx.handshake(&marker),
    ] {
        let err = fx.send(req).await.unwrap_err();
        assert_eq!(err.status_code(), 401);
        assert!(
            matches!(
                ServiceGatewayError::from(err),
                ServiceGatewayError::AuthFailed {
                    reason: FailureReason::PluginFailed,
                    ..
                }
            ),
            "expected AUTH_PLUGIN_FAILED"
        );
    }
    assert_eq!(fx.signer.seen().len(), 2);
    assert!(fx.guard.recorded_requests().await.is_empty());
    assert!(fx.handshakes(&marker).await.is_empty());
}

/// A handshake the engine refuses (two `Upgrade` lines) fails as it does
/// without signing, and the signer is not called: header sanitization
/// alone would collapse the lines into one `Upgrade: websocket` the engine
/// accepts.
#[tokio::test]
async fn refused_handshake_is_not_signed() {
    let handshake = |fx: &Fx, marker: &str| {
        let mut req = fx.handshake(marker);
        req.headers_mut()
            .append("upgrade", http::HeaderValue::from_static("websocket"));
        req
    };
    let setup = || Setup {
        passthrough_all: true,
        ..Setup::default()
    };

    let unsigned = Fx::with(
        Arc::new(Signer {
            output: Some(signed()),
            signs: false,
            hang: false,
            seen: Mutex::new(Vec::new()),
        }),
        setup(),
    )
    .await;
    let marker = Uuid::new_v4().to_string();
    let expected = unsigned
        .send(handshake(&unsigned, &marker))
        .await
        .unwrap_err();
    assert_eq!(expected.status_code(), 400);
    assert!(unsigned.handshakes(&marker).await.is_empty());

    let fx = Fx::with(Signer::new(Some(signed())), setup()).await;
    let marker = Uuid::new_v4().to_string();
    let err = fx.send(handshake(&fx, &marker)).await.unwrap_err();
    assert_eq!(err.status_code(), expected.status_code());
    assert_eq!(
        format!("{:?}", ServiceGatewayError::from(err)),
        format!("{:?}", ServiceGatewayError::from(expected))
    );
    assert!(fx.signer.seen().is_empty());
    assert!(fx.handshakes(&marker).await.is_empty());
}

/// An output the gateway cannot send as given fails the request with the
/// invalid-plugin-config error, before anything is sent.
#[tokio::test]
async fn unusable_signing_output_stops_the_request() {
    for output in [
        SigningOutput {
            set_headers: vec![("host".into(), b"elsewhere".to_vec())],
            query: QueryChange::Keep,
        },
        SigningOutput {
            set_headers: vec![],
            query: QueryChange::Replace("a=1#b".into()),
        },
    ] {
        let fx = Fx::new(Some(output)).await;
        let err = fx
            .send(fx.post(Body::Bytes(Bytes::from_static(b"{}"))))
            .await
            .unwrap_err();
        assert_eq!(err.status_code(), 400);
        assert!(fx.guard.recorded_requests().await.is_empty());
    }
}

/// A plugin that does not declare a signing phase is never asked to sign:
/// the request goes out as built.
#[tokio::test]
async fn plugin_that_does_not_sign_is_not_called() {
    let signer = Arc::new(Signer {
        output: Some(signed()),
        signs: false,
        hang: false,
        seen: Mutex::new(Vec::new()),
    });
    let fx = Fx::with(signer, Setup::default()).await;
    assert_eq!(
        fx.send(fx.post(Body::Bytes(Bytes::from_static(b"{}"))))
            .await
            .unwrap(),
        StatusCode::OK
    );
    assert!(fx.signer.seen().is_empty());
    let rec = fx.guard.recorded_requests().await;
    assert_eq!(rec.len(), 1);
    assert_eq!(rec[0].uri, format!("{}?q=a+b&added=1", fx.guard.path(PATH)));
    assert!(header_values(&rec[0], "x-signature").is_empty());
}

/// How many `oagw_request_duration_seconds` observations `phase` has.
fn phase_count(exporter: &InMemoryMetricExporter, phase: &str) -> u64 {
    exporter
        .get_finished_metrics()
        .unwrap()
        .iter()
        .flat_map(|rm| rm.scope_metrics())
        .flat_map(|sm| sm.metrics())
        .filter(|m| m.name() == "oagw_request_duration_seconds")
        .filter_map(|m| match m.data() {
            AggregatedMetrics::F64(MetricData::Histogram(h)) => Some(
                h.data_points()
                    .filter(|dp| {
                        dp.attributes()
                            .any(|kv| kv.key.as_str() == "phase" && kv.value.as_str() == phase)
                    })
                    .map(|dp| dp.count())
                    .sum::<u64>(),
            ),
            _ => None,
        })
        .sum()
}

/// The signing phase is timed as `phase="sign"`, once per signed request,
/// on a failure too, and not for a plugin that does not sign.
#[tokio::test]
async fn signing_phase_is_timed() {
    for (signs, output, signed_count) in [
        (true, Some(signed()), 1),
        (true, None, 1),
        (false, Some(signed()), 0),
    ] {
        let exporter = InMemoryMetricExporter::default();
        let provider = SdkMeterProvider::builder()
            .with_reader(PeriodicReader::builder(exporter.clone()).build())
            .build();
        let signer = Arc::new(Signer {
            output,
            signs,
            hang: false,
            seen: Mutex::new(Vec::new()),
        });
        let fx = Fx::with(
            signer,
            Setup {
                metrics: Some(Arc::new(OagwMetricsMeter::new(
                    &provider.meter("oagw"),
                    "oagw",
                ))),
                ..Setup::default()
            },
        )
        .await;
        let _ = fx
            .send(fx.post(Body::Bytes(Bytes::from_static(b"{}"))))
            .await;
        provider.force_flush().unwrap();
        assert_eq!(
            phase_count(&exporter, phase::SIGN),
            signed_count,
            "signs={signs}"
        );
        assert_eq!(phase_count(&exporter, phase::AUTH), 1, "signs={signs}");
    }
}

/// On the scheme's default port the URL and `Host` leave the port out, and
/// the signer still gets it.
#[tokio::test]
async fn default_port_is_given_although_the_url_omits_it() {
    let fx = Fx::with(
        Signer::new(None),
        Setup {
            port: Some(80),
            ..Setup::default()
        },
    )
    .await;
    let err = fx
        .send(fx.post(Body::Bytes(Bytes::from_static(b"{}"))))
        .await
        .unwrap_err();
    assert_eq!(err.status_code(), 401);
    let seen = fx.signer.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].scheme, "http");
    assert_eq!(seen[0].port, 80);
    assert_eq!(seen[0].lines("host"), [&b"127.0.0.1"[..]]);
}

/// A signing phase that does not answer is cut off at the request deadline
/// (504), and nothing reaches the upstream.
#[tokio::test]
async fn hung_signing_times_out_and_sends_nothing() {
    let signer = Arc::new(Signer {
        output: Some(signed()),
        signs: true,
        hang: true,
        seen: Mutex::new(Vec::new()),
    });
    let fx = Fx::with(
        signer,
        Setup {
            request_timeout: Some(Duration::from_millis(300)),
            ..Setup::default()
        },
    )
    .await;
    let err = fx
        .send(fx.post(Body::Bytes(Bytes::from_static(b"{}"))))
        .await
        .unwrap_err();
    assert_eq!(err.status_code(), 504);
    assert!(matches!(
        ServiceGatewayError::from(err),
        ServiceGatewayError::Timeout
    ));
    assert_eq!(fx.signer.seen().len(), 1);
    assert!(fx.guard.recorded_requests().await.is_empty());
}

/// `seen`'s header lines with `output` applied, grouped by name with the
/// lines of a name in order.
fn signed_lines(seen: &Seen, output: &SigningOutput) -> Vec<(String, String)> {
    let set: Vec<String> = output
        .set_headers
        .iter()
        .map(|(n, _)| n.to_ascii_lowercase())
        .collect();
    let mut lines: Vec<(String, String)> = seen
        .headers
        .iter()
        .filter(|(n, _)| !set.contains(n))
        .chain(&output.set_headers)
        .map(|(n, v)| {
            (
                n.to_ascii_lowercase(),
                String::from_utf8_lossy(v).into_owned(),
            )
        })
        .collect();
    lines.sort_by(|a, b| a.0.cmp(&b.0));
    lines
}

/// The header lines the upstream received, grouped like [`signed_lines`],
/// without the framing the engine derives from the body.
fn received_lines(r: &RecordedRequest) -> Vec<(String, String)> {
    let mut lines: Vec<(String, String)> = r
        .headers
        .iter()
        .map(|(n, v)| (n.to_ascii_lowercase(), v.clone()))
        .filter(|(n, _)| n != "content-length" && n != "transfer-encoding")
        .collect();
    lines.sort_by(|a, b| a.0.cmp(&b.0));
    lines
}

/// With `passthrough: all` the client's headers the engine drops or
/// rewrites (`Expect`, `Content-Length` on a streamed body, `Upgrade` in
/// another case) are not shown to the signer as sent: the signer sees
/// exactly the header lines the upstream receives, on both paths.
#[tokio::test]
async fn signer_sees_the_headers_the_upstream_receives() {
    let output = SigningOutput {
        set_headers: vec![("x-signature".into(), b"sig".to_vec())],
        query: QueryChange::Keep,
    };
    let fx = Fx::with(
        Signer::new(Some(output.clone())),
        Setup {
            passthrough_all: true,
            ..Setup::default()
        },
    )
    .await;

    let chunks = futures_util::stream::iter([
        Ok::<_, oagw_sdk::body::BoxError>(Bytes::from_static(b"{\"k\":")),
        Ok(Bytes::from_static(b"2}")),
    ]);
    let mut req = fx.post(Body::Stream(Box::pin(chunks)));
    req.headers_mut()
        .insert("expect", http::HeaderValue::from_static("100-continue"));
    req.headers_mut()
        .insert("content-length", http::HeaderValue::from_static("7"));
    assert_eq!(fx.send(req).await.unwrap(), StatusCode::OK);

    let marker = Uuid::new_v4().to_string();
    let mut handshake = fx.handshake(&marker);
    handshake
        .headers_mut()
        .insert("upgrade", http::HeaderValue::from_static("WebSocket"));
    assert_eq!(
        fx.send(handshake).await.unwrap(),
        StatusCode::SWITCHING_PROTOCOLS
    );

    let seen = fx.signer.seen();
    assert_eq!(seen.len(), 2);
    let (post, ws) = (&seen[0], &seen[1]);
    assert!(post.lines("expect").is_empty());
    assert!(post.lines("content-length").is_empty());
    assert_eq!(post.lines("x-dropped"), [&b"1"[..]]);
    assert_eq!(ws.lines("upgrade"), [&b"websocket"[..]]);
    assert_eq!(ws.lines("connection"), [&b"upgrade"[..]]);

    let rec = fx.guard.recorded_requests().await;
    assert_eq!(rec.len(), 1);
    assert_eq!(rec[0].body, br#"{"k":2}"#);
    assert_eq!(signed_lines(post, &output), received_lines(&rec[0]));
    let rec = fx.handshakes(&marker).await;
    assert_eq!(rec.len(), 1);
    assert_eq!(signed_lines(ws, &output), received_lines(&rec[0]));
}
