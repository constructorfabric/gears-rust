//! Generated clients with `#[client_manual]` methods against a real server.

#![allow(clippy::unwrap_used)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::extract::{Path, Query};
use axum::http::HeaderMap;
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::StreamExt as _;
use secrecy::SecretString;
use toolkit_contract::runtime::config::{ClientConfig, InternalTokenProvider};
use toolkit_contract::runtime::resolving::{EndpointResolver, ResolveError};
use toolkit_contract::wiring::ClientTuning;
use toolkit_contract_manual_fixture::handlers::{DemoHandler, StatefulHandler};
use toolkit_contract_manual_fixture::{
    Echo, ManualDemoApi, ManualDemoApiRest, ManualDemoApiRestClient, ManualDemoApiRestManual,
    ManualDemoApiRestResolvingClient, StatefulDemoApi, StatefulDemoApiRestClient,
    StatefulDemoApiRestResolvingClient,
};
use toolkit_security::{PlatformIdentity, PlatformSecurityContext, SecurityContext};

fn echo(value: String, headers: &HeaderMap) -> Json<Echo> {
    let header = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
    };
    Json(Echo {
        value,
        authorization: header("authorization"),
        internal_token: header("x-toolkit-internal-token"),
        marker: header("x-manual"),
    })
}

#[derive(serde::Deserialize)]
struct Count {
    count: u32,
}

