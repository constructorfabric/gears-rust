//! Router-level tests: the `SecurityContext` the gateway injects in production
//! is injected with an `Extension` layer.

use std::sync::Arc;

use async_trait::async_trait;
use authz_resolver_sdk::{
    AuthZResolverApi, PolicyEnforcer,
    constraints::{Constraint, InPredicate, Predicate},
    models::{EvaluationRequest, EvaluationResponse, EvaluationResponseContext},
};
use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode, header};
use axum::{Extension, Router};
use toolkit::api::OpenApiRegistryImpl;
use toolkit::api::canonical_prelude::CanonicalError;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::{ConnectOpts, DBProvider, Db, connect_db};
use toolkit_security::{PlatformSecurityContext, SecurityContext, pep_properties};
use tower::ServiceExt;
use uuid::Uuid;

use crate::api::rest::routes::register_routes;
use crate::api::rest::types::ConcreteService;
use crate::config::ConstructConfig;
use crate::domain::service::{Service, ServiceConfig};
use crate::infra::storage::migrations::Migrator;
use crate::infra::storage::sea_orm_repo::SeaOrmNoteRepository;

const PATH: &str = "/construct/v1/foundation-notes";

/// Allows every request, constrained to the subject's tenant.
struct AllowResolver;

#[async_trait]
impl AuthZResolverApi for AllowResolver {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        let root_id = request
            .context
            .tenant_context
            .as_ref()
            .and_then(|tc| tc.root_id)
            .or_else(|| {
                request
                    .subject
                    .properties
                    .get("tenant_id")
                    .and_then(|v| v.as_str())
                    .and_then(|s| Uuid::parse_str(s).ok())
            })
            .ok_or_else(|| {
                CanonicalError::internal("tenant context is required".to_owned()).create()
            })?;

        Ok(EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext {
                constraints: vec![Constraint {
                    predicates: vec![Predicate::In(InPredicate::new(
                        pep_properties::OWNER_TENANT_ID,
                        [root_id],
                    ))],
                }],
                ..Default::default()
            },
        })
    }
}

/// Denies every request: a subject without permission.
struct DenyResolver;

#[async_trait]
impl AuthZResolverApi for DenyResolver {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        _request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        Ok(EvaluationResponse {
            decision: false,
            context: EvaluationResponseContext::default(),
        })
    }
}

async fn inmem_db() -> Db {
    use sea_orm_migration::MigratorTrait;

    let opts = ConnectOpts {
        max_conns: Some(1),
        min_conns: Some(1),
        ..Default::default()
    };
    let db = connect_db("sqlite::memory:", opts)
        .await
        .expect("connect in-memory database");
    run_migrations_for_testing(&db, Migrator::migrations())
        .await
        .expect("run migrations");
    db
}

async fn router_with(resolver: Arc<dyn AuthZResolverApi>, tenant_id: Uuid) -> Router {
    let db = inmem_db().await;
    let config = ServiceConfig::try_from(&ConstructConfig::default()).expect("valid config");
    let service: Arc<ConcreteService> = Arc::new(Service::new(
        Arc::new(DBProvider::new(db)),
        Arc::new(SeaOrmNoteRepository::new()),
        PolicyEnforcer::new(resolver),
        config,
    ));
    let ctx = SecurityContext::builder()
        .subject_id(Uuid::new_v4())
        .subject_tenant_id(tenant_id)
        .build()
        .unwrap();
    register_routes(Router::new(), &OpenApiRegistryImpl::new(), service).layer(Extension(ctx))
}

async fn post(router: Router, body: &str) -> (StatusCode, String) {
    let request = Request::builder()
        .method(Method::POST)
        .uri(PATH)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_owned()))
        .unwrap();
    let response = router.oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

#[tokio::test]
async fn permitted_subject_gets_201_with_the_created_note() {
    let tenant_id = Uuid::new_v4();
    let router = router_with(Arc::new(AllowResolver), tenant_id).await;

    let (status, body) = post(router, r#"{"text":"hello"}"#).await;

    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    assert!(body.contains("\"id\""), "body: {body}");
    assert!(body.contains(&tenant_id.to_string()), "body: {body}");
    assert!(body.contains("\"text\":\"hello\""), "body: {body}");
}

#[tokio::test]
async fn blank_text_gets_400() {
    let router = router_with(Arc::new(AllowResolver), Uuid::new_v4()).await;

    let (status, body) = post(router, r#"{"text":"   "}"#).await;

    assert_eq!(status, StatusCode::BAD_REQUEST, "body: {body}");
}

#[tokio::test]
async fn text_with_nul_character_gets_400() {
    let router = router_with(Arc::new(AllowResolver), Uuid::new_v4()).await;

    let (status, body) = post(router, r#"{"text":"a\u0000b"}"#).await;

    assert_eq!(status, StatusCode::BAD_REQUEST, "body: {body}");
}

#[tokio::test]
async fn malformed_json_gets_400() {
    let router = router_with(Arc::new(AllowResolver), Uuid::new_v4()).await;

    let (status, body) = post(router, r#"{"text":"#).await;

    assert_eq!(status, StatusCode::BAD_REQUEST, "body: {body}");
}

#[tokio::test]
async fn empty_body_gets_400() {
    let router = router_with(Arc::new(AllowResolver), Uuid::new_v4()).await;

    let (status, body) = post(router, "").await;

    assert_eq!(status, StatusCode::BAD_REQUEST, "body: {body}");
}

#[tokio::test]
async fn denied_subject_gets_403() {
    let router = router_with(Arc::new(DenyResolver), Uuid::new_v4()).await;

    let (status, body) = post(router, r#"{"text":"hello"}"#).await;

    assert_eq!(status, StatusCode::FORBIDDEN, "body: {body}");
}
