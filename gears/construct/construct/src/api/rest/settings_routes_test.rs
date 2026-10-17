//! Router-level tests for the subject's settings routes. The security context
//! the gateway injects in production is injected with an `Extension` layer.

use std::sync::Arc;

use authz_resolver_sdk::{AuthZResolverApi, PolicyEnforcer};
use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode, header};
use axum::{Extension, Router};
use toolkit::api::OpenApiRegistryImpl;
use toolkit_canonical_errors::Problem;
use toolkit_db::{DBProvider, Db};
use tower::ServiceExt;
use uuid::Uuid;

use crate::api::rest::error::SubjectSettingsError;
use crate::api::rest::routes::register_routes;
use crate::config::ConstructConfig;
use crate::domain::service::{Service, ServiceConfig};
use crate::domain::subject_settings::{SubjectKey, SubjectSettings};
use crate::infra::storage::sea_orm_repo::SeaOrmNoteRepository;
use crate::test_support::{
    AllowResolver, DenyResolver, context_of, inmem_db, seed_subject_settings, settings_service,
};

const DEFAULT_ON: bool = true;
const SETTINGS_RESOURCE_TYPE: &str = "gts.cf.construct.subject.settings.v1~";

fn path(subject_id: Uuid) -> String {
    format!("/construct/v1/subjects/{subject_id}/settings")
}

/// The router as `subject_id` of `tenant_id` sees it.
fn router_for(
    db: &Db,
    resolver: Arc<dyn AuthZResolverApi>,
    subject_id: Uuid,
    tenant_id: Uuid,
) -> Router {
    let config = ServiceConfig::try_from(&ConstructConfig::default()).expect("valid config");
    let notes = Arc::new(Service::new(
        Arc::new(DBProvider::new(db.clone())),
        Arc::new(SeaOrmNoteRepository::new()),
        PolicyEnforcer::new(Arc::clone(&resolver)),
        config,
    ));
    let settings = Arc::new(settings_service(db, resolver, DEFAULT_ON));
    register_routes(Router::new(), &OpenApiRegistryImpl::new(), notes, settings)
        .layer(Extension(context_of(subject_id, tenant_id)))
}

