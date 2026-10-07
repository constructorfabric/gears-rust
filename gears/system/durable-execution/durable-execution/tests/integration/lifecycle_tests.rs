use super::*;
use crate::domain::persisted::*;
use crate::infra::{authorization::WorkerAuthorization, storage::StoreError};
use durable_execution_sdk::contracts::{ActivityDefinition, ExecutionDefinition};
struct ControlPlaneFault(Arc<AtomicBool>);
#[async_trait]
impl WorkerAuthorization for ControlPlaneFault {
    async fn scope(&self, _: &str) -> Result<toolkit_security::AccessScope, StoreError> {
        if self.0.load(Ordering::SeqCst) {
            Err(StoreError::DeliveryUnavailable)
        } else {
            Ok(toolkit_security::AccessScope::allow_all())
        }
    }
}
#[tokio::test]
async fn real_runtime_failure_exits_lifecycle_and_clears_readiness() {
    let database = crate::test_postgres::database().await;
    temp_env::async_with_vars([("DURABLE_LIFECYCLE_PG_URL", Some(database.url.as_str())), ("DURABLE_LIFECYCLE_SECRET", Some("fixture-secret"))], async {
    let ctx = context(serde_json::json!({
        "delivery_enabled":true, "service_client_id":"fixture", "service_client_secret_env":"DURABLE_LIFECYCLE_SECRET", "queue_database_url_env":"DURABLE_LIFECYCLE_PG_URL",
        "dispatch_interval_secs":1, "shutdown_timeout_secs":10
    })).await;
    let failed = Arc::new(AtomicBool::new(false));
    let mut gear = DurableExecutionGear::default();
    gear.init(&ctx).await.unwrap();
    let executor = gear.executor.take().unwrap();
    gear.executor
        .set(Arc::new(Executor {
            store: executor
                .store
                .clone()
                .with_authorization(Arc::new(ControlPlaneFault(failed.clone()))),
            registry: executor.registry.clone(),
            config: executor.config.clone(),
        }))
        .ok()
        .unwrap();
    gear.post_init(&system_context()).await.unwrap();
    let gear = Arc::new(gear);
    let check = gear.healthcheck(&ctx).unwrap();
    let task = tokio::spawn(gear.clone().serve(CancellationToken::new()));
    wait_healthy(check.as_ref()).await;
    failed.store(true, Ordering::SeqCst);
    let result = tokio::time::timeout(std::time::Duration::from_secs(20), task)
        .await
        .unwrap()
        .unwrap();
    assert!(result.is_err());
    assert_eq!(
        check.check().await.status,
        toolkit::HealthcheckStatus::Unhealthy
    );
    }).await;
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
    async fn scope(&self, _: &str) -> Result<toolkit_security::AccessScope, StoreError> {
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
impl durable_execution_sdk::contracts::ErasedActivity for ShutdownActivity {
    async fn execute(
        &self,
        ctx: durable_execution_sdk::ActivityContext,
        _: durable_execution_sdk::contracts::ActivityInput,
    ) -> Result<serde_json::Value, durable_execution_sdk::ActivityError> {
        self.active.fetch_add(1, Ordering::SeqCst);
        let _active = ActiveHandler(self.active.clone());
        self.started.send(()).unwrap();
        ctx.cancellation.cancelled().await;
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
impl durable_execution_sdk::contracts::ErasedActivity for Checkpoint {
    async fn execute(
        &self,
        _: durable_execution_sdk::ActivityContext,
        _: durable_execution_sdk::contracts::ActivityInput,
    ) -> Result<serde_json::Value, durable_execution_sdk::ActivityError> {
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
    store: &crate::infra::storage::JournalStore,
    id: durable_execution_sdk::RunId,
    mode: ShutdownMode,
) {
    use crate::domain::persisted::RunStatus;
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
    use durable_execution_sdk::*;
    use sea_orm::{ConnectionTrait, TransactionTrait};
    use std::time::Duration;
    use toolkit::RunnableCapability;
    use toolkit_security::{AccessScope, SecurityContext};
    let database = crate::test_postgres::database().await;
    let seconds = if mode == ShutdownMode::DatabaseStall {
        6
    } else {
        3
    };
    temp_env::async_with_vars(
        [
            ("DURABLE_SHUTDOWN_PG_URL", Some(database.url.as_str())),
            ("DURABLE_SHUTDOWN_SECRET", Some("fixture-secret")),
        ],
        Box::pin(async {
            let queue = format!("shutdown-{}", uuid::Uuid::new_v4());
            let ctx = context(serde_json::json!({
            "execute_activities":true,
            "default_queue":queue, "queues":{queue:2},
            "service_client_id":"fixture", "service_client_secret_env":"DURABLE_SHUTDOWN_SECRET",
            "queue_database_url_env":"DURABLE_SHUTDOWN_PG_URL",
            "dispatch_interval_secs":1, "shutdown_timeout_secs":seconds,
            "lease_secs":30, "heartbeat_secs":2
        })).await;
            let stalled = Arc::new(AtomicBool::new(false));
            let journal_database = crate::infra::storage::repository::tests::isolated_url().await;
            let store = crate::infra::storage::repository::tests::store_at(&journal_database)
                .await
                .with_authorization(Arc::new(ShutdownAuthorization(stalled.clone())));
            let admin = sea_orm::Database::connect(&journal_database.url)
                .await
                .unwrap();
            let mut gear = DurableExecutionGear::default();
            gear.init(&ctx).await.unwrap();
            let old = gear.executor.take().unwrap();
            let executor = Arc::new(Executor {
                store: store.clone(),
                registry: old.registry.clone(),
                config: old.config.clone(),
            });
            gear.executor.set(executor).ok().unwrap();
            let registrar = crate::infra::registrar::Registrar {
                store: store.clone(),
                registry: old.registry.clone(),
            };
            let tenant = uuid::Uuid::new_v4();
            ctx.client_hub()
                .register::<dyn WorkflowRegistry>(Arc::new(registrar));
            ctx.client_hub().register::<dyn DurableExecution>(Arc::new(
                crate::infra::service::Service {
                    store: store.clone(),
                    enforcer: PolicyEnforcer::new(Arc::new(TenantPolicy(tenant))),
                },
            ));
            let registrar = ctx.client_hub().get::<dyn WorkflowRegistry>().unwrap();
            let sdk = ctx.client_hub().get::<dyn DurableExecution>().unwrap();
            let (started, mut starts) = tokio::sync::mpsc::unbounded_channel();
            let (cancelled, mut cancellations) = tokio::sync::mpsc::unbounded_channel();
            let active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let cleaned = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let finish_cleanup = CancellationToken::new();
            registrar
                .register(ExecutionDefinition {
                    name: "lifecycle.shutdown.v1".into(),
                    parallel_groups: vec![],
                    activities: vec![
                        ActivityDefinition {
                            id: ActivityId("checkpoint".into()),
                            timeout: Duration::from_secs(60),
                            retry: RetryPolicy::default(),
                            handler: Arc::new(Checkpoint),
                        },
                        ActivityDefinition {
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

                    flow: None,
                })
                .await
                .unwrap();
            gear.post_init(&system_context()).await.unwrap();
            let health = gear.healthcheck(&ctx).unwrap();
            let lifecycle = gear.into_gear();
            let stop = CancellationToken::new();
            lifecycle.start(stop.clone()).await.unwrap();
            wait_healthy(health.as_ref()).await;
            let owner = SecurityContext::builder()
                .subject_id(uuid::Uuid::new_v4())
                .subject_tenant_id(tenant)
                .subject_type("user")
                .build()
                .unwrap();
            let mut runs = Vec::new();
            for _ in 0..2 {
                runs.push(
                    sdk.start(
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
                    .execute_unprepared("LOCK TABLE durable_runs IN SHARE MODE")
                    .await
                    .unwrap();
                Some(transaction)
            } else {
                None
            };
            let began = tokio::time::Instant::now();
            stop.cancel();
            let stopping =
                tokio::spawn(async move { lifecycle.stop(CancellationToken::new()).await });
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
            tokio::time::timeout(Duration::from_secs(seconds + 2), stopping)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            assert!(
                began.elapsed() < Duration::from_secs(seconds + 1),
                "workers must share one budget"
            );
            if let Some(blocker) = blocker {
                blocker.rollback().await.unwrap();
            }
            assert_eq!(
                health.check().await.status,
                toolkit::HealthcheckStatus::Unhealthy
            );
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
        }),
    )
    .await;
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
