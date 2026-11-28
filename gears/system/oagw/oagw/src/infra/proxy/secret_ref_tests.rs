//! Per-request secret references (ADR-0022): the `SREF-*` confirmation tests
//! that run through [`AppHarness`] — the in-process SDK client
//! (`facade().proxy_request`) and the REST router — against the shared mock
//! upstream, with a [`CredStoreSpy`] that records every `get` and
//! its context.
//!
//! Tenant tree: Root → Partner → Tenant, and Other, Tenant's sibling under
//! Partner. "No forward" means the mock upstream recorded no request.

use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use credstore_sdk::{
    CredStoreClientV1, CredStoreError, SECRET_TYPE_CATALOG, SharingMode as SecretSharing,
};
use http::{Method, StatusCode};
use oagw_sdk::reason::permission::DenialReason;
use oagw_sdk::{
    AuthConfig, Body, BurstConfig, CircuitBreakerConfig, CorsConfig, CorsHttpMethod,
    CreateRouteRequest, CreateUpstreamRequest, CreateUpstreamRequestBuilder, Endpoint,
    HTTP_PROTOCOL_ID, HeadersConfig, HttpMatch, HttpMethod, MatchRules, PassthroughMode,
    PathSuffixMode, PluginBinding, PluginsConfig, RateLimitAlgorithm, RateLimitConfig,
    RateLimitScope, RateLimitStrategy, RequestHeaderRules, SECRET_REF_HEADER, Scheme, Server,
    ServiceGatewayError, SharingMode, SustainedRate, UpdateUpstreamRequest, Window,
    field::SecretRefCode,
};
use serde_json::Value;
use tenant_resolver_sdk::TenantId;
use toolkit_canonical_errors::{CanonicalError, Problem};
use toolkit_security::SecurityContext;
use tower::ServiceExt;
use uuid::Uuid;

use super::credstore_spy::{CredStoreSpy, SpySecret};
use crate::domain::gts_helpers::{
    NOOP_AUTH_PLUGIN_ID, REQUEST_ID_TRANSFORM_PLUGIN_ID, REQUIRED_HEADERS_GUARD_PLUGIN_ID,
};
use crate::domain::plugin::{PluginError, TransformPlugin, TransformRequestContext};
use crate::domain::ports::OagwMetricsPort;
use crate::domain::ports::metric_labels::{
    CacheEviction, CacheLookup, CacheMap, CacheStale, RateLimitBucket, StreamOutcome,
};
use crate::domain::test_support::{DenyingAuthZResolverClient, MockTenantResolverClient};
use crate::test_support::{
    APIKEY_AUTH_PLUGIN_ID, AppHarness, AppHarnessBuilder, MockBody, MockGuard, MockResponse,
    RecordedRequest,
};

// ---------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------

const CHAT: &str = "/v1/chat";
const SSE: &str = "/v1/sse";

fn type_a() -> &'static str {
    SECRET_TYPE_CATALOG[1].gts_id
}

fn type_b() -> &'static str {
    SECRET_TYPE_CATALOG[2].gts_id
}

/// The tenant tree and one fixed subject per tenant.
#[derive(Clone, Copy)]
struct Tree {
    root: Uuid,
    partner: Uuid,
    tenant: Uuid,
    other: Uuid,
}

impl Tree {
    fn new() -> Self {
        Self {
            root: Uuid::new_v4(),
            partner: Uuid::new_v4(),
            tenant: Uuid::new_v4(),
            other: Uuid::new_v4(),
        }
    }

    fn resolver(&self) -> MockTenantResolverClient {
        MockTenantResolverClient::with_hierarchy(vec![
            TenantId(self.root),
            TenantId(self.partner),
            TenantId(self.tenant),
        ])
        .with_child(TenantId(self.other), TenantId(self.partner))
    }
}

/// The subject id every context of `tenant` carries.
fn subject_of(tenant: Uuid) -> Uuid {
    Uuid::from_u128(tenant.as_u128() ^ 0x5eed)
}

fn ctx(tenant: Uuid) -> SecurityContext {
    SecurityContext::builder()
        .subject_tenant_id(tenant)
        .subject_id(subject_of(tenant))
        .build()
        .expect("test security context")
}

/// An outcome comparable across the SDK and REST paths.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Outcome {
    status: u16,
    /// Problem `type` (the canonical category); empty on success.
    category: String,
    /// `context.reason` or `context.field_violations[0].reason`.
    reason: String,
    /// `context.field_violations[0].field`.
    field: String,
}

impl Outcome {
    fn from_problem(status: u16, problem: &Value) -> Self {
        let violation = &problem["context"]["field_violations"][0];
        let text = |v: &Value| v.as_str().unwrap_or_default().to_owned();
        Self {
            status,
            category: text(&problem["type"]),
            reason: if violation.is_null() {
                text(&problem["context"]["reason"])
            } else {
                text(&violation["reason"])
            },
            field: text(&violation["field"]),
        }
    }

    fn ok(status: u16) -> Self {
        Self {
            status,
            category: String::new(),
            reason: String::new(),
            field: String::new(),
        }
    }
}

fn problem_of(err: &CanonicalError) -> Value {
    serde_json::to_value(Problem::from(err.clone())).expect("problem serializes")
}

struct Fx {
    h: AppHarness,
    store: Arc<CredStoreSpy>,
    t: Tree,
    guard: MockGuard,
    alias: String,
}

impl Fx {
    async fn new() -> Self {
        Self::build(CredStoreSpy::empty(), |b| b).await
    }

    async fn build(
        store: CredStoreSpy,
        f: impl FnOnce(AppHarnessBuilder) -> AppHarnessBuilder,
    ) -> Self {
        Self::build_with_tree(Tree::new(), store, f).await
    }

    async fn build_with_tree(
        t: Tree,
        store: CredStoreSpy,
        f: impl FnOnce(AppHarnessBuilder) -> AppHarnessBuilder,
    ) -> Self {
        let store = Arc::new(store);
        let h = f(AppHarness::builder()
            .with_credstore(store.clone() as Arc<dyn CredStoreClientV1>)
            .with_tenant_resolver(t.resolver()))
        .build()
        .await;
        let mut guard = MockGuard::new();
        guard.mock(
            "POST",
            CHAT,
            MockResponse {
                status: 200,
                headers: vec![],
                body: MockBody::Json(serde_json::json!({"ok": true})),
            },
        );
        guard.mock(
            "GET",
            SSE,
            MockResponse {
                status: 200,
                headers: vec![],
                body: MockBody::Sse(vec!["one".into(), "two".into()]),
            },
        );
        Self {
            h,
            store,
            t,
            guard,
            alias: format!("sref-{}", Uuid::new_v4().as_simple()),
        }
    }

    fn server(&self) -> Server {
        Server {
            endpoints: vec![Endpoint {
                scheme: Scheme::Http,
                host: "127.0.0.1".into(),
                port: self.h.mock_port(),
            }],
        }
    }

    fn builder(&self, auth: Option<AuthConfig>) -> CreateUpstreamRequestBuilder {
        let b = CreateUpstreamRequest::builder(self.server(), HTTP_PROTOCOL_ID).alias(&self.alias);
        match auth {
            Some(auth) => b.auth(auth),
            None => b,
        }
    }

    /// Create `owner`'s upstream for the fixture alias, with a route for
    /// `POST /v1/chat` and `GET /v1/sse`. Returns the upstream id.
    async fn upstream(&self, owner: Uuid, auth: Option<AuthConfig>) -> Uuid {
        self.upstream_with(owner, self.builder(auth)).await
    }

    async fn upstream_with(&self, owner: Uuid, b: CreateUpstreamRequestBuilder) -> Uuid {
        let up = self
            .h
            .facade()
            .create_upstream(ctx(owner), b.build())
            .await
            .expect("create upstream");
        self.route(owner, up.id, vec![HttpMethod::Post], CHAT, vec![])
            .await;
        self.route(owner, up.id, vec![HttpMethod::Get], SSE, vec![])
            .await;
        up.id
    }

    async fn route(
        &self,
        owner: Uuid,
        upstream_id: Uuid,
        methods: Vec<HttpMethod>,
        path: &str,
        query_allowlist: Vec<String>,
    ) {
        self.h
            .facade()
            .create_route(
                ctx(owner),
                CreateRouteRequest::builder(
                    upstream_id,
                    MatchRules {
                        http: Some(HttpMatch {
                            methods,
                            path: self.guard.path(path),
                            query_allowlist,
                            path_suffix_mode: PathSuffixMode::Disabled,
                        }),
                        grpc: None,
                    },
                )
                .build(),
            )
            .await
            .expect("create route");
    }

    fn secret(&self, reference: &str, value: &str, owner: Uuid, visible_to: &[Uuid]) {
        assert!(
            self.store.add_secret(
                SpySecret::new(reference, value)
                    .owned_by(owner)
                    .visible_to(visible_to.iter().copied()),
            )
        );
    }

    /// A `POST /v1/chat` request with the given secret reference values.
    fn post(&self, refs: &[&str]) -> http::Request<Body> {
        self.request(Method::POST, CHAT, "", refs, &[])
    }

