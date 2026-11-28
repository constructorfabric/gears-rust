//! Unit tests for [`TenantService::erase_tenant`] with the in-memory
//! `FakeTenantRepo`: the name a deleted tenant carries, the
//! `TenantErased` delivery, and the line between erasure and deletion.

#![allow(clippy::expect_used, clippy::panic, reason = "test helpers")]

use std::sync::{Arc, Mutex};

use account_management_sdk::{
    CreateTenantRequest, PrincipalLifecycleEvent, PrincipalLifecycleHookError,
    TenantStatus as PublicTenantStatus,
};
use async_trait::async_trait;
use authz_resolver_sdk::models::{
    Capability, EvaluationRequest, EvaluationResponse, EvaluationResponseContext,
};
use authz_resolver_sdk::{AuthZResolverApi, PolicyEnforcer};
use toolkit::api::canonical_prelude::CanonicalError;
use toolkit_gts::gts_id;
use toolkit_security::{AccessScope, PlatformSecurityContext, SecurityContext};
use uuid::Uuid;

use crate::config::{AccountManagementConfig, RetentionConfig};
use crate::domain::error::DomainError;
use crate::domain::principal_lifecycle::{
    PrincipalLifecycleNotifier, build_tenant_hard_delete_hook,
};
use crate::domain::tenant::model::{TenantStatus, erased_tenant_name};
use crate::domain::tenant::repo::TenantRepo;
use crate::domain::tenant::resource_checker::InertResourceOwnershipChecker;
use crate::domain::tenant::service::TenantService;
use crate::domain::tenant::test_support::{
    FakeIdpProvisioner, FakeOutcome, FakeTenantRepo, constraint_bearing_enforcer,
    deny_all_enforcer, mock_enforcer,
};
use crate::infra::types_registry::test_helpers::{ConstantTypeRegistry, WithRegistry};

const CUSTOMER_TENANT_TYPE_ID: &str = gts_id!("cf.core.am.tenant_type.v1~cf.core.am.customer.v1~");
const ROOT: Uuid = Uuid::from_u128(0x100);
const CHILD: Uuid = Uuid::from_u128(0xE200);
const CHILD_NAME: &str = "Jane Doe Consulting";

fn ctx() -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::from_u128(0xDEAD))
        .subject_tenant_id(ROOT)
        .build()
        .expect("ctx")
}

/// One delivery as a hook saw it: the event, the actor the context
/// named, and the tenant's name in AM at that moment.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Delivery {
    event: PrincipalLifecycleEvent,
    subject_id: Uuid,
    name_in_am: Option<String>,
}

/// Notifier that records each delivery and answers from a script; once
/// the script is used up it answers `Ok(())`.
struct RecordingNotifier {
    repo: Arc<FakeTenantRepo>,
    answers: Mutex<Vec<Result<(), PrincipalLifecycleHookError>>>,
    seen: Mutex<Vec<Delivery>>,
}

impl RecordingNotifier {
    fn new(repo: Arc<FakeTenantRepo>) -> Arc<Self> {
        Self::answering(repo, Vec::new())
    }

    fn answering(
        repo: Arc<FakeTenantRepo>,
        answers: Vec<Result<(), PrincipalLifecycleHookError>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            repo,
            answers: Mutex::new(answers),
            seen: Mutex::new(Vec::new()),
        })
    }

    fn seen(&self) -> Vec<Delivery> {
        self.seen.lock().expect("lock").clone()
    }

    fn events(&self) -> Vec<PrincipalLifecycleEvent> {
        self.seen().into_iter().map(|d| d.event).collect()
    }
}

#[async_trait]
impl PrincipalLifecycleNotifier for RecordingNotifier {
    async fn notify(
        &self,
        ctx: &SecurityContext,
        event: &PrincipalLifecycleEvent,
    ) -> Result<(), PrincipalLifecycleHookError> {
        let tenant_id = match event {
            PrincipalLifecycleEvent::TenantErased { tenant_id }
            | PrincipalLifecycleEvent::TenantHardDelete { tenant_id }
            | PrincipalLifecycleEvent::UserDeprovisioned { tenant_id, .. } => *tenant_id,
            other => panic!("unexpected event {other:?}"),
        };
        self.seen.lock().expect("lock").push(Delivery {
            event: event.clone(),
            subject_id: ctx.subject_id(),
            name_in_am: self.repo.find_by_id_unchecked(tenant_id).map(|t| t.name),
        });
        let mut answers = self.answers.lock().expect("lock");
        if answers.is_empty() {
            Ok(())
        } else {
            answers.remove(0)
        }
    }
}

