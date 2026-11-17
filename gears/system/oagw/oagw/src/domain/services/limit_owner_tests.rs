//! Tenants that resolve different limits on one upstream must not share its
//! bucket: one tenant's requests would use up the other's limit.
//!
//! root -> parent -> child, one alias `svc`. The root's limit is `inherit`,
//! 1000/min; the parent's own is `private`, 1/min, and its noop auth is
//! shared so its upstream is the one the child resolves. The child has no
//! upstream and gets the root's limit on the parent's upstream.

#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use oagw_sdk::api::ServiceGatewayClientV1;
use oagw_sdk::body::Body;
use oagw_sdk::{
    AuthConfig, CreateRouteRequest, CreateUpstreamRequest, Endpoint, HttpMatch, HttpMethod,
    MatchRules, PathSuffixMode, RateLimitAlgorithm, RateLimitConfig, RateLimitScope,
    RateLimitStrategy, Scheme, Server, SharingMode, SustainedRate, Window,
};
use tenant_resolver_sdk::TenantId;
use toolkit::api::canonical_prelude::CanonicalError;
use toolkit::client_hub::ClientHub;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::domain::gts_helpers::NOOP_AUTH_PLUGIN_ID;
use crate::domain::test_support::{
    MockTenantResolverClient, TestCpBuilder, TestDpBuilder, build_test_gateway,
};

async fn server() -> u16 {
    let app = axum::Router::new().route("/v1/x", axum::routing::post(|| async { "ok" }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    port
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

fn ctx(tenant: Uuid) -> SecurityContext {
    SecurityContext::builder()
        .subject_tenant_id(tenant)
        .subject_id(Uuid::new_v4())
        .build()
        .unwrap()
}

fn per_minute(rate: u32, sharing: SharingMode) -> RateLimitConfig {
    RateLimitConfig {
        sharing,
        algorithm: RateLimitAlgorithm::TokenBucket,
        sustained: SustainedRate {
            rate,
            window: Window::Minute,
        },
        burst: None,
        budget: None,
        scope: RateLimitScope::Global,
        strategy: RateLimitStrategy::Reject,
        cost: 1,
        response_headers: false,
    }
}

async fn call(
    gw: &Arc<dyn ServiceGatewayClientV1>,
    who: &SecurityContext,
) -> Result<http::Response<Body>, CanonicalError> {
    let req = http::Request::post("/svc/v1/x").body(Body::Empty).unwrap();
    gw.proxy_request(who.clone(), req).await
}

#[tokio::test]
async fn a_descendants_requests_do_not_use_up_a_private_limit() {
    let port = server().await;
    let (root, parent, child) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    let hub = ClientHub::new();
    let gw =
        build_test_gateway(
            &hub,
            TestCpBuilder::new().with_tenant_resolver(MockTenantResolverClient::with_hierarchy(
                vec![TenantId(root), TenantId(parent), TenantId(child)],
            )),
            TestDpBuilder::new(),
        );
    let (root, parent, child) = (ctx(root), ctx(parent), ctx(child));

    let root_upstream = gw
        .create_upstream(
            root.clone(),
            CreateUpstreamRequest::builder(on(port), oagw_sdk::HTTP_PROTOCOL_ID)
                .alias("svc")
                .rate_limit(per_minute(1000, SharingMode::Inherit))
                .build(),
        )
        .await
        .unwrap();
    gw.create_route(
        root.clone(),
        CreateRouteRequest::builder(
            root_upstream.id,
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
    gw.create_upstream(
        parent.clone(),
        CreateUpstreamRequest::builder(on(port), oagw_sdk::HTTP_PROTOCOL_ID)
            .alias("svc")
            .auth(AuthConfig {
                plugin_type: NOOP_AUTH_PLUGIN_ID.into(),
                sharing: SharingMode::Inherit,
                config: None,
            })
            .rate_limit(per_minute(1, SharingMode::Private))
            .build(),
    )
    .await
    .unwrap();

    // In one bucket the child's request would use up the parent's 1/min.
    assert_eq!(call(&gw, &child).await.unwrap().status(), 200);
    assert_eq!(call(&gw, &parent).await.unwrap().status(), 200);
    let err = call(&gw, &parent).await.unwrap_err();
    assert!(
        matches!(err, CanonicalError::ResourceExhausted { .. }),
        "the parent's 1/min must hold, got {err:?}"
    );
    // The child is held to the root's limit, not the parent's.
    assert_eq!(call(&gw, &child).await.unwrap().status(), 200);
}

/// The same with a shared pool: the root enforces a shared budget, so every
/// tenant's limit is keyed on the pool owner. The parent's own `private`
/// 1/min (written before the root enforced) still caps it; the child, which
/// inherits the root's limit, must not share and reset the parent's bucket.
#[tokio::test]
async fn a_descendants_requests_do_not_reset_a_private_limit_under_a_shared_pool() {
    let port = server().await;
    let (root, parent, child) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    let hub = ClientHub::new();
    let gw =
        build_test_gateway(
            &hub,
            TestCpBuilder::new().with_tenant_resolver(MockTenantResolverClient::with_hierarchy(
                vec![TenantId(root), TenantId(parent), TenantId(child)],
            )),
            TestDpBuilder::new(),
        );
    let (root, parent, child) = (ctx(root), ctx(parent), ctx(child));

    // The parent binds first: under an enforcing root its private limit
    // would be refused at bind.
    gw.create_upstream(
        parent.clone(),
        CreateUpstreamRequest::builder(on(port), oagw_sdk::HTTP_PROTOCOL_ID)
            .alias("svc")
            .auth(AuthConfig {
                plugin_type: NOOP_AUTH_PLUGIN_ID.into(),
                sharing: SharingMode::Inherit,
                config: None,
            })
            .rate_limit(per_minute(1, SharingMode::Private))
            .build(),
    )
    .await
    .unwrap();
    let mut pool = per_minute(1000, SharingMode::Enforce);
    pool.budget = Some(oagw_sdk::BudgetConfig {
        mode: oagw_sdk::BudgetMode::Shared,
        total: Some(1000),
        overcommit_ratio: None,
    });
    let root_upstream = gw
        .create_upstream(
            root.clone(),
            CreateUpstreamRequest::builder(on(port), oagw_sdk::HTTP_PROTOCOL_ID)
                .alias("svc")
                .rate_limit(pool)
                .build(),
        )
        .await
        .unwrap();
    gw.create_route(
        root.clone(),
        CreateRouteRequest::builder(
            root_upstream.id,
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

    assert_eq!(call(&gw, &parent).await.unwrap().status(), 200);
    assert_eq!(call(&gw, &child).await.unwrap().status(), 200);
    let err = call(&gw, &parent).await.unwrap_err();
    assert!(
        matches!(err, CanonicalError::ResourceExhausted { .. }),
        "the parent's 1/min must hold under the shared pool, got {err:?}"
    );
    assert_eq!(call(&gw, &child).await.unwrap().status(), 200);
}
