use super::*;
use crate::authorization::worker::WorkerAuthorization;
use crate::registration::{RegistrationState, UnregisterMode, UnregisterOptions};
use crate::store::StoreError;
use crate::store::repository::tests::{definition, journal};
use crate::test_support::TenantAuthorizer;
use async_trait::async_trait;
use toolkit_security::AccessScope;

async fn database() -> toolkit_db::Db {
    let db = toolkit_db::connect_db("sqlite::memory:", toolkit_db::ConnectOpts::default())
        .await
        .unwrap();
    toolkit_db::migration_runner::run_migrations_for_gear(
        &db,
        "durable-consumer",
        crate::migrations(),
    )
    .await
    .unwrap();
    db
}

async fn builder() -> EngineBuilder {
    Engine::builder(
        database().await,
        Arc::new(TenantAuthorizer(uuid::Uuid::nil())),
    )
}

fn queue(name: &str) -> QueueConfig {
    QueueConfig::new(SecretString::from("postgres://fixture"), name)
}

async fn next_call(calls: &mut tokio::sync::mpsc::UnboundedReceiver<usize>) -> Option<usize> {
    tokio::time::timeout(Duration::from_secs(5), calls.recv())
        .await
        .unwrap()
}

async fn wait_ready(readiness: &Readiness) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !readiness.is_ready() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn readiness_is_set_only_while_serving() {
    let engine = builder().await.build().unwrap();
    let readiness = engine.readiness();
    assert!(!readiness.is_ready());
    let runner = engine.prepare().await.unwrap();
    assert!(!readiness.is_ready());
    let cancel = CancellationToken::new();
    let task = tokio::spawn(runner.run(cancel.clone()));
    wait_ready(&readiness).await;
    cancel.cancel();
    // Not ready from the moment shutdown starts, before the drain completes.
    assert!(!readiness.is_ready());
    task.await.unwrap().unwrap();
    assert!(!readiness.is_ready());
}

#[tokio::test]
async fn build_reports_the_first_invalid_setting() {
    let cases: Vec<(EngineBuilder, &str)> = vec![
        (
            builder().await.heartbeat(Duration::from_secs(40)),
            "heartbeat",
        ),
        (builder().await.lease(Duration::from_millis(1_500)), "lease"),
        (builder().await.lease(Duration::from_secs(86_401)), "lease"),
        (
            builder().await.dispatch_interval(Duration::ZERO),
            "dispatch_interval",
        ),
        (
            builder()
                .await
                .shutdown_timeout(Duration::from_secs(86_401)),
            "shutdown_timeout",
        ),
        (
            builder()
                .await
                .control_plane_failure_limit(NonZeroU32::new(129).unwrap()),
            "control_plane_failure_limit",
        ),
        (
            builder().await.authorization_timeout(Duration::ZERO),
            "authorization_timeout",
        ),
        (
            builder().await.mode(ExecutionMode::Worker(
                queue("orders").route("sync.v1", "missing"),
            )),
            "routes",
        ),
        (
            builder().await.mode(ExecutionMode::Dispatcher(
                queue("orders").queue("invalid.name", NonZeroUsize::MIN),
            )),
            "queues",
        ),
    ];
    for (builder, setting) in cases {
        let error = builder.build().unwrap_err();
        assert_eq!(error.setting(), setting, "{error}");
        assert!(error.to_string().contains(setting));
    }
}

#[tokio::test]
async fn queue_routes_resolve_against_declared_queues() {
    let queue = QueueConfig::new(
        SecretString::from("postgres://user:secret@host/db"),
        "orders",
    )
    .concurrency(NonZeroUsize::new(4).unwrap())
    .queue("fast", NonZeroUsize::MIN)
    .route("sync.v1", "fast");
    assert!(!format!("{queue:?}").contains("secret"));
    let engine = builder()
        .await
        .mode(ExecutionMode::Worker(queue))
        .shutdown_timeout(Duration::from_secs(7))
        .build()
        .unwrap();
    let config = &engine.executor.config;
    assert!(config.execute_activities);
    assert_eq!(
        config.queues,
        [("fast".into(), 1), ("orders".into(), 4)].into()
    );
    assert_eq!(config.queue_for("sync.v1"), "fast");
    assert_eq!(config.queue_for("sync.v2"), "orders");
    assert_eq!(engine.shutdown_timeout(), Duration::from_secs(7));
    assert!(!format!("{engine:?}").contains("secret"));
}

