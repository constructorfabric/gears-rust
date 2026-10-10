//! Restarts the real toolkit Outbox at journal/transport/processor-ack boundaries.
use super::*;
use crate::domain::persisted::*;
use crate::{
    domain::registry::Registry,
    infra::{
        executor::Executor,
        runtime::Runtime,
        storage::repository::tests::{definition, isolated_url, journal, store_at},
    },
};
use durable_execution_sdk::contracts::{ActivityInput, ErasedActivity};
use durable_execution_sdk::*;
use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use test_probe::{Phase, Probe};
use tokio_util::sync::CancellationToken;
use toolkit_security::{AccessScope, ScopeConstraint, ScopeFilter, pep_properties};

struct Fault {
    phase: Phase,
    arrived: tokio::sync::Notify,
    release: CancellationToken,
}
#[async_trait]
impl Probe for Fault {
    async fn fail_at(&self, phase: Phase) -> bool {
        if phase != self.phase {
            return false;
        }
        self.arrived.notify_one();
        self.release.cancelled().await;
        true
    }
}
struct Count(Arc<AtomicUsize>);
#[async_trait]
impl ErasedActivity for Count {
    async fn execute(
        &self,
        _: ActivityContext,
        _: ActivityInput,
    ) -> Result<serde_json::Value, ActivityError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(serde_json::json!("saved-checkpoint"))
    }
}
#[expect(
    clippy::unwrap_used,
    reason = "Fixture setup must fail the test immediately if an invariant or dependency is unavailable."
)]
async fn pending(db: &sea_orm::DatabaseConnection) -> i64 {
    db.query_one_raw(Statement::from_string(
        DatabaseBackend::Postgres,
        "SELECT (SELECT COUNT(*) FROM durable_delivery_outbox_incoming) + \
        (SELECT COUNT(*) FROM durable_delivery_outbox_outgoing o \
        JOIN durable_delivery_outbox_processor p ON p.partition_id=o.partition_id \
        WHERE o.seq>p.processed_seq) AS pending"
            .to_owned(),
    ))
    .await
    .unwrap()
    .unwrap()
    .try_get("", "pending")
    .unwrap()
}
#[tokio::test]
async fn outbox_restart_recovers_each_handoff_window_and_duplicates_do_not_repeat_checkpoints() {
    let transport_url = crate::test_postgres::database().await;
    for phase in [
        Phase::BeforeEnqueue,
        Phase::AfterEnqueue,
        Phase::AfterIntentAck,
    ] {
        let url = isolated_url().await;
        let store = store_at(&url).await;
        let db = sea_orm::Database::connect(&*url).await.unwrap();
        let name = format!("handoff-{}", uuid::Uuid::new_v4());
        let queue = Queue::connect(&transport_url, &name, 1).await.unwrap();
        let config = Config {
            execute_activities: true,
            default_queue: name.clone(),
            queues: [(name.clone(), 1)].into(),
            service_client_id: "fixture".into(),
            dispatch_interval_secs: 1,
            shutdown_timeout_secs: 10,
            ..Default::default()
        };
        let calls = Arc::new(AtomicUsize::new(0));
        let mut definition = definition();
        for a in &mut definition.activities {
            a.handler = Arc::new(Count(calls.clone()));
        }
        let registry = Arc::new(Registry::default());
        registry.register(definition).unwrap();
        let fault = Arc::new(Fault {
            phase,
            arrived: tokio::sync::Notify::new(),
            release: CancellationToken::new(),
        });
        let handle = store
            .start_outbox(DeliverToApalis {
                store: store.clone(),
                queues: [(name.clone(), queue.clone())].into(),
                config: config.clone(),
                probe: Some(fault.clone()),
            })
            .await
            .unwrap();
        let run = journal();
        let id = run.run.id;
        store.insert(AccessScope::allow_all(), run).await.unwrap();
        tokio::time::timeout(Duration::from_secs(20), fault.arrived.notified())
            .await
            .unwrap();
        assert_eq!(
            pending(&db).await,
            1,
            "framework message must still be unacknowledged at {phase:?}"
        );
        assert_eq!(
            store
                .deliveries(&AccessScope::allow_all(), Utc::now(), 100)
                .await
                .unwrap()
                .len(),
            usize::from(phase != Phase::AfterIntentAck)
        );
        let queued: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM apalis.jobs WHERE job_type=$1")
            .bind(&name)
            .fetch_one(&queue.test_pool())
            .await
            .unwrap();
        assert_eq!(queued, i64::from(phase != Phase::BeforeEnqueue));
        let stopping = tokio::spawn(async move { handle.stop().await });
        fault.release.cancel();
        tokio::time::timeout(Duration::from_secs(15), stopping)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(pending(&db).await, 1);
        // A new runtime/Outbox instance must discover the persisted command without a wakeup.
        let executor = Arc::new(Executor {
            store: store.clone(),
            registry,
            config,
        });
        let runtime = Runtime::prepare(executor, &transport_url).await.unwrap();
        let stop = CancellationToken::new();
        let task = tokio::spawn(runtime.run(stop.clone()));
        let saved = tokio::time::timeout(Duration::from_secs(40), async {
            loop {
                let current = store
                    .get(&AccessScope::allow_all(), id)
                    .await
                    .unwrap()
                    .unwrap();
                if current.run.status == RunStatus::Succeeded && pending(&db).await == 0 {
                    break current;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await;
        stop.cancel();
        tokio::time::timeout(Duration::from_secs(15), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let saved = saved.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2, "{phase:?}");
        assert!(saved.run.activities.iter().all(|a| a.attempts == 1 && a.result == Some(serde_json::json!("saved-checkpoint"))));
        let queued: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM apalis.jobs WHERE job_type=$1")
            .bind(&name)
            .fetch_one(&queue.test_pool())
            .await
            .unwrap();
        assert!(
            queued >= 2 + i64::from(phase != Phase::BeforeEnqueue),
            "the enqueue-before-ack windows must deliver duplicates"
        );
        db.close().await.unwrap();
    }
}

// Return a valid but temporarily narrower PDP scope, rather than an outage.
struct DispatchAuthorization {
    tenant: uuid::Uuid,
    hidden_tenant: uuid::Uuid,
    allowed: AtomicBool,
}
#[async_trait]
impl crate::infra::authorization::WorkerAuthorization for DispatchAuthorization {
    async fn scope(&self, action: &str) -> Result<AccessScope, StoreError> {
        if action != "dispatch" {
            return Ok(AccessScope::allow_all());
        }
        let tenant = if self.allowed.load(Ordering::SeqCst) {
            self.tenant
        } else {
            self.hidden_tenant
        };
        Ok(AccessScope::single(ScopeConstraint::new(vec![
            ScopeFilter::r#in(pep_properties::OWNER_TENANT_ID, vec![tenant.into()]),
        ])))
    }
}
struct NarrowAfterEnqueue(Arc<DispatchAuthorization>);
#[async_trait]
impl Probe for NarrowAfterEnqueue {
    async fn fail_at(&self, phase: Phase) -> bool {
        if phase == Phase::AfterEnqueue {
            self.0.allowed.store(false, Ordering::SeqCst);
        }
        false
    }
}
struct ObservedDelivery {
    inner: DeliverToApalis,
    results: tokio::sync::mpsc::UnboundedSender<bool>,
}
#[async_trait]
impl LeasedMessageHandler for ObservedDelivery {
    #[expect(
        clippy::unwrap_used,
        reason = "Fixture setup must fail the test immediately if an invariant or dependency is unavailable."
    )]
    async fn handle(&self, message: &OutboxMessage) -> MessageResult {
        let result = self.inner.handle(message).await;
        self.results
            .send(matches!(result, MessageResult::Retry))
            .unwrap();
        result
    }
}

#[tokio::test]
async fn outbox_retries_hidden_run_and_recovers_after_scope_restoration() {
    scope_restoration_recovers_delivery(false).await;
}

#[tokio::test]
async fn outbox_retries_revoked_intent_ack_and_recovers_with_late_handlers() {
    scope_restoration_recovers_delivery(true).await;
}

#[expect(
    clippy::unwrap_used,
    reason = "Fixture setup must fail the test immediately if an invariant or dependency is unavailable."
)]
#[expect(
    clippy::cognitive_complexity,
    reason = "Keep the ordered scope loss, retry, restoration and late binding phases of this end-to-end regression together."
)]
async fn scope_restoration_recovers_delivery(narrow_after_enqueue: bool) {
    let transport_url = crate::test_postgres::database().await;
    let url = isolated_url().await;
    let seed = journal();
    let id = seed.run.id;
    let authorization = Arc::new(DispatchAuthorization {
        tenant: seed.run.owner.tenant_id,
        hidden_tenant: uuid::Uuid::new_v4(),
        allowed: AtomicBool::new(narrow_after_enqueue),
    });
    let store = store_at(&url)
        .await
        .with_authorization(authorization.clone());
    let db = sea_orm::Database::connect(&*url).await.unwrap();
    let name = format!("scope-recovery-{}", uuid::Uuid::new_v4());
    let queue = Queue::connect(&transport_url, &name, 1).await.unwrap();
    let config = Config {
        execute_activities: true,
        default_queue: name.clone(),
        queues: [(name.clone(), 1)].into(),
        service_client_id: "fixture".into(),
        dispatch_interval_secs: 1,
        shutdown_timeout_secs: 10,
        ..Default::default()
    };
    let calls = Arc::new(AtomicUsize::new(0));
    let mut definition = definition();
    for activity in &mut definition.activities {
        activity.handler = Arc::new(Count(calls.clone()));
    }
    // Publish the shared contract without binding local Rust handlers.
    let registry = Arc::new(Registry::default());
    let registrar = crate::infra::registrar::Registrar {
        store: store.clone(),
        registry: registry.clone(),
    };
    registrar
        .register_contract(definition.contract())
        .await
        .unwrap();
    let (results, mut observed) = tokio::sync::mpsc::unbounded_channel();
    let probe: Option<Arc<dyn Probe>> = narrow_after_enqueue
        .then(|| Arc::new(NarrowAfterEnqueue(authorization.clone())) as Arc<dyn Probe>);
    let handle = store
        .start_outbox(ObservedDelivery {
            inner: DeliverToApalis {
                store: store.clone(),
                queues: [(name.clone(), queue.clone())].into(),
                config: config.clone(),
                probe,
            },
            results,
        })
        .await
        .unwrap();
    store.insert(AccessScope::allow_all(), seed).await.unwrap();
    let retry = tokio::time::timeout(Duration::from_secs(25), observed.recv())
        .await
        .unwrap()
        .unwrap();
    handle.stop().await;
    assert_retryable_delivery(&store, &db, retry).await;

    authorization.allowed.store(true, Ordering::SeqCst);
    let executor = Arc::new(Executor {
        store: store.clone(),
        registry,
        config,
    });
    let runtime = Runtime::prepare(executor, &transport_url).await.unwrap();
    let stop = CancellationToken::new();
    let task = tokio::spawn(runtime.run(stop.clone()));
    // Observe a completed transport attempt while handlers are absent. Late
    // binding must refresh the journal intent, even after Apalis consumes it.
    let consumed = wait_for_consumed_delivery(&queue, &name, &db).await;
    if consumed.is_ok() {
        let before = store
            .get(&AccessScope::allow_all(), id)
            .await
            .unwrap()
            .unwrap();
        assert_waiting_for_bindings(&before, &calls);
        registrar.register(definition).await.unwrap();
    }
    let completed = if consumed.is_ok() {
        Some(wait_for_completed_run(&store, id, &db).await)
    } else {
        None
    };
    stop.cancel();
    tokio::time::timeout(Duration::from_secs(15), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    consumed.unwrap();
    let saved = completed.unwrap().unwrap();
    assert_completed_checkpoints(&saved, &calls);
    db.close().await.unwrap();
}

#[expect(
    clippy::unwrap_used,
    reason = "Database observation failures must fail the integration test."
)]
async fn assert_retryable_delivery(
    store: &JournalStore,
    db: &sea_orm::DatabaseConnection,
    retry: bool,
) {
    assert!(
        retry,
        "a scoped miss must keep the shared Outbox message retryable"
    );
    assert_eq!(
        pending(db).await,
        1,
        "the real processor must not acknowledge it"
    );
    assert_eq!(
        store
            .deliveries(&AccessScope::allow_all(), Utc::now(), 100)
            .await
            .unwrap()
            .len(),
        1,
        "the durable intent must remain unacknowledged"
    );
}

