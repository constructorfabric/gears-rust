//! Exercises the real executor against the journal, including handler cleanup.
use super::*;
use crate::domain::persisted::*;
use crate::{domain::journal::Journal, infra::storage::repository::tests::store};
use durable_execution_sdk::contracts::{
    ActivityDefinition, ActivityInput, ErasedActivity, ExecutionDefinition,
};
use durable_execution_sdk::registration::{RegistrationState, UnregisterMode, UnregisterOptions};
use durable_execution_sdk::*;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use toolkit_security::AccessScope;

#[derive(Clone, Copy)]
enum Behavior {
    Success,
    GatedSuccess,
    Retry,
    Permanent,
    Panic,
    Cooperative,
    Uncooperative,
}
struct Handler {
    behavior: Behavior,
    calls: Arc<AtomicUsize>,
    cleaned: Arc<AtomicBool>,
    dropped: Arc<AtomicBool>,
    started: tokio::sync::mpsc::UnboundedSender<()>,
    finish: CancellationToken,
}
struct Dropped(Arc<AtomicBool>);
impl Drop for Dropped {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}
#[async_trait::async_trait]
impl ErasedActivity for Handler {
    async fn execute(
        &self,
        ctx: ActivityContext,
        _: ActivityInput,
    ) -> Result<serde_json::Value, ActivityError> {
        let _dropped = Dropped(self.dropped.clone());
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.started.send(()).unwrap();
        match self.behavior {
            Behavior::Success => Ok(serde_json::json!({"checkpoint": 42})),
            Behavior::GatedSuccess => {
                self.finish.cancelled().await;
                Ok(serde_json::json!({"checkpoint": 42}))
            }
            Behavior::Retry => Err(ActivityError::retryable("temporarily_unavailable")),
            Behavior::Permanent => Err(ActivityError::permanent("invalid_input")),
            Behavior::Panic => panic!("fixture handler panic"),
            Behavior::Cooperative => {
                ctx.cancellation.cancelled().await;
                tokio::task::yield_now().await;
                self.cleaned.store(true, Ordering::SeqCst);
                Ok(serde_json::Value::Null)
            }
            Behavior::Uncooperative => std::future::pending().await,
        }
    }
}
struct Fixture {
    executor: Arc<Executor>,
    id: RunId,
    calls: Arc<AtomicUsize>,
    cleaned: Arc<AtomicBool>,
    dropped: Arc<AtomicBool>,
    started: tokio::sync::mpsc::UnboundedReceiver<()>,
    finish: CancellationToken,
}
impl Fixture {
    async fn new(behavior: Behavior, timeout: Duration) -> Self {
        let store = store().await;
        let registry = Arc::new(Registry::default());
        let calls = Arc::new(AtomicUsize::new(0));
        let cleaned = Arc::new(AtomicBool::new(false));
        let dropped = Arc::new(AtomicBool::new(false));
        let (tx, started) = tokio::sync::mpsc::unbounded_channel();
        let finish = CancellationToken::new();
        let definition = ExecutionDefinition {
            name: "executor.behavior.v1".into(),
            parallel_groups: vec![],
            activities: vec![ActivityDefinition {
                id: ActivityId("step".into()),
                timeout,
                retry: RetryPolicy {
                    max_attempts: 2,
                    initial_delay_secs: 1,
                    max_delay_secs: 1,
                },
                handler: Arc::new(Handler {
                    behavior,
                    calls: calls.clone(),
                    cleaned: cleaned.clone(),
                    dropped: dropped.clone(),
                    started: tx,
                    finish: finish.clone(),
                }),
            }],

            flow: None,
        };
        let journal = Journal::new(
            RunId(uuid::Uuid::new_v4()),
            ExecutionOwner {
                tenant_id: uuid::Uuid::new_v4(),
                subject_id: uuid::Uuid::new_v4(),
            },
            &definition.contract(),
            serde_json::Value::Null,
            Utc::now(),
        )
        .unwrap();
        let id = journal.run.id;
        store
            .insert(AccessScope::allow_all(), journal)
            .await
            .unwrap();
        registry.register(definition).unwrap();
        registry.validate_bindings().unwrap();
        Self {
            executor: Arc::new(Executor {
                store,
                registry,
                config: ConfigForTests::config(),
            }),
            id,
            calls,
            cleaned,
            dropped,
            started,
            finish,
        }
    }
    fn spawn(&self, cancel: CancellationToken) -> tokio::task::JoinHandle<Result<(), StoreError>> {
        let executor = self.executor.clone();
        let id = self.id;
        tokio::spawn(async move { executor.execute(id, 0, cancel).await })
    }
    async fn journal(&self) -> Journal {
        self.executor
            .store
            .get(&AccessScope::allow_all(), self.id)
            .await
            .unwrap()
            .unwrap()
    }
    async fn started(&mut self) {
        tokio::time::timeout(Duration::from_secs(10), self.started.recv())
            .await
            .unwrap()
            .unwrap();
    }
}
struct ConfigForTests;
impl ConfigForTests {
    fn config() -> crate::config::Config {
        crate::config::Config {
            lease_secs: 4,
            heartbeat_secs: 1,
            shutdown_timeout_secs: 1,
            ..Default::default()
        }
    }
}
async fn joined(task: tokio::task::JoinHandle<Result<(), StoreError>>) {
    tokio::time::timeout(Duration::from_secs(15), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn retry_permanent_and_panic_persist_distinct_outcomes() {
    for (behavior, status, code) in [
        (
            Behavior::Retry,
            RunStatus::RetryWait,
            "temporarily_unavailable",
        ),
        (Behavior::Permanent, RunStatus::Failed, "invalid_input"),
        (Behavior::Panic, RunStatus::RetryWait, "activity_panicked"),
    ] {
        let fixture = Fixture::new(behavior, Duration::from_secs(30)).await;
        fixture
            .executor
            .execute(fixture.id, 0, CancellationToken::new())
            .await
            .unwrap();
        let saved = fixture.journal().await;
        assert_eq!(saved.run.status, status);
        assert_eq!(saved.run.activities[0].attempts, 1);
        assert_eq!(
            saved.run.activities[0].attempt_history[0]
                .error_code
                .as_deref(),
            Some(code)
        );
        assert_eq!(
            saved.run.next_attempt_at.is_some(),
            status == RunStatus::RetryWait
        );
        assert!(saved.run.activities[0].result.is_none());
        assert!(saved.lease_until.is_none());
        assert_eq!(fixture.calls.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn timeout_records_retry_only_after_cooperative_cleanup() {
    let fixture = Fixture::new(Behavior::Cooperative, Duration::from_millis(100)).await;
    fixture
        .executor
        .execute(fixture.id, 0, CancellationToken::new())
        .await
        .unwrap();
    let saved = fixture.journal().await;
    assert_eq!(saved.run.status, RunStatus::RetryWait);
    assert_eq!(
        saved.run.activities[0].attempt_history[0]
            .error_code
            .as_deref(),
        Some("activity_timeout")
    );
    assert!(fixture.cleaned.load(Ordering::SeqCst));
    assert!(fixture.dropped.load(Ordering::SeqCst));
    assert!(saved.lease_until.is_none());
}

#[tokio::test]
async fn shutdown_releases_only_after_cleanup_and_leaves_no_checkpoint() {
    let mut fixture = Fixture::new(Behavior::Cooperative, Duration::from_secs(30)).await;
    let cancel = CancellationToken::new();
    let task = fixture.spawn(cancel.clone());
    fixture.started().await;
    cancel.cancel();
    joined(task).await;
    let saved = fixture.journal().await;
    assert!(fixture.cleaned.load(Ordering::SeqCst));
    assert_eq!(saved.run.status, RunStatus::Queued);
    assert!(!saved.cancellation_requested);
    assert!(saved.run.activities[0].result.is_none());
    assert!(saved.lease_until.is_none());
    assert_eq!(
        saved.run.activities[0].attempt_history[0]
            .error_code
            .as_deref(),
        Some("lease_expired")
    );
}

#[tokio::test]
async fn unconfirmed_shutdown_or_timeout_retains_claim_until_recovery() {
    for timeout in [Duration::from_millis(100), Duration::from_secs(30)] {
        let mut fixture = Fixture::new(Behavior::Uncooperative, timeout).await;
        let cancel = CancellationToken::new();
        let task = fixture.spawn(cancel.clone());
        fixture.started().await;
        if timeout == Duration::from_secs(30) {
            cancel.cancel();
        }
        joined(task).await;
        let saved = fixture.journal().await;
        assert_eq!(saved.run.status, RunStatus::Running);
        assert!(saved.lease_until.unwrap() > Utc::now());
        assert!(saved.run.activities[0].result.is_none());
        assert!(!fixture.cleaned.load(Ordering::SeqCst));
        assert!(fixture.dropped.load(Ordering::SeqCst));
    }
}

#[tokio::test]
async fn heartbeat_renews_persisted_lease_and_prevents_recovery_of_long_task() {
    let mut fixture = Fixture::new(Behavior::Cooperative, Duration::from_secs(30)).await;
    let cancel = CancellationToken::new();
    let task = fixture.spawn(cancel.clone());
    fixture.started().await;
    let original = fixture.journal().await.lease_until.unwrap();
    let wait = (original - Utc::now()).to_std().unwrap() + Duration::from_millis(200);
    tokio::time::sleep(wait).await;
    fixture.executor.recover().await.unwrap();
    let saved = fixture.journal().await;
    assert!(Utc::now() > original);
    assert_eq!(saved.run.status, RunStatus::Running);
    assert!(saved.lease_until.unwrap() > Utc::now());
    assert_eq!(saved.run.activities[0].attempts, 1);
    cancel.cancel();
    joined(task).await;
    assert!(fixture.cleaned.load(Ordering::SeqCst));
}

#[tokio::test]
async fn ownership_loss_stops_handler_and_fences_its_result() {
    let mut fixture = Fixture::new(Behavior::Cooperative, Duration::from_secs(30)).await;
    let task = fixture.spawn(CancellationToken::new());
    fixture.started().await;
    let mut lost = fixture.journal().await;
    lost.fence += 1;
    fixture
        .executor
        .store
        .save(AccessScope::allow_all(), lost.revision, lost, false)
        .await
        .unwrap();
    joined(task).await;
    let saved = fixture.journal().await;
    assert!(fixture.cleaned.load(Ordering::SeqCst));
    assert!(saved.run.activities[0].result.is_none());
    assert_eq!(saved.run.status, RunStatus::Running);
    assert_eq!(saved.run.activities[0].attempts, 1);
}

#[tokio::test]
async fn user_cancellation_waits_for_handler_and_records_cancelled_attempt() {
    let mut fixture = Fixture::new(Behavior::Cooperative, Duration::from_secs(30)).await;
    let task = fixture.spawn(CancellationToken::new());
    fixture.started().await;
    let mut current = fixture.journal().await;
    current.request_cancel(Utc::now());
    fixture
        .executor
        .store
        .save(AccessScope::allow_all(), current.revision, current, false)
        .await
        .unwrap();
    joined(task).await;
    let saved = fixture.journal().await;
    assert!(fixture.cleaned.load(Ordering::SeqCst));
    assert_eq!(saved.run.status, RunStatus::Cancelled);
    assert_eq!(
        saved.run.activities[0].attempt_history[0].status,
        ActivityStatus::Cancelled
    );
    assert!(saved.run.activities[0].result.is_none());
    assert!(saved.lease_until.is_none());
}

#[tokio::test]
async fn wrong_activity_missing_run_and_terminal_duplicates_never_invoke_handler() {
    let fixture = Fixture::new(Behavior::Success, Duration::from_secs(30)).await;
    for (id, activity) in [
        (RunId(uuid::Uuid::new_v4()), None),
        (fixture.id, Some("wrong-step")),
    ] {
        fixture
            .executor
            .execute_target(
                id,
                0,
                activity,
                CancellationToken::new(),
                &crate::infra::shutdown::ShutdownBudget::new(Duration::from_secs(1)),
            )
            .await
            .unwrap();
    }
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
    fixture
        .executor
        .execute(fixture.id, 0, CancellationToken::new())
        .await
        .unwrap();
    fixture
        .executor
        .execute(fixture.id, 0, CancellationToken::new())
        .await
        .unwrap();
    let saved = fixture.journal().await;
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 1);
    assert_eq!(saved.run.status, RunStatus::Succeeded);
    assert_eq!(
        saved.run.activities[0].result,
        Some(serde_json::json!({"checkpoint":42}))
    );
}

#[tokio::test]
async fn definition_revocation_stops_cooperative_and_uncooperative_handlers_without_checkpoint() {
    for behavior in [Behavior::Cooperative, Behavior::Uncooperative] {
        let mut f = Fixture::new(behavior, Duration::from_secs(90)).await;
        let worker = f.spawn(CancellationToken::new());
        f.started().await;
        let registrar = crate::infra::registrar::Registrar {
            store: f.executor.store.clone(),
            registry: f.executor.registry.clone(),
        };
        let registration = registrar
            .registration("executor.behavior.v1")
            .await
            .unwrap();
        registrar
            .unregister(
                &registration.name,
                UnregisterOptions {
                    mode: UnregisterMode::CancelAndRelease,
                    expected_revision: registration.revision,
                },
            )
            .await
            .unwrap();
        f.executor.reconcile_definitions().await.unwrap();
        worker.await.unwrap().unwrap();
        assert!(f.dropped.load(Ordering::SeqCst));
        let journal = f.journal().await;
        assert!(journal.run.activities[0].result.is_none());
        if matches!(behavior, Behavior::Cooperative) {
            assert!(f.cleaned.load(Ordering::SeqCst));
            assert_eq!(journal.run.status, RunStatus::Cancelled);
        } else {
            assert_eq!(journal.run.status, RunStatus::Cancelling);
            tokio::time::sleep(Duration::from_secs(4)).await;
        }
        f.executor.reconcile_definitions().await.unwrap();
        assert_eq!(
            registrar
                .registration(&registration.name)
                .await
                .unwrap()
                .state,
            RegistrationState::Released
        );
        assert!(!f.executor.registry.available(&registration.name, 0));
    }
}

// Stall the actual authorization request made by monitoring, not the monitor
// future itself. The initial claim and later release/checkpoint requests work.
struct StalledAuthorization {
    action: &'static str,
    skip: usize,
    requests: AtomicUsize,
    entered: tokio::sync::Notify,
    dropped: Arc<AtomicBool>,
}
#[async_trait::async_trait]
impl crate::infra::authorization::WorkerAuthorization for StalledAuthorization {
    async fn scope(&self, action: &str) -> Result<AccessScope, StoreError> {
        if action == self.action && self.requests.fetch_add(1, Ordering::SeqCst) == self.skip {
            let _dropped = Dropped(self.dropped.clone());
            self.entered.notify_one();
            std::future::pending::<()>().await;
        }
        Ok(AccessScope::allow_all())
    }
}
impl Fixture {
    fn stall_authorization(
        &mut self,
        action: &'static str,
        skip: usize,
    ) -> Arc<StalledAuthorization> {
        let authorization = Arc::new(StalledAuthorization {
            action,
            skip,
            requests: AtomicUsize::new(0),
            entered: tokio::sync::Notify::new(),
            dropped: Arc::new(AtomicBool::new(false)),
        });
        self.executor = Arc::new(Executor {
            store: self
                .executor
                .store
                .clone()
                .with_authorization(authorization.clone()),
            registry: self.executor.registry.clone(),
            config: self.executor.config.clone(),
        });
        authorization
    }
}
impl StalledAuthorization {
    async fn entered(&self) {
        tokio::time::timeout(Duration::from_secs(10), self.entered.notified())
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn stalled_monitor_authorization_and_renewal_stop_before_confirmed_lease_expires() {
    for (action, skip) in [("execute", 1), ("heartbeat", 0)] {
        let mut fixture = Fixture::new(Behavior::Cooperative, Duration::from_secs(30)).await;
        let authorization = fixture.stall_authorization(action, skip);
        let worker = fixture.spawn(CancellationToken::new());
        fixture.started().await;
        authorization.entered().await;
        let before = fixture.journal().await;
        let until = before.lease_until.unwrap();
        let result = tokio::time::timeout(Duration::from_secs(10), worker)
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(result, Err(StoreError::AuthorizationUnavailable)));
        assert!(fixture.cleaned.load(Ordering::SeqCst));
        assert!(fixture.dropped.load(Ordering::SeqCst));
        assert!(authorization.dropped.load(Ordering::SeqCst));
        let saved = fixture.journal().await;
        assert!(
            Utc::now() < until,
            "handler must stop before recovery can replace it"
        );
        assert_eq!(saved.lease_until, Some(until));
        assert_eq!(saved.run.status, RunStatus::Running);
        assert_eq!(saved.run.activities[0].attempts, 1);
        assert!(saved.run.activities[0].result.is_none());
        assert!(!saved.cancellation_requested);
    }
}

#[tokio::test]
async fn completed_handler_checkpoints_while_monitor_authorization_is_stalled() {
    let mut fixture = Fixture::new(Behavior::GatedSuccess, Duration::from_secs(30)).await;
    let authorization = fixture.stall_authorization("execute", 1);
    let worker = fixture.spawn(CancellationToken::new());
    fixture.started().await;
    authorization.entered().await;
    fixture.finish.cancel();
    joined(worker).await;
    assert!(authorization.dropped.load(Ordering::SeqCst));
    let saved = fixture.journal().await;
    assert_eq!(saved.run.status, RunStatus::Succeeded);
    assert_eq!(saved.run.activities[0].attempts, 1);
    assert_eq!(
        saved.run.activities[0].result,
        Some(serde_json::json!({"checkpoint": 42}))
    );
    assert!(saved.lease_until.is_none());
}

#[tokio::test]
async fn shutdown_releases_handler_while_monitor_authorization_is_stalled() {
    let mut fixture = Fixture::new(Behavior::Cooperative, Duration::from_secs(30)).await;
    let authorization = fixture.stall_authorization("execute", 1);
    let shutdown = CancellationToken::new();
    let worker = fixture.spawn(shutdown.clone());
    fixture.started().await;
    authorization.entered().await;
    shutdown.cancel();
    joined(worker).await;
    assert!(authorization.dropped.load(Ordering::SeqCst));
    assert!(fixture.cleaned.load(Ordering::SeqCst));
    let saved = fixture.journal().await;
    assert_eq!(saved.run.status, RunStatus::Queued);
    assert!(saved.lease_until.is_none());
    assert!(saved.run.activities[0].result.is_none());
    assert!(!saved.cancellation_requested);
}

#[tokio::test]
async fn activity_timeout_records_retry_while_monitor_authorization_is_stalled() {
    let mut fixture = Fixture::new(Behavior::Cooperative, Duration::from_millis(500)).await;
    let authorization = fixture.stall_authorization("execute", 1);
    let worker = fixture.spawn(CancellationToken::new());
    fixture.started().await;
    authorization.entered().await;
    joined(worker).await;
    assert!(authorization.dropped.load(Ordering::SeqCst));
    assert!(fixture.cleaned.load(Ordering::SeqCst));
    let saved = fixture.journal().await;
    assert_eq!(saved.run.status, RunStatus::RetryWait);
    assert_eq!(
        saved.run.activities[0].attempt_history[0]
            .error_code
            .as_deref(),
        Some("activity_timeout")
    );
    assert!(saved.lease_until.is_none());
    assert!(saved.run.activities[0].result.is_none());
    assert!(!saved.cancellation_requested);
}

#[tokio::test]
async fn claim_persistence_cannot_outlive_its_lease_or_start_handler() {
    let mut fixture = Fixture::new(Behavior::Success, Duration::from_secs(30)).await;
    // The first definition read chooses the contract; the second authorizes the
    // claim write. Stall that real write authorization, before any handler exists.
    let authorization = fixture.stall_authorization("definition:get", 1);
    let worker = fixture.spawn(CancellationToken::new());
    authorization.entered().await;
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
    let result = tokio::time::timeout(Duration::from_secs(10), worker)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(result, Err(StoreError::Conflict)));
    assert!(authorization.dropped.load(Ordering::SeqCst));
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
    let journal = fixture.journal().await;
    assert_eq!(journal.run.status, RunStatus::Queued);
    assert_eq!(journal.run.activities[0].attempts, 0);
    assert!(journal.run.activities[0].result.is_none());
    assert!(journal.lease_until.is_none());
}

#[tokio::test]
async fn shutdown_during_claim_persistence_never_invokes_handler() {
    let mut fixture = Fixture::new(Behavior::Success, Duration::from_secs(30)).await;
    let authorization = fixture.stall_authorization("definition:get", 1);
    let stop = CancellationToken::new();
    let worker = fixture.spawn(stop.clone());
    authorization.entered().await;
    stop.cancel();
    joined(worker).await;
    assert!(authorization.dropped.load(Ordering::SeqCst));
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
    let journal = fixture.journal().await;
    assert_eq!(journal.run.activities[0].attempts, 0);
    assert!(!journal.cancellation_requested);
}

struct ObservedAuthorization(tokio::sync::mpsc::UnboundedSender<tokio::time::Instant>);
#[async_trait::async_trait]
impl crate::infra::authorization::WorkerAuthorization for ObservedAuthorization {
    async fn scope(&self, action: &str) -> Result<AccessScope, StoreError> {
        if action == "execute" {
            self.0.send(tokio::time::Instant::now()).unwrap();
        }
        Ok(AccessScope::allow_all())
    }
}

#[tokio::test]
async fn executor_monitor_uses_configured_heartbeat_cadence() {
    let mut fixture = Fixture::new(Behavior::GatedSuccess, Duration::from_secs(30)).await;
    let (observed, mut calls) = tokio::sync::mpsc::unbounded_channel();
    let config = Config {
        lease_secs: 8,
        heartbeat_secs: 2,
        ..fixture.executor.config.clone()
    };
    fixture.executor = Arc::new(Executor {
        store: fixture
            .executor
            .store
            .clone()
            .with_authorization(Arc::new(ObservedAuthorization(observed))),
        registry: fixture.executor.registry.clone(),
        config,
    });
    let stop = CancellationToken::new();
    let task = fixture.spawn(stop.clone());
    fixture.started().await;
    calls.recv().await.unwrap(); // Admission authorization.
    let first = calls.recv().await.unwrap(); // Immediate first monitor tick.
    assert!(
        tokio::time::timeout(Duration::from_millis(1200), calls.recv())
            .await
            .is_err()
    );
    let second = tokio::time::timeout(Duration::from_secs(2), calls.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(second.duration_since(first) >= Duration::from_millis(1900));
    stop.cancel();
    joined(task).await;
    assert!(fixture.dropped.load(Ordering::SeqCst));
}

#[tokio::test(start_paused = true)]
async fn delayed_monitor_ticks_do_not_burst() {
    let mut interval = monitor_interval(2);
    interval.tick().await;
    tokio::time::advance(Duration::from_secs(9)).await;
    interval.tick().await;
    assert!(
        tokio::time::timeout(Duration::from_secs(1), interval.tick())
            .await
            .is_err()
    );
}

struct InvalidCatalog;
#[async_trait::async_trait]
impl crate::infra::authorization::WorkerAuthorization for InvalidCatalog {
    async fn scope(&self, action: &str) -> Result<AccessScope, StoreError> {
        if action == "definition:get" {
            Err(StoreError::Invariant("invalid catalog fixture"))
        } else {
            Ok(AccessScope::allow_all())
        }
    }
}
#[tokio::test]
async fn catalog_invariant_is_not_reported_as_authorization_outage() {
    let mut fixture = Fixture::new(Behavior::Success, Duration::from_secs(30)).await;
    fixture.executor = Arc::new(Executor {
        store: fixture
            .executor
            .store
            .clone()
            .with_authorization(Arc::new(InvalidCatalog)),
        registry: fixture.executor.registry.clone(),
        config: fixture.executor.config.clone(),
    });
    let error = fixture
        .executor
        .execute(fixture.id, 0, CancellationToken::new())
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        StoreError::Invariant("invalid catalog fixture")
    ));
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.journal().await.run.status, RunStatus::Queued);
}
