//! Background work through the real `Authority` and the storage seam, with a
//! scripted `Authorizer` standing in for the embedding gear's policy.
use super::*;
use crate::runtime::{ActivityInput, ErasedActivity};
use crate::state::persisted::{ActivityStatus, RunStatus};
use crate::{
    ActivityContext, ActivityError, AuthorizationError, Authorizer, CallerAction, ConstraintPolicy,
    ExecutionOwner,
};
use crate::{
    catalog::registry::Registry,
    config::Config,
    engine::executor::Executor,
    store::repository::tests::{definition, journal, store},
};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use toolkit_security::SecurityContext;
use uuid::Uuid;

const ALLOW: u8 = 0;
const DENY: u8 = 1;
const UNAVAILABLE: u8 = 2;
/// Grants `cancel_definition` only a tenant-constrained scope.
const PARTIAL: u8 = 3;

/// Grants the service tenant-constrained scopes for run actions and unconstrained
/// scopes for catalog actions, or fails as scripted.
struct ScriptedPolicy {
    tenant: Uuid,
    mode: AtomicU8,
    actions: parking_lot::Mutex<Vec<ServiceAction>>,
}

#[async_trait]
impl Authorizer for ScriptedPolicy {
    async fn caller_scope(
        &self,
        _: &SecurityContext,
        _: CallerAction,
    ) -> Result<AccessScope, AuthorizationError> {
        Err(AuthorizationError::denied())
    }
    async fn service_scope(
        &self,
        action: ServiceAction,
    ) -> Result<AccessScope, AuthorizationError> {
        self.actions.lock().push(action);
        match self.mode.load(Ordering::SeqCst) {
            DENY => Err(AuthorizationError::denied()),
            UNAVAILABLE => Err(AuthorizationError::unavailable()
                .with_source(std::io::Error::other("policy endpoint down"))),
            mode => Ok(match action.constraints() {
                ConstraintPolicy::Required => AccessScope::for_tenant(self.tenant),
                _ if mode == PARTIAL && action == ServiceAction::CancelDefinition => {
                    AccessScope::for_tenant(self.tenant)
                }
                _ => AccessScope::allow_all(),
            }),
        }
    }
}

fn policy(tenant: Uuid) -> Arc<ScriptedPolicy> {
    Arc::new(ScriptedPolicy {
        tenant,
        mode: AtomicU8::new(ALLOW),
        actions: parking_lot::Mutex::new(vec![]),
    })
}

async fn fixture(
    policy: &Arc<ScriptedPolicy>,
    definition: crate::contracts::ExecutionDefinition,
) -> (crate::store::JournalStore, Executor) {
    fixture_with(policy, definition, Config::default()).await
}

async fn fixture_with(
    policy: &Arc<ScriptedPolicy>,
    definition: crate::contracts::ExecutionDefinition,
    config: Config,
) -> (crate::store::JournalStore, Executor) {
    let authority = Authority::new(policy.clone(), Duration::from_secs(5));
    let store = store().await.with_authorization(Arc::new(authority));
    let registry = Arc::new(Registry::default());
    registry.register(definition).unwrap();
    let executor = Executor {
        store: store.clone(),
        registry,
        config,
    };
    (store, executor)
}

