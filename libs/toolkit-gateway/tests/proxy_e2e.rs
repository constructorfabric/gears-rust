#![allow(clippy::unwrap_used, clippy::expect_used)]

//! End-to-end reverse-proxy tests: spin a real upstream axum server, register
//! it in a [`ProxyRegistry`], forward requests through a [`Forwarder`], and
//! assert status/body round-trip, `Authorization` propagation, and that
//! deregistration stops proxying.

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::Request;
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use http::{StatusCode, header};
use tokio::net::TcpListener;

use toolkit_gateway::{Endpoint, Forwarder, GearName, ProxyRegistry, RouteTemplate};
use toolkit_http::HttpClient;

/// Build an upstream echo handler labeled with `served_by` so a test can assert
/// which of several upstreams actually handled a forwarded request. The handler
/// also reflects the received method, path, `Authorization` header, and body.
fn echo_handler(served_by: &'static str) -> impl Fn(Request) -> BoxFuture + Clone {
    move |req: Request| Box::pin(echo(served_by, req)) as BoxFuture
}

/// Boxed future produced by [`echo_handler`] (a labeled echo response).
type BoxFuture = std::pin::Pin<Box<dyn std::future::Future<Output = Response> + Send>>;

/// Upstream echo: reflects the received method, path, `Authorization` header,
/// and body back as JSON (plus a `served_by` label) so the test can assert what
/// the gear saw and which upstream answered.
async fn echo(served_by: &'static str, req: Request) -> Response {
    let method = req.method().to_string();
    let path = req.uri().path().to_owned();
    let authorization = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    let body = axum::body::to_bytes(req.into_body(), 1 << 20)
        .await
        .expect("read upstream body");
    let payload = serde_json::json!({
        "served_by": served_by,
        "method": method,
        "path": path,
        "authorization": authorization,
        "body": String::from_utf8_lossy(&body),
    });
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        serde_json::to_vec(&payload).expect("serialize echo"),
    )
        .into_response()
}

/// Start an upstream echo server on an ephemeral port; returns its `Endpoint`.
/// Responses carry the `served_by` label so callers can distinguish upstreams.
async fn start_upstream(served_by: &'static str) -> Endpoint {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind upstream");
    let addr = listener.local_addr().expect("local addr");
    let app = Router::new().fallback(any(echo_handler(served_by)));
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve upstream");
    });
    // Give the spawned server a moment to start accepting connections.
    tokio::time::sleep(Duration::from_millis(50)).await;
    Endpoint::parse(&format!("http://{addr}")).expect("endpoint")
}

/// Read a forwarded response's status and JSON body.
async fn read_json(response: Response) -> (StatusCode, serde_json::Value) {
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .expect("read forwarded body");
    let value = if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
    };
    (status, value)
}

fn templates() -> Vec<RouteTemplate> {
    vec![
        RouteTemplate::new(http::Method::GET, "/calc/v1/items/{id}", true),
        RouteTemplate::new(http::Method::POST, "/calc/v1/items/{id}", true),
    ]
}

#[tokio::test]
async fn proxies_get_and_post_and_propagates_bearer() {
    let endpoint = start_upstream("calculator").await;
    let registry = Arc::new(ProxyRegistry::new());
    registry.register(
        GearName::from("calculator"),
        "calc-1",
        endpoint,
        templates(),
    );
    let forwarder = Forwarder::new(Arc::clone(&registry), HttpClient::new().expect("client"));

    // GET with a bearer token: expect 200 and the upstream to have seen the
    // path and Authorization header verbatim.
    let get_req = Request::builder()
        .method("GET")
        .uri("/calc/v1/items/42?verbose=1")
        .header(header::AUTHORIZATION, "Bearer tenant-jwt")
        .body(Body::empty())
        .expect("get request");
    let (status, json) = read_json(forwarder.forward(get_req).await).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["method"], "GET");
    assert_eq!(json["path"], "/calc/v1/items/42");
    assert_eq!(json["authorization"], "Bearer tenant-jwt");

    // POST with a body: expect the body to round-trip to the upstream.
    let post_req = Request::builder()
        .method("POST")
        .uri("/calc/v1/items/7")
        .header(header::AUTHORIZATION, "Bearer tenant-jwt")
        .header(header::CONTENT_TYPE, "text/plain")
        .body(Body::from("payload-body"))
        .expect("post request");
    let (status, json) = read_json(forwarder.forward(post_req).await).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["method"], "POST");
    assert_eq!(json["body"], "payload-body");
}

#[tokio::test]
async fn routes_to_correct_upstream_among_several_gears() {
    // Two distinct gears, each backed by its own upstream on a different path.
    // Forwarding to each path must reach the matching upstream (proven by the
    // `served_by` label), catching any router/forwarder misrouting at the HTTP
    // layer rather than only in the non-I/O registry unit tests.
    let calc_endpoint = start_upstream("calculator").await;
    let billing_endpoint = start_upstream("billing").await;

    let registry = Arc::new(ProxyRegistry::new());
    registry.register(
        GearName::from("calculator"),
        "calc-1",
        calc_endpoint,
        vec![RouteTemplate::new(
            http::Method::GET,
            "/calc/v1/items/{id}",
            false,
        )],
    );
    registry.register(
        GearName::from("billing"),
        "billing-1",
        billing_endpoint,
        vec![RouteTemplate::new(
            http::Method::GET,
            "/billing/v1/invoices/{id}",
            false,
        )],
    );
    let forwarder = Forwarder::new(Arc::clone(&registry), HttpClient::new().expect("client"));

    let calc_req = Request::builder()
        .uri("/calc/v1/items/42")
        .body(Body::empty())
        .expect("calc request");
    let (status, json) = read_json(forwarder.forward(calc_req).await).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["served_by"], "calculator");
    assert_eq!(json["path"], "/calc/v1/items/42");

    let billing_req = Request::builder()
        .uri("/billing/v1/invoices/7")
        .body(Body::empty())
        .expect("billing request");
    let (status, json) = read_json(forwarder.forward(billing_req).await).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["served_by"], "billing");
    assert_eq!(json["path"], "/billing/v1/invoices/7");
}

#[tokio::test]
async fn deregister_stops_proxying() {
    let endpoint = start_upstream("calculator").await;
    let registry = Arc::new(ProxyRegistry::new());
    let gear = GearName::from("calculator");
    registry.register(gear.clone(), "calc-1", endpoint, templates());
    let forwarder = Forwarder::new(Arc::clone(&registry), HttpClient::new().expect("client"));

    let ok_req = Request::builder()
        .uri("/calc/v1/items/1")
        .body(Body::empty())
        .expect("request");
    let (status, _) = read_json(forwarder.forward(ok_req).await).await;
    assert_eq!(status, StatusCode::OK);

    assert!(registry.deregister(&gear, "calc-1"));

    let gone_req = Request::builder()
        .uri("/calc/v1/items/1")
        .body(Body::empty())
        .expect("request");
    let response = forwarder.forward(gone_req).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("application/problem+json"),
    );
}