async fn send(
    router: Router,
    method: Method,
    uri: &str,
    body: Option<&str>,
) -> (StatusCode, String) {
    let mut builder = Request::builder().method(method).uri(uri);
    if body.is_some() {
        builder = builder.header(header::CONTENT_TYPE, "application/json");
    }
    let request = builder
        .body(body.map_or_else(Body::empty, |b| Body::from(b.to_owned())))
        .unwrap();
    let response = router.oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

fn json(body: &str) -> serde_json::Value {
    serde_json::from_str(body).expect("JSON body")
}

/// The problem a refused settings call answers with.
fn refusal() -> serde_json::Value {
    serde_json::to_value(Problem::from(
        SubjectSettingsError::permission_denied()
            .with_reason("ACCESS_DENIED")
            .create(),
    ))
    .expect("problem serializes")
}

#[tokio::test]
async fn owner_without_stored_settings_gets_200_with_the_tenant_default() {
    let db = inmem_db().await;
    let (tenant, subject) = (Uuid::new_v4(), Uuid::new_v4());

    let (status, body) = send(
        router_for(&db, Arc::new(AllowResolver), subject, tenant),
        Method::GET,
        &path(subject),
        None,
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        json(&body),
        serde_json::json!({
            "subject_id": subject.to_string(),
            "personalization_enabled": DEFAULT_ON,
            "erasure_in_progress": false,
        })
    );
}

#[tokio::test]
async fn owner_changes_the_setting_and_reads_it_back() {
    let db = inmem_db().await;
    let (tenant, subject) = (Uuid::new_v4(), Uuid::new_v4());
    let router = || router_for(&db, Arc::new(AllowResolver), subject, tenant);

    let (status, body) = send(
        router(),
        Method::PUT,
        &path(subject),
        Some(r#"{"personalization_enabled":false}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(json(&body)["personalization_enabled"], false);

    let (status, body) = send(router(), Method::GET, &path(subject), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(json(&body)["personalization_enabled"], false);
    assert_eq!(json(&body)["erasure_in_progress"], false);
}

#[tokio::test]
async fn the_erasure_flag_is_shown_and_survives_a_change() {
    let db = inmem_db().await;
    let (tenant, subject) = (Uuid::new_v4(), Uuid::new_v4());
    seed_subject_settings(
        &db,
        SubjectKey {
            tenant_id: tenant,
            subject_id: subject,
        },
        SubjectSettings {
            personalization_enabled: false,
            erasure_in_progress: true,
        },
    )
    .await;

    let (status, body) = send(
        router_for(&db, Arc::new(AllowResolver), subject, tenant),
        Method::PUT,
        &path(subject),
        Some(r#"{"personalization_enabled":true}"#),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(json(&body)["personalization_enabled"], true);
    assert_eq!(json(&body)["erasure_in_progress"], true);
}

#[tokio::test]
async fn another_subject_gets_403_for_both_routes() {
    let db = inmem_db().await;
    let tenant = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let caller = Uuid::new_v4();
    let router = || router_for(&db, Arc::new(AllowResolver), caller, tenant);

    let (get_status, get_body) = send(router(), Method::GET, &path(owner), None).await;
    let (put_status, put_body) = send(
        router(),
        Method::PUT,
        &path(owner),
        Some(r#"{"personalization_enabled":false}"#),
    )
    .await;

    for (status, body) in [(get_status, get_body), (put_status, put_body)] {
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
        let problem = json(&body);
        assert_eq!(problem["context"], refusal()["context"], "{body}");
        assert_eq!(
            problem["context"]["resource_type"], SETTINGS_RESOURCE_TYPE,
            "a settings refusal, not a note refusal: {body}"
        );
        assert_eq!(problem["context"]["reason"], "ACCESS_DENIED", "{body}");
        assert!(
            !body.contains("may not read"),
            "the internal message stays out of the answer: {body}"
        );
    }
}

#[tokio::test]
async fn the_owner_never_sees_its_row_of_another_tenant() {
    let db = inmem_db().await;
    let (own_tenant, other_tenant, subject) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    seed_subject_settings(
        &db,
        SubjectKey {
            tenant_id: other_tenant,
            subject_id: subject,
        },
        SubjectSettings {
            personalization_enabled: !DEFAULT_ON,
            erasure_in_progress: true,
        },
    )
    .await;

    let (status, body) = send(
        router_for(&db, Arc::new(AllowResolver), subject, own_tenant),
        Method::GET,
        &path(subject),
        None,
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(json(&body)["personalization_enabled"], DEFAULT_ON, "{body}");
    assert_eq!(json(&body)["erasure_in_progress"], false, "{body}");
}

#[tokio::test]
async fn both_routes_answer_no_store() {
    let db = inmem_db().await;
    let (tenant, subject) = (Uuid::new_v4(), Uuid::new_v4());

    for (method, body) in [
        (Method::GET, None),
        (Method::PUT, Some(r#"{"personalization_enabled":false}"#)),
    ] {
        let mut builder = Request::builder().method(method).uri(path(subject));
        if body.is_some() {
            builder = builder.header(header::CONTENT_TYPE, "application/json");
        }
        let request = builder
            .body(body.map_or_else(Body::empty, |b| Body::from(b.to_owned())))
            .unwrap();
        let response = router_for(&db, Arc::new(AllowResolver), subject, tenant)
            .oneshot(request)
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|v| v.to_str().ok()),
            Some("no-store")
        );
    }
}

#[tokio::test]
async fn owner_the_platform_denies_gets_403() {
    let db = inmem_db().await;
    let (tenant, subject) = (Uuid::new_v4(), Uuid::new_v4());

    let (status, body) = send(
        router_for(&db, Arc::new(DenyResolver), subject, tenant),
        Method::PUT,
        &path(subject),
        Some(r#"{"personalization_enabled":false}"#),
    )
    .await;

    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
}

#[tokio::test]
async fn a_body_that_tries_to_set_the_erasure_flag_gets_422() {
    let db = inmem_db().await;
    let (tenant, subject) = (Uuid::new_v4(), Uuid::new_v4());

    let (status, body) = send(
        router_for(&db, Arc::new(AllowResolver), subject, tenant),
        Method::PUT,
        &path(subject),
        Some(r#"{"personalization_enabled":true,"erasure_in_progress":false}"#),
    )
    .await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
}

#[tokio::test]
async fn a_body_without_the_setting_gets_422_and_bad_json_gets_400() {
    let db = inmem_db().await;
    let (tenant, subject) = (Uuid::new_v4(), Uuid::new_v4());
    let router = || router_for(&db, Arc::new(AllowResolver), subject, tenant);

    let (missing, _) = send(router(), Method::PUT, &path(subject), Some("{}")).await;
    let (wrong_type, _) = send(
        router(),
        Method::PUT,
        &path(subject),
        Some(r#"{"personalization_enabled":"yes"}"#),
    )
    .await;
    let (broken, _) = send(router(), Method::PUT, &path(subject), Some("{")).await;

    assert_eq!(missing, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(wrong_type, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(broken, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_subject_id_that_is_not_a_uuid_gets_400() {
    let db = inmem_db().await;
    let (tenant, subject) = (Uuid::new_v4(), Uuid::new_v4());

    let (status, body) = send(
        router_for(&db, Arc::new(AllowResolver), subject, tenant),
        Method::GET,
        "/construct/v1/subjects/not-a-uuid/settings",
        None,
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}
