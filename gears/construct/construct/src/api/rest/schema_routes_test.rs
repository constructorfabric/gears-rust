//! Router-level tests for the profile schema routes: the security context the gateway injects in production is
//! injected with an `Extension` layer.

use std::sync::Arc;

use authz_resolver_sdk::{AuthZResolverApi, PolicyEnforcer};
use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode, header};
use axum::{Extension, Router};
use serde_json::{Value, json};
use toolkit::api::OpenApiRegistryImpl;
use toolkit_db::{DBProvider, Db};
use tower::ServiceExt;
use uuid::Uuid;

use crate::api::rest::routes::register_schema_routes;
use crate::domain::schema_service::SchemaService;
use crate::infra::storage::subject_schema_repo::SeaOrmSubjectSchemaRepository;
use crate::test_support::{AllowResolver, DenyResolver, context_of, inmem_db};

fn path(subject_id: Uuid) -> String {
    format!("/construct/v1/subjects/{subject_id}/schema")
}

fn router_for(
    db: &Db,
    resolver: Arc<dyn AuthZResolverApi>,
    subject_id: Uuid,
    tenant_id: Uuid,
) -> Router {
    router_with(
        db,
        resolver,
        subject_id,
        tenant_id,
        Arc::new(crate::test_support::RecordingEffects::default()),
    )
}

/// The router with graph storage's side of a change put in.
fn router_with(
    db: &Db,
    resolver: Arc<dyn AuthZResolverApi>,
    subject_id: Uuid,
    tenant_id: Uuid,
    effects: Arc<crate::test_support::RecordingEffects>,
) -> Router {
    let service = SchemaService::new(
        Arc::new(DBProvider::new(db.clone())),
        Arc::new(SeaOrmSubjectSchemaRepository::new()),
        effects,
        Arc::new(crate::test_support::RecordingSchemaEvents::default()),
        PolicyEnforcer::new(resolver),
    );
    register_schema_routes(
        Router::new(),
        &OpenApiRegistryImpl::new(),
        Arc::new(service),
    )
    .layer(Extension(context_of(subject_id, tenant_id)))
}

async fn send(
    router: Router,
    method: Method,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, header::HeaderMap, String) {
    let mut builder = Request::builder().method(method).uri(uri);
    if body.is_some() {
        builder = builder.header(header::CONTENT_TYPE, "application/json");
    }
    let request = builder
        .body(body.map_or_else(Body::empty, |body| Body::from(body.to_string())))
        .unwrap();
    let response = router.oneshot(request).await.unwrap();
    let (status, headers) = (response.status(), response.headers().clone());
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (
        status,
        headers,
        String::from_utf8_lossy(&bytes).into_owned(),
    )
}

/// The change that adds "pets", written as the wire format the host sends: it pins the format, which
/// `test_support::add_pets` builds through the types.
fn add_pets() -> Value {
    json!({ "changes": [{
        "op": "add_node_kind",
        "kind": {
            "name": "pets",
            "description": "An animal the person keeps.",
            "owner": "root",
            "key": "name",
            "properties": [
                { "name": "name", "required": true, "schema": { "type": "string", "maxLength": 100 } },
                { "name": "species", "schema": { "type": "string", "maxLength": 100 } }
            ]
        }
    }] })
}