fn service(
    repo: Arc<FakeTenantRepo>,
    enforcer: PolicyEnforcer,
    retention_window_secs: u64,
) -> TenantService<FakeTenantRepo> {
    TenantService::new(
        repo,
        Arc::new(FakeIdpProvisioner::new(FakeOutcome::Ok)),
        Arc::new(InertResourceOwnershipChecker),
        crate::domain::tenant_type::inert_tenant_type_checker(),
        enforcer,
        AccountManagementConfig {
            retention: RetentionConfig {
                default_window_secs: retention_window_secs,
                ..RetentionConfig::default()
            },
            ..AccountManagementConfig::default()
        },
    )
    .with_registry(Arc::new(ConstantTypeRegistry(CUSTOMER_TENANT_TYPE_ID)))
}

/// A service whose hooks are `notifier`, over a repo holding `ROOT` and
/// an active `CHILD` named [`CHILD_NAME`].
async fn setup() -> (
    TenantService<FakeTenantRepo>,
    Arc<FakeTenantRepo>,
    Arc<RecordingNotifier>,
) {
    let repo = Arc::new(FakeTenantRepo::with_root(ROOT));
    let notifier = RecordingNotifier::new(repo.clone());
    let svc = service(repo.clone(), mock_enforcer(), 90 * 24 * 3600)
        .with_principal_lifecycle(notifier.clone());
    create_child(&svc).await;
    (svc, repo, notifier)
}

async fn create_child(svc: &TenantService<FakeTenantRepo>) {
    svc.create_tenant(
        &ctx(),
        CreateTenantRequest::new(
            CHILD,
            ROOT,
            CHILD_NAME,
            gts::GtsTypeId::new(CUSTOMER_TENANT_TYPE_ID),
        ),
    )
    .await
    .expect("create child");
}

fn stored_name(repo: &FakeTenantRepo, id: Uuid) -> String {
    repo.find_by_id_unchecked(id).expect("row").name
}

#[tokio::test]
async fn erase_replaces_the_name_of_a_deleted_tenant_with_its_id_and_keeps_it_deleted() {
    let (svc, repo, _notifier) = setup().await;
    let deleted = svc.delete_tenant(&ctx(), CHILD).await.expect("soft delete");
    let retention_before = repo
        .state
        .lock()
        .expect("lock")
        .retention
        .get(&CHILD)
        .copied();

    let erased = svc.erase_tenant(&ctx(), CHILD).await.expect("erase");

    assert_eq!(erased.name, CHILD.to_string());
    assert_eq!(erased.name, erased_tenant_name(CHILD));
    assert_eq!(erased.status, PublicTenantStatus::Deleted);
    assert_eq!(
        erased.deleted_at, deleted.deleted_at,
        "deletion instant kept"
    );
    assert_eq!(stored_name(&repo, CHILD), CHILD.to_string());
    assert_eq!(
        repo.state
            .lock()
            .expect("lock")
            .retention
            .get(&CHILD)
            .copied(),
        retention_before,
        "the retention timer is not touched"
    );
    let read_back = svc.get_tenant(&ctx(), CHILD).await.expect("read");
    assert_eq!(
        read_back.name,
        CHILD.to_string(),
        "later reads carry the id"
    );
}

#[tokio::test]
async fn erase_delivers_tenant_erased_after_the_name_is_gone() {
    let (svc, _repo, notifier) = setup().await;
    svc.delete_tenant(&ctx(), CHILD).await.expect("soft delete");
    assert!(notifier.seen().is_empty(), "a soft delete delivers nothing");

    svc.erase_tenant(&ctx(), CHILD).await.expect("erase");

    assert_eq!(
        notifier.seen(),
        vec![Delivery {
            event: PrincipalLifecycleEvent::TenantErased { tenant_id: CHILD },
            subject_id: ctx().subject_id(),
            name_in_am: Some(CHILD.to_string()),
        }],
        "one TenantErased, under the caller's identity, once AM no longer holds the name"
    );
}

#[tokio::test]
async fn erase_is_idempotent_and_delivers_the_event_on_every_call() {
    let (svc, repo, notifier) = setup().await;
    svc.delete_tenant(&ctx(), CHILD).await.expect("soft delete");
    svc.erase_tenant(&ctx(), CHILD).await.expect("first erase");
    let after_first = repo.find_by_id_unchecked(CHILD).expect("row");

    let second = svc.erase_tenant(&ctx(), CHILD).await.expect("second erase");

    assert_eq!(second.name, CHILD.to_string());
    assert_eq!(
        repo.find_by_id_unchecked(CHILD).expect("row").updated_at,
        after_first.updated_at,
        "an erased tenant is not written again"
    );
    assert_eq!(
        notifier.events(),
        vec![PrincipalLifecycleEvent::TenantErased { tenant_id: CHILD }; 2],
        "a retry delivers the event again"
    );
}