#[tokio::test]
async fn control_plane_allows_hot_registration_and_clears_readiness_on_shutdown() {
    let engine = builder().await.build().unwrap();
    let registry = engine.registry();
    let registrar = engine.registrar.clone();
    let bindings = engine.executor.registry.clone();
    let definition = definition();
    registry
        .register_contract(definition.contract().clone())
        .await
        .unwrap();
    let readiness = engine.readiness();
    let runner = engine.prepare().await.unwrap();
    let cancel = CancellationToken::new();
    let task = tokio::spawn(runner.run(cancel.clone()));
    wait_ready(&readiness).await;
    let name = definition.name().to_owned();
    assert!(!bindings.available(&name, 0));
    registrar.register(definition).await.unwrap();
    // Registered while serving: the handlers are bound at once.
    assert!(bindings.available(&name, 0));
    cancel.cancel();
    task.await.unwrap().unwrap();
    assert!(!readiness.is_ready());
}

#[tokio::test]
async fn control_plane_reconciles_stopping_without_activity_workers() {
    let engine = builder()
        .await
        .dispatch_interval(Duration::from_secs(1))
        .build()
        .unwrap();
    let registry = engine.registry();
    let bindings = engine.executor.registry.clone();
    let d = definition();
    let registered = engine.registrar.register(d.clone()).await.unwrap();
    // Seed a run as a delivery node would; this process has no queue.
    let store = engine.executor.store.clone();
    store
        .start_outbox(crate::store::repository::tests::ParkedDelivery)
        .await
        .unwrap()
        .stop()
        .await;
    store
        .insert(AccessScope::allow_all(), journal())
        .await
        .unwrap();
    let readiness = engine.readiness();
    let runner = engine.prepare().await.unwrap();
    let cancel = CancellationToken::new();
    let serving = tokio::spawn(runner.run(cancel.clone()));
    wait_ready(&readiness).await;
    registry
        .unregister(
            d.name(),
            UnregisterOptions::new(UnregisterMode::CancelAndRelease, registered.revision),
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if registry.registration(d.name()).await.unwrap().state == RegistrationState::Released
                && !bindings.available(d.name(), 0)
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    cancel.cancel();
    serving.await.unwrap().unwrap();
}

#[tokio::test]
async fn shutdown_interrupts_a_paused_definition_reconciliation() {
    struct Paused(tokio::sync::mpsc::UnboundedSender<()>);
    #[async_trait]
    impl WorkerAuthorization for Paused {
        async fn scope(&self, _: crate::ServiceAction) -> Result<AccessScope, StoreError> {
            self.0.send(()).unwrap();
            std::future::pending().await
        }
    }
    let (entered, mut observed) = tokio::sync::mpsc::unbounded_channel();
    let engine = builder()
        .await
        .build()
        .unwrap()
        .with_worker_authorization(Arc::new(Paused(entered)));
    let readiness = engine.readiness();
    let runner = engine.prepare().await.unwrap();
    let cancel = CancellationToken::new();
    let task = tokio::spawn(runner.run(cancel.clone()));
    tokio::time::timeout(Duration::from_secs(5), observed.recv())
        .await
        .unwrap()
        .unwrap();
    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(!readiness.is_ready());
}

struct DatabaseFault {
    persistent: bool,
    calls: std::sync::atomic::AtomicUsize,
    observed: tokio::sync::mpsc::UnboundedSender<usize>,
}

#[async_trait]
impl WorkerAuthorization for DatabaseFault {
    async fn scope(&self, _: crate::ServiceAction) -> Result<AccessScope, StoreError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        self.observed.send(call).unwrap();
        if self.persistent || call == 0 {
            Err(StoreError::Database(toolkit_db::DbError::from(
                sea_orm::DbErr::Custom("fixture database unavailable".into()),
            )))
        } else {
            Ok(AccessScope::allow_all())
        }
    }
}

