#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode};
use github_mirror::api::rest::routes::{ConcreteService, register_routes};
use github_mirror::domain::sync::SyncPoolRunner;
use tokio_util::sync::CancellationToken;
use toolkit::api::OpenApiRegistryImpl;
use toolkit_security::SecurityContext;
use tower::ServiceExt;
use uuid::Uuid;

fn router_for(service: Arc<ConcreteService>, ctx: SecurityContext) -> Router {
    let openapi = OpenApiRegistryImpl::new();
    register_routes(Router::new(), &openapi, service).layer(axum::Extension(ctx))
}

async fn send(router: Router, method: Method, uri: &str) -> axum::http::Response<Body> {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .body(Body::empty())
        .unwrap();
    router.oneshot(request).await.unwrap()
}

async fn body_json(response: axum::http::Response<Body>) -> serde_json::Value {
    let bytes = to_bytes(response.into_body(), 1_000_000).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn the_pool_runs_every_queued_sync_and_stops_when_cancelled() {
    let service = common::service_with_github(
        common::inmem_db().await,
        "https://api.github.com",
        Arc::new(common::FakeGithub {
            result: Some(common::fetched_repository()),
        }),
    );
    let jobs = service
        .take_sync_receiver()
        .await
        .expect("the job receiver must still be available");
    let cancel = CancellationToken::new();
    let pool = tokio::spawn(
        SyncPoolRunner::new(Arc::clone(&service), jobs, 1, cancel.clone()).run(),
    );

    let mut sessions = Vec::new();
    for _ in 0..3 {
        let router = router_for(Arc::clone(&service), common::caller_in(Uuid::new_v4()));
        let response = send(
            router.clone(),
            Method::POST,
            "/github-mirror/v1/repos/rust-lang/rust/sync",
        )
        .await;
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let id = body_json(response).await["session_id"]
            .as_str()
            .expect("session_id")
            .to_owned();
        sessions.push((router, id));
    }

    for (router, id) in &sessions {
        let mut status = serde_json::Value::Null;
        for _ in 0..200 {
            let uri = format!("/github-mirror/v1/sessions/{id}");
            status = body_json(send(router.clone(), Method::GET, &uri).await).await["status"].clone();
            if status == "complete" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert_eq!(status, "complete", "session {id} must be run by the pool");
    }

    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(5), pool)
        .await
        .expect("the pool must stop once cancelled")
        .expect("the pool task must not panic");
}
