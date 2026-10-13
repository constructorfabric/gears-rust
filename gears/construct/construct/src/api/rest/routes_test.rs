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

use crate::api::rest::error::FoundationNoteError;
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
        // The route creates a note, so the policy service must be asked for that.
        assert_eq!(request.resource.resource_type, "construct.foundation_note");
        assert_eq!(request.action.name, "create");
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

/// The policy service answers with an error instead of a decision.
struct ErroringResolver(fn() -> CanonicalError);

#[async_trait]
impl AuthZResolverApi for ErroringResolver {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        _request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        Err((self.0)())
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

/// Sends `body` with the given content type (none when `None`) and returns
/// the status, the response content type and the response body.
async fn post_raw(
    router: Router,
    body: &str,
    content_type: Option<&str>,
) -> (StatusCode, Option<String>, String) {
    let mut builder = Request::builder().method(Method::POST).uri(PATH);
    if let Some(content_type) = content_type {
        builder = builder.header(header::CONTENT_TYPE, content_type);
    }
    let request = builder.body(Body::from(body.to_owned())).unwrap();
    let response = router.oneshot(request).await.unwrap();
    let status = response.status();
    let response_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (
        status,
        response_type,
        String::from_utf8_lossy(&bytes).into_owned(),
    )
}

async fn post(router: Router, body: &str) -> (StatusCode, String) {
    let (status, _, body) = post_raw(router, body, Some("application/json")).await;
    (status, body)
}

fn is_problem_json(content_type: Option<&str>) -> bool {
    content_type.is_some_and(|v| v.starts_with("application/problem+json"))
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
async fn empty_body_gets_400() {
    let router = router_with(Arc::new(AllowResolver), Uuid::new_v4()).await;

    let (status, body) = post(router, "").await;

    assert_eq!(status, StatusCode::BAD_REQUEST, "body: {body}");
}

#[tokio::test]
async fn malformed_json_gets_a_problem_body() {
    let router = router_with(Arc::new(AllowResolver), Uuid::new_v4()).await;

    let (status, content_type, body) =
        post_raw(router, r#"{"text":"#, Some("application/json")).await;

    assert_eq!(status, StatusCode::BAD_REQUEST, "body: {body}");
    assert!(
        is_problem_json(content_type.as_deref()),
        "content type: {content_type:?}"
    );
}

#[tokio::test]
async fn json_of_the_wrong_shape_gets_a_problem_body() {
    let router = router_with(Arc::new(AllowResolver), Uuid::new_v4()).await;

    let (status, content_type, body) =
        post_raw(router, r#"{"text":5}"#, Some("application/json")).await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "body: {body}");
    assert!(
        is_problem_json(content_type.as_deref()),
        "content type: {content_type:?}"
    );
}

#[tokio::test]
async fn unknown_field_in_the_body_gets_a_problem_body() {
    let router = router_with(Arc::new(AllowResolver), Uuid::new_v4()).await;
    let body = format!(r#"{{"text":"hello","tenant_id":"{}"}}"#, Uuid::new_v4());

    let (status, content_type, body) = post_raw(router, &body, Some("application/json")).await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "body: {body}");
    assert!(is_problem_json(content_type.as_deref()), "{content_type:?}");
}

#[tokio::test]
async fn missing_content_type_gets_a_problem_body() {
    let router = router_with(Arc::new(AllowResolver), Uuid::new_v4()).await;

    let (status, content_type, body) = post_raw(router, r#"{"text":"hello"}"#, None).await;

    assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE, "body: {body}");
    assert!(
        is_problem_json(content_type.as_deref()),
        "content type: {content_type:?}"
    );
}

#[tokio::test]
async fn retryable_policy_failures_get_503_without_internal_detail() {
    let failures: [fn() -> CanonicalError; 3] = [
        || {
            CanonicalError::service_unavailable()
                .with_detail("pdp at 10.0.0.7 is down")
                .create()
        },
        || FoundationNoteError::deadline_exceeded("pdp at 10.0.0.7 is slow").create(),
        || {
            FoundationNoteError::resource_exhausted("pdp at 10.0.0.7 is busy")
                .with_quota_violation("policy", "rate limit")
                .create()
        },
    ];

    for failure in failures {
        let router = router_with(Arc::new(ErroringResolver(failure)), Uuid::new_v4()).await;

        let (status, content_type, body) =
            post_raw(router, r#"{"text":"hello"}"#, Some("application/json")).await;

        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "body: {body}");
        assert!(is_problem_json(content_type.as_deref()), "{content_type:?}");
        assert!(!body.contains("10.0.0.7"), "body: {body}");
    }
}

#[tokio::test]
async fn broken_policy_service_gets_500_without_its_cause() {
    let resolver =
        ErroringResolver(|| CanonicalError::internal("client wiring secret".to_owned()).create());
    let router = router_with(Arc::new(resolver), Uuid::new_v4()).await;

    let (status, content_type, body) =
        post_raw(router, r#"{"text":"hello"}"#, Some("application/json")).await;

    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "body: {body}");
    assert!(is_problem_json(content_type.as_deref()), "{content_type:?}");
    assert!(!body.contains("secret"), "body: {body}");
}

#[tokio::test]
async fn denied_subject_gets_403() {
    let router = router_with(Arc::new(DenyResolver), Uuid::new_v4()).await;

    let (status, body) = post(router, r#"{"text":"hello"}"#).await;

    assert_eq!(status, StatusCode::FORBIDDEN, "body: {body}");
}