fn kinds(body: &str) -> Vec<String> {
    serde_json::from_str::<Value>(body).unwrap()["schema"]["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|kind| kind["name"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn the_owner_reads_the_default_schema_and_the_answer_is_not_stored() {
    let db = inmem_db().await;
    let (tenant, subject) = (Uuid::new_v4(), Uuid::new_v4());

    let (status, headers, body) = send(
        router_for(&db, Arc::new(AllowResolver), subject, tenant),
        Method::GET,
        &path(subject),
        None,
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{body}");
    let parsed: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(parsed["version"], 0);
    assert_eq!(parsed["subject_id"], subject.to_string());
    assert!(kinds(&body).contains(&"project".to_owned()));
    assert_eq!(headers[header::CACHE_CONTROL], "no-store");
}

#[tokio::test]
async fn the_owner_adds_a_kind_and_reads_it_back() {
    let db = inmem_db().await;
    let (tenant, subject) = (Uuid::new_v4(), Uuid::new_v4());

    let (status, _, body) = send(
        router_for(&db, Arc::new(AllowResolver), subject, tenant),
        Method::PATCH,
        &path(subject),
        Some(add_pets()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(serde_json::from_str::<Value>(&body).unwrap()["version"], 1);

    let (_, _, read) = send(
        router_for(&db, Arc::new(AllowResolver), subject, tenant),
        Method::GET,
        &path(subject),
        None,
    )
    .await;
    assert!(kinds(&read).contains(&"pets".to_owned()));
}

#[tokio::test]
async fn another_persons_schema_is_refused_with_403_for_the_read_and_the_change() {
    let db = inmem_db().await;
    let (tenant, caller, other) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());

    for (method, body) in [(Method::GET, None), (Method::PATCH, Some(add_pets()))] {
        let (status, _, answer) = send(
            router_for(&db, Arc::new(AllowResolver), caller, tenant),
            method,
            &path(other),
            body,
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{answer}");
    }
    let (_, _, read) = send(
        router_for(&db, Arc::new(AllowResolver), other, tenant),
        Method::GET,
        &path(other),
        None,
    )
    .await;
    assert!(
        !kinds(&read).contains(&"pets".to_owned()),
        "nothing changed"
    );
}

#[tokio::test]
async fn a_denial_of_the_policy_is_403_too() {
    let db = inmem_db().await;
    let (tenant, subject) = (Uuid::new_v4(), Uuid::new_v4());

    let (status, _, body) = send(
        router_for(&db, Arc::new(DenyResolver), subject, tenant),
        Method::GET,
        &path(subject),
        None,
    )
    .await;

    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
}

#[tokio::test]
async fn a_change_that_breaks_a_rule_is_422_with_the_reason_and_changes_nothing() {
    let db = inmem_db().await;
    let (tenant, subject) = (Uuid::new_v4(), Uuid::new_v4());
    let bad = json!({ "changes": [{
        "op": "add_node_kind",
        "kind": {
            "name": "project",
            "owner": "root",
            "key": "name",
            "properties": [{ "name": "name", "schema": { "type": "string" } }]
        }
    }] });

    let (status, _, body) = send(
        router_for(&db, Arc::new(AllowResolver), subject, tenant),
        Method::PATCH,
        &path(subject),
        Some(bad),
    )
    .await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(body.contains("already in use"), "{body}");
    let (_, _, read) = send(
        router_for(&db, Arc::new(AllowResolver), subject, tenant),
        Method::GET,
        &path(subject),
        None,
    )
    .await;
    assert_eq!(serde_json::from_str::<Value>(&read).unwrap()["version"], 0);
}

#[tokio::test]
async fn a_change_with_an_unknown_op_is_refused_before_the_service() {
    let db = inmem_db().await;
    let (tenant, subject) = (Uuid::new_v4(), Uuid::new_v4());

    let (status, _, body) = send(
        router_for(&db, Arc::new(AllowResolver), subject, tenant),
        Method::PATCH,
        &path(subject),
        Some(json!({ "changes": [{ "op": "rename_everything" }] })),
    )
    .await;

    assert!(status.is_client_error(), "{status} {body}");
}

#[tokio::test]
async fn a_change_stored_but_not_swept_is_503_and_tells_the_owner_what_finishes_it() {
    let db = inmem_db().await;
    let (tenant, subject) = (Uuid::new_v4(), Uuid::new_v4());
    let effects = Arc::new(crate::test_support::RecordingEffects::default());
    effects
        .fail_forget
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let router = router_with(&db, Arc::new(AllowResolver), subject, tenant, effects);

    let (status, _, body) = send(router, Method::PATCH, &path(subject), Some(add_pets())).await;

    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert!(body.contains("empty change"), "{body}");
    assert!(
        !body.contains("graph storage is down"),
        "the cause stays in the log: {body}"
    );
}