/// `0..count`, or a 500 for `count=0` so stream errors can be exercised.
async fn numbers(Query(q): Query<Count>) -> axum::response::Response {
    use axum::response::IntoResponse as _;
    if q.count == 0 {
        return axum::http::StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    Json((0..q.count).collect::<Vec<u32>>()).into_response()
}

async fn start_server() -> String {
    let app =
        Router::new()
            .route(
                "/manual-demo/v1/echo",
                post(|headers: HeaderMap, Json(value): Json<String>| async move {
                    echo(value, &headers)
                }),
            )
            .route(
                "/manual-demo/v1/items/{id}",
                get(|headers: HeaderMap, Path(id): Path<String>| async move { echo(id, &headers) }),
            )
            .route(
                "/manual-demo/v1/audit/{id}",
                get(|headers: HeaderMap, Path(id): Path<String>| async move { echo(id, &headers) }),
            )
            .route("/manual-demo/v1/ticks", get(numbers))
            .route("/manual-demo/v1/feed", get(numbers));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

fn tenant_ctx() -> SecurityContext {
    SecurityContext::builder()
        .subject_id(uuid::Uuid::nil())
        .subject_tenant_id(uuid::Uuid::nil())
        .bearer_token("tenant.jwt".to_owned())
        .build()
        .unwrap()
}

fn platform_ctx() -> PlatformSecurityContext {
    PlatformSecurityContext::new(PlatformIdentity::Shared {
        name: "test-caller".to_owned(),
    })
}

#[tokio::test]
async fn generated_and_manual_methods_share_one_client() {
    let url = start_server().await;
    // `new` exists because the projection names a default handler.
    let api: Arc<dyn ManualDemoApi> =
        Arc::new(ManualDemoApiRestClient::new(ClientConfig::new(url)).unwrap());

    let generated = api.echo(tenant_ctx(), "x".to_owned()).await.unwrap();
    assert_eq!(generated.value, "x");
    assert_eq!(
        generated.authorization.as_deref(),
        Some("Bearer tenant.jwt")
    );
    assert_eq!(
        generated.marker, None,
        "generated dispatch, not the handler"
    );

    let manual = api.lookup(tenant_ctx(), "42".to_owned()).await.unwrap();
    assert_eq!(manual.value, "42");
    assert_eq!(manual.authorization.as_deref(), Some("Bearer tenant.jwt"));
    assert_eq!(manual.internal_token, None);
    assert_eq!(manual.marker.as_deref(), Some("1"));
}

#[tokio::test]
async fn manual_platform_method_sends_the_internal_token() {
    let url = start_server().await;
    let cfg = ClientConfig::new(url).with_internal_token_provider(
        InternalTokenProvider::from_token(SecretString::from("sa.jwt")),
    );
    let client = ManualDemoApiRestClient::new(cfg).unwrap();

    let echo = ManualDemoApi::audit(&client, platform_ctx(), "7".to_owned())
        .await
        .unwrap();
    assert_eq!(echo.internal_token.as_deref(), Some("sa.jwt"));
    assert_eq!(echo.authorization, None);
    assert_eq!(echo.marker.as_deref(), Some("1"));
}

#[tokio::test]
async fn projection_trait_and_dyn_handler_dispatch_to_the_handler() {
    let url = start_server().await;
    let handler: Arc<dyn ManualDemoApiRestManual> = Arc::new(DemoHandler);
    let client = ManualDemoApiRestClient::with_manual(ClientConfig::new(url), handler).unwrap();
    let api: Arc<dyn ManualDemoApiRest> = Arc::new(client);

    let echo = ManualDemoApiRest::lookup(&*api, tenant_ctx(), "9".to_owned())
        .await
        .unwrap();
    assert_eq!(echo.marker.as_deref(), Some("1"));
}

#[tokio::test]
async fn manual_streams_keep_both_open_shapes() {
    let url = start_server().await;
    let client = ManualDemoApiRestClient::new(ClientConfig::new(url)).unwrap();

    let ticks: Vec<u32> = ManualDemoApi::ticks(&client, tenant_ctx(), 3)
        .map(Result::unwrap)
        .collect()
        .await;
    assert_eq!(ticks, vec![0, 1, 2]);

    let feed: Vec<u32> = ManualDemoApi::feed(&client, tenant_ctx(), 2)
        .await
        .unwrap()
        .map(Result::unwrap)
        .collect()
        .await;
    assert_eq!(feed, vec![0, 1]);
}

#[tokio::test]
async fn stateful_handler_is_shared_by_with_manual_clients() {
    let handler = Arc::new(StatefulHandler::default());
    let client = StatefulDemoApiRestClient::with_manual(
        ClientConfig::new("http://provider.invalid"),
        Arc::clone(&handler),
    )
    .unwrap();

    client.count(tenant_ctx()).await.unwrap();
    let echo = client.count(tenant_ctx()).await.unwrap();
    assert_eq!(echo.value, "2");
    assert_eq!(echo.marker.as_deref(), Some("http://provider.invalid"));
    assert_eq!(handler.calls(), 2);
}

/// Returns `endpoints[n]` on the `n`-th lookup, then the last one.
struct Churn {
    endpoints: Vec<String>,
    lookups: AtomicUsize,
}

#[async_trait::async_trait]
impl EndpointResolver for Churn {
    async fn resolve_endpoint(&self, _gear: &str) -> Result<Option<String>, ResolveError> {
        let n = self.lookups.fetch_add(1, Ordering::SeqCst);
        Ok(self
            .endpoints
            .get(n)
            .or_else(|| self.endpoints.last())
            .cloned())
    }
}

#[tokio::test]
async fn endpoint_churn_rebuilds_the_transport_around_the_same_handler() {
    let handler = Arc::new(StatefulHandler::default());
    let resolver = Arc::new(Churn {
        endpoints: vec!["http://a.invalid".to_owned(), "http://b.invalid".to_owned()],
        lookups: AtomicUsize::new(0),
    });
    let client = StatefulDemoApiRestResolvingClient::with_manual(
        resolver,
        "stateful-demo",
        ClientTuning::default(),
        Arc::clone(&handler),
    );

    let first = client.count(tenant_ctx()).await.unwrap();
    let second = client.count(tenant_ctx()).await.unwrap();
    assert_eq!(first.marker.as_deref(), Some("http://a.invalid"));
    assert_eq!(second.marker.as_deref(), Some("http://b.invalid"));
    assert_eq!(second.value, "2", "handler state survives the rebuild");
    assert_eq!(handler.calls(), 2);
}

#[tokio::test]
async fn default_resolving_client_keeps_its_new_constructor() {
    let url = start_server().await;
    let resolver = Arc::new(Churn {
        endpoints: vec![url],
        lookups: AtomicUsize::new(0),
    });
    // What `#[toolkit::consumes]` expands to.
    let api: Arc<dyn ManualDemoApi> = Arc::new(ManualDemoApiRestResolvingClient::new(
        resolver,
        "manual-demo",
        ClientTuning::default(),
    ));

    let echo = api.lookup(tenant_ctx(), "1".to_owned()).await.unwrap();
    assert_eq!(echo.marker.as_deref(), Some("1"));
    let generated = api.echo(tenant_ctx(), "g".to_owned()).await.unwrap();
    assert_eq!(generated.marker, None);
}

#[tokio::test]
async fn clients_over_a_caller_supplied_http_client() {
    let url = start_server().await;
    let config = ClientConfig::new(url);
    let http =
        toolkit_contract::runtime::client::build_default_http_client("test", &config).unwrap();

    // Default handler.
    let default = ManualDemoApiRestClient::with_http_client(http.clone(), config.clone());
    let generated = ManualDemoApi::echo(&default, tenant_ctx(), "g".to_owned())
        .await
        .unwrap();
    assert_eq!(generated.value, "g");

    // Explicit handler.
    let explicit =
        ManualDemoApiRestClient::with_http_client_and_manual(http, config, Arc::new(DemoHandler));
    let manual = ManualDemoApi::lookup(&explicit, tenant_ctx(), "m".to_owned())
        .await
        .unwrap();
    assert_eq!(manual.marker.as_deref(), Some("1"));
}

#[tokio::test]
async fn manual_stream_errors_arrive_as_items() {
    let url = start_server().await;
    let client = ManualDemoApiRestClient::new(ClientConfig::new(url)).unwrap();

    let ticks: Vec<_> = ManualDemoApi::ticks(&client, tenant_ctx(), 0)
        .collect()
        .await;
    assert!(matches!(ticks.as_slice(), [Err(_)]), "{ticks:?}");

    // The fallible open succeeds; the failed request is the stream's item.
    let feed: Vec<_> = ManualDemoApi::feed(&client, tenant_ctx(), 0)
        .await
        .unwrap()
        .collect()
        .await;
    assert!(matches!(feed.as_slice(), [Err(_)]), "{feed:?}");
}