    fn request(
        &self,
        method: Method,
        path: &str,
        query: &str,
        refs: &[&str],
        headers: &[(&str, &str)],
    ) -> http::Request<Body> {
        let mut b = http::Request::builder().method(method.clone()).uri(format!(
            "/{}{}{query}",
            self.alias,
            self.guard.path(path)
        ));
        for r in refs {
            b = b.header(SECRET_REF_HEADER, *r);
        }
        for (k, v) in headers {
            b = b.header(*k, *v);
        }
        if method == Method::POST {
            b = b.header(http::header::CONTENT_TYPE, "application/json");
            b.body(Body::from(r#"{"model":"m"}"#)).unwrap()
        } else {
            b.body(Body::Empty).unwrap()
        }
    }

    /// Send through the in-process SDK client.
    async fn sdk(&self, caller: Uuid, req: http::Request<Body>) -> Result<Outcome, CanonicalError> {
        self.sdk_as(ctx(caller), req).await
    }

    async fn sdk_as(
        &self,
        ctx: SecurityContext,
        req: http::Request<Body>,
    ) -> Result<Outcome, CanonicalError> {
        match self.h.facade().proxy_request(ctx, req).await {
            Ok(resp) => {
                let status = resp.status().as_u16();
                // Drain, so a streamed body completes.
                let _ = resp.into_body().into_bytes().await;
                Ok(Outcome::ok(status))
            }
            Err(e) => Err(e),
        }
    }

    /// Send through the SDK; an error becomes its [`Outcome`].
    async fn sdk_outcome(&self, caller: Uuid, req: http::Request<Body>) -> Outcome {
        match self.sdk(caller, req).await {
            Ok(o) => o,
            Err(e) => Outcome::from_problem(e.status_code(), &problem_of(&e)),
        }
    }

    /// Send the same request through the REST router as `caller`.
    async fn rest(&self, caller: Uuid, req: http::Request<Body>) -> (Outcome, Value) {
        let (mut parts, body) = req.into_parts();
        parts.uri = format!("/oagw/v1/proxy{}", parts.uri).parse().unwrap();
        let bytes = body.into_bytes().await.unwrap();
        let req = http::Request::from_parts(parts, axum::body::Body::from(bytes));
        let resp = self
            .h
            .router_as(ctx(caller))
            .oneshot(req)
            .await
            .expect("router");
        let status = resp.status().as_u16();
        if status >= 400 {
            // Every gateway error says so (scenarios 31.4–31.11).
            assert_eq!(
                resp.headers()
                    .get("x-oagw-error-source")
                    .and_then(|v| v.to_str().ok()),
                Some("gateway"),
                "error source of a {status}"
            );
        }
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        if status < 400 {
            return (Outcome::ok(status), Value::Null);
        }
        let json: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        (Outcome::from_problem(status, &json), json)
    }

    async fn recorded(&self) -> Vec<RecordedRequest> {
        self.guard.recorded_requests().await
    }

    fn gets(&self) -> Vec<String> {
        self.store.gets().into_iter().map(|g| g.reference).collect()
    }
}

fn header_values(r: &RecordedRequest, name: &str) -> Vec<String> {
    r.headers
        .iter()
        .filter(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.clone())
        .collect()
}

fn apikey(sharing: SharingMode, entries: &[(&str, &str)]) -> AuthConfig {
    let mut config: HashMap<String, String> = [
        ("header".to_owned(), "authorization".to_owned()),
        ("prefix".to_owned(), "Bearer ".to_owned()),
    ]
    .into_iter()
    .collect();
    for (k, v) in entries {
        config.insert((*k).to_owned(), (*v).to_owned());
    }
    AuthConfig {
        plugin_type: APIKEY_AUTH_PLUGIN_ID.into(),
        sharing,
        config: Some(config),
    }
}

/// API-key auth in `request_ref` mode with an optional default and types.
fn request_ref(sharing: SharingMode, default: Option<&str>, types: Option<&str>) -> AuthConfig {
    let mut entries = vec![("credential_mode", "request_ref")];
    if let Some(d) = default {
        entries.push(("secret_ref", d));
    }
    if let Some(t) = types {
        entries.push(("request_secret_types", t));
    }
    apikey(sharing, &entries)
}

/// API-key auth in `config` mode (no `credential_mode` key).
fn config_mode(sharing: SharingMode, secret_ref: &str) -> AuthConfig {
    apikey(sharing, &[("secret_ref", secret_ref)])
}

fn one_per_minute() -> RateLimitConfig {
    RateLimitConfig {
        sharing: SharingMode::Private,
        algorithm: RateLimitAlgorithm::TokenBucket,
        sustained: SustainedRate {
            rate: 1,
            window: Window::Minute,
        },
        burst: Some(BurstConfig { capacity: 1 }),
        scope: RateLimitScope::Tenant,
        strategy: RateLimitStrategy::Reject,
        cost: 1,
        budget: None,
        response_headers: false,
    }
}

fn bad_request(reason: &str) -> Outcome {
    Outcome {
        status: 400,
        category: "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~".into(),
        reason: reason.into(),
        field: SECRET_REF_HEADER.into(),
    }
}

fn forbidden(reason: &str) -> Outcome {
    Outcome {
        status: 403,
        category: "gts://gts.cf.core.errors.err.v1~cf.core.err.permission_denied.v1~".into(),
        reason: reason.into(),
        field: String::new(),
    }
}

/// Guard 6: a header rule or transform set the auth header, a gateway
/// misconfiguration, so a 500 with no reason code.
fn auth_header_conflict() -> Outcome {
    Outcome {
        status: 500,
        category: "gts://gts.cf.core.errors.err.v1~cf.core.err.internal.v1~".into(),
        reason: String::new(),
        field: String::new(),
    }
}

const NOT_ALLOWED: &str = oagw_sdk::field::SECRET_REF_NOT_ALLOWED;
const REQUIRED: &str = oagw_sdk::field::SECRET_REF_REQUIRED;
const INVALID: &str = oagw_sdk::field::INVALID_SECRET_REF;
const NOT_ACCESSIBLE: &str = oagw_sdk::reason::permission::SECRET_REF_NOT_ACCESSIBLE;
const POLICY_DENIED: &str = oagw_sdk::reason::permission::SECRET_REF_POLICY_DENIED;

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// SREF-CFG-001: an API-key auth config without `credential_mode` behaves as
/// `config` and is stored unchanged.
#[tokio::test]
async fn cfg_001_absent_mode_is_config_and_stored_unchanged() {
    let fx = Fx::new().await;
    fx.secret("cfg-key", "sk-cfg", fx.t.tenant, &[fx.t.tenant]);
    let auth = config_mode(SharingMode::Private, "cred://cfg-key");
    let id = fx.upstream(fx.t.tenant, Some(auth.clone())).await;

    let stored =
        fx.h.facade()
            .get_upstream(ctx(fx.t.tenant), id)
            .await
            .unwrap();
    assert_eq!(stored.auth.unwrap().config, auth.config);

    fx.sdk(fx.t.tenant, fx.post(&[])).await.unwrap();
    let rec = fx.recorded().await;
    assert_eq!(header_values(&rec[0], "authorization"), ["Bearer sk-cfg"]);
}

/// SREF-CFG-002: an unknown `credential_mode` is rejected with 400.
#[tokio::test]
async fn cfg_002_unknown_mode_rejected() {
    let fx = Fx::new().await;
    let err =
        fx.h.facade()
            .create_upstream(
                ctx(fx.t.tenant),
                fx.builder(Some(apikey(
                    SharingMode::Private,
                    &[("credential_mode", "header_ref")],
                )))
                .build(),
            )
            .await
            .unwrap_err();
    let o = Outcome::from_problem(err.status_code(), &problem_of(&err));
    assert_eq!(
        (o.status, o.field.as_str(), o.reason.as_str()),
        (
            400,
            "auth.config.credential_mode",
            oagw_sdk::field::INVALID_VALUE
        )
    );
}

/// SREF-CFG-003: a malformed `request_secret_types` entry, and
/// `request_secret_types` outside `request_ref` mode, are rejected with 400.
#[tokio::test]
async fn cfg_003_bad_secret_types_rejected() {
    let fx = Fx::new().await;
    let good = format!("{}, {}", type_a(), type_b());
    let cases: Vec<(&str, AuthConfig)> = vec![
        ("empty", request_ref(SharingMode::Private, None, Some(""))),
        (
            "not a gts id",
            request_ref(SharingMode::Private, None, Some("api-key")),
        ),
        (
            "empty entry",
            request_ref(SharingMode::Private, None, Some(&format!("{},", type_a()))),
        ),
        (
            "config mode",
            apikey(
                SharingMode::Private,
                &[("secret_ref", "k"), ("request_secret_types", type_a())],
            ),
        ),
        (
            "explicit config mode",
            apikey(
                SharingMode::Private,
                &[
                    ("credential_mode", "config"),
                    ("secret_ref", "k"),
                    ("request_secret_types", type_a()),
                ],
            ),
        ),
    ];
    for (case, auth) in cases {
        let err =
            fx.h.facade()
                .create_upstream(ctx(fx.t.tenant), fx.builder(Some(auth)).build())
                .await
                .expect_err(case);
        let o = Outcome::from_problem(err.status_code(), &problem_of(&err));
        let reason = if case.contains("config mode") {
            oagw_sdk::field::VALIDATION
        } else {
            oagw_sdk::field::INVALID_GTS_FORMAT
        };
        assert_eq!(
            (o.status, o.field.as_str(), o.reason.as_str()),
            (400, "auth.config.request_secret_types", reason),
            "{case}"
        );
    }
    fx.h.facade()
        .create_upstream(
            ctx(fx.t.tenant),
            fx.builder(Some(request_ref(SharingMode::Private, None, Some(&good))))
                .build(),
        )
        .await
        .expect("a valid list is accepted");
}

/// SREF-CFG-007: in both credential modes, management rejects an API-key
/// auth config with a missing auth header, an invalid header name, or a
/// header the gateway sets or strips itself (`Host`, body framing,
/// hop-by-hop, forwarding headers, `X-OAGW-*`), with 400 on
/// `auth.config.header`.
#[tokio::test]
async fn cfg_007_auth_header_must_be_injectable() {
    let fx = Fx::new().await;
    fx.secret("k", "sk-k", fx.t.tenant, &[fx.t.tenant]);
    for mode in ["request_ref", "config"] {
        // `config` mode needs its `secret_ref`; the header is what fails.
        let entries = |header: Option<&str>| {
            let mut config: HashMap<String, String> =
                [("credential_mode".to_owned(), mode.to_owned())].into();
            if mode == "config" {
                config.insert("secret_ref".to_owned(), "k".to_owned());
            }
            if let Some(h) = header {
                config.insert("header".to_owned(), h.to_owned());
            }
            AuthConfig {
                plugin_type: APIKEY_AUTH_PLUGIN_ID.into(),
                sharing: SharingMode::Private,
                config: Some(config),
            }
        };
        let mut cases: Vec<(String, AuthConfig, &str)> = vec![
            ("missing".into(), entries(None), oagw_sdk::field::REQUIRED),
            (
                "bad header".into(),
                entries(Some("bad header")),
                oagw_sdk::field::INVALID_VALUE,
            ),
        ];
        for h in [
            "Host",
            "content-length",
            "Transfer-Encoding",
            "connection",
            "te",
            "upgrade",
            "proxy-authorization",
            "X-OAGW-Target-Host",
            "X-Forwarded-For",
            "forwarded",
            "x-real-ip",
        ] {
            cases.push((h.into(), entries(Some(h)), oagw_sdk::field::INVALID_VALUE));
        }
        for (case, auth, reason) in cases {
            let err =
                fx.h.facade()
                    .create_upstream(ctx(fx.t.tenant), fx.builder(Some(auth)).build())
                    .await
                    .expect_err(&format!("{mode}: {case}"));
            let o = Outcome::from_problem(err.status_code(), &problem_of(&err));
            assert_eq!(
                (o.status, o.field.as_str(), o.reason.as_str()),
                (400, "auth.config.header", reason),
                "{mode}: {case}"
            );
        }
    }
}

/// SREF-CFG-008: in both credential modes, management rejects an API-key
/// `prefix` that is not a valid header value or is longer than
/// `MAX_PREFIX_LEN`, with 400 on `auth.config.prefix`; a usual prefix such as
/// `Bearer ` is accepted.
#[tokio::test]
async fn cfg_008_prefix_must_be_a_header_value() {
    use crate::domain::plugin::credentials::MAX_PREFIX_LEN;

    let too_long = "p".repeat(MAX_PREFIX_LEN + 1);
    for mode in ["request_ref", "config"] {
        // One fixture per mode: both create an upstream under its alias.
        let fx = Fx::new().await;
        fx.secret("k", "sk-k", fx.t.tenant, &[fx.t.tenant]);
        let with_prefix = |prefix: &str| {
            let mut entries = vec![("credential_mode", mode), ("prefix", prefix)];
            if mode == "config" {
                entries.push(("secret_ref", "k"));
            }
            apikey(SharingMode::Private, &entries)
        };
        for (case, prefix) in [
            ("control char", "Bearer\n"),
            ("too long", too_long.as_str()),
        ] {
            let err =
                fx.h.facade()
                    .create_upstream(
                        ctx(fx.t.tenant),
                        fx.builder(Some(with_prefix(prefix))).build(),
                    )
                    .await
                    .expect_err(&format!("{mode}: {case}"));
            let o = Outcome::from_problem(err.status_code(), &problem_of(&err));
            assert_eq!(
                (o.status, o.field.as_str(), o.reason.as_str()),
                (400, "auth.config.prefix", oagw_sdk::field::INVALID_VALUE),
                "{mode}: {case}"
            );
        }
        fx.h.facade()
            .create_upstream(
                ctx(fx.t.tenant),
                fx.builder(Some(with_prefix("Bearer "))).build(),
            )
            .await
            .unwrap_or_else(|e| panic!("{mode}: a usual prefix is accepted: {e:?}"));
    }
}

/// SREF-CFG-004: management rejects an API-key auth in `config` mode
/// without `secret_ref` with 400 on `auth.config.secret_ref`, with or
/// without an explicit `credential_mode: config`; `request_ref` mode without
/// `secret_ref` is accepted (no default: a request must name a reference).
#[tokio::test]
async fn cfg_004_secret_ref_required_only_in_config_mode() {
    let fx = Fx::new().await;
    for auth in [
        apikey(SharingMode::Private, &[]),
        apikey(SharingMode::Private, &[("credential_mode", "config")]),
    ] {
        let err =
            fx.h.facade()
                .create_upstream(ctx(fx.t.tenant), fx.builder(Some(auth)).build())
                .await
                .unwrap_err();
        let o = Outcome::from_problem(err.status_code(), &problem_of(&err));
        assert_eq!(
            (o.status, o.field.as_str(), o.reason.as_str()),
            (400, "auth.config.secret_ref", oagw_sdk::field::REQUIRED)
        );
    }

    fx.upstream(
        fx.t.tenant,
        Some(request_ref(SharingMode::Private, None, None)),
    )
    .await;
}

/// SREF-CFG-005: a configured default in `request_ref` mode goes through
/// the existing write-time accessibility check.
#[tokio::test]
async fn cfg_005_default_checked_at_write() {
    let fx = Fx::new().await;
    let err =
        fx.h.facade()
            .create_upstream(
                ctx(fx.t.tenant),
                fx.builder(Some(request_ref(
                    SharingMode::Private,
                    Some("cred://missing-default"),
                    None,
                )))
                .build(),
            )
            .await
            .unwrap_err();
    assert!(
        matches!(
            ServiceGatewayError::from(err),
            ServiceGatewayError::FailedPrecondition { ref subject, .. }
                if subject == oagw_sdk::precondition::SECRET_REF
        ),
        "a default that does not resolve is refused like any secret_ref"
    );
    let gets = fx.store.gets();
    assert_eq!(gets.len(), 1);
    assert_eq!(gets[0].reference, "missing-default");
    assert_eq!(gets[0].ctx.subject_tenant_id(), fx.t.tenant);
}

/// SREF-CFG-006: the header on an upstream whose auth uses another plugin,
/// or that has no auth, returns 400 `SECRET_REF_NOT_ALLOWED` with no forward.
#[tokio::test]
async fn cfg_006_other_plugin_or_no_auth_rejects_header() {
    for auth in [
        Some(AuthConfig {
            plugin_type: NOOP_AUTH_PLUGIN_ID.into(),
            sharing: SharingMode::Private,
            config: Some(
                [("credential_mode".to_owned(), "request_ref".to_owned())]
                    .into_iter()
                    .collect(),
            ),
        }),
        None,
    ] {
        let fx = Fx::new().await;
        fx.secret("k", "sk", fx.t.tenant, &[fx.t.tenant]);
        fx.upstream(fx.t.tenant, auth).await;
        let o = fx.sdk_outcome(fx.t.tenant, fx.post(&["cred://k"])).await;
        assert_eq!(o, bad_request(NOT_ALLOWED));
        assert!(fx.recorded().await.is_empty());
        assert!(fx.gets().is_empty());
    }
}

// ---------------------------------------------------------------------------
// Hierarchy
// ---------------------------------------------------------------------------

/// SREF-HIER-001: an ancestor auth in `request_ref` mode with
/// `sharing: inherit` applies to a descendant that does not override auth.
#[tokio::test]
async fn hier_001_inherited_request_ref_applies() {
    let fx = Fx::new().await;
    fx.secret("own", "sk-tenant", fx.t.tenant, &[fx.t.tenant]);
    fx.upstream(
        fx.t.partner,
        Some(request_ref(SharingMode::Inherit, None, None)),
    )
    .await;
    fx.sdk(fx.t.tenant, fx.post(&["cred://own"])).await.unwrap();
    let rec = fx.recorded().await;
    assert_eq!(
        header_values(&rec[0], "authorization"),
        ["Bearer sk-tenant"]
    );
}

/// SREF-HIER-001 (converse): a Partner auth in `request_ref` mode with
/// `sharing: private` is local-only. Tenant's own upstream without auth,
/// visible through Partner's shared rate limit, gets none of it: the header
/// is not allowed there, and a request without it is sent with no key.
#[tokio::test]
async fn hier_001_private_request_ref_not_inherited() {
    let fx = Fx::new().await;
    fx.secret("own", "sk-tenant", fx.t.tenant, &[fx.t.tenant]);
    let shared_limit = RateLimitConfig {
        sharing: SharingMode::Inherit,
        ..one_per_minute()
    };
    fx.upstream_with(
        fx.t.partner,
        fx.builder(Some(request_ref(SharingMode::Private, None, None)))
            .rate_limit(shared_limit),
    )
    .await;
    fx.upstream(fx.t.tenant, None).await;
    assert_eq!(
        fx.sdk_outcome(fx.t.tenant, fx.post(&["own"])).await,
        bad_request(NOT_ALLOWED)
    );
    fx.sdk(fx.t.tenant, fx.post(&[])).await.unwrap();
    let rec = fx.recorded().await;
    assert_eq!(rec.len(), 1);
    assert!(header_values(&rec[0], "authorization").is_empty());
    assert!(fx.gets().is_empty());
}

/// SREF-HIER-002: an ancestor that enforces `config` mode rejects a
/// descendant request carrying the header with `SECRET_REF_NOT_ALLOWED`, even
/// when the descendant's own layer sets `request_ref`.
#[tokio::test]
async fn hier_002_enforced_config_mode_wins() {
    let fx = Fx::new().await;
    fx.secret("root-key", "sk-root", fx.t.root, &[fx.t.root, fx.t.tenant]);
    fx.secret("own", "sk-tenant", fx.t.tenant, &[fx.t.tenant]);
    // The descendant binds while the ancestor's auth is `inherit`; the
    // ancestor then switches it to `enforce`.
    let root_up = fx
        .upstream(
            fx.t.root,
            Some(config_mode(SharingMode::Inherit, "cred://root-key")),
        )
        .await;
    fx.upstream(
        fx.t.tenant,
        Some(request_ref(SharingMode::Inherit, None, None)),
    )
    .await;
    fx.h.facade()
        .update_upstream(
            ctx(fx.t.root),
            root_up,
            UpdateUpstreamRequest::builder(fx.server(), HTTP_PROTOCOL_ID)
                .alias(&fx.alias)
                .auth(config_mode(SharingMode::Enforce, "cred://root-key"))
                .build(),
        )
        .await
        .expect("enforce");

    let o = fx.sdk_outcome(fx.t.tenant, fx.post(&["cred://own"])).await;
    assert_eq!(o, bad_request(NOT_ALLOWED));
    assert!(fx.recorded().await.is_empty());
    // Without the header the enforced configured key goes out.
    fx.sdk(fx.t.tenant, fx.post(&[])).await.unwrap();
    let rec = fx.recorded().await;
    assert_eq!(header_values(&rec[0], "authorization"), ["Bearer sk-root"]);
}

/// SREF-HIER-003: an ancestor that enforces `request_ref` mode applies its
/// default and type allowlist to every descendant.
#[tokio::test]
async fn hier_003_enforced_request_ref_default_and_types() {
    let fx = Fx::new().await;
    fx.store.add_secret(
        SpySecret::new("platform", "sk-platform")
            .owned_by(fx.t.root)
            .secret_type(type_a()),
    );
    fx.store.add_secret(
        SpySecret::new("tenant-b", "sk-b")
            .owned_by(fx.t.tenant)
            .secret_type(type_b())
            .visible_to([fx.t.tenant]),
    );
    fx.store.add_secret(
        SpySecret::new("tenant-a", "sk-a")
            .owned_by(fx.t.tenant)
            .secret_type(type_a())
            .visible_to([fx.t.tenant]),
    );
    let root_auth = request_ref(
        SharingMode::Inherit,
        Some("cred://platform"),
        Some(type_a()),
    );
    let root_up = fx.upstream(fx.t.root, Some(root_auth)).await;
    // The descendant's own layer, stored before the ancestor enforces,
    // names another default and no type list.
    fx.upstream(
        fx.t.tenant,
        Some(request_ref(
            SharingMode::Inherit,
            Some("cred://tenant-b"),
            None,
        )),
    )
    .await;
    fx.h.facade()
        .update_upstream(
            ctx(fx.t.root),
            root_up,
            UpdateUpstreamRequest::builder(fx.server(), HTTP_PROTOCOL_ID)
                .alias(&fx.alias)
                .auth(request_ref(
                    SharingMode::Enforce,
                    Some("cred://platform"),
                    Some(type_a()),
                ))
                .build(),
        )
        .await
        .expect("enforce");

    for caller in [fx.t.tenant, fx.t.partner] {
        fx.store.clear_gets();
        fx.sdk(caller, fx.post(&[])).await.unwrap();
        assert_eq!(fx.gets(), ["platform"]);
    }
    let rec = fx.recorded().await;
    assert!(
        rec.iter()
            .all(|r| header_values(r, "authorization") == ["Bearer sk-platform"])
    );
    let o = fx
        .sdk_outcome(fx.t.tenant, fx.post(&["cred://tenant-b"]))
        .await;
    assert_eq!(o, forbidden(POLICY_DENIED));
    fx.sdk(fx.t.tenant, fx.post(&["cred://tenant-a"]))
        .await
        .unwrap();
}

/// SREF-HIER-004: a descendant override that switches the mode needs the
/// existing `override_auth` grant.
#[tokio::test]
async fn hier_004_mode_switch_needs_override_auth() {
    use crate::domain::model::{AuthConfig as DAuth, SharingMode as DSharing};
    use crate::domain::services::ControlPlaneService;
    use crate::domain::services::ControlPlaneServiceImpl;
    use crate::domain::ssrf::SsrfGuard;
    use crate::infra::storage::{InMemoryRouteRepo, InMemoryUpstreamRepo};

    struct DenyAction(&'static str);
    #[async_trait::async_trait]
    impl authz_resolver_sdk::AuthZResolverApi for DenyAction {
        async fn evaluate(
            &self,
            _: toolkit_security::PlatformSecurityContext,
            request: authz_resolver_sdk::EvaluationRequest,
        ) -> Result<authz_resolver_sdk::EvaluationResponse, CanonicalError> {
            Ok(authz_resolver_sdk::EvaluationResponse {
                decision: request.action.name != self.0,
                context: authz_resolver_sdk::EvaluationResponseContext {
                    constraints: vec![],
                    deny_reason: None,
                },
            })
        }

        async fn evaluate_batch(
            &self,
            ctx: toolkit_security::PlatformSecurityContext,
            req: authz_resolver_sdk::EvaluationBatchRequest,
        ) -> Result<authz_resolver_sdk::EvaluationBatchResponse, CanonicalError> {
            authz_resolver_sdk::evaluate_batch_sequentially(self, ctx, req).await
        }
    }
    let t = Tree::new();
    let auth = |mode: &str| DAuth {
        plugin_type: APIKEY_AUTH_PLUGIN_ID.into(),
        sharing: DSharing::Inherit,
        config: Some(
            [
                ("header".to_owned(), "authorization".to_owned()),
                ("credential_mode".to_owned(), mode.to_owned()),
            ]
            .into_iter()
            .collect(),
        ),
    };
    let make_req = |auth: DAuth| crate::domain::model::CreateUpstreamRequest {
        id: None,
        server: crate::domain::model::Server {
            endpoints: vec![crate::domain::model::Endpoint {
                scheme: crate::domain::model::Scheme::Https,
                host: "api.example.com".into(),
                port: 443,
            }],
        },
        protocol: HTTP_PROTOCOL_ID.into(),
        alias: None,
        auth: Some(auth),
        headers: None,
        plugins: None,
        rate_limit: None,
        circuit_breaker: None,
        cors: None,
        tags: vec![],
        enabled: true,
    };
    for (pdp, allowed) in [
        (DenyAction("override_auth"), false),
        (DenyAction("none"), true),
    ] {
        let svc = ControlPlaneServiceImpl::new(
            Arc::new(InMemoryUpstreamRepo::new()),
            Arc::new(InMemoryRouteRepo::new()),
            Arc::new(t.resolver()),
            authz_resolver_sdk::PolicyEnforcer::new(Arc::new(pdp)),
            Arc::new({
                let store = CredStoreSpy::empty();
                store.add_secret(
                    SpySecret::new("partner-key", "sk-partner")
                        .owned_by(t.partner)
                        .visible_to([t.partner]),
                );
                store
            }),
            Arc::new(SsrfGuard::disabled()),
            None,
        );
        // The partner's config-mode layer, which needs its `secret_ref`.
        let mut partner_auth = auth("config");
        let partner_config = partner_auth.config.as_mut().unwrap();
        partner_config.remove("credential_mode");
        partner_config.insert("secret_ref".to_owned(), "partner-key".to_owned());
        svc.create_upstream(&ctx(t.partner), make_req(partner_auth))
            .await
            .unwrap();
        let result = svc
            .create_upstream(&ctx(t.tenant), make_req(auth("request_ref")))
            .await;
        assert_eq!(result.is_ok(), allowed, "{result:?}");
        if !allowed {
            assert!(matches!(
                result,
                Err(crate::domain::error::DomainError::Forbidden { .. })
            ));
        }
    }
}

// ---------------------------------------------------------------------------
// Request header
// ---------------------------------------------------------------------------

/// SREF-REQ-001: each row of the mode table gives the stated result, with
/// and without the header.
#[tokio::test]
async fn req_001_mode_table() {
    // `config`: header → 400; none → the configured secret.
    let fx = Fx::new().await;
    fx.secret("cfg", "sk-cfg", fx.t.tenant, &[fx.t.tenant]);
    fx.secret("req", "sk-req", fx.t.tenant, &[fx.t.tenant]);
    fx.upstream(fx.t.tenant, Some(config_mode(SharingMode::Private, "cfg")))
        .await;
    assert_eq!(
        fx.sdk_outcome(fx.t.tenant, fx.post(&["req"])).await,
        bad_request(NOT_ALLOWED)
    );
    fx.sdk(fx.t.tenant, fx.post(&[])).await.unwrap();
    assert_eq!(
        header_values(&fx.recorded().await[0], "authorization"),
        ["Bearer sk-cfg"]
    );

    // `request_ref` with a default: header → requested; none → default.
    let fx = Fx::new().await;
    fx.secret("dflt", "sk-default", fx.t.tenant, &[fx.t.tenant]);
    fx.secret("req", "sk-req", fx.t.tenant, &[fx.t.tenant]);
    fx.upstream(
        fx.t.tenant,
        Some(request_ref(SharingMode::Private, Some("dflt"), None)),
    )
    .await;
    fx.store.clear_gets();
    fx.sdk(fx.t.tenant, fx.post(&["req"])).await.unwrap();
    fx.sdk(fx.t.tenant, fx.post(&[])).await.unwrap();
    let auths: Vec<_> = fx
        .recorded()
        .await
        .iter()
        .map(|r| header_values(r, "authorization"))
        .collect();
    assert_eq!(auths, [["Bearer sk-req"], ["Bearer sk-default"]]);
    assert_eq!(fx.gets(), ["req", "dflt"]);

    // `request_ref` without a default: header → requested; none → 400.
    let fx = Fx::new().await;
    fx.secret("req", "sk-req", fx.t.tenant, &[fx.t.tenant]);
    fx.upstream(
        fx.t.tenant,
        Some(request_ref(SharingMode::Private, None, None)),
    )
    .await;
    fx.sdk(fx.t.tenant, fx.post(&["req"])).await.unwrap();
    assert_eq!(
        fx.sdk_outcome(fx.t.tenant, fx.post(&[])).await,
        bad_request(REQUIRED)
    );
    assert_eq!(fx.recorded().await.len(), 1);
}

/// Shared setup: Tenant's own `request_ref` upstream without a default and a
/// Tenant-owned secret `k`.
async fn tenant_request_ref() -> Fx {
    let fx = Fx::new().await;
    fx.secret("k", "sk-k", fx.t.tenant, &[fx.t.tenant]);
    fx.upstream(
        fx.t.tenant,
        Some(request_ref(SharingMode::Private, None, None)),
    )
    .await;
    fx
}

/// SREF-REQ-002: a reference with and without the `cred://` prefix
/// resolves to the same secret.
#[tokio::test]
async fn req_002_cred_prefix_optional() {
    let fx = tenant_request_ref().await;
    fx.store.clear_gets();
    fx.sdk(fx.t.tenant, fx.post(&["cred://k"])).await.unwrap();
    fx.sdk(fx.t.tenant, fx.post(&["k"])).await.unwrap();
    assert_eq!(fx.gets(), ["k", "k"]);
    for r in fx.recorded().await {
        assert_eq!(header_values(&r, "authorization"), ["Bearer sk-k"]);
    }
}

/// SREF-REQ-003: an empty, malformed, repeated or comma-separated header
/// returns 400 `INVALID_SECRET_REF`, including two identical values.
#[tokio::test]
async fn req_003_malformed_header_rejected() {
    let fx = tenant_request_ref().await;
    fx.store.clear_gets();
    let cases: &[&[&str]] = &[
        &[""],
        &["   "],
        &["cred://"],
        &["has:colon"],
        &["two words"],
        &["k/path"],
        &["k", "other"],
        &["k", "k"],
        &["k,other"],
        &["cred://k, cred://other"],
    ];
    for refs in cases {
        let o = fx.sdk_outcome(fx.t.tenant, fx.post(refs)).await;
        assert_eq!(o, bad_request(INVALID), "{refs:?}");
    }
    assert!(fx.recorded().await.is_empty());
    assert!(fx.gets().is_empty());
}

/// SREF-REQ-005: a reference in the URI or query is not read, and is
/// forwarded or rejected exactly as without this feature.
#[tokio::test]
async fn req_005_query_reference_not_read() {
    let fx = Fx::new().await;
    fx.secret("dflt", "sk-default", fx.t.tenant, &[fx.t.tenant]);
    fx.secret("k", "sk-k", fx.t.tenant, &[fx.t.tenant]);
    let id = fx
        .upstream(
            fx.t.tenant,
            Some(request_ref(SharingMode::Private, Some("dflt"), None)),
        )
        .await;
    fx.route(
        fx.t.tenant,
        id,
        vec![HttpMethod::Get],
        "/v1/q",
        vec!["secret_ref".into(), SECRET_REF_HEADER.into()],
    )
    .await;
    fx.store.clear_gets();

    // Allowed query parameter: forwarded as is, the default is injected.
    let req = fx.request(Method::GET, "/v1/q", "?secret_ref=cred%3A%2F%2Fk", &[], &[]);
    let _ = fx.sdk(fx.t.tenant, req).await;
    let rec = fx.recorded().await;
    assert_eq!(rec.len(), 1);
    assert!(
        rec[0].uri.ends_with("?secret_ref=cred%3A%2F%2Fk"),
        "{}",
        rec[0].uri
    );
    assert_eq!(
        header_values(&rec[0], "authorization"),
        ["Bearer sk-default"]
    );
    assert_eq!(fx.gets(), ["dflt"]);

    // Allowed, and named like the header: still not read.
    let req = fx.request(Method::GET, "/v1/q", "?x-oagw-secret-ref=k", &[], &[]);
    let _ = fx.sdk(fx.t.tenant, req).await;
    let rec = fx.recorded().await;
    assert_eq!(rec.len(), 2);
    assert!(
        rec[1].uri.ends_with("?x-oagw-secret-ref=k"),
        "{}",
        rec[1].uri
    );
    assert_eq!(
        header_values(&rec[1], "authorization"),
        ["Bearer sk-default"]
    );
    assert_eq!(fx.gets(), ["dflt", "dflt"]);

    // Not allowed: rejected as any unknown query parameter is.
    let req = fx.request(Method::POST, CHAT, "?x-oagw-secret-ref=k", &[], &[]);
    let o = fx.sdk_outcome(fx.t.tenant, req).await;
    assert_eq!(
        (o.status, o.reason.as_str()),
        (400, oagw_sdk::field::QUERY_NOT_ALLOWED)
    );

    // In the path: rejected as any path suffix the route refuses.
    let req = fx.request(Method::POST, "/v1/chat/x-oagw-secret-ref/k", "", &[], &[]);
    let o = fx.sdk_outcome(fx.t.tenant, req).await;
    assert_eq!(
        (o.status, o.reason.as_str()),
        (400, oagw_sdk::field::PATH_SUFFIX_NOT_ALLOWED)
    );
    assert_eq!(fx.gets(), ["dflt", "dflt"]);
    assert_eq!(fx.recorded().await.len(), 2);
}

/// SREF-REQ-006, SREF-SDK-001: the provider never receives
/// `X-OAGW-Secret-Ref`, on success and under every passthrough mode, through
/// `facade().proxy_request` and the REST router alike, and gets the
/// configured header and prefix with the referenced secret (WebSocket and SSE
/// handshakes: see SREF-SDK-004).
#[tokio::test]
async fn req_006_header_never_forwarded() {
    for (mode, allowlist) in [
        (PassthroughMode::None, vec![]),
        (
            PassthroughMode::Allowlist,
            vec![SECRET_REF_HEADER.to_owned(), "x-keep".to_owned()],
        ),
        (PassthroughMode::All, vec![]),
    ] {
        let fx = Fx::new().await;
        fx.secret("k", "sk-k", fx.t.tenant, &[fx.t.tenant]);
        fx.upstream_with(
            fx.t.tenant,
            fx.builder(Some(request_ref(SharingMode::Private, None, None)))
                .headers(HeadersConfig {
                    request: Some(RequestHeaderRules {
                        set: HashMap::new(),
                        add: HashMap::new(),
                        remove: vec![],
                        passthrough: mode,
                        passthrough_allowlist: allowlist,
                    }),
                    response: None,
                }),
        )
        .await;
        let req = fx.request(Method::POST, CHAT, "", &["cred://k"], &[("x-keep", "1")]);
        fx.sdk(fx.t.tenant, req).await.unwrap();
        let req = fx.request(Method::POST, CHAT, "", &["cred://k"], &[("x-keep", "1")]);
        assert_eq!(fx.rest(fx.t.tenant, req).await.0.status, 200);
        for r in fx.recorded().await {
            assert!(header_values(&r, SECRET_REF_HEADER).is_empty(), "{mode:?}");
            assert_eq!(header_values(&r, "authorization"), ["Bearer sk-k"]);
        }
    }
}

// ---------------------------------------------------------------------------
// Resolution
// ---------------------------------------------------------------------------

/// SREF-RES-001: CredStore `get` runs with the invoking caller's security
/// context, never a system or upstream-owner context.
#[tokio::test]
async fn res_001_read_under_caller_context() {
    let fx = Fx::new().await;
    fx.secret("own", "sk-tenant", fx.t.tenant, &[fx.t.tenant]);
    fx.upstream(
        fx.t.partner,
        Some(request_ref(SharingMode::Inherit, None, None)),
    )
    .await;
    fx.store.clear_gets();
    let caller = ctx(fx.t.tenant);
    fx.sdk_as(caller.clone(), fx.post(&["own"])).await.unwrap();
    let gets = fx.store.gets();
    assert_eq!(gets.len(), 1);
    assert_eq!(gets[0].ctx.subject_tenant_id(), caller.subject_tenant_id());
    assert_eq!(gets[0].ctx.subject_id(), caller.subject_id());
    assert_ne!(gets[0].ctx.subject_tenant_id(), fx.t.partner);
}

/// SREF-RES-002: the provider receives the configured header and prefix
/// followed by the secret value from that same response.
#[tokio::test]
async fn res_002_configured_header_and_prefix() {
    let fx = Fx::new().await;
    fx.secret("k", "value-123", fx.t.tenant, &[fx.t.tenant]);
    let mut auth = request_ref(SharingMode::Private, None, None);
    let config = auth.config.as_mut().unwrap();
    config.insert("header".into(), "X-Api-Key".into());
    config.insert("prefix".into(), "Key ".into());
    fx.upstream(fx.t.tenant, Some(auth)).await;
    fx.sdk(fx.t.tenant, fx.post(&["k"])).await.unwrap();
    let rec = fx.recorded().await;
    assert_eq!(header_values(&rec[0], "x-api-key"), ["Key value-123"]);
    assert!(header_values(&rec[0], "authorization").is_empty());
}

/// SREF-RES-003: a rotated value is used on the next request with no OAGW
/// write; no value or record ID is cached between requests.
#[tokio::test]
async fn res_003_rotation_applies_next_request() {
    let fx = tenant_request_ref().await;
    fx.store.clear_gets();
    fx.sdk(fx.t.tenant, fx.post(&["k"])).await.unwrap();
    assert_eq!(fx.store.set_value("k", "sk-rotated"), 1);
    fx.sdk(fx.t.tenant, fx.post(&["k"])).await.unwrap();
    let auths: Vec<_> = fx
        .recorded()
        .await
        .iter()
        .map(|r| header_values(r, "authorization"))
        .collect();
    assert_eq!(auths, [["Bearer sk-k"], ["Bearer sk-rotated"]]);
    // One read per request: nothing is served from a cache.
    assert_eq!(fx.gets(), ["k", "k"]);
}

/// SREF-RES-004, SREF-SDK-002: a reference resolved to the caller's own
/// secret and the same reference resolved to an ancestor's shared secret both
/// reach the provider when the owner passes the owner-chain guard. Each
/// `proxy_request` reads under the `SecurityContext` passed to it, so the two
/// tenants get different keys for the same reference.
#[tokio::test]
async fn res_004_own_and_inherited_secret_both_injected() {
    let fx = Fx::new().await;
    // Tenant has its own `k`; Other resolves Partner's shared `k`.
    fx.secret("k", "sk-tenant-own", fx.t.tenant, &[fx.t.tenant]);
    fx.store.add_secret(
        SpySecret::new("k", "sk-partner-shared")
            .owned_by(fx.t.partner)
            .sharing(SecretSharing::Shared)
            .visible_to([fx.t.other, fx.t.partner]),
    );
    fx.upstream(
        fx.t.partner,
        Some(request_ref(SharingMode::Inherit, None, None)),
    )
    .await;
    fx.sdk(fx.t.tenant, fx.post(&["k"])).await.unwrap();
    fx.sdk(fx.t.other, fx.post(&["k"])).await.unwrap();
    let auths: Vec<_> = fx
        .recorded()
        .await
        .iter()
        .map(|r| header_values(r, "authorization"))
        .collect();
    assert_eq!(
        auths,
        [["Bearer sk-tenant-own"], ["Bearer sk-partner-shared"]]
    );
}

/// SREF-RES-005: a secret that is missing, private to another subject, or
/// owned by the sibling Other returns 403 `SECRET_REF_NOT_ACCESSIBLE` with
/// identical bodies and no forward.
#[tokio::test]
async fn res_005_missing_private_sibling_identical() {
    let fx = Fx::new().await;
    fx.store.add_secret(
        SpySecret::new("private", "sk-private")
            .owned_by(fx.t.tenant)
            .private_to(Uuid::new_v4()),
    );
    fx.secret("sibling", "sk-other", fx.t.other, &[fx.t.other]);
    fx.upstream(
        fx.t.partner,
        Some(request_ref(SharingMode::Inherit, None, None)),
    )
    .await;
    let mut bodies = vec![];
    for r in ["missing", "private", "sibling"] {
        let err = fx.sdk(fx.t.tenant, fx.post(&[r])).await.unwrap_err();
        let o = Outcome::from_problem(err.status_code(), &problem_of(&err));
        assert_eq!(o, forbidden(NOT_ACCESSIBLE), "{r}");
        bodies.push(problem_of(&err));
        let (o, body) = fx.rest(fx.t.tenant, fx.post(&[r])).await;
        assert_eq!(o, forbidden(NOT_ACCESSIBLE), "{r}");
        bodies.push(body);
    }
    assert!(
        bodies[0] == bodies[2] && bodies[2] == bodies[4],
        "{bodies:#?}"
    );
    assert!(
        bodies[1] == bodies[3] && bodies[3] == bodies[5],
        "{bodies:#?}"
    );
    assert!(fx.recorded().await.is_empty());
}

/// SREF-RES-005 (CredStore refusals): a `get` that fails with `NotFound`,
/// `AccessDenied` or `InvalidSecretRef` answers exactly as a missing secret,
/// over the SDK and REST, with no forward.
#[tokio::test]
async fn res_005_credstore_refusals_identical_to_missing() {
    // Per-request fields aside, the bodies must not differ.
    let strip = |mut v: Value| {
        if let Some(o) = v.as_object_mut() {
            o.remove("instance");
            o.remove("trace_id");
        }
        v
    };
    let stores: [fn() -> CredStoreSpy; 4] = [
        CredStoreSpy::empty,
        || CredStoreSpy::failing_with(|| CredStoreError::NotFound),
        || CredStoreSpy::failing_with(|| CredStoreError::AccessDenied),
        || CredStoreSpy::failing_with(|| CredStoreError::invalid_ref("refused")),
    ];
    let mut sdk_bodies = vec![];
    let mut rest_bodies = vec![];
    for (i, store) in stores.into_iter().enumerate() {
        let fx = Fx::build(store(), |b| b).await;
        fx.upstream(
            fx.t.tenant,
            Some(request_ref(SharingMode::Private, None, None)),
        )
        .await;
        let err = fx.sdk(fx.t.tenant, fx.post(&["k"])).await.unwrap_err();
        let o = Outcome::from_problem(err.status_code(), &problem_of(&err));
        assert_eq!(o, forbidden(NOT_ACCESSIBLE), "store {i}");
        sdk_bodies.push(strip(problem_of(&err)));
        let (o, body) = fx.rest(fx.t.tenant, fx.post(&["k"])).await;
        assert_eq!(o, forbidden(NOT_ACCESSIBLE), "store {i}");
        rest_bodies.push(strip(body));
        assert!(fx.recorded().await.is_empty(), "store {i}");
    }
    for bodies in [&sdk_bodies, &rest_bodies] {
        assert!(bodies.windows(2).all(|w| w[0] == w[1]), "{bodies:#?}");
    }
}

// ---------------------------------------------------------------------------
// Guards
// ---------------------------------------------------------------------------

/// Partner-owned `request_ref` upstream and secrets owned by Root, Partner
/// and Tenant, all visible to Tenant.
async fn partner_upstream_with_owners() -> Fx {
    let fx = Fx::new().await;
    let t = fx.t;
    fx.secret("root", "sk-root", t.root, &[t.tenant]);
    fx.secret("partner", "sk-partner", t.partner, &[t.tenant]);
    fx.secret("tenant", "sk-tenant", t.tenant, &[t.tenant]);
    fx
}

/// SREF-GUARD-001: with the upstream owned by Partner, a secret owned by
/// Partner or Tenant is injected and a secret owned by Root is rejected with
/// 403 `SECRET_REF_POLICY_DENIED`.
#[tokio::test]
async fn guard_001_owner_chain_from_partner() {
    let fx = partner_upstream_with_owners().await;
    fx.upstream(
        fx.t.partner,
        Some(request_ref(SharingMode::Inherit, None, None)),
    )
    .await;
    fx.sdk(fx.t.tenant, fx.post(&["partner"])).await.unwrap();
    fx.sdk(fx.t.tenant, fx.post(&["tenant"])).await.unwrap();
    assert_eq!(
        fx.sdk_outcome(fx.t.tenant, fx.post(&["root"])).await,
        forbidden(POLICY_DENIED)
    );
    let auths: Vec<_> = fx
        .recorded()
        .await
        .iter()
        .map(|r| header_values(r, "authorization"))
        .collect();
    assert_eq!(auths, [["Bearer sk-partner"], ["Bearer sk-tenant"]]);
}

/// SREF-GUARD-002: with an upstream owned by Tenant on Tenant's own host, a
/// shared reference owned by Partner or Root is rejected, and Tenant's own
/// secret is injected.
#[tokio::test]
async fn guard_002_tenant_upstream_only_own_keys() {
    let fx = partner_upstream_with_owners().await;
    fx.upstream(
        fx.t.tenant,
        Some(request_ref(SharingMode::Private, None, None)),
    )
    .await;
    for r in ["partner", "root"] {
        assert_eq!(
            fx.sdk_outcome(fx.t.tenant, fx.post(&[r])).await,
            forbidden(POLICY_DENIED),
            "{r}"
        );
    }
    fx.sdk(fx.t.tenant, fx.post(&["tenant"])).await.unwrap();
    let rec = fx.recorded().await;
    assert_eq!(rec.len(), 1);
    assert_eq!(
        header_values(&rec[0], "authorization"),
        ["Bearer sk-tenant"]
    );
}

/// SREF-GUARD-003: the owner-chain guard compares against the auth owner from
/// the merge, not the upstream that won alias resolution: Tenant's winning
/// upstream has no auth and inherits Partner's.
#[tokio::test]
async fn guard_003_auth_owner_not_winning_upstream() {
    let fx = partner_upstream_with_owners().await;
    fx.upstream(
        fx.t.partner,
        Some(request_ref(SharingMode::Inherit, None, None)),
    )
    .await;
    // Tenant's own upstream for the alias, with no auth of its own.
    fx.upstream(fx.t.tenant, None).await;
    fx.sdk(fx.t.tenant, fx.post(&["partner"])).await.unwrap();
    assert_eq!(
        fx.sdk_outcome(fx.t.tenant, fx.post(&["root"])).await,
        forbidden(POLICY_DENIED)
    );
    assert_eq!(fx.recorded().await.len(), 1);
}

/// SREF-GUARD-004: with `request_secret_types` set, a secret of a listed
/// type is injected and one of another type is rejected with
/// `SECRET_REF_POLICY_DENIED`.
#[tokio::test]
async fn guard_004_type_allowlist() {
    let fx = Fx::new().await;
    for (r, ty) in [("a", type_a()), ("b", type_b())] {
        fx.store.add_secret(
            SpySecret::new(r, format!("sk-{r}"))
                .owned_by(fx.t.tenant)
                .secret_type(ty),
        );
    }
    fx.upstream(
        fx.t.tenant,
        Some(request_ref(SharingMode::Private, None, Some(type_a()))),
    )
    .await;
    fx.sdk(fx.t.tenant, fx.post(&["a"])).await.unwrap();
    assert_eq!(
        fx.sdk_outcome(fx.t.tenant, fx.post(&["b"])).await,
        forbidden(POLICY_DENIED)
    );
    assert_eq!(fx.recorded().await.len(), 1);
}

/// SREF-GUARD-005: a requested reference that fails any check never falls
/// back to the default secret; the CredStore spy shows no read of the
/// default.
#[tokio::test]
async fn guard_005_no_fallback_to_default() {
    let fx = partner_upstream_with_owners().await;
    fx.store.add_secret(
        SpySecret::new("wrong-type", "sk-wt")
            .owned_by(fx.t.tenant)
            .secret_type(type_b()),
    );
    fx.store.add_secret(
        SpySecret::new("dflt", "sk-default")
            .owned_by(fx.t.partner)
            .secret_type(type_a()),
    );
    fx.upstream(
        fx.t.partner,
        Some(request_ref(
            SharingMode::Inherit,
            Some("dflt"),
            Some(type_a()),
        )),
    )
    .await;
    for (r, expected) in [
        ("missing", forbidden(NOT_ACCESSIBLE)),
        ("root", forbidden(POLICY_DENIED)),
        ("wrong-type", forbidden(POLICY_DENIED)),
        ("bad:ref", bad_request(INVALID)),
    ] {
        fx.store.clear_gets();
        assert_eq!(fx.sdk_outcome(fx.t.tenant, fx.post(&[r])).await, expected);
        assert!(
            !fx.gets().contains(&"dflt".to_owned()),
            "{r}: {:?}",
            fx.gets()
        );
    }
    assert!(fx.recorded().await.is_empty());
}

/// SREF-GUARD-006: the owner-chain and type guards also apply to the default
/// secret in `request_ref` mode.
#[tokio::test]
async fn guard_006_guards_apply_to_default() {
    let fx = Fx::new().await;
    fx.store.add_secret(
        SpySecret::new("root-default", "sk-root")
            .owned_by(fx.t.root)
            .secret_type(type_a()),
    );
    fx.store.add_secret(
        SpySecret::new("typed-default", "sk-b")
            .owned_by(fx.t.partner)
            .secret_type(type_b()),
    );
    // Owner above the auth owner (Partner).
    fx.upstream(
        fx.t.partner,
        Some(request_ref(
            SharingMode::Inherit,
            Some("root-default"),
            None,
        )),
    )
    .await;
    assert_eq!(
        fx.sdk_outcome(fx.t.tenant, fx.post(&[])).await,
        forbidden(POLICY_DENIED)
    );

    // A default of a type outside the allowlist.
    let fx2 = Fx::build_with_tree(fx.t, CredStoreSpy::empty(), |b| b).await;
    fx2.store.add_secret(
        SpySecret::new("typed-default", "sk-b")
            .owned_by(fx.t.partner)
            .secret_type(type_b()),
    );
    fx2.upstream(
        fx.t.partner,
        Some(request_ref(
            SharingMode::Inherit,
            Some("typed-default"),
            Some(type_a()),
        )),
    )
    .await;
    assert_eq!(
        fx2.sdk_outcome(fx.t.tenant, fx2.post(&[])).await,
        forbidden(POLICY_DENIED)
    );
    assert!(fx.recorded().await.is_empty());
    assert!(fx2.recorded().await.is_empty());
}

/// SREF-GUARD-007: the owner-chain guard needs no tenant resolver call
/// beyond the one alias resolution already makes.
#[tokio::test]
async fn guard_007_no_extra_tenant_resolver_call() {
    let t = Tree::new();
    let resolver = t.resolver();
    let calls = resolver.calls();
    let store = Arc::new(CredStoreSpy::empty());
    let h = AppHarness::builder()
        .with_credstore(store.clone() as Arc<dyn CredStoreClientV1>)
        .with_tenant_resolver(resolver)
        .build()
        .await;
    let fx = Fx {
        h,
        store,
        t,
        guard: {
            let mut g = MockGuard::new();
            g.mock(
                "POST",
                CHAT,
                MockResponse {
                    status: 200,
                    headers: vec![],
                    body: MockBody::Json(serde_json::json!({})),
                },
            );
            g
        },
        alias: format!("sref-{}", Uuid::new_v4().as_simple()),
    };
    fx.secret("partner", "sk-partner", t.partner, &[t.tenant]);
    fx.secret("root", "sk-root", t.root, &[t.tenant]);
    fx.upstream(
        t.partner,
        Some(request_ref(SharingMode::Inherit, None, None)),
    )
    .await;
    // One resolver call per request, of any method: the chain alias
    // resolution builds. The owner-chain guard reuses it.
    for (r, expected) in [
        ("partner", Outcome::ok(200)),
        ("root", forbidden(POLICY_DENIED)),
    ] {
        let before = calls.load(Ordering::SeqCst);
        assert_eq!(
            fx.sdk_outcome(t.tenant, fx.post(&[r])).await,
            expected,
            "{r}"
        );
        assert_eq!(calls.load(Ordering::SeqCst) - before, 1, "{r}");
    }
}

// ---------------------------------------------------------------------------
// Header integrity
// ---------------------------------------------------------------------------

async fn tenant_request_ref_with_rules(
    rules: RequestHeaderRules,
    plugins: Option<PluginsConfig>,
) -> Fx {
    let fx = Fx::new().await;
    fx.secret("k", "sk-k", fx.t.tenant, &[fx.t.tenant]);
    let mut b = fx
        .builder(Some(request_ref(SharingMode::Private, None, None)))
        .headers(HeadersConfig {
            request: Some(rules),
            response: None,
        });
    if let Some(p) = plugins {
        b = b.plugins(p);
    }
    fx.upstream_with(fx.t.tenant, b).await;
    fx
}

fn rules(
    set: &[(&str, &str)],
    add: &[(&str, &str)],
    remove: &[&str],
    passthrough: PassthroughMode,
) -> RequestHeaderRules {
    let map = |kv: &[(&str, &str)]| {
        kv.iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    };
    RequestHeaderRules {
        set: map(set),
        add: map(add),
        remove: remove.iter().map(|s| (*s).to_owned()).collect(),
        passthrough,
        passthrough_allowlist: vec![],
    }
}

/// SREF-HDR-001: the auth header is stripped before header rules run, so
/// a rule that removes it changes nothing: the key goes in after the rules.
#[tokio::test]
async fn hdr_001_rule_removing_auth_header_is_harmless() {
    let fx = tenant_request_ref_with_rules(
        rules(&[], &[], &["Authorization"], PassthroughMode::None),
        None,
    )
    .await;
    fx.sdk(fx.t.tenant, fx.post(&["k"])).await.unwrap();
    let rec = fx.recorded().await;
    assert_eq!(header_values(&rec[0], "authorization"), ["Bearer sk-k"]);
}

/// SREF-HDR-002: a header rule that sets the auth header, which was
/// stripped before the rules ran, is a gateway misconfiguration: 500
/// internal, not a caller-side denial, with no forward.
#[tokio::test]
async fn hdr_002_rule_replacing_auth_header_denied() {
    let fx = tenant_request_ref_with_rules(
        rules(
            &[("authorization", "Bearer other")],
            &[],
            &[],
            PassthroughMode::None,
        ),
        None,
    )
    .await;
    assert_eq!(
        fx.sdk_outcome(fx.t.tenant, fx.post(&["k"])).await,
        auth_header_conflict()
    );
    assert!(fx.recorded().await.is_empty());
}

/// SREF-HDR-003: a rule that adds an auth header value is rejected the same
/// way.
#[tokio::test]
async fn hdr_003_rule_appending_auth_header_denied() {
    let fx = tenant_request_ref_with_rules(
        rules(
            &[],
            &[("authorization", "Bearer sk-k")],
            &[],
            PassthroughMode::None,
        ),
        None,
    )
    .await;
    assert_eq!(
        fx.sdk_outcome(fx.t.tenant, fx.post(&["k"])).await,
        auth_header_conflict()
    );
    assert!(fx.recorded().await.is_empty());
}

/// SREF-HDR-004: a client-supplied auth header is replaced by the injected
/// value, never sent alongside it.
#[tokio::test]
async fn hdr_004_client_auth_header_replaced() {
    let fx = tenant_request_ref_with_rules(rules(&[], &[], &[], PassthroughMode::All), None).await;
    let req = fx.request(
        Method::POST,
        CHAT,
        "",
        &["k"],
        &[
            ("authorization", "Bearer client"),
            ("authorization", "Basic x"),
        ],
    );
    fx.sdk(fx.t.tenant, req).await.unwrap();
    let rec = fx.recorded().await;
    assert_eq!(header_values(&rec[0], "authorization"), ["Bearer sk-k"]);
}

/// SREF-HDR-005: header rules and transforms that leave the auth header
/// alone still apply, and the request is forwarded.
#[tokio::test]
async fn hdr_005_unrelated_rules_and_transforms_apply() {
    let fx = tenant_request_ref_with_rules(
        rules(
            &[("x-set", "1")],
            &[("x-add", "2")],
            &["x-drop"],
            PassthroughMode::All,
        ),
        Some(PluginsConfig {
            sharing: SharingMode::Private,
            items: vec![PluginBinding {
                plugin_ref: REQUEST_ID_TRANSFORM_PLUGIN_ID.into(),
                config: HashMap::new(),
            }],
        }),
    )
    .await;
    let req = fx.request(Method::POST, CHAT, "", &["k"], &[("x-drop", "gone")]);
    fx.sdk(fx.t.tenant, req).await.unwrap();
    let rec = fx.recorded().await;
    assert_eq!(header_values(&rec[0], "authorization"), ["Bearer sk-k"]);
    assert_eq!(header_values(&rec[0], "x-set"), ["1"]);
    assert_eq!(header_values(&rec[0], "x-add"), ["2"]);
    assert!(header_values(&rec[0], "x-drop").is_empty());
    assert_eq!(header_values(&rec[0], "x-request-id").len(), 1);
}

/// What [`AuthTransform`] does to the `authorization` header.
#[derive(Clone, Copy, Debug)]
enum AuthEdit {
    Remove,
    Replace,
    Append,
    /// Copy every `authorization` value it sees into the `key` query
    /// parameter, as a transform leaking the key would.
    CopyToQuery,
}

/// A transform plugin that changes the `authorization` header.
struct AuthTransform(AuthEdit);

#[async_trait::async_trait]
impl TransformPlugin for AuthTransform {
    async fn on_request(&self, ctx: &mut TransformRequestContext) -> Result<(), PluginError> {
        let is_auth = |k: &str| k.eq_ignore_ascii_case("authorization");
        match self.0 {
            AuthEdit::Remove => ctx.headers.retain(|(k, _)| !is_auth(k)),
            AuthEdit::Replace => {
                ctx.headers.retain(|(k, _)| !is_auth(k));
                ctx.headers
                    .push(("authorization".to_owned(), "Bearer transform".to_owned()));
            }
            AuthEdit::Append => ctx
                .headers
                .push(("authorization".to_owned(), "Bearer transform".to_owned())),
            AuthEdit::CopyToQuery => {
                let seen: Vec<String> = ctx
                    .headers
                    .iter()
                    .filter(|(k, _)| is_auth(k))
                    .map(|(_, v)| v.clone())
                    .collect();
                ctx.query
                    .extend(seen.into_iter().map(|v| ("key".to_owned(), v)));
            }
        }
        Ok(())
    }
}

/// A `request_ref` upstream for the tenant with [`AuthTransform`] bound.
/// Writes accept only implemented plugin ids, so the test transform takes
/// the built-in request-id transform's place in this harness.
async fn tenant_request_ref_with_transform(edit: AuthEdit) -> Fx {
    let plugin_ref = REQUEST_ID_TRANSFORM_PLUGIN_ID;
    let fx = Fx::build(CredStoreSpy::empty(), move |b| {
        b.with_transform_plugin(plugin_ref, Arc::new(AuthTransform(edit)))
    })
    .await;
    fx.secret("k", "sk-k", fx.t.tenant, &[fx.t.tenant]);
    fx.upstream_with(
        fx.t.tenant,
        fx.builder(Some(request_ref(SharingMode::Private, None, None)))
            .plugins(PluginsConfig {
                sharing: SharingMode::Private,
                items: vec![PluginBinding {
                    plugin_ref: plugin_ref.into(),
                    config: HashMap::new(),
                }],
            }),
    )
    .await;
    fx
}

/// SREF-HDR-002, SREF-HDR-003 (transforms): through the pipeline, a
/// transform that sets the auth header, by replacing or appending, is
/// refused with a 500 internal error, and nothing is forwarded.
#[tokio::test]
async fn hdr_002_003_transform_setting_auth_header_denied() {
    for edit in [AuthEdit::Replace, AuthEdit::Append] {
        let fx = tenant_request_ref_with_transform(edit).await;
        assert_eq!(
            fx.sdk_outcome(fx.t.tenant, fx.post(&["k"])).await,
            auth_header_conflict(),
            "{edit:?}"
        );
        assert_eq!(fx.gets(), ["k"], "{edit:?}");
        assert!(fx.recorded().await.is_empty(), "{edit:?}");
    }
}

/// SREF-HDR-001 (transforms): a transform that removes the auth header
/// finds nothing to remove; the key goes in after it.
#[tokio::test]
async fn hdr_001_transform_removing_auth_header_is_harmless() {
    let fx = tenant_request_ref_with_transform(AuthEdit::Remove).await;
    fx.sdk(fx.t.tenant, fx.post(&["k"])).await.unwrap();
    let rec = fx.recorded().await;
    assert_eq!(header_values(&rec[0], "authorization"), ["Bearer sk-k"]);
}

/// SREF-HDR-006: transforms run before the key is injected, so a transform
/// that copies the auth header into the query cannot move the key there,
/// and a client-supplied auth header is stripped before transforms see it.
#[tokio::test]
async fn hdr_006_transform_never_sees_the_key() {
    let fx = tenant_request_ref_with_transform(AuthEdit::CopyToQuery).await;
    let req = fx.request(
        Method::POST,
        CHAT,
        "",
        &["k"],
        &[("authorization", "Bearer client")],
    );
    fx.sdk(fx.t.tenant, req).await.unwrap();
    let rec = fx.recorded().await;
    assert!(!rec[0].uri.contains("key="), "{}", rec[0].uri);
    assert_eq!(header_values(&rec[0], "authorization"), ["Bearer sk-k"]);
}

/// SREF-HDR-004 (custom header): with the key in `x-api-key`, which the
/// gateway does not strip, a client value forwarded by `passthrough: all`
/// is replaced by the injected one, never sent alongside it.
#[tokio::test]
async fn hdr_004_client_custom_auth_header_replaced() {
    let fx = Fx::new().await;
    fx.secret("k", "sk-k", fx.t.tenant, &[fx.t.tenant]);
    fx.upstream_with(
        fx.t.tenant,
        fx.builder(Some(apikey(
            SharingMode::Private,
            &[
                ("credential_mode", "request_ref"),
                ("header", "x-api-key"),
                ("prefix", ""),
            ],
        )))
        .headers(HeadersConfig {
            request: Some(rules(&[], &[], &[], PassthroughMode::All)),
            response: None,
        }),
    )
    .await;
    let req = fx.request(
        Method::POST,
        CHAT,
        "",
        &["k"],
        &[("x-api-key", "client-1"), ("x-api-key", "client-2")],
    );
    fx.sdk(fx.t.tenant, req).await.unwrap();
    let rec = fx.recorded().await;
    assert_eq!(header_values(&rec[0], "x-api-key"), ["sk-k"]);
}

// ---------------------------------------------------------------------------
// Ordering and errors
// ---------------------------------------------------------------------------

/// SREF-ERR-001: a caller without invoke permission gets the existing 403
/// before the header is parsed; a bad header on an unknown alias gets the
/// existing not-found.
#[tokio::test]
async fn err_001_invoke_and_routing_first() {
    let fx = Fx::build(CredStoreSpy::empty(), |b| {
        b.with_authz_client(Arc::new(DenyingAuthZResolverClient))
    })
    .await;
    let o = fx
        .sdk_outcome(fx.t.tenant, fx.post(&["bad:ref", "x"]))
        .await;
    assert_eq!(o.status, 403);
    assert_ne!(o.reason, INVALID);
    assert_ne!(o.reason, POLICY_DENIED);
    assert!(fx.gets().is_empty());

    let fx = Fx::new().await;
    let o = fx
        .sdk_outcome(fx.t.tenant, fx.post(&["bad:ref", "x"]))
        .await;
    assert_eq!(o.status, 404);
    assert!(fx.gets().is_empty());
}

/// A Tenant `request_ref` upstream limited to one request per minute.
async fn limited(store: CredStoreSpy, default: Option<&str>) -> Fx {
    let fx = Fx::build(store, |b| {
        b.with_request_timeout(Duration::from_millis(500))
    })
    .await;
    fx.upstream_with(
        fx.t.tenant,
        fx.builder(Some(request_ref(SharingMode::Private, default, None)))
            .rate_limit(one_per_minute()),
    )
    .await;
    fx
}

/// SREF-ERR-002: syntax and mode errors return before rate admission, debit
/// nothing, and make no CredStore read.
#[tokio::test]
async fn err_002_syntax_and_mode_errors_debit_nothing() {
    let fx = limited(CredStoreSpy::empty(), None).await;
    fx.secret("k", "sk-k", fx.t.tenant, &[fx.t.tenant]);
    fx.store.clear_gets();
    assert_eq!(
        fx.sdk_outcome(fx.t.tenant, fx.post(&["bad:ref"])).await,
        bad_request(INVALID)
    );
    assert_eq!(
        fx.sdk_outcome(fx.t.tenant, fx.post(&[])).await,
        bad_request(REQUIRED)
    );
    assert!(fx.gets().is_empty());
    // The single token is still there.
    fx.sdk(fx.t.tenant, fx.post(&["k"])).await.unwrap();

    // A `config` upstream: the mode error debits nothing either.
    let fx = Fx::new().await;
    fx.secret("cfg", "sk-cfg", fx.t.tenant, &[fx.t.tenant]);
    fx.upstream_with(
        fx.t.tenant,
        fx.builder(Some(config_mode(SharingMode::Private, "cfg")))
            .rate_limit(one_per_minute()),
    )
    .await;
    fx.store.clear_gets();
    assert_eq!(
        fx.sdk_outcome(fx.t.tenant, fx.post(&["k"])).await,
        bad_request(NOT_ALLOWED)
    );
    assert!(fx.gets().is_empty());
    fx.sdk(fx.t.tenant, fx.post(&[])).await.unwrap();
}

/// SREF-ERR-003: an exhausted upstream or route limit returns 429 with no
/// CredStore read.
#[tokio::test]
async fn err_003_exhausted_limit_no_read() {
    let fx = limited(CredStoreSpy::empty(), None).await;
    fx.secret("k", "sk-k", fx.t.tenant, &[fx.t.tenant]);
    fx.store.clear_gets();
    fx.sdk(fx.t.tenant, fx.post(&["k"])).await.unwrap();
    let err = fx.sdk(fx.t.tenant, fx.post(&["k"])).await.unwrap_err();
    assert_eq!(err.status_code(), 429);
    assert!(matches!(
        ServiceGatewayError::from(err),
        ServiceGatewayError::RateLimited { .. }
    ));
    assert_eq!(fx.gets(), ["k"]);
    assert_eq!(fx.recorded().await.len(), 1);

    // A route limit, on an upstream without one.
    let fx = Fx::new().await;
    fx.secret("k", "sk-k", fx.t.tenant, &[fx.t.tenant]);
    let up =
        fx.h.facade()
            .create_upstream(
                ctx(fx.t.tenant),
                fx.builder(Some(request_ref(SharingMode::Private, None, None)))
                    .build(),
            )
            .await
            .expect("create upstream");
    fx.h.facade()
        .create_route(
            ctx(fx.t.tenant),
            CreateRouteRequest::builder(
                up.id,
                MatchRules {
                    http: Some(HttpMatch {
                        methods: vec![HttpMethod::Post],
                        path: fx.guard.path(CHAT),
                        query_allowlist: vec![],
                        path_suffix_mode: PathSuffixMode::Disabled,
                    }),
                    grpc: None,
                },
            )
            .rate_limit(one_per_minute())
            .build(),
        )
        .await
        .expect("create route");
    fx.store.clear_gets();
    fx.sdk(fx.t.tenant, fx.post(&["k"])).await.unwrap();
    let err = fx.sdk(fx.t.tenant, fx.post(&["k"])).await.unwrap_err();
    assert_eq!(err.status_code(), 429);
    assert_eq!(fx.gets(), ["k"]);
    assert_eq!(fx.recorded().await.len(), 1);
}

/// SREF-ERR-004: a CredStore outage or timeout returns 503 service
/// unavailable with reason `CREDENTIAL_STORE_UNAVAILABLE`, and keeps the
/// limit debits already taken.
#[tokio::test]
async fn err_004_outage_and_timeout_are_503_and_keep_debit() {
    for store in [
        CredStoreSpy::always_failing(),
        CredStoreSpy::never_answering(),
    ] {
        let fx = limited(store, None).await;
        let err = fx.sdk(fx.t.tenant, fx.post(&["k"])).await.unwrap_err();
        assert_eq!(err.status_code(), 503);
        let p = problem_of(&err);
        assert!(p["type"].as_str().unwrap().contains("service_unavailable"));
        assert_eq!(
            p["context"]["reason"],
            oagw_sdk::reason::unavailable::CREDENTIAL_STORE_UNAVAILABLE,
            "{p}"
        );
        // The debit stays: the next request is over the limit, and is
        // refused before any CredStore read.
        let err = fx.sdk(fx.t.tenant, fx.post(&["k"])).await.unwrap_err();
        assert_eq!(err.status_code(), 429);
        assert_eq!(fx.gets(), ["k"]);
        assert!(fx.recorded().await.is_empty());
    }
}

/// SREF-ERR-008: guard plugins run before the CredStore read, so a request
/// a guard rejects makes no read. It was admitted first and keeps its debit.
#[tokio::test]
async fn err_008_guard_rejection_makes_no_read_and_keeps_debit() {
    let fx = Fx::new().await;
    fx.secret("k", "sk-k", fx.t.tenant, &[fx.t.tenant]);
    fx.upstream_with(
        fx.t.tenant,
        fx.builder(Some(request_ref(SharingMode::Private, None, None)))
            .rate_limit(one_per_minute())
            .plugins(PluginsConfig {
                sharing: SharingMode::Private,
                items: vec![PluginBinding {
                    plugin_ref: REQUIRED_HEADERS_GUARD_PLUGIN_ID.into(),
                    config: HashMap::from([(
                        "required_request_headers".to_owned(),
                        "x-needed".to_owned(),
                    )]),
                }],
            }),
    )
    .await;
    fx.store.clear_gets();
    let err = fx.sdk(fx.t.tenant, fx.post(&["k"])).await.unwrap_err();
    assert_eq!(err.status_code(), 400);
    assert!(fx.gets().is_empty());
    // The debit stays: a request the guard accepts is now over the limit.
    let req = fx.request(Method::POST, CHAT, "", &["k"], &[("x-needed", "1")]);
    let err = fx.sdk(fx.t.tenant, req).await.unwrap_err();
    assert_eq!(err.status_code(), 429);
    assert!(fx.gets().is_empty());
    assert!(fx.recorded().await.is_empty());
}

/// SREF-ERR-004 (retry hint and write errors): CredStore's own retry hint
/// reaches the 503's `retry_after_seconds`, rounded up to whole seconds;
/// without one the gateway's transient default (5 s) applies. An error only a
/// write can cause is a 500 with no retry hint, since retrying cannot help.
/// None of them reaches the provider.
#[tokio::test]
async fn err_004_credstore_retry_hint_and_write_errors() {
    /// Case name, the error CredStore returns, the expected status and
    /// `retry_after_seconds`.
    type Case = (&'static str, fn() -> CredStoreError, u16, Option<u64>);
    let cases: [Case; 5] = [
        (
            "hint",
            || CredStoreError::ServiceUnavailable {
                detail: "down".into(),
                retry_after: Some(Duration::from_millis(6500)),
            },
            503,
            Some(7),
        ),
        (
            "no hint",
            || CredStoreError::service_unavailable("down"),
            503,
            Some(5),
        ),
        (
            "type violation",
            || CredStoreError::TypeViolation {
                reason: "r".into(),
                detail: "d".into(),
            },
            500,
            None,
        ),
        ("conflict", || CredStoreError::Conflict, 500, None),
        (
            "unsupported transition",
            || CredStoreError::UnsupportedTransition { detail: "d".into() },
            500,
            None,
        ),
    ];
    for (case, error, status, retry_after) in cases {
        let fx = Fx::build(CredStoreSpy::failing_with(error), |b| b).await;
        fx.upstream(
            fx.t.tenant,
            Some(request_ref(SharingMode::Private, None, None)),
        )
        .await;
        let (o, problem) = fx.rest(fx.t.tenant, fx.post(&["k"])).await;
        assert_eq!(o.status, status, "{case}: {problem}");
        assert_eq!(
            problem["context"]["retry_after_seconds"].as_u64(),
            retry_after,
            "{case}: {problem}"
        );
        assert!(fx.recorded().await.is_empty(), "{case}");
    }
}

/// SREF-ERR-004 (deadline): in `request_ref` mode the CredStore read shares
/// the gateway request timeout with endpoint selection and the exchange. A
/// read that takes most of the timeout leaves only the rest for a provider
/// that does not answer.
///
/// One shared budget ends near `TIMEOUT`; a separate budget after the read
/// would end near `READ + TIMEOUT`. The bound sits halfway between the two,
/// so each side has `READ / 2` of slack for scheduling on a loaded runner.
#[tokio::test]
async fn err_004_read_shares_the_request_deadline() {
    const READ: Duration = Duration::from_millis(1000);
    const TIMEOUT: Duration = Duration::from_millis(1200);
    let mut fx = Fx::build(CredStoreSpy::empty().answering_after(READ), |b| {
        b.with_request_timeout(TIMEOUT)
    })
    .await;
    fx.secret("k", "sk-k", fx.t.tenant, &[fx.t.tenant]);
    // The provider holds its answer until the test ends.
    let _hold = fx.guard.mock_gated(
        "POST",
        CHAT,
        MockResponse {
            status: 200,
            headers: vec![],
            body: MockBody::Json(serde_json::json!({})),
        },
    );
    fx.upstream(
        fx.t.tenant,
        Some(request_ref(SharingMode::Private, None, None)),
    )
    .await;
    let start = std::time::Instant::now();
    let err = fx.sdk(fx.t.tenant, fx.post(&["k"])).await.unwrap_err();
    let elapsed = start.elapsed();
    assert_eq!(err.status_code(), 504);
    assert!(elapsed >= READ, "the read ran first: {elapsed:?}");
    // One TIMEOUT budget, not READ plus TIMEOUT after it.
    let bound = TIMEOUT + READ / 2;
    assert!(elapsed < bound, "{elapsed:?} >= {bound:?}");
}

/// A circuit breaker that opens on the first failure and stays open a
/// minute.
fn open_on_first_failure() -> CircuitBreakerConfig {
    CircuitBreakerConfig {
        failure_threshold: 1,
        timeout_seconds: 60,
        ..CircuitBreakerConfig::default()
    }
}

/// A provider that holds its answer to `POST /v1/chat` until the sender is
/// dropped, for the next request.
fn hold_chat(fx: &mut Fx) -> tokio::sync::oneshot::Sender<()> {
    fx.guard.mock_gated(
        "POST",
        CHAT,
        MockResponse {
            status: 200,
            headers: vec![],
            body: MockBody::Json(serde_json::json!({})),
        },
    )
}

/// A deadline hit after the CredStore read used up most of the shared
/// request timeout is the read's doing, not the upstream's: the circuit
/// stays closed.
#[tokio::test]
async fn circuit_breaker_ignores_a_deadline_the_credstore_read_used_up() {
    const READ: Duration = Duration::from_millis(600);
    const TIMEOUT: Duration = Duration::from_millis(1000);
    let mut fx = Fx::build(CredStoreSpy::empty().answering_after(READ), |b| {
        b.with_request_timeout(TIMEOUT)
    })
    .await;
    fx.secret("k", "sk-k", fx.t.tenant, &[fx.t.tenant]);
    fx.upstream_with(
        fx.t.tenant,
        fx.builder(Some(request_ref(SharingMode::Private, None, None)))
            .circuit_breaker(open_on_first_failure()),
    )
    .await;
    for i in 0..2 {
        let _hold = hold_chat(&mut fx);
        let err = fx.sdk(fx.t.tenant, fx.post(&["k"])).await.unwrap_err();
        assert_eq!(err.status_code(), 504, "request {i}");
    }
    assert_eq!(fx.recorded().await.len(), 2, "both reached the provider");
}

/// A deadline hit with the whole request timeout left for the exchange is
/// the upstream's: the next request fails fast.
#[tokio::test]
async fn circuit_breaker_counts_a_deadline_with_the_full_budget() {
    let mut fx = Fx::build(CredStoreSpy::empty(), |b| {
        b.with_request_timeout(Duration::from_millis(300))
    })
    .await;
    fx.upstream_with(
        fx.t.tenant,
        fx.builder(None).circuit_breaker(open_on_first_failure()),
    )
    .await;
    let _hold = hold_chat(&mut fx);
    let err = fx.sdk(fx.t.tenant, fx.post(&[])).await.unwrap_err();
    assert_eq!(err.status_code(), 504);
    let o = fx.sdk_outcome(fx.t.tenant, fx.post(&[])).await;
    assert_eq!(o.status, 503);
    assert_eq!(fx.recorded().await.len(), 1, "the second failed fast");
}

/// An open circuit rejects a `request_ref` request before its CredStore
/// read.
#[tokio::test]
async fn open_circuit_reads_no_secret() {
    let mut fx = Fx::new().await;
    fx.secret("k", "sk-k", fx.t.tenant, &[fx.t.tenant]);
    fx.guard.mock(
        "POST",
        CHAT,
        MockResponse {
            status: 503,
            headers: vec![],
            body: MockBody::Json(serde_json::json!({})),
        },
    );
    fx.upstream_with(
        fx.t.tenant,
        fx.builder(Some(request_ref(SharingMode::Private, None, None)))
            .circuit_breaker(open_on_first_failure()),
    )
    .await;
    assert_eq!(
        fx.sdk_outcome(fx.t.tenant, fx.post(&["k"])).await.status,
        503
    );
    assert_eq!(fx.gets(), ["k"]);
    for _ in 0..3 {
        let err = fx.sdk(fx.t.tenant, fx.post(&["k"])).await.unwrap_err();
        assert!(
            matches!(err, CanonicalError::ServiceUnavailable { ref ctx, .. }
                if ctx.retry_after_seconds == Some(60)),
            "{err:?}"
        );
    }
    assert_eq!(fx.gets(), ["k"], "no read while open");
}

/// Failures the error table does not list keep the API-key plugin's mapping
/// in `request_ref` mode (ADR-0022, "Errors and observability"): a secret
/// value that is not UTF-8 or not a valid header value returns 401
/// `AUTH_PLUGIN_INTERNAL` with no forward. (Writes refuse a config without a
/// usable `header`: SREF-CFG-007.)
#[tokio::test]
async fn request_ref_unusable_secret_is_auth_plugin_internal() {
    let cases: [(&str, Vec<u8>, AuthConfig); 2] = [
        (
            "not utf-8",
            vec![0xff, 0xfe],
            request_ref(SharingMode::Private, None, None),
        ),
        (
            "not a header value",
            b"sk\nbad".to_vec(),
            request_ref(SharingMode::Private, None, None),
        ),
    ];
    for (case, value, auth) in cases {
        let fx = Fx::new().await;
        fx.store.add_secret(
            SpySecret::new("k", value)
                .owned_by(fx.t.tenant)
                .visible_to([fx.t.tenant]),
        );
        fx.upstream(fx.t.tenant, Some(auth)).await;
        let o = fx.sdk_outcome(fx.t.tenant, fx.post(&["k"])).await;
        assert_eq!(
            (o.status, o.reason.as_str()),
            (401, oagw_sdk::reason::auth::PLUGIN_INTERNAL),
            "{case}"
        );
        assert!(fx.recorded().await.is_empty(), "{case}");
    }
}

/// SREF-ERR-006: a CORS preflight neither reads CredStore nor consumes
/// capacity.
#[tokio::test]
async fn err_006_preflight_free() {
    let fx = Fx::new().await;
    fx.secret("k", "sk-k", fx.t.tenant, &[fx.t.tenant]);
    fx.upstream_with(
        fx.t.tenant,
        fx.builder(Some(request_ref(SharingMode::Private, None, None)))
            .rate_limit(one_per_minute())
            .cors(CorsConfig {
                sharing: SharingMode::Private,
                enabled: true,
                allowed_origins: vec!["https://app.example.com".into()],
                allowed_methods: vec![CorsHttpMethod::Post],
                expose_headers: vec![],
                allow_credentials: false,
            }),
    )
    .await;
    fx.store.clear_gets();
    let req = fx.request(
        Method::OPTIONS,
        CHAT,
        "",
        &["k"],
        &[
            ("origin", "https://app.example.com"),
            ("access-control-request-method", "POST"),
            ("access-control-request-headers", SECRET_REF_HEADER),
        ],
    );
    let (o, _) = fx.rest(fx.t.tenant, req).await;
    assert!(o.status < 300, "{o:?}");
    assert!(fx.gets().is_empty());
    // The single token is still there.
    let req = fx.request(
        Method::POST,
        CHAT,
        "",
        &["k"],
        &[("origin", "https://app.example.com")],
    );
    assert_eq!(fx.rest(fx.t.tenant, req).await.0.status, 200);
}

// ---------------------------------------------------------------------------
// SDK usage (and the error table: SREF-ERR-005, SREF-ERR-007)
// ---------------------------------------------------------------------------

/// One row of the parity table: a fresh fixture, a request, and the expected
/// outcome and upstream call count.
struct Row {
    name: &'static str,
    store: fn() -> CredStoreSpy,
    auth: fn(&Tree) -> Option<AuthConfig>,
    kind: Kind,
    refs: &'static [&'static str],
    expected: Outcome,
    calls: usize,
}

/// How a parity row sets up Tenant's upstream.
#[derive(Clone, Copy)]
enum Kind {
    /// With the row's auth only.
    Plain,
    /// Plus a header rule that replaces the auth header.
    AuthHeaderRule,
    /// Plus a one-per-minute limit, already used up.
    Exhausted,
}

fn store_with_secrets() -> CredStoreSpy {
    CredStoreSpy::empty()
}

fn rate_limited() -> Outcome {
    Outcome {
        status: 429,
        category: "gts://gts.cf.core.errors.err.v1~cf.core.err.resource_exhausted.v1~".into(),
        reason: String::new(),
        field: String::new(),
    }
}

fn unavailable() -> Outcome {
    Outcome {
        status: 503,
        category: "gts://gts.cf.core.errors.err.v1~cf.core.err.service_unavailable.v1~".into(),
        reason: oagw_sdk::reason::unavailable::CREDENTIAL_STORE_UNAVAILABLE.into(),
        field: String::new(),
    }
}

/// SREF-SDK-003: every row of the mode table and every error in the error
/// table, through both `facade().proxy_request` and the REST router, gives
/// the same status, canonical category, reason and upstream call count.
///
/// SREF-ERR-005: every error in the table has its stated status, canonical
/// category and reason; the 400s name the field `x-oagw-secret-ref`.
///
/// SREF-ERR-007: every failure in the table makes zero provider calls.
#[tokio::test]
async fn sdk_003_err_005_err_007_parity_table() {
    let rows = [
        Row {
            name: "config, header",
            store: store_with_secrets,
            auth: |_| Some(config_mode(SharingMode::Private, "own")),
            kind: Kind::Plain,
            refs: &["own"],
            expected: bad_request(NOT_ALLOWED),
            calls: 0,
        },
        Row {
            name: "config, no header",
            store: store_with_secrets,
            auth: |_| Some(config_mode(SharingMode::Private, "own")),
            kind: Kind::Plain,
            refs: &[],
            expected: Outcome::ok(200),
            calls: 1,
        },
        Row {
            name: "request_ref default, header",
            store: store_with_secrets,
            auth: |_| Some(request_ref(SharingMode::Private, Some("own"), None)),
            kind: Kind::Plain,
            refs: &["own"],
            expected: Outcome::ok(200),
            calls: 1,
        },
        Row {
            name: "request_ref default, no header",
            store: store_with_secrets,
            auth: |_| Some(request_ref(SharingMode::Private, Some("own"), None)),
            kind: Kind::Plain,
            refs: &[],
            expected: Outcome::ok(200),
            calls: 1,
        },
        Row {
            name: "request_ref no default, header",
            store: store_with_secrets,
            auth: |_| Some(request_ref(SharingMode::Private, None, None)),
            kind: Kind::Plain,
            refs: &["own"],
            expected: Outcome::ok(200),
            calls: 1,
        },
        Row {
            name: "request_ref no default, no header",
            store: store_with_secrets,
            auth: |_| Some(request_ref(SharingMode::Private, None, None)),
            kind: Kind::Plain,
            refs: &[],
            expected: bad_request(REQUIRED),
            calls: 0,
        },
        Row {
            name: "malformed header",
            store: store_with_secrets,
            auth: |_| Some(request_ref(SharingMode::Private, None, None)),
            kind: Kind::Plain,
            refs: &["a,b"],
            expected: bad_request(INVALID),
            calls: 0,
        },
        Row {
            name: "repeated header",
            store: store_with_secrets,
            auth: |_| Some(request_ref(SharingMode::Private, None, None)),
            kind: Kind::Plain,
            refs: &["own", "own"],
            expected: bad_request(INVALID),
            calls: 0,
        },
        Row {
            name: "missing secret",
            store: store_with_secrets,
            auth: |_| Some(request_ref(SharingMode::Private, None, None)),
            kind: Kind::Plain,
            refs: &["missing"],
            expected: forbidden(NOT_ACCESSIBLE),
            calls: 0,
        },
        Row {
            name: "owner outside the chain",
            store: store_with_secrets,
            auth: |_| Some(request_ref(SharingMode::Private, None, None)),
            kind: Kind::Plain,
            refs: &["root"],
            expected: forbidden(POLICY_DENIED),
            calls: 0,
        },
        Row {
            name: "type not allowed",
            store: store_with_secrets,
            auth: |_| Some(request_ref(SharingMode::Private, None, Some(type_b()))),
            kind: Kind::Plain,
            refs: &["own"],
            expected: forbidden(POLICY_DENIED),
            calls: 0,
        },
        Row {
            name: "auth header changed",
            store: store_with_secrets,
            auth: |_| Some(request_ref(SharingMode::Private, None, None)),
            kind: Kind::AuthHeaderRule,
            refs: &["own"],
            expected: auth_header_conflict(),
            calls: 0,
        },
        Row {
            name: "credstore outage",
            store: CredStoreSpy::always_failing,
            auth: |_| Some(request_ref(SharingMode::Private, None, None)),
            kind: Kind::Plain,
            refs: &["own"],
            expected: unavailable(),
            calls: 0,
        },
        Row {
            name: "empty header",
            store: store_with_secrets,
            auth: |_| Some(request_ref(SharingMode::Private, None, None)),
            kind: Kind::Plain,
            refs: &[""],
            expected: bad_request(INVALID),
            calls: 0,
        },
        Row {
            name: "other plugin, header",
            store: store_with_secrets,
            auth: |_| {
                Some(AuthConfig {
                    plugin_type: NOOP_AUTH_PLUGIN_ID.into(),
                    sharing: SharingMode::Private,
                    config: None,
                })
            },
            kind: Kind::Plain,
            refs: &["own"],
            expected: bad_request(NOT_ALLOWED),
            calls: 0,
        },
        Row {
            name: "no auth, header",
            store: store_with_secrets,
            auth: |_| None,
            kind: Kind::Plain,
            refs: &["own"],
            expected: bad_request(NOT_ALLOWED),
            calls: 0,
        },
        Row {
            name: "credstore not found",
            store: || CredStoreSpy::failing_with(|| CredStoreError::NotFound),
            auth: |_| Some(request_ref(SharingMode::Private, None, None)),
            kind: Kind::Plain,
            refs: &["own"],
            expected: forbidden(NOT_ACCESSIBLE),
            calls: 0,
        },
        Row {
            name: "credstore access denied",
            store: || CredStoreSpy::failing_with(|| CredStoreError::AccessDenied),
            auth: |_| Some(request_ref(SharingMode::Private, None, None)),
            kind: Kind::Plain,
            refs: &["own"],
            expected: forbidden(NOT_ACCESSIBLE),
            calls: 0,
        },
        Row {
            name: "credstore service unavailable",
            store: || CredStoreSpy::failing_with(|| CredStoreError::service_unavailable("down")),
            auth: |_| Some(request_ref(SharingMode::Private, None, None)),
            kind: Kind::Plain,
            refs: &["own"],
            expected: unavailable(),
            calls: 0,
        },
        Row {
            name: "credstore no plugin",
            store: || CredStoreSpy::failing_with(|| CredStoreError::NoPluginAvailable),
            auth: |_| Some(request_ref(SharingMode::Private, None, None)),
            kind: Kind::Plain,
            refs: &["own"],
            expected: unavailable(),
            calls: 0,
        },
        Row {
            name: "credstore timeout",
            store: CredStoreSpy::never_answering,
            auth: |_| Some(request_ref(SharingMode::Private, None, None)),
            kind: Kind::Plain,
            refs: &["own"],
            expected: unavailable(),
            calls: 0,
        },
        Row {
            name: "limit exhausted",
            store: store_with_secrets,
            auth: |_| Some(request_ref(SharingMode::Private, None, None)),
            kind: Kind::Exhausted,
            refs: &["own"],
            expected: rate_limited(),
            calls: 0,
        },
    ];
    for row in rows {
        let mut via = vec![];
        for rest in [false, true] {
            let fx = Fx::build((row.store)(), |b| {
                b.with_request_timeout(Duration::from_millis(500))
            })
            .await;
            let t = fx.t;
            fx.store.add_secret(
                SpySecret::new("own", "sk-own")
                    .owned_by(t.tenant)
                    .secret_type(type_a()),
            );
            fx.store
                .add_secret(SpySecret::new("root", "sk-root").owned_by(t.root));
            let auth = (row.auth)(&t);
            match row.kind {
                Kind::Plain => {
                    fx.upstream(t.tenant, auth).await;
                }
                Kind::Exhausted => {
                    fx.upstream_with(t.tenant, fx.builder(auth).rate_limit(one_per_minute()))
                        .await;
                    fx.sdk(t.tenant, fx.post(&["own"])).await.unwrap();
                }
                Kind::AuthHeaderRule => {
                    fx.upstream_with(
                        t.tenant,
                        fx.builder(auth).headers(HeadersConfig {
                            request: Some(rules(
                                &[("authorization", "Bearer x")],
                                &[],
                                &[],
                                PassthroughMode::None,
                            )),
                            response: None,
                        }),
                    )
                    .await;
                }
            }
            let before = fx.recorded().await.len();
            let outcome = if rest {
                fx.rest(t.tenant, fx.post(row.refs)).await.0
            } else {
                fx.sdk_outcome(t.tenant, fx.post(row.refs)).await
            };
            let calls = fx.recorded().await.len() - before;
            assert_eq!(outcome, row.expected, "{} (rest={rest})", row.name);
            assert_eq!(calls, row.calls, "{} (rest={rest})", row.name);
            via.push((outcome, calls));
        }
        assert_eq!(via[0], via[1], "{}: SDK and REST differ", row.name);
    }
}

/// SREF-SDK-004: an SSE request and a WebSocket handshake through
/// `proxy_request` with the header inject the key and strip the header, and
/// the stream is delivered (for WebSocket: the upstream switches protocols).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sdk_004_sse_and_websocket() {
    let fx = tenant_request_ref().await;
    // SSE.
    let req = fx.request(Method::GET, SSE, "", &["k"], &[]);
    let resp =
        fx.h.facade()
            .proxy_request(ctx(fx.t.tenant), req)
            .await
            .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().into_bytes().await.unwrap();
    let text = String::from_utf8_lossy(&body);
    assert!(
        text.contains("data: one") && text.contains("data: two"),
        "{text}"
    );
    let rec = fx.recorded().await;
    assert_eq!(header_values(&rec[0], "authorization"), ["Bearer sk-k"]);
    assert!(header_values(&rec[0], SECRET_REF_HEADER).is_empty());

    // WebSocket handshake to the mock's fixed echo handler.
    let up =
        fx.h.facade()
            .create_upstream(
                ctx(fx.t.tenant),
                CreateUpstreamRequest::builder(fx.server(), HTTP_PROTOCOL_ID)
                    .alias(format!("{}-ws", fx.alias))
                    .auth(request_ref(SharingMode::Private, None, None))
                    .headers(HeadersConfig {
                        request: Some(rules(&[], &[], &[], PassthroughMode::All)),
                        response: None,
                    })
                    .build(),
            )
            .await
            .unwrap();
    fx.h.facade()
        .create_route(
            ctx(fx.t.tenant),
            CreateRouteRequest::builder(
                up.id,
                MatchRules {
                    http: Some(HttpMatch {
                        methods: vec![HttpMethod::Get],
                        path: "/ws/echo".into(),
                        query_allowlist: vec![],
                        path_suffix_mode: PathSuffixMode::Disabled,
                    }),
                    grpc: None,
                },
            )
            .build(),
        )
        .await
        .unwrap();
    let marker = Uuid::new_v4().to_string();
    let req = http::Request::get(format!("/{}-ws/ws/echo", fx.alias))
        .header("upgrade", "websocket")
        .header("connection", "Upgrade")
        .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==")
        .header("sec-websocket-version", "13")
        .header(SECRET_REF_HEADER, "cred://k")
        .header("x-sref-marker", &marker)
        .body(Body::Empty)
        .unwrap();
    let resp =
        fx.h.facade()
            .proxy_request(ctx(fx.t.tenant), req)
            .await
            .unwrap();
    assert_eq!(resp.status(), StatusCode::SWITCHING_PROTOCOLS);
    let all = fx.guard.all_recorded_requests().await;
    let ws = all
        .iter()
        .find(|r| header_values(r, "x-sref-marker") == [marker.clone()])
        .expect("the upstream saw the handshake");
    assert_eq!(header_values(ws, "authorization"), ["Bearer sk-k"]);
    assert!(header_values(ws, SECRET_REF_HEADER).is_empty());
}

/// SREF-SDK-005: errors returned by `facade().proxy_request`, converted with
/// `ServiceGatewayError::from`, give `InvalidSecretRef` with each
/// `SecretRefCode`, `PermissionDenied` with each new `DenialReason`, and
/// `Unavailable` for a failing CredStore.
#[tokio::test]
async fn sdk_005_typed_errors() {
    let fx = Fx::new().await;
    fx.secret("own", "sk-own", fx.t.tenant, &[fx.t.tenant]);
    fx.secret("root", "sk-root", fx.t.root, &[fx.t.tenant]);
    fx.upstream(
        fx.t.tenant,
        Some(request_ref(SharingMode::Private, None, None)),
    )
    .await;
    let typed = async |refs: &[&str]| {
        ServiceGatewayError::from(fx.sdk(fx.t.tenant, fx.post(refs)).await.unwrap_err())
    };
    assert!(matches!(
        typed(&["a,b"]).await,
        ServiceGatewayError::InvalidSecretRef {
            code: SecretRefCode::Invalid,
            ..
        }
    ));
    assert!(matches!(
        typed(&[]).await,
        ServiceGatewayError::InvalidSecretRef {
            code: SecretRefCode::Required,
            ..
        }
    ));
    assert!(matches!(
        typed(&["missing"]).await,
        ServiceGatewayError::PermissionDenied {
            reason: DenialReason::SecretRefNotAccessible,
            ..
        }
    ));
    assert!(matches!(
        typed(&["root"]).await,
        ServiceGatewayError::PermissionDenied {
            reason: DenialReason::SecretRefPolicyDenied,
            ..
        }
    ));

    let fx = Fx::new().await;
    fx.upstream(fx.t.tenant, None).await;
    assert!(matches!(
        ServiceGatewayError::from(fx.sdk(fx.t.tenant, fx.post(&["own"])).await.unwrap_err()),
        ServiceGatewayError::InvalidSecretRef {
            code: SecretRefCode::NotAllowed,
            ..
        }
    ));

    let fx = Fx::build(CredStoreSpy::always_failing(), |b| b).await;
    fx.upstream(
        fx.t.tenant,
        Some(request_ref(SharingMode::Private, None, None)),
    )
    .await;
    assert!(matches!(
        ServiceGatewayError::from(fx.sdk(fx.t.tenant, fx.post(&["own"])).await.unwrap_err()),
        ServiceGatewayError::Unavailable { .. }
    ));
}

// ---------------------------------------------------------------------------
// Observability
// ---------------------------------------------------------------------------

/// Records the request and error counters' labels.
#[derive(Default)]
struct LabelRecorder {
    rows: Mutex<Vec<Vec<String>>>,
}

impl LabelRecorder {
    fn rows(&self) -> Vec<Vec<String>> {
        self.rows.lock().unwrap().clone()
    }
}

impl OagwMetricsPort for LabelRecorder {
    fn record_request(
        &self,
        host: &str,
        path: &str,
        method: &str,
        status: u16,
        mode: Option<crate::domain::plugin::credentials::CredentialMode>,
    ) {
        let mode = mode.map_or("unknown", |m| m.as_str());
        self.rows.lock().unwrap().push(vec![
            "request".into(),
            host.into(),
            path.into(),
            method.into(),
            status.to_string(),
            mode.into(),
        ]);
    }
    fn record_error(
        &self,
        host: &str,
        path: &str,
        error_type: &str,
        mode: Option<crate::domain::plugin::credentials::CredentialMode>,
    ) {
        let mode = mode.map_or("unknown", |m| m.as_str());
        self.rows.lock().unwrap().push(vec![
            "error".into(),
            host.into(),
            path.into(),
            error_type.into(),
            mode.into(),
        ]);
    }
    fn record_request_duration_seconds(&self, _: &str, _: &str, _: &str, _: f64) {}
    fn increment_in_flight(&self, _: &str) {}
    fn decrement_in_flight(&self, _: &str) {}
    fn record_stream_duration_seconds(&self, _: &str, _: &str, _: StreamOutcome, _: f64) {}
    fn record_rate_limit_exceeded(&self, _: &str, _: &str, _: RateLimitBucket) {}
    fn record_rate_limit_usage_ratio(&self, _: &str, _: &str, _: RateLimitBucket, _: f64) {}
    fn increment_active_websocket_sessions(&self, _: &str) {}
    fn decrement_active_websocket_sessions(&self, _: &str) {}
    fn record_websocket_session_duration_seconds(&self, _: &str, _: StreamOutcome, _: f64) {}
    fn record_rate_limit_sweep(&self, _: u64, _: u64) {}
    fn record_registry_provisioning_failure(&self, _: &str, _: &str) {}
    fn record_registry_provisioning_run(&self, _: &str) {}
    fn record_credential_withheld(&self, _: &str, _: &'static str) {}
    fn record_proxy_cache_lookup(&self, _: CacheMap, _: CacheLookup) {}
    fn record_proxy_cache_entries(&self, _: CacheMap, _: u64) {}
    fn record_proxy_cache_evictions(&self, _: CacheMap, _: CacheEviction, _: u64) {}
    fn record_proxy_cache_change(&self, _: &str) {}
    fn record_proxy_cache_flush(&self, _: &str) {}
    fn record_proxy_cache_load_discarded(&self, _: CacheMap) {}
    fn record_proxy_cache_stale_served(&self, _: CacheMap, _: CacheStale) {}
    fn record_proxy_cache_publish_failures(&self, _: &str, _: u64) {}
}

const OBS_REF: &str = "sref-obs-reference-7f3a";
const OBS_VALUE: &str = "sk-obs-value-91c2";

/// SREF-OBS-001: error bodies, logs and metric labels never contain the
/// secret value, the reference or the auth header, including on guard
/// failures and CredStore errors.
#[tokio::test]
#[tracing_test::traced_test]
async fn obs_001_nothing_secret_leaks() {
    let metrics = Arc::new(LabelRecorder::default());
    let m = metrics.clone();
    let fx = Fx::build(CredStoreSpy::empty(), move |b| b.with_metrics(m)).await;
    fx.store.add_secret(
        SpySecret::new(OBS_REF, OBS_VALUE)
            .owned_by(fx.t.tenant)
            .secret_type(type_a()),
    );
    fx.upstream(
        fx.t.tenant,
        Some(request_ref(SharingMode::Private, None, Some(type_b()))),
    )
    .await;
    let mut bodies = vec![];
    // Type guard failure, a missing reference, a malformed one.
    for r in [
        OBS_REF.to_owned(),
        format!("{OBS_REF}-missing"),
        format!("{OBS_REF},x"),
    ] {
        let err = fx.sdk(fx.t.tenant, fx.post(&[&r])).await.unwrap_err();
        bodies.push(problem_of(&err).to_string());
        bodies.push(fx.rest(fx.t.tenant, fx.post(&[&r])).await.1.to_string());
    }
    // An owner outside the chain: Root's secret on Tenant's upstream.
    let root_ref = format!("{OBS_REF}-root");
    fx.store.add_secret(
        SpySecret::new(root_ref.as_str(), OBS_VALUE)
            .owned_by(fx.t.root)
            .secret_type(type_b()),
    );
    let err = fx
        .sdk(fx.t.tenant, fx.post(&[&root_ref]))
        .await
        .unwrap_err();
    assert_eq!(
        Outcome::from_problem(err.status_code(), &problem_of(&err)),
        forbidden(POLICY_DENIED)
    );
    bodies.push(problem_of(&err).to_string());
    bodies.push(
        fx.rest(fx.t.tenant, fx.post(&[&root_ref]))
            .await
            .1
            .to_string(),
    );

    // A header rule that replaces the injected header.
    let m = metrics.clone();
    let integrity = Fx::build(CredStoreSpy::empty(), move |b| b.with_metrics(m)).await;
    integrity
        .store
        .add_secret(SpySecret::new(OBS_REF, OBS_VALUE).owned_by(integrity.t.tenant));
    integrity
        .upstream_with(
            integrity.t.tenant,
            integrity
                .builder(Some(request_ref(SharingMode::Private, None, None)))
                .headers(HeadersConfig {
                    request: Some(rules(
                        &[("authorization", "Bearer x")],
                        &[],
                        &[],
                        PassthroughMode::None,
                    )),
                    response: None,
                }),
        )
        .await;
    let err = integrity
        .sdk(integrity.t.tenant, integrity.post(&[OBS_REF]))
        .await
        .unwrap_err();
    assert_eq!(
        Outcome::from_problem(err.status_code(), &problem_of(&err)),
        auth_header_conflict()
    );
    bodies.push(problem_of(&err).to_string());

    // A CredStore error and a CredStore timeout.
    for store in [
        CredStoreSpy::always_failing(),
        CredStoreSpy::never_answering(),
    ] {
        let m = metrics.clone();
        let failing = Fx::build(store, move |b| {
            b.with_metrics(m)
                .with_request_timeout(Duration::from_millis(300))
        })
        .await;
        failing
            .upstream(
                failing.t.tenant,
                Some(request_ref(SharingMode::Private, None, None)),
            )
            .await;
        let err = failing
            .sdk(failing.t.tenant, failing.post(&[OBS_REF]))
            .await
            .unwrap_err();
        assert_eq!(err.status_code(), 503);
        bodies.push(problem_of(&err).to_string());
        bodies.push(
            failing
                .rest(failing.t.tenant, failing.post(&[OBS_REF]))
                .await
                .1
                .to_string(),
        );
    }

    for body in &bodies {
        assert!(!body.contains(OBS_REF), "{body}");
        assert!(!body.contains(OBS_VALUE), "{body}");
        assert!(!body.contains("Bearer"), "{body}");
    }
    assert!(!logs_contain(OBS_REF));
    assert!(!logs_contain(OBS_VALUE));
    assert!(!logs_contain("Bearer sk"));
    for row in metrics.rows() {
        for label in row {
            assert!(
                !label.contains(OBS_REF) && !label.contains(OBS_VALUE),
                "{label}"
            );
        }
    }
}

/// SREF-OBS-002: a successful request logs the upstream, route and request
/// IDs with the secret type, and on CredStore v1 also the record ID and
/// owner tenant.
#[tokio::test]
#[tracing_test::traced_test]
async fn obs_002_success_logs_record_and_owner() {
    let fx = Fx::new().await;
    let record_id = Uuid::new_v4();
    fx.store.add_secret(
        SpySecret::new(OBS_REF, OBS_VALUE)
            .id(record_id)
            .owned_by(fx.t.tenant)
            .secret_type(type_a()),
    );
    let upstream_id = fx
        .upstream(
            fx.t.tenant,
            Some(request_ref(SharingMode::Private, None, None)),
        )
        .await;
    let request_id = format!("req-{}", Uuid::new_v4().as_simple());
    let req = fx.request(
        Method::POST,
        CHAT,
        "",
        &[OBS_REF],
        &[("x-request-id", &request_id)],
    );
    fx.sdk(fx.t.tenant, req).await.unwrap();
    assert!(logs_contain("request_ref secret resolved"));
    assert!(logs_contain(&format!("record_id={record_id}")));
    assert!(logs_contain(&format!("owner_tenant_id={}", fx.t.tenant)));
    assert!(logs_contain(&format!("secret_type={}", type_a())));
    assert!(logs_contain(&format!("upstream_id={upstream_id}")));
    assert!(logs_contain("route_id="));
    assert!(
        logs_contain(&format!("request_id=\"{request_id}\""))
            || logs_contain(&format!("request_id={request_id}"))
    );
    assert!(!logs_contain(OBS_VALUE));
}

/// Send a POST naming `reference`, with `x-request-id: request_id`, and
/// ignore the outcome (the caller asserts on the logs).
async fn send_with_request_id(fx: &Fx, reference: &str, request_id: &str) {
    let req = fx.request(
        Method::POST,
        CHAT,
        "",
        &[reference],
        &[("x-request-id", request_id)],
    );
    let _ = fx.sdk(fx.t.tenant, req).await;
}

/// Whether the captured logs (`logs_contain`, from `#[traced_test]`) show
/// `fx`'s caller, `upstream` and `request_id` together with `message`.
fn assert_logged(
    logs_contain: impl Fn(&str) -> bool,
    fx: &Fx,
    upstream: Uuid,
    request_id: &str,
    message: &str,
) {
    assert!(logs_contain(message), "{message}");
    // `tracing` prints a `&str` field quoted.
    assert!(
        logs_contain(&format!("request_id=\"{request_id}\""))
            || logs_contain(&format!("request_id={request_id}")),
        "{message}"
    );
    assert!(
        logs_contain(&format!("caller_tenant_id={}", fx.t.tenant)),
        "{message}"
    );
    assert!(
        logs_contain(&format!("upstream_id={upstream}")),
        "{message}"
    );
}

/// SREF-OBS-004: every denial and failure on the read logs the caller
/// tenant, upstream, route and request IDs, and never the reference: a
/// missing secret, a refused read, the owner-chain and type guards, a stored
/// value the header cannot carry, and a CredStore failure.
#[tokio::test]
#[tracing_test::traced_test]
async fn obs_004_denials_and_failures_carry_request_ids() {
    let rid = |case: &str| format!("obs4-{case}-{}", Uuid::new_v4().as_simple());

    // Missing secret.
    let fx = Fx::new().await;
    let up = fx
        .upstream(
            fx.t.tenant,
            Some(request_ref(SharingMode::Private, None, None)),
        )
        .await;
    let (id, reference) = (rid("missing"), "obs4-missing-ref");
    send_with_request_id(&fx, reference, &id).await;
    assert_logged(logs_contain, &fx, up, &id, "no value for the reference");

    // A read CredStore refuses.
    let fx = Fx::build(
        CredStoreSpy::failing_with(|| CredStoreError::AccessDenied),
        |b| b,
    )
    .await;
    let up = fx
        .upstream(
            fx.t.tenant,
            Some(request_ref(SharingMode::Private, None, None)),
        )
        .await;
    let id = rid("refused");
    send_with_request_id(&fx, "obs4-refused-ref", &id).await;
    assert_logged(
        logs_contain,
        &fx,
        up,
        &id,
        "request_ref secret not accessible",
    );

    // Owner above the auth owner (the tenant owns the upstream).
    let fx = Fx::new().await;
    fx.secret("obs4-partner-ref", "sk-p", fx.t.partner, &[fx.t.tenant]);
    let up = fx
        .upstream(
            fx.t.tenant,
            Some(request_ref(SharingMode::Private, None, None)),
        )
        .await;
    let id = rid("owner");
    send_with_request_id(&fx, "obs4-partner-ref", &id).await;
    assert_logged(
        logs_contain,
        &fx,
        up,
        &id,
        "outside the auth owner's subtree",
    );

    // Type not allowed.
    let fx = Fx::new().await;
    fx.secret("obs4-typed-ref", "sk-t", fx.t.tenant, &[fx.t.tenant]);
    let up = fx
        .upstream(
            fx.t.tenant,
            Some(request_ref(SharingMode::Private, None, Some(type_b()))),
        )
        .await;
    let id = rid("type");
    send_with_request_id(&fx, "obs4-typed-ref", &id).await;
    assert_logged(
        logs_contain,
        &fx,
        up,
        &id,
        "type is not allowed by the upstream",
    );

    // A stored value the header cannot carry.
    let fx = Fx::new().await;
    fx.store.add_secret(
        SpySecret::new("obs4-bad-ref", vec![0xff, 0xfe])
            .owned_by(fx.t.tenant)
            .visible_to([fx.t.tenant]),
    );
    let up = fx
        .upstream(
            fx.t.tenant,
            Some(request_ref(SharingMode::Private, None, None)),
        )
        .await;
    let id = rid("value");
    send_with_request_id(&fx, "obs4-bad-ref", &id).await;
    assert_logged(logs_contain, &fx, up, &id, "cannot be injected");

    // CredStore failure.
    let fx = Fx::build(
        CredStoreSpy::failing_with(|| CredStoreError::service_unavailable("down")),
        |b| b,
    )
    .await;
    let up = fx
        .upstream(
            fx.t.tenant,
            Some(request_ref(SharingMode::Private, None, None)),
        )
        .await;
    let id = rid("failure");
    send_with_request_id(&fx, "obs4-failure-ref", &id).await;
    assert_logged(logs_contain, &fx, up, &id, "credential store failed");

    for reference in [
        "obs4-missing-ref",
        "obs4-refused-ref",
        "obs4-partner-ref",
        "obs4-typed-ref",
        "obs4-bad-ref",
        "obs4-failure-ref",
    ] {
        assert!(!logs_contain(reference), "{reference} must not be logged");
    }
}

/// SREF-OBS-003: request metrics carry `credential_mode=config|request_ref`
/// and no reference or record ID label.
#[tokio::test]
async fn obs_003_credential_mode_label() {
    let metrics = Arc::new(LabelRecorder::default());
    let m = metrics.clone();
    let fx = Fx::build(CredStoreSpy::empty(), move |b| b.with_metrics(m)).await;
    let record_id = Uuid::new_v4();
    fx.store.add_secret(
        SpySecret::new(OBS_REF, OBS_VALUE)
            .id(record_id)
            .owned_by(fx.t.tenant),
    );
    fx.upstream(
        fx.t.tenant,
        Some(request_ref(SharingMode::Private, None, None)),
    )
    .await;
    fx.sdk(fx.t.tenant, fx.post(&[OBS_REF])).await.unwrap();
    let _ = fx.sdk(fx.t.tenant, fx.post(&["missing"])).await;
    // A `config` upstream on another alias.
    let cfg_alias = format!("{}-cfg", fx.alias);
    let up =
        fx.h.facade()
            .create_upstream(
                ctx(fx.t.tenant),
                CreateUpstreamRequest::builder(fx.server(), HTTP_PROTOCOL_ID)
                    .alias(&cfg_alias)
                    .build(),
            )
            .await
            .unwrap();
    fx.route(fx.t.tenant, up.id, vec![HttpMethod::Post], CHAT, vec![])
        .await;
    let req = http::Request::post(format!("/{cfg_alias}{}", fx.guard.path(CHAT)))
        .header(http::header::CONTENT_TYPE, "application/json")
        .body(Body::from("{}"))
        .unwrap();
    fx.sdk(fx.t.tenant, req).await.unwrap();
    // An alias that does not resolve: the mode is not known yet.
    let req = http::Request::post(format!("/{}-none{}", fx.alias, fx.guard.path(CHAT)))
        .header(SECRET_REF_HEADER, OBS_REF)
        .body(Body::from("{}"))
        .unwrap();
    let _ = fx.sdk(fx.t.tenant, req).await.unwrap_err();

    let rows = metrics.rows();
    let modes: Vec<(&str, &str)> = rows
        .iter()
        .map(|r| (r[0].as_str(), r.last().unwrap().as_str()))
        .collect();
    // A gateway error counts in both the request and the error counter.
    assert_eq!(
        modes,
        [
            ("request", "request_ref"),
            ("request", "request_ref"),
            ("error", "request_ref"),
            ("request", "config"),
            ("request", "unknown"),
            ("error", "unknown"),
        ]
    );
    for row in rows {
        for label in row {
            assert!(!label.contains(OBS_REF), "{label}");
            assert!(!label.contains(&record_id.to_string()), "{label}");
        }
    }
}

// ---------------------------------------------------------------------------
// Unchanged behavior
// ---------------------------------------------------------------------------

/// SREF-REG-002 (harness half; the end-to-end half is
/// `testing/e2e/suites/oagw/test_request_secret_refs.py`): an upstream in
/// `request_ref` mode created through the REST Management API, a tenant
/// secret, and a REST proxy request with the header: the provider receives
/// that tenant's key.
#[tokio::test]
async fn reg_002_rest_end_to_end() {
    let fx = Fx::new().await;
    fx.secret("tenant-openai", "sk-tenant-a", fx.t.tenant, &[fx.t.tenant]);
    let body = serde_json::json!({
        "server": {"endpoints": [{"host": "127.0.0.1", "port": fx.h.mock_port(), "scheme": "http"}]},
        "protocol": HTTP_PROTOCOL_ID,
        "alias": fx.alias,
        "enabled": true,
        "tags": [],
        "auth": {
            "type": APIKEY_AUTH_PLUGIN_ID,
            "sharing": "private",
            "config": {"header": "authorization", "prefix": "Bearer ", "credential_mode": "request_ref"}
        }
    });
    let req = http::Request::post("/oagw/v1/upstreams")
        .header(http::header::CONTENT_TYPE, "application/json")
        .body(axum::body::Body::from(body.to_string()))
        .unwrap();
    let resp = fx.h.router_as(ctx(fx.t.tenant)).oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let created: Value = serde_json::from_slice(
        &axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    let upstream_id = crate::test_support::parse_resource_gts(created["id"].as_str().unwrap())
        .expect("upstream id")
        .1;
    fx.route(
        fx.t.tenant,
        upstream_id,
        vec![HttpMethod::Post],
        CHAT,
        vec![],
    )
    .await;
    let (o, _) = fx
        .rest(fx.t.tenant, fx.post(&["cred://tenant-openai"]))
        .await;
    assert_eq!(o.status, 200);
    let rec = fx.recorded().await;
    assert_eq!(
        header_values(&rec[0], "authorization"),
        ["Bearer sk-tenant-a"]
    );
    assert!(header_values(&rec[0], SECRET_REF_HEADER).is_empty());
}
