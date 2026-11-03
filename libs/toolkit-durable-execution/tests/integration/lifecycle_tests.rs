use super::*;
use crate::authorization::worker::WorkerAuthorization;
use crate::contracts::ActivityDefinition;
use crate::state::persisted::*;
use crate::store::StoreError;
use std::sync::atomic::AtomicBool;

/// A migrated engine database in a fresh PostgreSQL schema.
async fn engine_database() -> (crate::test_postgres::Database, toolkit_db::Db) {
    let database = crate::store::repository::tests::isolated_url().await;
    let db = toolkit_db::connect_db(&database.url, toolkit_db::ConnectOpts::default())
        .await
        .unwrap();
    toolkit_db::migration_runner::run_migrations_for_testing(&db, crate::migrations())
        .await
        .unwrap();
    (database, db)
}

fn queue(url: &str, name: &str) -> QueueConfig {
    QueueConfig::new(SecretString::from(url.to_owned()), name)
        .concurrency(NonZeroUsize::new(2).unwrap())
}

struct ControlPlaneFault(Arc<AtomicBool>);
#[async_trait]
impl WorkerAuthorization for ControlPlaneFault {
    async fn scope(
        &self,
        _: crate::ServiceAction,
    ) -> Result<toolkit_security::AccessScope, StoreError> {
        if self.0.load(Ordering::SeqCst) {
            Err(StoreError::DeliveryUnavailable)
        } else {
            Ok(toolkit_security::AccessScope::allow_all())
        }
    }
}

#[tokio::test]
async fn real_runtime_failure_exits_lifecycle_and_clears_readiness() {
    let (database, db) = engine_database().await;
    let failed = Arc::new(AtomicBool::new(false));
    let engine = Engine::builder(db, Arc::new(TenantAuthorizer(uuid::Uuid::nil())))
        .mode(ExecutionMode::Dispatcher(queue(
            &database.url,
            &format!("lifecycle-{}", uuid::Uuid::new_v4()),
        )))
        .dispatch_interval(Duration::from_secs(1))
        .shutdown_timeout(Duration::from_secs(10))
        .build()
        .unwrap()
        .with_worker_authorization(Arc::new(ControlPlaneFault(failed.clone())));
    let readiness = engine.readiness();
    let runner = engine.prepare().await.unwrap();
    let task = tokio::spawn(runner.run(CancellationToken::new()));
    wait_ready(&readiness).await;
    failed.store(true, Ordering::SeqCst);
    let result = tokio::time::timeout(Duration::from_secs(20), task)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result.unwrap_err().kind(), LifecycleErrorKind::Storage);
    assert!(!readiness.is_ready());
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ShutdownMode {
    Cooperative,
    Uncooperative,
    AuthorizationStall,
    DatabaseStall,
}

struct ShutdownAuthorization(Arc<AtomicBool>);
#[async_trait]
impl WorkerAuthorization for ShutdownAuthorization {
    async fn scope(
        &self,
        _: crate::ServiceAction,
    ) -> Result<toolkit_security::AccessScope, StoreError> {
        if self.0.load(Ordering::SeqCst) {
            std::future::pending().await
        } else {
            Ok(toolkit_security::AccessScope::allow_all())
        }
    }
}

struct ShutdownActivity {
    mode: ShutdownMode,
    started: tokio::sync::mpsc::UnboundedSender<()>,
    cancelled: tokio::sync::mpsc::UnboundedSender<()>,
    finish_cleanup: CancellationToken,
    active: Arc<std::sync::atomic::AtomicUsize>,
    cleaned: Arc<std::sync::atomic::AtomicUsize>,
}
struct ActiveHandler(Arc<std::sync::atomic::AtomicUsize>);
impl Drop for ActiveHandler {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
#[async_trait]
impl crate::runtime::ErasedActivity for ShutdownActivity {
    async fn execute(
        &self,
        ctx: crate::ActivityContext,
        _: crate::runtime::ActivityInput,
    ) -> Result<serde_json::Value, crate::ActivityError> {
        self.active.fetch_add(1, Ordering::SeqCst);
        let _active = ActiveHandler(self.active.clone());
        self.started.send(()).unwrap();
        ctx.cancellation().cancelled().await;
        self.cancelled.send(()).unwrap();
        if self.mode == ShutdownMode::Uncooperative {
            return std::future::pending().await;
        }
        self.finish_cleanup.cancelled().await;
        self.cleaned.fetch_add(1, Ordering::SeqCst);
        Ok(serde_json::json!("cleanup complete"))
    }
}
struct Checkpoint;
#[async_trait]
impl crate::runtime::ErasedActivity for Checkpoint {
    async fn execute(
        &self,
        _: crate::ActivityContext,
        _: crate::runtime::ActivityInput,
    ) -> Result<serde_json::Value, crate::ActivityError> {
        Ok(serde_json::json!({"checkpoint":42}))
    }
}

async fn wait_for_blocked_write(blocker: &sea_orm::DatabaseTransaction) {
    use sea_orm::ConnectionTrait;
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            let row = blocker.query_one_raw(sea_orm::Statement::from_string(sea_orm::DatabaseBackend::Postgres,
                "SELECT COUNT(*) AS blocked FROM pg_locks WHERE NOT granted AND pg_backend_pid() = ANY(pg_blocking_pids(pid))".to_owned()))
                .await.unwrap().unwrap();
            if row.try_get::<i64>("", "blocked").unwrap() > 0 { break; }
            tokio::task::yield_now().await;
        }
    }).await.unwrap();
}

