//! Tests exercise the real `AuthN` exchange and `PolicyEnforcer`, with controlled endpoints.
use super::*;
use crate::domain::persisted::RunStatus;
use crate::{
    domain::registry::Registry,
    infra::{
        executor::Executor,
        storage::repository::tests::{definition, journal, store},
    },
};
use authn_resolver_sdk::{AuthNResolverError, AuthenticationResult};
use authz_resolver_sdk::{
    AuthZResolverApi,
    constraints::{Constraint, InPredicate, Predicate},
    models::{EvaluationRequest, EvaluationResponse, EvaluationResponseContext},
};
use durable_execution_sdk::contracts::{ActivityInput, ErasedActivity};
use durable_execution_sdk::{ActivityContext, ActivityError, ExecutionOwner};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::{PlatformSecurityContext, SecurityContext};
use uuid::Uuid;

struct Identity {
    subject: Uuid,
    tenant: Uuid,
    exchanges: AtomicUsize,
    unavailable: AtomicBool,
}
#[async_trait]
impl AuthNResolverClient for Identity {
    async fn authenticate(&self, _: &str) -> Result<AuthenticationResult, AuthNResolverError> {
        Err(AuthNResolverError::NoPluginAvailable)
    }
    async fn exchange_client_credentials(
        &self,
        request: &ClientCredentialsRequest,
    ) -> Result<AuthenticationResult, AuthNResolverError> {
        assert_eq!(request.client_id, "worker");
        self.exchanges.fetch_add(1, Ordering::SeqCst);
        if self.unavailable.load(Ordering::SeqCst) {
            return Err(AuthNResolverError::NoPluginAvailable);
        }
        Ok(AuthenticationResult {
            security_context: SecurityContext::builder()
                .subject_id(self.subject)
                .subject_tenant_id(self.tenant)
                .subject_type("service")
                .build()
                .unwrap(),
        })
    }
}
struct Policy {
    subject: Uuid,
    tenant: Uuid,
    mode: AtomicU8,
    actions: parking_lot::Mutex<Vec<String>>,
}
#[async_trait]
impl AuthZResolverApi for Policy {
    async fn evaluate(
        &self,
        _: PlatformSecurityContext,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        assert_eq!(request.subject.id, self.subject);
        self.actions.lock().push(request.action.name.clone());
        if self.mode.load(Ordering::SeqCst) == 2 {
            return Err(CanonicalError::service_unavailable().create());
        }
        Ok(EvaluationResponse {
            decision: matches!(self.mode.load(Ordering::SeqCst), 0 | 3),
            context: EvaluationResponseContext {
                constraints: if request.resource.resource_type == "durable_execution.definition"
                    || (request.action.name == "cancel_definition"
                        && self.mode.load(Ordering::SeqCst) != 3)
                {
                    vec![]
                } else {
                    vec![Constraint {
                        predicates: vec![Predicate::In(InPredicate::new(
                            pep_properties::OWNER_TENANT_ID,
                            [self.tenant],
                        ))],
                    }]
                },
                ..EvaluationResponseContext::default()
            },
        })
    }
}
// Explicit test credentials avoid mutating process environment in concurrent tests.
struct Credentials(ServiceAuthorization);
#[async_trait]
impl WorkerAuthorization for Credentials {
    async fn scope(&self, action: &str) -> Result<AccessScope, StoreError> {
        self.0
            .authorize(
                ClientCredentialsRequest {
                    client_id: "worker".into(),
                    client_secret: secrecy::SecretString::from("fixture"),
                    scopes: vec![],
                },
                action,
            )
            .await
    }
}
fn authorization(tenant: Uuid) -> (Arc<Credentials>, Arc<Identity>, Arc<Policy>) {
    let subject = Uuid::new_v4();
    let identity = Arc::new(Identity {
        subject,
        tenant,
        exchanges: AtomicUsize::new(0),
        unavailable: AtomicBool::new(false),
    });
    let policy = Arc::new(Policy {
        subject,
        tenant,
        mode: AtomicU8::new(0),
        actions: parking_lot::Mutex::new(vec![]),
    });
    (
        Arc::new(Credentials(ServiceAuthorization {
            authn: identity.clone(),
            enforcer: PolicyEnforcer::new(policy.clone()),
            config: Config::default(),
        })),
        identity,
        policy,
    )
}

