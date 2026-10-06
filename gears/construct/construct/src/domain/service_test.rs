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
use toolkit_canonical_errors::{Problem, resource_error};
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

/// Builds resource errors for the policy service stubs, without the REST layer.
#[resource_error(gts_id!("cf.construct.foundation.note.v1~"))]
struct PolicyTestError;

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

/// One request to the policy service: resource type, action and the tenant
/// named as the owner of the resource.
type AskedOfPolicyService = (String, String, Option<String>);

/// Records what the service asks the policy service for, then allows it like
/// `AllowResolver`.
struct RecordingResolver {
    asked: std::sync::Mutex<Vec<AskedOfPolicyService>>,
}

#[async_trait]
impl AuthZResolverApi for RecordingResolver {
    async fn evaluate(
        &self,
        ctx: PlatformSecurityContext,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        self.asked.lock().unwrap().push((
            request.resource.resource_type.clone(),
            request.action.name.clone(),
            request
                .resource
                .properties
                .get(pep_properties::OWNER_TENANT_ID)
                .and_then(|v| v.as_str())
                .map(str::to_owned),
        ));
        AllowResolver.evaluate(ctx, request).await
    }
}

/// The policy service answers with an error instead of a decision. The error
/// is chosen per test, to show which failures are retryable.
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

/// The policy service never answers.
struct PendingResolver;

#[async_trait]
impl AuthZResolverApi for PendingResolver {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        _request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        std::future::pending().await
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
    service_with_enforcer(db, PolicyEnforcer::new(resolver))
}

fn service_with_enforcer(db: &Db, enforcer: PolicyEnforcer) -> ConcreteService {
    Service::new(
        Arc::new(DBProvider::new(db.clone())),
        Arc::new(SeaOrmNoteRepository::new()),
        enforcer,
        test_service_config(),
    )
}

/// Everything a caller can read in a `Problem`, as one string.
fn visible_text(problem: &Problem) -> String {
    format!(
        "{} {} {} {}",
        problem.problem_type, problem.title, problem.detail, problem.context
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
async fn service_asks_for_the_note_resource_the_matching_action_and_the_tenant() {
    let db = inmem_db().await;
    let resolver = Arc::new(RecordingResolver {
        asked: std::sync::Mutex::new(Vec::new()),
    });
    let svc = service_with(&db, resolver.clone());
    let tenant = Uuid::new_v4();
    let ctx = context_in(tenant);

    let created = svc.create_note(&ctx, note("hello")).await.unwrap();
    svc.get_note(&ctx, created.id).await.unwrap();

    let note_resource = "construct.foundation_note".to_owned();
    let tenant = Some(tenant.to_string());
    let asked = resolver.asked.lock().unwrap().clone();
    assert_eq!(
        asked,
        vec![
            (note_resource.clone(), "create".to_owned(), tenant.clone()),
            (note_resource, "get".to_owned(), tenant),
        ],
    );
}

/// Runs both operations against a policy service behind `enforcer`.
async fn failures_of_both_operations(enforcer: PolicyEnforcer) -> (DomainError, DomainError) {
    let db = inmem_db().await;
    let svc = service_with_enforcer(&db, enforcer);
    let ctx = context_in(Uuid::new_v4());

    let created = svc.create_note(&ctx, note("hello")).await;
    let read = svc.get_note(&ctx, Uuid::new_v4()).await;
    (created.unwrap_err(), read.unwrap_err())
}

/// A policy service that answers with the error built by `error`.
fn erroring(error: fn() -> CanonicalError) -> PolicyEnforcer {
    PolicyEnforcer::new(Arc::new(ErroringResolver(error)))
}

fn assert_unavailable((created, read): (DomainError, DomainError)) {
    assert!(
        matches!(created, DomainError::Unavailable(_)),
        "{created:?}"
    );
    assert!(matches!(read, DomainError::Unavailable(_)), "{read:?}");
}

#[tokio::test]
async fn unavailable_policy_service_gives_an_unavailable_error() {
    let enforcer = erroring(|| CanonicalError::service_unavailable().create());

    assert_unavailable(failures_of_both_operations(enforcer).await);
}

#[tokio::test]
async fn deadline_exceeded_policy_service_gives_an_unavailable_error() {
    let enforcer = erroring(|| PolicyTestError::deadline_exceeded("too slow").create());

    assert_unavailable(failures_of_both_operations(enforcer).await);
}

#[tokio::test]
async fn throttled_policy_service_gives_an_unavailable_error() {
    let enforcer = erroring(|| {
        PolicyTestError::resource_exhausted("too many requests")
            .with_quota_violation("policy", "rate limit")
            .create()
    });

    assert_unavailable(failures_of_both_operations(enforcer).await);
}

#[tokio::test]
async fn unanswered_policy_request_gives_an_unavailable_error() {
    let enforcer = PolicyEnforcer::new(Arc::new(PendingResolver))
        .with_deadline(std::time::Duration::from_millis(10));

    assert_unavailable(failures_of_both_operations(enforcer).await);
}

#[tokio::test]
async fn broken_policy_service_gives_an_internal_error() {
    let enforcer =
        erroring(|| CanonicalError::internal("policy client is misconfigured".to_owned()).create());

    let (created, read) = failures_of_both_operations(enforcer).await;

    // The real cause is kept for the log, so a defect can be found.
    for error in [created, read] {
        let DomainError::Internal(cause) = error else {
            panic!("expected an internal error, got {error:?}");
        };
        assert!(cause.contains("policy client is misconfigured"), "{cause}");
    }
}

#[tokio::test]
async fn client_error_for_a_broken_policy_service_hides_its_cause() {
    let db = inmem_db().await;
    let enforcer =
        erroring(|| CanonicalError::internal("policy client is misconfigured".to_owned()).create());
    let client: Arc<dyn ConstructClientV1> = Arc::new(LocalClient::new(Arc::new(
        service_with_enforcer(&db, enforcer),
    )));
    let ctx = context_in(Uuid::new_v4());

    let error = client.create_note(&ctx, note("hello")).await.unwrap_err();

    let problem = Problem::from(error);
    assert_eq!(problem.status, Some(500));
    assert!(!visible_text(&problem).contains("misconfigured"));
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
