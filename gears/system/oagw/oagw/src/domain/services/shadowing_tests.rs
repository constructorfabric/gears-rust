//! A descendant that shadows an ancestor's alias with its own server must not
//! receive the ancestor's credential or headers.
//!
//! Each test runs the whole pipeline: a root tenant's upstream `svc` on server A
//! with an API key, a child tenant's upstream `svc` on server B, and a request
//! the child proxies to `/svc/v1/x`. The servers record what they receive.

#![allow(clippy::unwrap_used)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::HeaderMap;
use oagw_sdk::ServiceGatewayError;
use oagw_sdk::api::ServiceGatewayClientV1;
use oagw_sdk::body::Body;
use oagw_sdk::{
    AuthConfig, CreateRouteRequest, CreateUpstreamRequest, Endpoint, HeadersConfig, HttpMatch,
    HttpMethod, MatchRules, PathSuffixMode, RequestHeaderRules, Scheme, Server, SharingMode,
    UpdateUpstreamRequest,
};
use tenant_resolver_sdk::TenantId;
use toolkit::client_hub::ClientHub;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::domain::test_support::{
    APIKEY_AUTH_PLUGIN_ID, MockTenantResolverClient, TestCpBuilder, TestDpBuilder,
    build_test_gateway,
};

/// What one server received: the `x-api-key` and `x-root-token` of each request.
type Seen = Arc<Mutex<Vec<(Option<String>, Option<String>)>>>;

async fn record(State(seen): State<Seen>, headers: HeaderMap) -> &'static str {
    let get = |name: &str| headers.get(name).map(|v| v.to_str().unwrap().to_owned());
    seen.lock()
        .unwrap()
        .push((get("x-api-key"), get("x-root-token")));
    "ok"
}