async fn database_fault(
    persistent: bool,
    interval: u64,
) -> (
    Runner,
    Readiness,
    tokio::sync::mpsc::UnboundedReceiver<usize>,
) {
    let (observed, calls) = tokio::sync::mpsc::unbounded_channel();
    let engine = builder()
        .await
        .dispatch_interval(Duration::from_secs(interval))
        .control_plane_failure_limit(NonZeroU32::new(2).unwrap())
        .build()
        .unwrap()
        .with_worker_authorization(Arc::new(DatabaseFault {
            persistent,
            calls: std::sync::atomic::AtomicUsize::new(0),
            observed,
        }));
    let readiness = engine.readiness();
    (engine.prepare().await.unwrap(), readiness, calls)
}

#[tokio::test]
async fn controller_retries_database_errors_and_enforces_failure_limit() {
    for persistent in [false, true] {
        let (runner, readiness, mut calls) = database_fault(persistent, 1).await;
        let cancel = CancellationToken::new();
        let task = tokio::spawn(runner.run(cancel.clone()));
        assert_eq!(next_call(&mut calls).await, Some(0));
        assert!(readiness.is_ready());
        assert_eq!(next_call(&mut calls).await, Some(1));
        if !persistent {
            assert!(!task.is_finished());
            cancel.cancel();
        }
        let result = tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            result.as_ref().err().map(LifecycleError::kind),
            persistent.then_some(LifecycleErrorKind::Storage)
        );
        if persistent {
            // Exactly `limit` attempts before the controller gives up.
            assert!(calls.try_recv().is_err());
        }
        assert!(!readiness.is_ready());
    }
}

#[tokio::test]
async fn controller_shutdown_interrupts_database_backoff() {
    let (runner, readiness, mut calls) = database_fault(true, 60).await;
    let cancel = CancellationToken::new();
    let task = tokio::spawn(runner.run(cancel.clone()));
    assert_eq!(next_call(&mut calls).await, Some(0));
    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(!readiness.is_ready());
}