fn assert_waiting_for_bindings(journal: &crate::domain::journal::Journal, calls: &AtomicUsize) {
    assert_eq!(journal.run.status, RunStatus::Queued);
    assert!(
        journal
            .run
            .activities
            .iter()
            .all(|activity| activity.attempts == 0)
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

fn assert_completed_checkpoints(journal: &crate::domain::journal::Journal, calls: &AtomicUsize) {
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert!(journal.run.activities.iter().all(|activity| {
        activity.attempts == 1 && activity.result == Some(serde_json::json!("saved-checkpoint"))
    }));
}

#[expect(
    clippy::unwrap_used,
    reason = "Database observation failures must fail the integration test."
)]
async fn wait_for_consumed_delivery(
    queue: &Queue,
    name: &str,
    db: &sea_orm::DatabaseConnection,
) -> Result<(), tokio::time::error::Elapsed> {
    tokio::time::timeout(Duration::from_secs(40), async {
        loop {
            let done: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM apalis.jobs WHERE job_type=$1 AND status='Done'",
            )
            .bind(name)
            .fetch_one(&queue.test_pool())
            .await
            .unwrap();
            if done > 0 && pending(db).await == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
}

#[expect(
    clippy::unwrap_used,
    reason = "Database observation failures must fail the integration test."
)]
async fn wait_for_completed_run(
    store: &JournalStore,
    id: RunId,
    db: &sea_orm::DatabaseConnection,
) -> Result<crate::domain::journal::Journal, tokio::time::error::Elapsed> {
    tokio::time::timeout(Duration::from_secs(40), async {
        loop {
            let current = store
                .get(&AccessScope::allow_all(), id)
                .await
                .unwrap()
                .unwrap();
            if current.run.status == RunStatus::Succeeded && pending(db).await == 0 {
                break current;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
}
