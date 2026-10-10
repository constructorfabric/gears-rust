//! Router-level tests: the `SecurityContext` the gateway injects in production
//! is injected with an `Extension` layer.

use std::sync::Arc;

use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode, header};
use axum::{Extension, Router};
use serde_json::{Value, json};
use toolkit::api::OpenApiRegistryImpl;
use toolkit_security::SecurityContext;
use tower::ServiceExt;
use uuid::Uuid;

use crate::api::rest::routes::register_routes;
use crate::test_support::{DenyResolver, IntakeFixture, chat_record, inmem_db};

const PATH: &str = "/construct/v1/records";

async fn router_with(fixture: &IntakeFixture) -> Router {
    let db = inmem_db().await;
    let intake = Arc::new(fixture.build(&db));
    let ctx = SecurityContext::builder()
        .subject_id(Uuid::new_v4())
        .subject_tenant_id(Uuid::new_v4())
        .build()
        .unwrap();
    register_routes(Router::new(), &OpenApiRegistryImpl::new(), intake).layer(Extension(ctx))
}

struct Answer {
    status: StatusCode,
    content_type: Option<String>,
    body: String,
}

impl Answer {
    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or(Value::Null)
    }

    fn is_problem(&self) -> bool {
        self.content_type
            .as_deref()
            .is_some_and(|v| v.starts_with("application/problem+json"))
    }
}

async fn send(router: Router, uri: &str, body: &str, content_type: Option<&str>) -> Answer {
    let mut builder = Request::builder().method(Method::POST).uri(uri);
    if let Some(content_type) = content_type {
        builder = builder.header(header::CONTENT_TYPE, content_type);
    }
    let response = router
        .oneshot(builder.body(Body::from(body.to_owned())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    Answer {
        status,
        content_type,
        body: String::from_utf8_lossy(&bytes).into_owned(),
    }
}

async fn post(router: Router, tenant: Uuid, record: &Value) -> Answer {
    send(
        router,
        &format!("{PATH}?tenant={tenant}"),
        &record.to_string(),
        Some("application/json"),
    )
    .await
}

#[tokio::test]
async fn a_received_record_gets_202() {
    let router = router_with(&IntakeFixture::default()).await;

    let answer = post(
        router,
        Uuid::new_v4(),
        &chat_record(Uuid::new_v4(), "p", "v"),
    )
    .await;

    assert_eq!(answer.status, StatusCode::ACCEPTED, "body: {}", answer.body);
    assert_eq!(answer.json(), json!({ "outcome": "received" }));
}

#[tokio::test]
async fn a_repeat_gets_200() {
    let router = router_with(&IntakeFixture::default()).await;
    let (tenant, record) = (Uuid::new_v4(), chat_record(Uuid::new_v4(), "p", "v"));
    let first = post(router.clone(), tenant, &record).await;
    assert_eq!(first.status, StatusCode::ACCEPTED, "body: {}", first.body);

    let again = post(router, tenant, &record).await;

    assert_eq!(again.status, StatusCode::OK, "body: {}", again.body);
    assert_eq!(again.json(), json!({ "outcome": "repeat" }));
}

#[tokio::test]
async fn a_refused_record_gets_422_naming_type_place_and_rule() {
    let router = router_with(&IntakeFixture::default()).await;
    let mut record = chat_record(Uuid::new_v4(), "p", "v");
    record["payload"]["role"] = json!("system");

    let answer = post(router, Uuid::new_v4(), &record).await;

    assert_eq!(
        answer.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "body: {}",
        answer.body
    );
    assert!(
        answer.is_problem(),
        "content type: {:?}",
        answer.content_type
    );
    let body = answer.json();
    let violation = &body["context"]["field_violations"][0];
    assert_eq!(violation["field"], "/payload/role", "body: {body}");
    assert_eq!(violation["description"], "enum");
    assert_eq!(violation["reason"], "SCHEMA_VIOLATION");
    assert!(
        answer.body.contains(construct_sdk::gts::CHAT_MESSAGE_TYPE),
        "the type is named: {body}"
    );
}

#[tokio::test]
async fn a_record_with_an_id_gets_422() {
    let router = router_with(&IntakeFixture::default()).await;
    let mut record = chat_record(Uuid::new_v4(), "p", "v");
    record["id"] =
        json!("gts.cf.connectors.core.record.v1~cf.construct.chat.message.v1~a.b.c.d.v1");

    let answer = post(router, Uuid::new_v4(), &record).await;

    assert_eq!(
        answer.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "body: {}",
        answer.body
    );
    assert_eq!(
        answer.json()["context"]["field_violations"][0]["reason"],
        "ID_IN_PUSH"
    );
}

#[tokio::test]
async fn a_connector_without_permission_gets_403() {
    let fixture = IntakeFixture {
        resolver: Arc::new(DenyResolver),
        ..IntakeFixture::default()
    };
    let router = router_with(&fixture).await;

    let answer = post(
        router,
        Uuid::new_v4(),
        &chat_record(Uuid::new_v4(), "p", "v"),
    )
    .await;

    assert_eq!(
        answer.status,
        StatusCode::FORBIDDEN,
        "body: {}",
        answer.body
    );
    assert!(answer.is_problem());
}

#[tokio::test]
async fn a_request_without_a_tenant_gets_a_problem_and_takes_nothing() {
    let fixture = IntakeFixture::default();
    let router = router_with(&fixture).await;
    let record = chat_record(Uuid::new_v4(), "p", "v").to_string();

    let missing = send(router.clone(), PATH, &record, Some("application/json")).await;
    let malformed = send(
        router,
        &format!("{PATH}?tenant=not-a-uuid"),
        &record,
        Some("application/json"),
    )
    .await;

    for answer in [missing, malformed] {
        assert_eq!(
            answer.status,
            StatusCode::BAD_REQUEST,
            "body: {}",
            answer.body
        );
        assert!(
            answer.is_problem(),
            "content type: {:?}",
            answer.content_type
        );
    }
    assert!(fixture.received().is_empty());
}

#[tokio::test]
async fn a_body_that_is_not_json_gets_a_problem() {
    let router = router_with(&IntakeFixture::default()).await;
    let uri = format!("{PATH}?tenant={}", Uuid::new_v4());

    let malformed = send(router.clone(), &uri, "{not json", Some("application/json")).await;
    let untyped = send(router, &uri, "{}", None).await;

    assert_eq!(
        malformed.status,
        StatusCode::BAD_REQUEST,
        "body: {}",
        malformed.body
    );
    assert!(malformed.is_problem());
    assert_eq!(
        untyped.status,
        StatusCode::UNSUPPORTED_MEDIA_TYPE,
        "body: {}",
        untyped.body
    );
    assert!(untyped.is_problem());
}

#[tokio::test]
async fn without_processing_a_record_gets_503_and_can_be_sent_again() {
    let fixture = IntakeFixture {
        no_processing: true,
        ..IntakeFixture::default()
    };
    let router = router_with(&fixture).await;
    let (tenant, record) = (Uuid::new_v4(), chat_record(Uuid::new_v4(), "p", "v"));

    let first = post(router.clone(), tenant, &record).await;
    let again = post(router, tenant, &record).await;

    for answer in [first, again] {
        assert_eq!(
            answer.status,
            StatusCode::SERVICE_UNAVAILABLE,
            "body: {}",
            answer.body
        );
        assert!(answer.is_problem());
    }
}
