//! End-to-end dispatcher tests (`eb-dispatcher-routing` §2/§3) driven
//! through the real `register_dispatcher_routes` router against genuine
//! mock HTTP instances registered via the `standalone` `ServiceDiscoveryV1`
//! provider (`test_support::{standalone_event_broker_cluster, mock_instance}`)
//! - not hand-rolled fakes.
//!
//! `classifies_every_endpoint_to_the_correct_role` doubles as the "no
//! instance registered" `503` case (task 3.4, spec "Forwarding error
//! semantics") since nothing is registered against a fresh `ClientHub` -
//! the `503` detail (which names the resolved role's service name) is what
//! makes classification observable through real `forward()` behavior.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use axum::Extension;
use axum::Router;
use axum::body::{Body, Bytes};
use axum::http::{Method, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use cluster_sdk::ServiceRegistration;
use http_body_util::BodyExt;
use tokio_stream::StreamExt;
use toolkit::api::OpenApiRegistry;
use toolkit::api::operation_builder::OperationSpec;
use tower::ServiceExt;

use super::register_dispatcher_routes;
use crate::infra::dispatcher::DispatcherState;

struct NoopOpenApiRegistry;

impl OpenApiRegistry for NoopOpenApiRegistry {
    fn register_operation(&self, _spec: &OperationSpec) {}

    fn ensure_schema_raw(
        &self,
        name: &str,
        _schemas: Vec<(
            String,
            utoipa::openapi::RefOr<utoipa::openapi::schema::Schema>,
        )>,
    ) -> String {
        name.to_owned()
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

fn build_router(state: Arc<DispatcherState>) -> Router {
    register_dispatcher_routes(Router::new(), &NoopOpenApiRegistry).layer(Extension(state))
}

/// Drives `method path` through `router` and returns the response body as
/// text (for asserting on the `Problem` `detail`).
async fn response_text(router: Router, method: Method, path: &str) -> (StatusCode, String) {
    let request = Request::builder()
        .method(method)
        .uri(path)
        .body(Body::empty())
        .expect("request must build");
    let response = router
        .oneshot(request)
        .await
        .expect("the service itself must not error");
    let status = response.status();
    let body = response
        .into_body()
        .collect()
        .await
        .expect("body must collect")
        .to_bytes();
    (status, String::from_utf8_lossy(&body).into_owned())
}

#[tokio::test]
async fn classifies_every_endpoint_to_the_correct_role() {
    // DESIGN.md:1354-1392's routing table: 5 Ingest + 11 Delivery + 3 Shared
    // (Shared forwards to Ingest - design.md D2). Expected role is the
    // `ServiceDiscoveryV1` service name `forward()` resolves (design.md D4).
    let cases: &[(Method, &str, &str)] = &[
        (Method::POST, "/event-broker/v1/events", "ingest"),
        (Method::POST, "/event-broker/v1/events:batch", "ingest"),
        (Method::POST, "/event-broker/v1/producers", "ingest"),
        (
            Method::GET,
            "/event-broker/v1/producers/p1/cursors",
            "ingest",
        ),
        (
            Method::POST,
            "/event-broker/v1/producers/p1:reset",
            "ingest",
        ),
        (Method::GET, "/event-broker/v1/events:stream", "delivery"),
        (Method::GET, "/event-broker/v1/events:sse", "delivery"),
        (Method::POST, "/event-broker/v1/consumer-groups", "delivery"),
        (Method::GET, "/event-broker/v1/consumer-groups", "delivery"),
        (
            Method::GET,
            "/event-broker/v1/consumer-groups/g1",
            "delivery",
        ),
        (
            Method::DELETE,
            "/event-broker/v1/consumer-groups/g1",
            "delivery",
        ),
        (Method::POST, "/event-broker/v1/subscriptions", "delivery"),
        (Method::GET, "/event-broker/v1/subscriptions", "delivery"),
        (Method::GET, "/event-broker/v1/subscriptions/s1", "delivery"),
        (
            Method::DELETE,
            "/event-broker/v1/subscriptions/s1",
            "delivery",
        ),
        (
            Method::POST,
            "/event-broker/v1/subscriptions/s1:seek",
            "delivery",
        ),
        (Method::GET, "/event-broker/v1/topics", "ingest"),
        (Method::GET, "/event-broker/v1/topics/segments", "ingest"),
        (Method::GET, "/event-broker/v1/event-types", "ingest"),
    ];
    assert_eq!(
        cases.len(),
        19,
        "DESIGN.md:1354-1392 lists 19 endpoints - update this table deliberately if that changes"
    );

    let (_hub, cluster) = crate::test_support::standalone_event_broker_cluster().await;
    let state = Arc::new(DispatcherState::new(cluster));

    for (method, path, expected_role) in cases.iter().cloned() {
        let router = build_router(Arc::clone(&state));
        let (status, body) = response_text(router, method.clone(), path).await;
        assert_eq!(
            status,
            StatusCode::SERVICE_UNAVAILABLE,
            "{method} {path} - nothing is registered, expected 503; body: {body}"
        );
        let expected_detail = format!("no {expected_role} instance registered");
        assert!(
            body.contains(&expected_detail),
            "{method} {path} expected detail to contain {expected_detail:?}, body was: {body}"
        );
    }
}

#[tokio::test]
async fn forwards_to_the_registered_instance() {
    let (_hub, cluster) = crate::test_support::standalone_event_broker_cluster().await;
    let mock_router = Router::new().route(
        "/event-broker/v1/topics",
        get(|| async { "mock topics response" }),
    );
    let mock = crate::test_support::mock_instance(&cluster, "ingest", mock_router).await;

    let state = Arc::new(DispatcherState::new(cluster));
    let router = build_router(state);
    let (status, body) = response_text(router, Method::GET, "/event-broker/v1/topics").await;

    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body, "mock topics response");
    drop(mock);
}

#[tokio::test]
async fn random_selection_reaches_both_registered_instances() {
    let (_hub, cluster) = crate::test_support::standalone_event_broker_cluster().await;
    let router_a = Router::new().route("/event-broker/v1/subscriptions", get(|| async { "A" }));
    let router_b = Router::new().route("/event-broker/v1/subscriptions", get(|| async { "B" }));
    let mock_a = crate::test_support::mock_instance(&cluster, "delivery", router_a).await;
    let mock_b = crate::test_support::mock_instance(&cluster, "delivery", router_b).await;

    let state = Arc::new(DispatcherState::new(cluster));
    let (mut seen_a, mut seen_b) = (false, false);
    for _ in 0..40 {
        let router = build_router(Arc::clone(&state));
        let (status, body) =
            response_text(router, Method::GET, "/event-broker/v1/subscriptions").await;
        assert_eq!(status, StatusCode::OK, "body: {body}");
        match body.as_str() {
            "A" => seen_a = true,
            "B" => seen_b = true,
            other => panic!("unexpected response body: {other}"),
        }
        if seen_a && seen_b {
            break;
        }
    }

    assert!(
        seen_a && seen_b,
        "expected both mock instances to receive at least one request across 40 tries \
         (seen_a={seen_a}, seen_b={seen_b})"
    );
    drop((mock_a, mock_b));
}

#[tokio::test]
async fn returns_503_with_distinct_detail_when_resolved_instance_is_unreachable() {
    let (_hub, cluster) = crate::test_support::standalone_event_broker_cluster().await;

    // Bind then immediately drop: a concrete, closed target that refuses
    // connections - unlike a merely-unassigned port, whose OS behavior
    // varies by platform.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("binding an ephemeral local port must not fail");
    let addr = listener
        .local_addr()
        .expect("bound listener has an address");
    drop(listener);

    let _registration = cluster
        .service_discovery
        .register(ServiceRegistration {
            name: "ingest".to_owned(),
            instance_id: None,
            address: format!("http://{addr}"),
            metadata: HashMap::new(),
        })
        .await
        .expect("registering an instance address must not fail");

    let state = Arc::new(DispatcherState::new(cluster));
    let router = build_router(state);
    let (status, body) = response_text(router, Method::GET, "/event-broker/v1/topics").await;

    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "body: {body}");
    assert!(
        body.contains("resolved ingest instance unreachable"),
        "body: {body}"
    );
    assert!(
        !body.contains("no ingest instance registered"),
        "must be the distinct unreachable detail, not the no-instance-registered one; body: {body}"
    );
}

/// Emits three heartbeat bytes a little apart, then goes silent (holding the
/// connection open) rather than ever completing the response - simulating a
/// long-poll/SSE upstream (`DESIGN.md:1825`'s heartbeat-cadence framing).
async fn heartbeat_then_silence() -> Response {
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(4);
    tokio::spawn(async move {
        for _ in 0..3 {
            tokio::time::sleep(Duration::from_millis(30)).await;
            if tx.send(Ok(Bytes::from_static(b"."))).await.is_err() {
                return;
            }
        }
        tokio::time::sleep(Duration::from_hours(1)).await;
    });
    let body = Body::from_stream(tokio_stream::wrappers::ReceiverStream::new(rx));
    (StatusCode::OK, body).into_response()
}

#[tokio::test]
async fn idle_timeout_closes_connection_after_silence_but_not_between_heartbeats() {
    let (_hub, cluster) = crate::test_support::standalone_event_broker_cluster().await;
    let mock_router = Router::new().route(
        "/event-broker/v1/events:stream",
        get(heartbeat_then_silence),
    );
    let mock = crate::test_support::mock_instance(&cluster, "delivery", mock_router).await;

    // Heartbeats arrive every 30ms; a 150ms idle window must survive each
    // one but close shortly after they stop (spec "Idle-timeout on proxied
    // streaming connections").
    let state =
        Arc::new(DispatcherState::new(cluster).with_idle_timeout(Duration::from_millis(150)));
    let router = build_router(state);
    let request = Request::builder()
        .method(Method::GET)
        .uri("/event-broker/v1/events:stream")
        .body(Body::empty())
        .expect("request must build");
    let response = router
        .oneshot(request)
        .await
        .expect("the service itself must not error");
    assert_eq!(response.status(), StatusCode::OK);

    let mut stream = response.into_body().into_data_stream();
    let mut heartbeat_bytes = 0usize;
    let drained = tokio::time::timeout(Duration::from_secs(2), async {
        while let Some(frame) = stream.next().await {
            match frame {
                Ok(chunk) => heartbeat_bytes += chunk.len(),
                Err(_) => break,
            }
        }
    })
    .await;

    assert!(
        drained.is_ok(),
        "the idle-timeout must close the connection well within 2s of the last heartbeat, \
         not hang forever"
    );
    assert!(
        heartbeat_bytes >= 1,
        "expected at least one heartbeat byte to arrive before the connection closed"
    );
    drop(mock);
}