#[tokio::test]
async fn controller_pauses_on_authorization_failures_beyond_the_limit() {
    struct Revoked(std::sync::atomic::AtomicUsize);
    #[async_trait]
    impl WorkerAuthorization for Revoked {
        async fn scope(&self, _: crate::ServiceAction) -> Result<AccessScope, StoreError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Err(StoreError::Authorization(
                crate::AuthorizationError::unavailable(),
            ))
        }
    }
    let authorization = Arc::new(Revoked(std::sync::atomic::AtomicUsize::new(0)));
    let engine = builder()
        .await
        .dispatch_interval(Duration::from_secs(1))
        .control_plane_failure_limit(NonZeroU32::MIN)
        .build()
        .unwrap()
        .with_worker_authorization(authorization.clone());
    let readiness = engine.readiness();
    let runner = engine.prepare().await.unwrap();
    let cancel = CancellationToken::new();
    let task = tokio::spawn(runner.run(cancel.clone()));
    tokio::time::timeout(Duration::from_secs(10), async {
        while authorization.0.load(Ordering::SeqCst) < 3 {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    assert!(!task.is_finished());
    assert!(readiness.is_ready());
    cancel.cancel();
    task.await.unwrap().unwrap();
}

/// Never answers, so only the engine's timeout ends the call.
struct StalledAuthorizer;
#[async_trait]
impl crate::Authorizer for StalledAuthorizer {
    async fn caller_scope(
        &self,
        _: &toolkit_security::SecurityContext,
        _: crate::CallerAction,
    ) -> Result<AccessScope, crate::AuthorizationError> {
        std::future::pending().await
    }
    async fn service_scope(
        &self,
        _: crate::ServiceAction,
    ) -> Result<AccessScope, crate::AuthorizationError> {
        std::future::pending().await
    }
}

fn caller(tenant: uuid::Uuid) -> toolkit_security::SecurityContext {
    toolkit_security::SecurityContext::builder()
        .subject_id(uuid::Uuid::new_v4())
        .subject_tenant_id(tenant)
        .build()
        .unwrap()
}

#[tokio::test]
async fn configured_authorization_timeout_bounds_caller_operations() {
    let engine = Engine::builder(database().await, Arc::new(StalledAuthorizer))
        .authorization_timeout(Duration::from_millis(1_000))
        .build()
        .unwrap();
    let began = std::time::Instant::now();
    let error = engine
        .client()
        .get(
            &caller(uuid::Uuid::new_v4()),
            crate::RunId(uuid::Uuid::new_v4()),
        )
        .await
        .unwrap_err();
    assert_eq!(error.kind(), crate::ErrorKind::Unavailable);
    assert!(std::error::Error::source(&error).is_some());
    let elapsed = began.elapsed();
    assert!(
        elapsed >= Duration::from_millis(900) && elapsed < Duration::from_secs(4),
        "{elapsed:?}"
    );
}

#[tokio::test]
async fn control_plane_rejects_scheduling_as_a_failed_precondition() {
    let tenant = uuid::Uuid::new_v4();
    let engine = Engine::builder(database().await, Arc::new(TenantAuthorizer(tenant)))
        .build()
        .unwrap();
    let d = definition();
    engine.registrar.register(d.clone()).await.unwrap();
    let error = engine
        .client()
        .raw()
        .start(
            &caller(tenant),
            d.name(),
            serde_json::Value::Null,
            crate::StartOptions::default(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.kind(), crate::ErrorKind::FailedPrecondition);
    assert_eq!(error.code(), crate::codes::DELIVERY_DISABLED);
    assert!(!error.is_retryable());
}

#[tokio::test]
async fn scheduling_before_delivery_starts_is_retryable() {
    let tenant = uuid::Uuid::new_v4();
    let engine = Engine::builder(database().await, Arc::new(TenantAuthorizer(tenant)))
        .mode(ExecutionMode::Worker(queue("orders")))
        .build()
        .unwrap();
    let d = definition();
    engine.registrar.register(d.clone()).await.unwrap();
    let error = engine
        .client()
        .raw()
        .start(
            &caller(tenant),
            d.name(),
            serde_json::Value::Null,
            crate::StartOptions::default(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.kind(), crate::ErrorKind::Unavailable);
    assert!(error.is_retryable());
    // A failed `prepare` hands the engine back; scheduling stays retryable.
    let (_, engine) = engine.prepare().await.unwrap_err().into_parts();
    let error = engine
        .client()
        .raw()
        .start(
            &caller(tenant),
            d.name(),
            serde_json::Value::Null,
            crate::StartOptions::default(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.kind(), crate::ErrorKind::Unavailable);
}

#[tokio::test]
async fn failed_prepare_hands_the_engine_back_for_a_retry() {
    let engine = builder()
        .await
        .mode(ExecutionMode::Worker(QueueConfig::new(
            SecretString::from("sqlite::memory:"),
            "orders",
        )))
        .build()
        .unwrap();
    let registry = engine.registry();
    registry
        .register_contract(definition().contract().clone())
        .await
        .unwrap();
    let error = engine.prepare().await.unwrap_err();
    assert_eq!(error.kind(), LifecycleErrorKind::QueueUnavailable);
    let (cause, engine) = error.into_parts();
    assert!(std::error::Error::source(&cause).is_some());
    // The same engine, catalog included, is still usable.
    assert!(
        engine
            .registry()
            .registration(definition().name())
            .await
            .is_ok()
    );
}

#[test]
fn runtime_failures_map_to_lifecycle_kinds() {
    let cause = || -> Box<dyn std::error::Error + Send + Sync> { "fixture".into() };
    for (error, kind) in [
        (
            RuntimeError::Bindings(crate::error::domain::DomainError::Internal("fixture")),
            LifecycleErrorKind::IncompleteBindings,
        ),
        (
            RuntimeError::Queue(cause()),
            LifecycleErrorKind::QueueUnavailable,
        ),
        (RuntimeError::Storage(cause()), LifecycleErrorKind::Storage),
        (RuntimeError::Worker(cause()), LifecycleErrorKind::Worker),
        (
            RuntimeError::ShutdownTimedOut,
            LifecycleErrorKind::ShutdownTimedOut,
        ),
    ] {
        let lifecycle = LifecycleError::from(error);
        assert_eq!(lifecycle.kind(), kind);
        assert_eq!(
            std::error::Error::source(&lifecycle).is_some(),
            kind != LifecycleErrorKind::ShutdownTimedOut
        );
    }
}

#[tokio::test]
async fn builder_defaults_and_settings_map_onto_the_engine() {
    let defaults = builder().await.build().unwrap();
    let config = &defaults.executor.config;
    assert!(!config.execute_activities);
    assert_eq!(config.lease_secs, EngineBuilder::DEFAULT_LEASE.as_secs());
    assert_eq!(
        config.heartbeat_secs,
        EngineBuilder::DEFAULT_HEARTBEAT.as_secs()
    );
    assert_eq!(
        config.dispatch_interval_secs,
        EngineBuilder::DEFAULT_DISPATCH_INTERVAL.as_secs()
    );
    assert_eq!(
        config.shutdown_timeout_secs,
        EngineBuilder::DEFAULT_SHUTDOWN_TIMEOUT.as_secs()
    );
    assert_eq!(
        config.control_plane_failure_limit,
        EngineBuilder::DEFAULT_CONTROL_PLANE_FAILURE_LIMIT.get()
    );
    assert_eq!(
        defaults.shutdown_timeout(),
        EngineBuilder::DEFAULT_SHUTDOWN_TIMEOUT
    );

    // Distinct values catch two settings swapped in `build`.
    let custom = builder()
        .await
        .lease(Duration::from_secs(90))
        .heartbeat(Duration::from_secs(7))
        .dispatch_interval(Duration::from_secs(3))
        .shutdown_timeout(Duration::from_secs(11))
        .control_plane_failure_limit(NonZeroU32::new(9).unwrap())
        .build()
        .unwrap();
    let config = &custom.executor.config;
    assert_eq!(
        (
            config.lease_secs,
            config.heartbeat_secs,
            config.dispatch_interval_secs,
            config.shutdown_timeout_secs,
            config.control_plane_failure_limit
        ),
        (90, 7, 3, 11, 9)
    );
}

#[tokio::test]
async fn dispatcher_mode_delivers_without_executing_activities() {
    let engine = builder()
        .await
        .mode(ExecutionMode::Dispatcher(queue("orders")))
        .build()
        .unwrap();
    let config = &engine.executor.config;
    assert!(!config.execute_activities);
    assert_eq!(
        config.queues,
        [("orders".into(), QueueConfig::DEFAULT_CONCURRENCY.get())].into()
    );
    assert_eq!(config.queue_for("any.v1"), "orders");
}

#[tokio::test]
async fn every_duration_setting_requires_whole_seconds_and_the_first_error_wins() {
    let fraction = Duration::from_millis(1_500);
    for (builder, setting) in [
        (builder().await.heartbeat(fraction), "heartbeat"),
        (
            builder().await.dispatch_interval(fraction),
            "dispatch_interval",
        ),
        (
            builder().await.shutdown_timeout(fraction),
            "shutdown_timeout",
        ),
        // Two invalid settings: the earlier one in validation order is reported.
        (builder().await.lease(fraction).heartbeat(fraction), "lease"),
        (
            builder()
                .await
                .authorization_timeout(Duration::ZERO)
                .lease(fraction),
            "authorization_timeout",
        ),
    ] {
        let error = builder.build().unwrap_err();
        assert_eq!(error.setting(), setting, "{error}");
    }
}

#[tokio::test]
async fn dropping_the_running_future_clears_readiness() {
    let engine = builder().await.build().unwrap();
    let readiness = engine.readiness();
    let runner = engine.prepare().await.unwrap();
    // Never cancelled: only the dropped future can clear readiness.
    let task = tokio::spawn(runner.run(CancellationToken::new()));
    wait_ready(&readiness).await;
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert!(!readiness.is_ready());
}

#[tokio::test]
async fn a_handed_back_engine_can_be_prepared_again() {
    let engine = builder()
        .await
        .mode(ExecutionMode::Worker(QueueConfig::new(
            SecretString::from("sqlite::memory:"),
            "orders",
        )))
        .build()
        .unwrap();
    let (_, engine) = engine.prepare().await.unwrap_err().into_parts();
    let (_, engine) = engine.prepare().await.unwrap_err().into_parts();
    assert_eq!(
        engine.prepare().await.unwrap_err().kind(),
        LifecycleErrorKind::QueueUnavailable
    );
}

#[tokio::test]
async fn registering_the_same_workflow_twice_in_one_process_conflicts() {
    let engine = builder().await.build().unwrap();
    let registry = engine.registry();
    let workflow = || {
        crate::workflow::WorkflowBuilder::<u32>::new("engine.twice.v1")
            .then(crate::workflow::Step::new(
                "double",
                Duration::from_secs(5),
                |_, n: u32| async move { Ok(n * 2) },
            ))
            .build()
            .unwrap()
    };
    registry.register_workflow(workflow()).await.unwrap();
    assert!(engine.executor.registry.available("engine.twice.v1", 0));
    let error = registry.register_workflow(workflow()).await.unwrap_err();
    assert_eq!(error.kind(), crate::ErrorKind::AlreadyExists);
}

#[tokio::test]
async fn control_plane_cancels_runs_but_rejects_resuming_them() {
    let tenant = uuid::Uuid::new_v4();
    let subject = uuid::Uuid::new_v4();
    let db = database().await;
    let engine = Engine::builder(db.clone(), Arc::new(TenantAuthorizer(tenant)))
        .build()
        .unwrap();
    let d = definition();
    engine.registrar.register(d.clone()).await.unwrap();
    // Seed the run through a separate store, as a delivery node would; this
    // engine itself has no delivery.
    let seeding = crate::store::JournalStore::new(Arc::new(toolkit_db::DBProvider::new(db)));
    seeding
        .start_outbox(crate::store::repository::tests::ParkedDelivery)
        .await
        .unwrap()
        .stop()
        .await;
    let mut run = journal();
    run.run.owner = crate::ExecutionOwner {
        tenant_id: tenant,
        subject_id: subject,
    };
    let id = run.run.id;
    seeding.insert(AccessScope::allow_all(), run).await.unwrap();

    let ctx = toolkit_security::SecurityContext::builder()
        .subject_id(subject)
        .subject_tenant_id(tenant)
        .build()
        .unwrap();
    let client = engine.client();
    assert_eq!(
        client
            .cancel(&ctx, id, crate::CancelOptions::new(0))
            .await
            .unwrap(),
        crate::CancelResult::Requested
    );
    let cancelled = client.get(&ctx, id).await.unwrap();
    let error = client
        .resume(&ctx, id, crate::ContinueOptions::new(cancelled.epoch))
        .await
        .unwrap_err();
    assert_eq!(error.kind(), crate::ErrorKind::FailedPrecondition);
    assert_eq!(error.code(), crate::codes::DELIVERY_DISABLED);
    let after = client.get(&ctx, id).await.unwrap();
    assert_eq!(after.state, cancelled.state);
    assert_eq!(after.cursor, cancelled.cursor);
}

#[cfg(feature = "integration")]
#[path = "../integration/lifecycle_tests.rs"]
mod integration;