async fn server() -> (u16, Seen) {
    let seen = Seen::default();
    let app = axum::Router::new()
        .route("/v1/x", axum::routing::post(record))
        .with_state(seen.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (port, seen)
}

fn ctx(tenant: Uuid) -> SecurityContext {
    SecurityContext::builder()
        .subject_tenant_id(tenant)
        .subject_id(Uuid::new_v4())
        .build()
        .unwrap()
}

/// `127.0.0.1` and a TEST-NET address nothing listens on, both on `port`
/// (all endpoints of an upstream share one port).
fn with_unused_endpoint(port: u16) -> Server {
    let mut server = on(port);
    server.endpoints.push(Endpoint {
        scheme: Scheme::Http,
        host: "192.0.2.1".into(),
        port,
    });
    server
}

fn on(port: u16) -> Server {
    Server {
        endpoints: vec![Endpoint {
            scheme: Scheme::Http,
            host: "127.0.0.1".into(),
            port,
        }],
    }
}

fn key(secret_ref: &str, sharing: SharingMode) -> AuthConfig {
    AuthConfig {
        plugin_type: APIKEY_AUTH_PLUGIN_ID.into(),
        sharing,
        config: Some(HashMap::from([
            ("header".into(), "x-api-key".into()),
            ("secret_ref".into(), secret_ref.into()),
        ])),
    }
}

fn root_headers() -> HeadersConfig {
    HeadersConfig {
        request: Some(RequestHeaderRules {
            set: HashMap::from([("x-root-token".into(), "root-token".into())]),
            ..RequestHeaderRules::default()
        }),
        response: None,
    }
}

struct Tenants {
    gw: Arc<dyn ServiceGatewayClientV1>,
    root: SecurityContext,
    child: SecurityContext,
}

/// A root and a child tenant; the root owns `svc` on `root_port` with its key
/// under `sharing`, its `x-root-token` header and a route `POST /v1/x`.
async fn tenants(root_port: u16, sharing: SharingMode) -> Tenants {
    let (root, child) = (Uuid::new_v4(), Uuid::new_v4());
    let hub = ClientHub::new();
    let gw = build_test_gateway(
        &hub,
        TestCpBuilder::new()
            .with_tenant_resolver(MockTenantResolverClient::with_hierarchy(vec![
                TenantId(root),
                TenantId(child),
            ]))
            .with_credentials(vec![
                ("cred://root-key".into(), "root-secret".into()),
                ("cred://child-key".into(), "child-secret".into()),
            ]),
        TestDpBuilder::new(),
    );
    let tenants = Tenants {
        gw,
        root: ctx(root),
        child: ctx(child),
    };
    let upstream = tenants
        .gw
        .create_upstream(
            tenants.root.clone(),
            CreateUpstreamRequest::builder(on(root_port), oagw_sdk::HTTP_PROTOCOL_ID)
                .alias("svc")
                .auth(key("cred://root-key", sharing))
                .headers(root_headers())
                .build(),
        )
        .await
        .unwrap();
    tenants
        .gw
        .create_route(
            tenants.root.clone(),
            CreateRouteRequest::builder(
                upstream.id,
                MatchRules {
                    http: Some(HttpMatch {
                        methods: vec![HttpMethod::Post],
                        path: "/v1/x".into(),
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
    tenants
}

impl Tenants {
    /// Replace the root's upstream: endpoints `server`, its key under
    /// `sharing`, its header.
    async fn update_root(&self, server: Server, sharing: SharingMode) {
        let upstreams = self
            .gw
            .list_upstreams(self.root.clone(), &oagw_sdk::ListQuery::default())
            .await
            .unwrap();
        let root_upstream = upstreams.into_iter().find(|u| u.alias == "svc").unwrap();
        self.gw
            .update_upstream(
                self.root.clone(),
                root_upstream.id,
                UpdateUpstreamRequest::builder(server, oagw_sdk::HTTP_PROTOCOL_ID)
                    .alias("svc")
                    .auth(key("cred://root-key", sharing))
                    .headers(root_headers())
                    .build(),
            )
            .await
            .unwrap();
    }

    async fn shadow(
        &self,
        port: u16,
        auth: Option<AuthConfig>,
    ) -> Result<oagw_sdk::Upstream, toolkit_canonical_errors::CanonicalError> {
        let mut req =
            CreateUpstreamRequest::builder(on(port), oagw_sdk::HTTP_PROTOCOL_ID).alias("svc");
        if let Some(auth) = auth {
            req = req.auth(auth);
        }
        self.gw
            .create_upstream(self.child.clone(), req.build())
            .await
    }

    async fn child_call(
        &self,
    ) -> Result<http::Response<Body>, toolkit_canonical_errors::CanonicalError> {
        let req = http::Request::post("/svc/v1/x").body(Body::Empty).unwrap();
        self.gw.proxy_request(self.child.clone(), req).await
    }
}

/// The reported scenario: the child points `svc` at its own server and sets no
/// key of its own. Before the fix its server received the root's key.
#[tokio::test]
async fn an_inherited_key_stays_on_the_ancestors_server() {
    let (port_a, seen_a) = server().await;
    let (port_b, seen_b) = server().await;
    let t = tenants(port_a, SharingMode::Inherit).await;
    t.shadow(port_b, None).await.unwrap();

    let resp = t.child_call().await.unwrap();
    assert_eq!(resp.status(), 200);

    assert_eq!(*seen_b.lock().unwrap(), vec![(None, None)]);
    assert!(seen_a.lock().unwrap().is_empty());
}

/// An enforced key must be applied, and it cannot be applied to another
/// server: binding the alias there is refused.
#[tokio::test]
async fn an_enforced_key_cannot_be_bound_to_another_server() {
    let (port_a, _) = server().await;
    let (port_b, seen_b) = server().await;
    let t = tenants(port_a, SharingMode::Enforce).await;

    let err = t.shadow(port_b, None).await.unwrap_err();
    assert!(matches!(
        ServiceGatewayError::from(err),
        ServiceGatewayError::Validation { .. }
    ),);
    assert!(seen_b.lock().unwrap().is_empty());
}

/// A shadow bound while the key was `inherit`, after the root switched it to
/// `enforce`: the proxy refuses the call instead of sending the key.
#[tokio::test]
async fn a_key_enforced_after_the_bind_is_refused_at_proxy_time() {
    let (port_a, _) = server().await;
    let (port_b, seen_b) = server().await;
    let t = tenants(port_a, SharingMode::Inherit).await;
    t.shadow(port_b, None).await.unwrap();

    t.update_root(on(port_a), SharingMode::Enforce).await;

    let err = t.child_call().await.unwrap_err();
    assert!(matches!(
        ServiceGatewayError::from(err),
        ServiceGatewayError::PermissionDenied { .. }
    ),);
    assert!(seen_b.lock().unwrap().is_empty());
}

/// Binding on the root's own server keeps inheriting its key and headers.
#[tokio::test]
async fn a_shadow_on_the_same_server_inherits_the_key() {
    let (port_a, seen_a) = server().await;
    let t = tenants(port_a, SharingMode::Inherit).await;
    t.shadow(port_a, None).await.unwrap();

    t.child_call().await.unwrap();
    assert_eq!(
        *seen_a.lock().unwrap(),
        vec![(Some("root-secret".into()), Some("root-token".into()))]
    );
}

/// Bring your own key: the child's key goes to the child's server.
#[tokio::test]
async fn the_childs_own_key_goes_to_the_childs_server() {
    let (port_a, _) = server().await;
    let (port_b, seen_b) = server().await;
    let t = tenants(port_a, SharingMode::Inherit).await;
    t.shadow(port_b, Some(key("cred://child-key", SharingMode::Private)))
        .await
        .unwrap();

    t.child_call().await.unwrap();
    assert_eq!(
        *seen_b.lock().unwrap(),
        vec![(Some("child-secret".into()), None)]
    );
}

/// The review case: the root adds an endpoint after the child bound on its
/// old one. The child's endpoint is still one the root configured, so it keeps
/// the key.
#[tokio::test]
async fn a_root_adding_an_endpoint_keeps_existing_bindings_working() {
    let (port_a, seen_a) = server().await;
    let t = tenants(port_a, SharingMode::Inherit).await;
    t.shadow(port_a, None).await.unwrap();

    t.update_root(with_unused_endpoint(port_a), SharingMode::Inherit)
        .await;

    t.child_call().await.unwrap();
    assert_eq!(
        *seen_a.lock().unwrap(),
        vec![(Some("root-secret".into()), Some("root-token".into()))]
    );
}

/// Under `enforce` too, a child may bind on part of the root's endpoints.
#[tokio::test]
async fn an_enforced_key_reaches_a_child_on_one_of_the_roots_endpoints() {
    let (port_a, seen_a) = server().await;
    let t = tenants(port_a, SharingMode::Inherit).await;
    t.update_root(with_unused_endpoint(port_a), SharingMode::Enforce)
        .await;
    t.shadow(port_a, None).await.unwrap();

    t.child_call().await.unwrap();
    assert_eq!(
        *seen_a.lock().unwrap(),
        vec![(Some("root-secret".into()), Some("root-token".into()))]
    );
}