#[tokio::test]
async fn worker_scope_uses_service_identity_but_preserves_run_owner_and_tenant_isolation() {
    let tenant = Uuid::new_v4();
    let (auth, identity, policy) = authorization(tenant);
    let store = store().await.with_authorization(auth);
    let mut allowed = journal();
    allowed.run.owner = ExecutionOwner {
        tenant_id: tenant,
        subject_id: Uuid::new_v4(),
    };
    let owner = allowed.run.owner.clone();
    let id = allowed.run.id;
    let foreign = journal();
    let foreign_id = foreign.run.id;
    store
        .insert(AccessScope::allow_all(), allowed)
        .await
        .unwrap();
    store
        .insert(AccessScope::allow_all(), foreign)
        .await
        .unwrap();
    let registry = Arc::new(Registry::default());
    registry.register(definition()).unwrap();
    let executor = Executor {
        store: store.clone(),
        registry,
        config: Config::default(),
    };
    executor
        .execute(foreign_id, 0, tokio_util::sync::CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        store
            .get(&AccessScope::allow_all(), foreign_id)
            .await
            .unwrap()
            .unwrap()
            .run
            .status,
        RunStatus::Queued
    );
    executor
        .execute(id, 0, tokio_util::sync::CancellationToken::new())
        .await
        .unwrap();
    let current = store
        .get(&AccessScope::allow_all(), id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(current.run.owner, owner);
    assert_eq!(
        current.run.activities[0].status,
        crate::domain::persisted::ActivityStatus::Succeeded
    );
    assert_ne!(owner.subject_id, identity.subject);
    assert!(identity.exchanges.load(Ordering::SeqCst) >= 3);
    assert!(
        policy
            .actions
            .lock()
            .iter()
            .all(|action| matches!(action.as_str(), "execute" | "get"))
    );
}

#[tokio::test]
async fn authn_and_pdp_outages_pause_without_workflow_failure_and_recover() {
    let tenant = Uuid::new_v4();
    let (auth, identity, policy) = authorization(tenant);
    let store = store().await.with_authorization(auth);
    let mut run = journal();
    run.run.owner.tenant_id = tenant;
    let id = run.run.id;
    store.insert(AccessScope::allow_all(), run).await.unwrap();
    let registry = Arc::new(Registry::default());
    registry.register(definition()).unwrap();
    let executor = Executor {
        store: store.clone(),
        registry,
        config: Config::default(),
    };
    identity.unavailable.store(true, Ordering::SeqCst);
    assert!(matches!(
        executor
            .execute(id, 0, tokio_util::sync::CancellationToken::new())
            .await,
        Err(StoreError::AuthorizationUnavailable)
    ));
    identity.unavailable.store(false, Ordering::SeqCst);
    for mode in [1, 2] {
        policy.mode.store(mode, Ordering::SeqCst);
        assert!(matches!(
            executor
                .execute(id, 0, tokio_util::sync::CancellationToken::new())
                .await,
            Err(StoreError::Authorization(_))
        ));
        let untouched = store
            .get(&AccessScope::allow_all(), id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(untouched.run.status, RunStatus::Queued);
        assert_eq!(untouched.revision, 0);
    }
    policy.mode.store(0, Ordering::SeqCst);
    executor
        .execute(id, 0, tokio_util::sync::CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        store
            .get(&AccessScope::allow_all(), id)
            .await
            .unwrap()
            .unwrap()
            .run
            .activities[0]
            .status,
        crate::domain::persisted::ActivityStatus::Succeeded
    );
}

struct UntilCancelled {
    started: Arc<tokio::sync::Notify>,
    stopped: Arc<AtomicBool>,
}
#[async_trait]
impl ErasedActivity for UntilCancelled {
    async fn execute(
        &self,
        context: ActivityContext,
        _: ActivityInput,
    ) -> Result<serde_json::Value, ActivityError> {
        self.started.notify_one();
        context.cancellation.cancelled().await;
        self.stopped.store(true, Ordering::SeqCst);
        Err(ActivityError::Cancelled)
    }
}
#[tokio::test]
async fn access_revocation_stops_handler_without_recording_business_cancellation() {
    let tenant = Uuid::new_v4();
    let (auth, _, policy) = authorization(tenant);
    let store = store().await.with_authorization(auth);
    let started = Arc::new(tokio::sync::Notify::new());
    let stopped = Arc::new(AtomicBool::new(false));
    let mut def = definition();
    def.activities[0].handler = Arc::new(UntilCancelled {
        started: started.clone(),
        stopped: stopped.clone(),
    });
    let mut run = journal();
    run.run.owner.tenant_id = tenant;
    let id = run.run.id;
    store.insert(AccessScope::allow_all(), run).await.unwrap();
    let registry = Arc::new(Registry::default());
    registry.register(def).unwrap();
    let executor = Executor {
        store: store.clone(),
        registry,
        config: Config::default(),
    };
    let worker = tokio::spawn(async move {
        executor
            .execute(id, 0, tokio_util::sync::CancellationToken::new())
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), started.notified())
        .await
        .unwrap();
    policy.mode.store(1, Ordering::SeqCst);
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), worker)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(result, Err(StoreError::Authorization(_))));
    assert!(stopped.load(Ordering::SeqCst));
    let current = store
        .get(&AccessScope::allow_all(), id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(current.run.status, RunStatus::Running);
    assert!(!current.cancellation_requested);
    assert!(current.run.activities[0].result.is_none());
    assert!(current.lease_until.is_some());
}