async fn assert_shutdown_journal(
    store: &crate::store::JournalStore,
    id: crate::RunId,
    mode: ShutdownMode,
) {
    use crate::state::persisted::RunStatus;
    use toolkit_security::AccessScope;
    let journal = store
        .get(&AccessScope::allow_all(), id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        journal.run.activities[0].result,
        Some(serde_json::json!({"checkpoint":42}))
    );
    assert_eq!(journal.run.activities[1].attempts, 1);
    assert!(journal.run.activities[1].result.is_none());
    assert!(!journal.cancellation_requested);
    if mode == ShutdownMode::Cooperative {
        assert_eq!(journal.run.status, RunStatus::Queued);
        assert!(journal.lease_until.is_none());
    } else {
        assert_eq!(journal.run.status, RunStatus::Running);
        assert!(journal.lease_until.unwrap() > chrono::Utc::now());
    }
}

async fn lifecycle_shutdown(mode: ShutdownMode) {
    use crate::*;
    use sea_orm::{ConnectionTrait, TransactionTrait};
    use toolkit_security::{AccessScope, SecurityContext};
    let seconds = if mode == ShutdownMode::DatabaseStall {
        6
    } else {
        3
    };
    {
        {
            let (journal_database, db) = engine_database().await;
            let stalled = Arc::new(AtomicBool::new(false));
            let tenant = uuid::Uuid::new_v4();
            let engine = Engine::builder(db, Arc::new(TenantAuthorizer(tenant)))
                .mode(ExecutionMode::Worker(queue(
                    &journal_database.url,
                    &format!("shutdown-{}", uuid::Uuid::new_v4()),
                )))
                .dispatch_interval(Duration::from_secs(1))
                .shutdown_timeout(Duration::from_secs(seconds))
                .lease(Duration::from_secs(30))
                .heartbeat(Duration::from_secs(2))
                .build()
                .unwrap()
                .with_worker_authorization(Arc::new(ShutdownAuthorization(stalled.clone())));
            let store = engine.executor.store.clone();
            let admin = sea_orm::Database::connect(&journal_database.url)
                .await
                .unwrap();
            let registrar = engine.registrar.clone();
            let sdk = engine.client();
            let (started, mut starts) = tokio::sync::mpsc::unbounded_channel();
            let (cancelled, mut cancellations) = tokio::sync::mpsc::unbounded_channel();
            let active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let cleaned = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let finish_cleanup = CancellationToken::new();
            registrar
                .register(
                    crate::contracts::DefinitionParts {
                        saga: false,
                        name: "lifecycle.shutdown.v1".into(),
                        parallel_groups: vec![],
                        activities: vec![
                            ActivityDefinition {
                                compensation: None,
                                fanout: None,
                                id: ActivityId("checkpoint".into()),
                                timeout: Duration::from_secs(60),
                                retry: RetryPolicy::default(),
                                handler: Arc::new(Checkpoint),
                            },
                            ActivityDefinition {
                                compensation: None,
                                fanout: None,
                                id: ActivityId("work".into()),
                                timeout: Duration::from_secs(60),
                                retry: RetryPolicy::default(),
                                handler: Arc::new(ShutdownActivity {
                                    mode,
                                    started,
                                    cancelled,
                                    finish_cleanup: finish_cleanup.clone(),
                                    active: active.clone(),
                                    cleaned: cleaned.clone(),
                                }),
                            },
                        ],

                        flow: crate::contracts::DataFlowSpec::independent(["checkpoint", "work"]),
                    }
                    .build()
                    .unwrap(),
                )
                .await
                .unwrap();
            let readiness = engine.readiness();
            let runner = engine.prepare().await.unwrap();
            let stop = CancellationToken::new();
            let serving = tokio::spawn(runner.run(stop.clone()));
            wait_ready(&readiness).await;
            let owner = SecurityContext::builder()
                .subject_id(uuid::Uuid::new_v4())
                .subject_tenant_id(tenant)
                .subject_type("user")
                .build()
                .unwrap();
            let mut runs = Vec::new();
            for _ in 0..2 {
                runs.push(
                    sdk.raw()
                        .start(
                            &owner,
                            "lifecycle.shutdown.v1",
                            serde_json::Value::Null,
                            StartOptions::default(),
                        )
                        .await
                        .unwrap()
                        .run_id,
                );
            }
            for _ in 0..2 {
                tokio::time::timeout(Duration::from_secs(30), starts.recv())
                    .await
                    .unwrap()
                    .unwrap();
            }
            assert_eq!(active.load(Ordering::SeqCst), 2);
            if mode == ShutdownMode::AuthorizationStall {
                stalled.store(true, Ordering::SeqCst);
            }
            let blocker = if mode == ShutdownMode::DatabaseStall {
                let transaction = admin.begin().await.unwrap();
                transaction
                    .execute_unprepared("LOCK TABLE durable_execution__runs IN SHARE MODE")
                    .await
                    .unwrap();
                Some(transaction)
            } else {
                None
            };
            let began = tokio::time::Instant::now();
            stop.cancel();
            let stopping = serving;
            for _ in 0..2 {
                tokio::time::timeout(Duration::from_secs(1), cancellations.recv())
                    .await
                    .unwrap()
                    .unwrap();
            }
            for id in runs.iter().filter(|_| mode != ShutdownMode::DatabaseStall) {
                let journal = store
                    .get(&AccessScope::allow_all(), *id)
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(journal.run.status, RunStatus::Running);
                assert!(journal.lease_until.is_some());
            }
            finish_cleanup.cancel();
            if let Some(blocker) = blocker.as_ref() {
                wait_for_blocked_write(blocker).await;
            }
            let stopped = tokio::time::timeout(Duration::from_secs(seconds + 2), stopping)
                .await
                .unwrap()
                .unwrap();
            if matches!(
                mode,
                ShutdownMode::AuthorizationStall | ShutdownMode::DatabaseStall
            ) {
                // The stalled release outlives the budget: the host learns that work
                // did not drain, and the retained claims are checked below.
                assert_eq!(
                    stopped.unwrap_err().kind(),
                    LifecycleErrorKind::ShutdownTimedOut
                );
            } else {
                stopped.unwrap();
            }
            assert!(
                began.elapsed() < Duration::from_secs(seconds + 1),
                "workers must share one budget"
            );
            if let Some(blocker) = blocker {
                blocker.rollback().await.unwrap();
            }
            assert!(!readiness.is_ready());
            tokio::time::timeout(Duration::from_secs(1), async {
                while active.load(Ordering::SeqCst) != 0 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            assert_eq!(
                cleaned.load(Ordering::SeqCst),
                if mode == ShutdownMode::Uncooperative {
                    0
                } else {
                    2
                }
            );
            for id in runs {
                assert_shutdown_journal(&store, id, mode).await;
            }
        }
    }
}

#[tokio::test]
async fn lifecycle_drains_active_workers_after_cleanup_preserving_checkpoints() {
    lifecycle_shutdown(ShutdownMode::Cooperative).await;
}
#[tokio::test]
async fn lifecycle_bounds_uncooperative_workers_and_retains_claims() {
    lifecycle_shutdown(ShutdownMode::Uncooperative).await;
}
#[tokio::test]
async fn lifecycle_bounds_stalled_release_authorization_and_retains_claims() {
    lifecycle_shutdown(ShutdownMode::AuthorizationStall).await;
}

#[tokio::test]
async fn lifecycle_bounds_blocked_database_release_and_retains_claims() {
    lifecycle_shutdown(ShutdownMode::DatabaseStall).await;
}