#[tokio::test]
async fn erase_refuses_a_tenant_that_is_not_deleted() {
    let (svc, repo, notifier) = setup().await;

    let active = svc.erase_tenant(&ctx(), CHILD).await;
    svc.suspend_tenant(&ctx(), CHILD).await.expect("suspend");
    let suspended = svc.erase_tenant(&ctx(), CHILD).await;
    let root = svc.erase_tenant(&ctx(), ROOT).await;

    for (label, result) in [("active", active), ("suspended", suspended), ("root", root)] {
        assert!(
            matches!(result, Err(DomainError::Conflict { .. })),
            "{label}: expected Conflict, got {result:?}"
        );
    }
    assert_eq!(
        stored_name(&repo, CHILD),
        CHILD_NAME,
        "the name is untouched"
    );
    assert!(notifier.seen().is_empty(), "nothing is delivered");
}

#[tokio::test]
async fn erase_hides_missing_and_provisioning_tenants_as_not_found() {
    let (svc, repo, notifier) = setup().await;
    let provisioning = Uuid::from_u128(0xE300);
    let mut row = repo.find_by_id_unchecked(CHILD).expect("child");
    row.id = provisioning;
    row.status = TenantStatus::Provisioning;
    repo.insert_tenant_raw(row);

    for id in [Uuid::from_u128(0xE399), provisioning] {
        let result = svc.erase_tenant(&ctx(), id).await;
        assert!(
            matches!(result, Err(DomainError::NotFound { .. })),
            "{id}: expected NotFound, got {result:?}"
        );
    }
    assert!(notifier.seen().is_empty());
}

#[tokio::test]
async fn a_failed_delivery_fails_the_request_but_keeps_the_erasure_for_the_retry() {
    let repo = Arc::new(FakeTenantRepo::with_root(ROOT));
    let notifier = RecordingNotifier::answering(
        repo.clone(),
        vec![
            Err(PrincipalLifecycleHookError::Retryable {
                detail: "usage store down".to_owned(),
            }),
            Err(PrincipalLifecycleHookError::Terminal {
                detail: "bug".to_owned(),
            }),
        ],
    );
    let svc = service(repo.clone(), mock_enforcer(), 90 * 24 * 3600)
        .with_principal_lifecycle(notifier.clone());
    create_child(&svc).await;
    svc.delete_tenant(&ctx(), CHILD).await.expect("soft delete");

    let retryable = svc.erase_tenant(&ctx(), CHILD).await;
    assert!(
        matches!(retryable, Err(DomainError::ServiceUnavailable { .. })),
        "a retryable hook failure is a 503: {retryable:?}"
    );
    assert_eq!(
        stored_name(&repo, CHILD),
        CHILD.to_string(),
        "AM's own copy is erased even though delivery failed"
    );

    let terminal = svc.erase_tenant(&ctx(), CHILD).await;
    assert!(
        matches!(terminal, Err(DomainError::Internal { .. })),
        "a terminal hook failure is a 500: {terminal:?}"
    );

    svc.erase_tenant(&ctx(), CHILD)
        .await
        .expect("the retry succeeds once every hook does");
    assert_eq!(
        notifier.events(),
        vec![PrincipalLifecycleEvent::TenantErased { tenant_id: CHILD }; 3]
    );
}

#[tokio::test]
async fn erase_works_without_hooks_wired() {
    let repo = Arc::new(FakeTenantRepo::with_root(ROOT));
    let svc = service(repo.clone(), mock_enforcer(), 90 * 24 * 3600);
    create_child(&svc).await;
    svc.delete_tenant(&ctx(), CHILD).await.expect("soft delete");

    svc.erase_tenant(&ctx(), CHILD).await.expect("erase");

    assert_eq!(stored_name(&repo, CHILD), CHILD.to_string());
}

#[tokio::test]
async fn erase_is_refused_when_the_pdp_denies_it() {
    let (setup_svc, repo, _) = setup().await;
    setup_svc
        .delete_tenant(&ctx(), CHILD)
        .await
        .expect("soft delete");
    let notifier = RecordingNotifier::new(repo.clone());
    let svc =
        service(repo.clone(), deny_all_enforcer(), 0).with_principal_lifecycle(notifier.clone());

    let result = svc.erase_tenant(&ctx(), CHILD).await;

    assert!(
        matches!(result, Err(DomainError::CrossTenantDenied { .. })),
        "{result:?}"
    );
    assert_eq!(stored_name(&repo, CHILD), CHILD_NAME);
    assert!(notifier.seen().is_empty());
}

