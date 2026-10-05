//! Service tests on an in-memory `SQLite` database: `DBRunner` is sealed and
//! cannot be mocked, so every test runs real database operations.

use std::sync::Arc;

use async_trait::async_trait;
use authz_resolver_sdk::{
    AuthZResolverApi, PolicyEnforcer,
    constraints::{Constraint, InPredicate, Predicate},
    models::{EvaluationRequest, EvaluationResponse, EvaluationResponseContext},
};
use construct_sdk::ConstructClientV1;
use construct_sdk::models::NewFoundationNote;
use toolkit::api::canonical_prelude::CanonicalError;
use toolkit_canonical_errors::Problem;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::{ConnectOpts, DBProvider, Db, connect_db};
use toolkit_security::{PlatformSecurityContext, SecurityContext, pep_properties};
use uuid::Uuid;

use crate::config::ConstructConfig;
use crate::domain::error::DomainError;
use crate::domain::local_client::LocalClient;
use crate::domain::service::{Service, ServiceConfig};
use crate::infra::storage::migrations::Migrator;
use crate::infra::storage::sea_orm_repo::SeaOrmNoteRepository;

type ConcreteService = Service<SeaOrmNoteRepository>;

/// Allows every request and constrains the answer to the subject's tenant and,
/// when the request names one, the resource id (like a real PDP).
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

        let mut predicates = vec![Predicate::In(InPredicate::new(
            pep_properties::OWNER_TENANT_ID,
            [root_id],
        ))];
        if let Some(resource_id) = request.resource.id {
            predicates.push(Predicate::In(InPredicate::new(
                pep_properties::RESOURCE_ID,
                [resource_id],
            )));
        }

        Ok(EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext {
                constraints: vec![Constraint { predicates }],
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

/// The policy service fails: it answers with an error instead of a decision.
struct FailingResolver;

#[async_trait]
impl AuthZResolverApi for FailingResolver {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        _request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        Err(CanonicalError::internal("policy service is down".to_owned()).create())
    }
}

/// Allows the request but returns no constraints, so the scope cannot be built.
struct UnconstrainedResolver;

#[async_trait]
impl AuthZResolverApi for UnconstrainedResolver {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        _request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        Ok(EvaluationResponse {
            decision: true,
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

fn context_in(tenant_id: Uuid) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::new_v4())
        .subject_tenant_id(tenant_id)
        .build()
        .unwrap()
}

fn service_with(db: &Db, resolver: Arc<dyn AuthZResolverApi>) -> ConcreteService {
    Service::new(
        Arc::new(DBProvider::new(db.clone())),
        Arc::new(SeaOrmNoteRepository::new()),
        PolicyEnforcer::new(resolver),
        test_service_config(),
    )
}

fn note(text: &str) -> NewFoundationNote {
    NewFoundationNote::new(text)
}

fn test_service_config() -> ServiceConfig {
    ServiceConfig::try_from(&ConstructConfig::default()).expect("default config is valid")
}

#[tokio::test]
async fn permitted_subject_creates_and_reads_a_note() {
    let db = inmem_db().await;
    let svc = service_with(&db, Arc::new(AllowResolver));
    let tenant = Uuid::new_v4();
    let ctx = context_in(tenant);

    let created = svc.create_note(&ctx, note("hello")).await.unwrap();
    assert_eq!(created.tenant_id, tenant);
    assert_eq!(created.text, "hello");

    let read = svc.get_note(&ctx, created.id).await.unwrap();
    assert_eq!(read, created);
}

#[tokio::test]
async fn subject_without_permission_is_denied() {
    let db = inmem_db().await;
    let svc = service_with(&db, Arc::new(DenyResolver));
    let ctx = context_in(Uuid::new_v4());

    let created = svc.create_note(&ctx, note("hello")).await;
    assert!(matches!(created, Err(DomainError::Forbidden(_))));

    let read = svc.get_note(&ctx, Uuid::new_v4()).await;
    assert!(matches!(read, Err(DomainError::Forbidden(_))));
}

#[tokio::test]
async fn failing_policy_service_gives_an_internal_error() {
    let db = inmem_db().await;
    let svc = service_with(&db, Arc::new(FailingResolver));
    let ctx = context_in(Uuid::new_v4());

    let created = svc.create_note(&ctx, note("hello")).await;
    assert!(
        matches!(created, Err(DomainError::Internal(_))),
        "{created:?}"
    );

    let read = svc.get_note(&ctx, Uuid::new_v4()).await;
    assert!(matches!(read, Err(DomainError::Internal(_))), "{read:?}");
}

#[tokio::test]
async fn allow_without_constraints_is_forbidden() {
    let db = inmem_db().await;
    let svc = service_with(&db, Arc::new(UnconstrainedResolver));
    let ctx = context_in(Uuid::new_v4());

    let created = svc.create_note(&ctx, note("hello")).await;
    assert!(
        matches!(created, Err(DomainError::Forbidden(_))),
        "{created:?}"
    );

    let read = svc.get_note(&ctx, Uuid::new_v4()).await;
    assert!(matches!(read, Err(DomainError::Forbidden(_))), "{read:?}");
}

#[tokio::test]
async fn note_of_another_tenant_is_not_found() {
    let db = inmem_db().await;
    let svc = service_with(&db, Arc::new(AllowResolver));
    let owner = context_in(Uuid::new_v4());
    let stranger = context_in(Uuid::new_v4());

    let created = svc.create_note(&owner, note("private")).await.unwrap();

    let read = svc.get_note(&stranger, created.id).await;
    assert!(matches!(read, Err(DomainError::NotFound)));
}

#[tokio::test]
async fn unknown_note_is_not_found() {
    let db = inmem_db().await;
    let svc = service_with(&db, Arc::new(AllowResolver));
    let ctx = context_in(Uuid::new_v4());

    let read = svc.get_note(&ctx, Uuid::new_v4()).await;
    assert!(matches!(read, Err(DomainError::NotFound)));
}

#[tokio::test]
async fn blank_and_oversized_text_is_rejected() {
    let db = inmem_db().await;
    let svc = service_with(&db, Arc::new(AllowResolver));
    let ctx = context_in(Uuid::new_v4());

    let blank = svc.create_note(&ctx, note("   ")).await;
    assert!(matches!(blank, Err(DomainError::Validation { .. })));

    let too_long = "x".repeat(test_service_config().max_text_length + 1);
    let oversized = svc.create_note(&ctx, note(&too_long)).await;
    assert!(matches!(oversized, Err(DomainError::Validation { .. })));
}

#[tokio::test]
async fn text_with_nul_character_is_rejected() {
    let db = inmem_db().await;
    let svc = service_with(&db, Arc::new(AllowResolver));
    let ctx = context_in(Uuid::new_v4());

    let rejected = svc.create_note(&ctx, note("a\u{0}b")).await;
    assert!(matches!(rejected, Err(DomainError::Validation { .. })));
}

#[tokio::test]
async fn local_client_round_trips_a_note_and_maps_not_found() {
    let db = inmem_db().await;
    let client: Arc<dyn ConstructClientV1> = Arc::new(LocalClient::new(Arc::new(service_with(
        &db,
        Arc::new(AllowResolver),
    ))));
    let ctx = context_in(Uuid::new_v4());

    let created = client.create_note(&ctx, note("hello")).await.unwrap();
    let read = client.get_note(&ctx, created.id).await.unwrap();
    assert_eq!(read, created);

    let missing = client.get_note(&ctx, Uuid::new_v4()).await.unwrap_err();
    assert_eq!(Problem::from(missing).status, Some(404));
}

#[tokio::test]
async fn text_of_exactly_the_byte_limit_is_accepted() {
    let db = inmem_db().await;
    let svc = service_with(&db, Arc::new(AllowResolver));
    let ctx = context_in(Uuid::new_v4());

    let at_limit = "x".repeat(test_service_config().max_text_length);
    let created = svc.create_note(&ctx, note(&at_limit)).await.unwrap();
    assert_eq!(created.text, at_limit);
}

#[tokio::test]
async fn multibyte_text_over_the_byte_limit_is_rejected() {
    let db = inmem_db().await;
    let svc = service_with(&db, Arc::new(AllowResolver));
    let ctx = context_in(Uuid::new_v4());

    let limit = test_service_config().max_text_length;
    let chars_under_limit_bytes_over = "\u{e4}".repeat(limit.div_ceil(2) + 1);
    assert!(chars_under_limit_bytes_over.chars().count() < limit);
    assert!(chars_under_limit_bytes_over.len() > limit);

    let rejected = svc
        .create_note(&ctx, note(&chars_under_limit_bytes_over))
        .await;
    assert!(matches!(rejected, Err(DomainError::Validation { .. })));
}