#[tokio::test]
async fn service_scope_preserves_run_owner_and_tenant_isolation() {
    let tenant = Uuid::new_v4();
    let policy = policy(tenant);
    let (store, executor) = fixture(&policy, definition()).await;
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

    // The run of another tenant is invisible to this service scope.
    executor
        .execute(foreign_id, 0, CancellationToken::new())
        .await
        .unwrap();
    let untouched = store
        .get(&AccessScope::allow_all(), foreign_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(untouched.run.status, RunStatus::Queued);

    executor
        .execute(id, 0, CancellationToken::new())
        .await
        .unwrap();
    let current = store
        .get(&AccessScope::allow_all(), id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(current.run.owner, owner);
    assert_eq!(current.run.activities[0].status, ActivityStatus::Succeeded);
    assert!(policy.actions.lock().iter().all(|action| matches!(
        action,
        ServiceAction::Execute | ServiceAction::DefinitionGet
    )));
}

#[tokio::test]
async fn authorization_outages_pause_without_workflow_failure_and_recover() {
    let tenant = Uuid::new_v4();
    let policy = policy(tenant);
    let (store, executor) = fixture(&policy, definition()).await;
    let mut run = journal();
    run.run.owner.tenant_id = tenant;
    let id = run.run.id;
    store.insert(AccessScope::allow_all(), run).await.unwrap();
    for mode in [DENY, UNAVAILABLE] {
        policy.mode.store(mode, Ordering::SeqCst);
        assert!(matches!(
            executor.execute(id, 0, CancellationToken::new()).await,
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
    policy.mode.store(ALLOW, Ordering::SeqCst);
    executor
        .execute(id, 0, CancellationToken::new())
        .await
        .unwrap();
    let recovered = store
        .get(&AccessScope::allow_all(), id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        recovered.run.activities[0].status,
        ActivityStatus::Succeeded
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
        context.cancellation().cancelled().await;
        self.stopped.store(true, Ordering::SeqCst);
        Err(ActivityError::Cancelled)
    }
}

#[tokio::test]
async fn access_revocation_stops_the_handler_without_recording_business_cancellation() {
    let tenant = Uuid::new_v4();
    let policy = policy(tenant);
    let started = Arc::new(tokio::sync::Notify::new());
    let stopped = Arc::new(AtomicBool::new(false));
    let blocking = crate::test_support::edit(definition(), |d| {
        d.activities[0].handler = Arc::new(UntilCancelled {
            started: started.clone(),
            stopped: stopped.clone(),
        });
    });
    // A one-second heartbeat guarantees a monitor pass after the revocation, so the
    // test does not depend on the scheduler running it during the first pass.
    let config = Config {
        heartbeat_secs: 1,
        lease_secs: 4,
        ..Config::default()
    };
    let (store, executor) = fixture_with(&policy, blocking, config).await;
    let mut run = journal();
    run.run.owner.tenant_id = tenant;
    let id = run.run.id;
    store.insert(AccessScope::allow_all(), run).await.unwrap();
    let worker =
        tokio::spawn(async move { executor.execute(id, 0, CancellationToken::new()).await });
    tokio::time::timeout(Duration::from_secs(5), started.notified())
        .await
        .unwrap();
    policy.mode.store(DENY, Ordering::SeqCst);
    let result = tokio::time::timeout(Duration::from_secs(5), worker)
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
async fn catalog_reconciliation_keeps_bindings_until_service_access_returns() {
    use crate::registration::{RegistrationState, UnregisterMode, UnregisterOptions};
    let policy = policy(Uuid::new_v4());
    let authority = Authority::new(policy.clone(), Duration::from_secs(5));
    let store = store().await.with_authorization(Arc::new(authority));
    let registrar = crate::catalog::registrar::Registrar {
        store: store.clone(),
        registry: Arc::new(Registry::default()),
    };
    let d = definition();
    registrar.register(d.clone()).await.unwrap();

    // A tenant-constrained grant cannot cancel the runs of every tenant.
    policy.mode.store(PARTIAL, Ordering::SeqCst);
    let error = registrar
        .unregister(
            d.name(),
            UnregisterOptions::new(UnregisterMode::CancelAndRelease, 0),
        )
        .await
        .unwrap_err();
    assert_eq!(error.kind(), crate::ErrorKind::PermissionDenied);
    assert_eq!(
        registrar.registration(d.name()).await.unwrap().state,
        RegistrationState::Active
    );
    policy.mode.store(DENY, Ordering::SeqCst);
    assert!(
        registrar
            .unregister(d.name(), UnregisterOptions::new(UnregisterMode::Retain, 0))
            .await
            .is_err()
    );

    policy.mode.store(ALLOW, Ordering::SeqCst);
    registrar
        .unregister(
            d.name(),
            UnregisterOptions::new(UnregisterMode::CancelAndRelease, 0),
        )
        .await
        .unwrap();
    let executor = Executor {
        store: store.clone(),
        registry: registrar.registry.clone(),
        config: Config::default(),
    };
    // While service authorization fails, the draining definition keeps its bindings.
    for mode in [DENY, UNAVAILABLE] {
        policy.mode.store(mode, Ordering::SeqCst);
        assert!(executor.reconcile_definitions().await.is_err());
        assert!(registrar.registry.available(d.name(), 0));
    }
    policy.mode.store(ALLOW, Ordering::SeqCst);
    assert_eq!(
        registrar.registration(d.name()).await.unwrap().state,
        RegistrationState::Stopping
    );
    policy.mode.store(ALLOW, Ordering::SeqCst);
    executor.reconcile_definitions().await.unwrap();
    let released = registrar.registration(d.name()).await.unwrap();
    assert_eq!(released.state, RegistrationState::Released);
    assert!(!registrar.registry.available(d.name(), 0));
    policy.mode.store(DENY, Ordering::SeqCst);
    assert!(
        registrar
            .activate(d.name(), released.revision)
            .await
            .is_err()
    );
    policy.mode.store(ALLOW, Ordering::SeqCst);
    assert_eq!(registrar.registration(d.name()).await.unwrap(), released);
}