#[tokio::test]
async fn erase_outside_the_caller_subtree_is_not_found() {
    let (setup_svc, repo, _) = setup().await;
    setup_svc
        .delete_tenant(&ctx(), CHILD)
        .await
        .expect("soft delete");
    let other = Uuid::from_u128(0xE201);
    setup_svc
        .create_tenant(
            &ctx(),
            CreateTenantRequest::new(
                other,
                ROOT,
                "sibling",
                gts::GtsTypeId::new(CUSTOMER_TENANT_TYPE_ID),
            ),
        )
        .await
        .expect("sibling");
    // The caller's scope is the sibling's subtree, which does not hold CHILD.
    let svc = service(repo.clone(), constraint_bearing_enforcer(other), 0);

    let result = svc.erase_tenant(&ctx(), CHILD).await;

    assert!(
        matches!(result, Err(DomainError::NotFound { .. })),
        "{result:?}"
    );
    assert_eq!(stored_name(&repo, CHILD), CHILD_NAME);
}

/// The `(action, resource id)` of every PDP evaluation, in order.
type Evaluations = Arc<Mutex<Vec<(String, Option<String>)>>>;

/// PDP that permits like [`mock_enforcer`] and records the action and
/// resource id of every evaluation.
struct ActionRecordingPdp {
    seen: Evaluations,
}

#[async_trait]
impl AuthZResolverApi for ActionRecordingPdp {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        use authz_resolver_sdk::constraints::{Constraint, InTenantSubtreePredicate, Predicate};
        use toolkit_security::pep_properties;

        self.seen.lock().expect("lock").push((
            request.action.name.clone(),
            request.resource.id.map(|id| id.to_string()),
        ));
        Ok(EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext {
                constraints: vec![Constraint {
                    predicates: vec![Predicate::InTenantSubtree(InTenantSubtreePredicate::new(
                        pep_properties::RESOURCE_ID,
                        ROOT,
                    ))],
                }],
                deny_reason: None,
            },
        })
    }
}

#[tokio::test]
async fn erase_asks_the_pdp_for_the_erase_action_on_the_tenant() {
    let (setup_svc, repo, _) = setup().await;
    setup_svc
        .delete_tenant(&ctx(), CHILD)
        .await
        .expect("soft delete");
    let seen: Evaluations = Arc::new(Mutex::new(Vec::new()));
    let pdp: Arc<dyn AuthZResolverApi> = Arc::new(ActionRecordingPdp { seen: seen.clone() });
    let enforcer = PolicyEnforcer::new(pdp).with_capabilities(vec![Capability::TenantHierarchy]);
    let svc = service(repo, enforcer, 0);

    svc.erase_tenant(&ctx(), CHILD).await.expect("erase");

    assert_eq!(
        *seen.lock().expect("lock"),
        vec![("erase".to_owned(), Some(CHILD.to_string()))],
        "one PDP call, for `erase` on the tenant itself"
    );
}

#[tokio::test]
async fn a_hard_delete_is_never_reported_as_an_erasure() {
    let repo = Arc::new(FakeTenantRepo::with_root(ROOT));
    let notifier = RecordingNotifier::new(repo.clone());
    // Window 0: a soft-deleted tenant is due on the next tick.
    let svc = service(repo.clone(), mock_enforcer(), 0).with_principal_lifecycle(notifier.clone());
    svc.register_hard_delete_hook(build_tenant_hard_delete_hook(notifier.clone()));
    create_child(&svc).await;
    svc.delete_tenant(&ctx(), CHILD).await.expect("soft delete");

    let result = svc.hard_delete_batch(64).await;

    assert_eq!(result.cleaned, 1);
    assert_eq!(
        notifier.events(),
        vec![PrincipalLifecycleEvent::TenantHardDelete { tenant_id: CHILD }],
        "a hard delete delivers TenantHardDelete and no TenantErased"
    );
    assert_eq!(
        notifier.seen()[0].name_in_am.as_deref(),
        Some(CHILD_NAME),
        "the name is still there when the hard delete is announced"
    );
    assert!(
        repo.find_by_id(&AccessScope::allow_all(), CHILD)
            .await
            .expect("repo")
            .is_none()
    );
}

#[tokio::test]
async fn an_erased_tenant_is_still_hard_deleted_at_the_end_of_its_window() {
    let repo = Arc::new(FakeTenantRepo::with_root(ROOT));
    let notifier = RecordingNotifier::new(repo.clone());
    let svc = service(repo.clone(), mock_enforcer(), 0).with_principal_lifecycle(notifier.clone());
    svc.register_hard_delete_hook(build_tenant_hard_delete_hook(notifier.clone()));
    create_child(&svc).await;
    svc.delete_tenant(&ctx(), CHILD).await.expect("soft delete");
    svc.erase_tenant(&ctx(), CHILD).await.expect("erase");

    let result = svc.hard_delete_batch(64).await;

    assert_eq!(result.cleaned, 1);
    assert_eq!(
        notifier.events(),
        vec![
            PrincipalLifecycleEvent::TenantErased { tenant_id: CHILD },
            PrincipalLifecycleEvent::TenantHardDelete { tenant_id: CHILD },
        ],
        "each act delivers its own event, once"
    );
}