#[tokio::test]
async fn definition_management_refreshes_identity_and_rejects_partial_scope_before_revocation() {
    use durable_execution_sdk::WorkflowRegistry;
    use durable_execution_sdk::registration::{
        RegistrationState, UnregisterMode, UnregisterOptions,
    };
    let tenant = Uuid::new_v4();
    let (auth, identity, policy) = authorization(tenant);
    let store = store().await.with_authorization(auth);
    let registrar = crate::infra::registrar::Registrar {
        store: store.clone(),
        registry: Arc::new(Registry::default()),
    };
    let d = definition();
    registrar.register(d.clone()).await.unwrap();
    policy.mode.store(3, Ordering::SeqCst);
    assert!(matches!(
        registrar
            .unregister(
                &d.name,
                UnregisterOptions {
                    mode: UnregisterMode::CancelAndRelease,
                    expected_revision: 0
                }
            )
            .await,
        Err(toolkit_canonical_errors::CanonicalError::PermissionDenied { .. })
    ));
    assert_eq!(
        registrar.registration(&d.name).await.unwrap().state,
        RegistrationState::Active
    );
    policy.mode.store(1, Ordering::SeqCst);
    assert!(
        registrar
            .unregister(
                &d.name,
                UnregisterOptions {
                    mode: UnregisterMode::Retain,
                    expected_revision: 0
                }
            )
            .await
            .is_err()
    );
    policy.mode.store(0, Ordering::SeqCst);
    registrar
        .unregister(
            &d.name,
            UnregisterOptions {
                mode: UnregisterMode::CancelAndRelease,
                expected_revision: 0,
            },
        )
        .await
        .unwrap();
    policy.mode.store(2, Ordering::SeqCst);
    let executor = Executor {
        store: store.clone(),
        registry: registrar.registry.clone(),
        config: Config::default(),
    };
    assert!(executor.reconcile_definitions().await.is_err());
    assert!(registrar.registry.available(&d.name, 0));
    policy.mode.store(0, Ordering::SeqCst);
    identity.unavailable.store(true, Ordering::SeqCst);
    assert!(executor.reconcile_definitions().await.is_err());
    assert!(registrar.registry.available(&d.name, 0));
    identity.unavailable.store(false, Ordering::SeqCst);
    executor.reconcile_definitions().await.unwrap();
    let released = registrar.registration(&d.name).await.unwrap();
    assert_eq!(released.state, RegistrationState::Released);
    assert!(!registrar.registry.available(&d.name, 0));
    policy.mode.store(1, Ordering::SeqCst);
    assert!(
        registrar
            .activate(&d.name, released.revision)
            .await
            .is_err()
    );
    policy.mode.store(0, Ordering::SeqCst);
    assert_eq!(registrar.registration(&d.name).await.unwrap(), released);
    assert!(identity.exchanges.load(Ordering::SeqCst) > 10);
}
