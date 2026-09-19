#![allow(clippy::expect_used)]
//! The six operator endpoints over the shared service: public shapes only,
//! canonical statuses and tokens, and 204 on a repeated delete.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use axum::{Extension, Router};
use serde_json::{Value, json};
use toolkit::api::openapi_registry::OpenApiRegistryImpl;
use tower::ServiceExt as _;

use super::super::routes::{PATH_PREFIX, register_routes};
use crate::domain::Service;
use crate::test_support::{METRIC_TOKENS, PermitUnconstrainedPdp, bound_service, ctx};

fn app(service: Arc<Service>) -> Router {
    register_routes(Router::new(), &OpenApiRegistryImpl::new(), service).layer(Extension(ctx()))
}

async fn operator_app() -> Router {
    app(bound_service(Arc::new(PermitUnconstrainedPdp)).await)
}

async fn send(
    app: &Router,
    method: Method,
    path: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(format!("{PATH_PREFIX}{path}"));
    let body = match body {
        Some(json) => {
            request = request.header(header::CONTENT_TYPE, "application/json");
            Body::from(json.to_string())
        }
        None => Body::empty(),
    };
    let response = app
        .clone()
        .oneshot(request.body(body).expect("request"))
        .await
        .expect("response");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()))
    };
    (status, value)
}

fn metric_policy() -> Value {
    json!({
        "scope": { "kind": "metric", "metric": METRIC_TOKENS },
        "engine_id": "most-restrictive-wins",
        "engine_config": {},
        "comment": "first"
    })
}

async fn created(app: &Router, body: Value) -> String {
    let (status, body) = send(app, Method::POST, "/policies", Some(body)).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    body["policy_id"].as_str().expect("policy_id").to_owned()
}

#[tokio::test]
async fn create_read_update_rollback_and_history_round_trip() {
    let app = operator_app().await;
    let id = created(&app, metric_policy()).await;

    let (status, body) = send(&app, Method::GET, &format!("/policies/{id}"), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["version"], json!(1));
    assert_eq!(body["scope"]["kind"], json!("metric"));
    assert_eq!(body["created_by"], json!(ctx().subject_id().to_string()));
    assert!(
        body.get("schema_snapshot").is_none(),
        "server-owned snapshots do not leave the service: {body}"
    );

    let (status, body) = send(
        &app,
        Method::PATCH,
        &format!("/policies/{id}"),
        Some(json!({ "if_match_version": 1, "timeout_ms": 2, "comment": "tighter" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["version"], json!(2));
    assert_eq!(body["timeout_ms"], json!(2));

    let (status, body) = send(
        &app,
        Method::PATCH,
        &format!("/policies/{id}"),
        Some(json!({ "if_match_version": 1 })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(body.to_string().contains("VERSION_CONFLICT"), "{body}");

    let (status, body) = send(
        &app,
        Method::POST,
        &format!("/policies/{id}/rollback"),
        Some(json!({ "target_version": 1, "comment": "undo" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["version"], json!(1));
    assert_eq!(body["state"], json!("active"));

    let (status, body) = send(
        &app,
        Method::POST,
        &format!("/policies/{id}/rollback"),
        Some(json!({ "target_version": 2 })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body.to_string().contains("VERSION_ROLLED_BACK"), "{body}");

    let (status, body) = send(
        &app,
        Method::GET,
        &format!("/policies/{id}/versions?limit=1"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["items"].as_array().map(Vec::len), Some(1));
    assert!(body["next_cursor"].is_string(), "{body}");
    let cursor = body["next_cursor"].as_str().expect("cursor").to_owned();
    let (status, body) = send(
        &app,
        Method::GET,
        &format!("/policies/{id}/versions?limit=1&cursor={cursor}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["items"][0]["version"], json!(2));
    assert_eq!(body["items"][0]["state"], json!("rolled_back"));

    let (status, body) = send(
        &app,
        Method::GET,
        &format!("/policies/{id}?version=9"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body.to_string().contains("UNKNOWN_POLICY_VERSION"),
        "{body}"
    );
}

#[tokio::test]
async fn server_owned_fields_and_unknown_engines_are_rejected_at_the_edge() {
    let app = operator_app().await;
    for spoofed in ["created_by", "schema_snapshot", "compiled_artifact"] {
        let mut body = metric_policy();
        body[spoofed] = json!("caller controlled");
        let (status, answer) = send(&app, Method::POST, "/policies", Some(body)).await;
        // The JSON extractor refuses an unknown field before the handler runs.
        assert_eq!(
            status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{spoofed}: {answer}"
        );
    }

    let mut body = metric_policy();
    body["engine_id"] = json!("starlark");
    let (status, answer) = send(&app, Method::POST, "/policies", Some(body)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{answer}");
    let rendered = answer.to_string();
    assert!(
        rendered.contains("UNKNOWN_ENGINE") && rendered.contains("most-restrictive-wins"),
        "names the registered engines: {rendered}"
    );

    let mut body = metric_policy();
    body["engine_config"] = json!({ "x": 1 });
    let (status, answer) = send(&app, Method::POST, "/policies", Some(body)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{answer}");
    assert!(
        answer.to_string().contains("INVALID_ENGINE_CONFIG"),
        "{answer}"
    );
}

#[tokio::test]
async fn delete_is_idempotent_protects_global_and_404s_only_for_a_never_created_id() {
    let app = operator_app().await;
    let id = created(&app, metric_policy()).await;
    let (status, _) = send(
        &app,
        Method::DELETE,
        &format!("/policies/{id}?comment=retire"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = send(&app, Method::DELETE, &format!("/policies/{id}"), None).await;
    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "a repeated delete is a no-op"
    );
    let (status, body) = send(&app, Method::GET, &format!("/policies/{id}"), None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body.to_string().contains("POLICY_DELETED"), "{body}");

    // The scope is free again: a new policy gets a new id and history.
    let replacement = created(&app, metric_policy()).await;
    assert_ne!(replacement, id);

    let (status, body) = send(&app, Method::DELETE, "/policies/never-created", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");

    created(
        &app,
        json!({
            "scope": { "kind": "global" },
            "engine_id": "most-restrictive-wins",
            "engine_config": {}
        }),
    )
    .await;
    let (status, body) = send(&app, Method::DELETE, "/policies/global", None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body.to_string()
            .contains("CANNOT_DELETE_SEEDED_GLOBAL_POLICY"),
        "{body}"
    );
    let (status, body) = send(&app, Method::GET, "/policies/global", None).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the global policy stays active: {body}"
    );
}
