use super::*;
#[tokio::test]
async fn readiness_guard_clears_health_when_dropped() {
    let ready = Arc::new(AtomicBool::new(false));
    let check = Readiness(ready.clone());
    assert_eq!(
        check.check().await.status,
        toolkit::HealthcheckStatus::Unhealthy
    );
    {
        let _guard = ReadyGuard(ready.clone());
        ready.store(true, Ordering::Release);
        assert_eq!(
            check.check().await.status,
            toolkit::HealthcheckStatus::Healthy
        );
    }
    assert_eq!(
        check.check().await.status,
        toolkit::HealthcheckStatus::Unhealthy
    );
}

struct Configuration(serde_json::Value);
impl toolkit::config::ConfigProvider for Configuration {
    fn get_gear_config(&self, _: &str) -> Option<&serde_json::Value> {
        Some(&self.0)
    }
}
pub struct TenantPolicy(pub uuid::Uuid);
#[async_trait]
impl AuthZResolverApi for TenantPolicy {
    async fn evaluate(
        &self,
        _: toolkit_security::PlatformSecurityContext,
        _: authz_resolver_sdk::models::EvaluationRequest,
    ) -> Result<
        authz_resolver_sdk::models::EvaluationResponse,
        toolkit_canonical_errors::CanonicalError,
    > {
        use authz_resolver_sdk::{
            constraints::{Constraint, InPredicate, Predicate},
            models::{EvaluationResponse, EvaluationResponseContext},
        };
        Ok(EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext {
                constraints: vec![Constraint {
                    predicates: vec![Predicate::In(InPredicate::new(
                        toolkit_security::pep_properties::OWNER_TENANT_ID,
                        [self.0],
                    ))],
                }],
                ..Default::default()
            },
        })
    }
}
struct Identity;
#[async_trait]
impl authn_resolver_sdk::AuthNResolverClient for Identity {
    async fn authenticate(
        &self,
        _: &str,
    ) -> Result<authn_resolver_sdk::AuthenticationResult, authn_resolver_sdk::AuthNResolverError>
    {
        Err(authn_resolver_sdk::AuthNResolverError::NoPluginAvailable)
    }
    async fn exchange_client_credentials(
        &self,
        _: &authn_resolver_sdk::ClientCredentialsRequest,
    ) -> Result<authn_resolver_sdk::AuthenticationResult, authn_resolver_sdk::AuthNResolverError>
    {
        Err(authn_resolver_sdk::AuthNResolverError::NoPluginAvailable)
    }
}
pub async fn context(config: serde_json::Value) -> GearCtx {
    let db = toolkit_db::connect_db("sqlite::memory:", toolkit_db::ConnectOpts::default())
        .await
        .unwrap();
    toolkit_db::migration_runner::run_migrations_for_gear(
        &db,
        "durable-execution",
        DurableExecutionGear::default().migrations(),
    )
    .await
    .unwrap();
    let hub = Arc::new(toolkit::ClientHub::new());
    hub.register::<dyn AuthZResolverApi>(Arc::new(TenantPolicy(uuid::Uuid::nil())));
    hub.register::<dyn authn_resolver_sdk::AuthNResolverClient>(Arc::new(Identity));
    GearCtx::new(
        "durable-execution",
        uuid::Uuid::new_v4(),
        Arc::new(Configuration(serde_json::json!({"config":config}))),
        hub,
        CancellationToken::new(),
    )
    .with_db(toolkit_db::DBProvider::new(db))
}
pub fn system_context() -> toolkit::runtime::SystemContext {
    toolkit::runtime::SystemContext::new(
        uuid::Uuid::new_v4(),
        Arc::new(toolkit::runtime::GearManager::new()),
        Arc::new(toolkit::runtime::GrpcInstallerStore::new()),
    )
}
async fn wait_healthy(check: &dyn Healthcheck) {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while check.check().await.status != toolkit::HealthcheckStatus::Healthy {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn metadata_lifecycle_publishes_sdk_allows_hot_registration_and_clears_readiness_on_shutdown()
{
    struct Fixture;
    #[async_trait]
    impl crate::infra::authorization::WorkerAuthorization for Fixture {
        async fn scope(
            &self,
            _: &str,
        ) -> Result<toolkit_security::AccessScope, crate::infra::storage::StoreError> {
            Ok(toolkit_security::AccessScope::allow_all())
        }
    }
    let ctx = context(serde_json::json!({})).await;
    let gear = Arc::new(DurableExecutionGear::default());
    let check = gear.healthcheck(&ctx).unwrap();
    assert_eq!(check.name(), "durable-execution");
    assert_eq!(
        check.check().await.status,
        toolkit::HealthcheckStatus::Unhealthy
    );
    gear.init(&ctx).await.unwrap();
    let executor = gear.executor.get().unwrap();
    ctx.client_hub().register::<dyn WorkflowRegistry>(Arc::new(
        crate::infra::registrar::Registrar {
            store: executor.store.clone().with_authorization(Arc::new(Fixture)),
            registry: executor.registry.clone(),
        },
    ));
    let registrar = ctx.client_hub().get::<dyn WorkflowRegistry>().unwrap();
    let definition = crate::infra::storage::repository::tests::definition();
    registrar
        .register_contract(definition.contract())
        .await
        .unwrap();
    assert!(durable_execution_sdk::DurableExecutionClient::resolve(&ctx.client_hub()).is_ok());
    gear.post_init(&system_context()).await.unwrap();

    let cancel = CancellationToken::new();
    let task = tokio::spawn(gear.clone().serve(cancel.clone()));
    wait_healthy(check.as_ref()).await;
    registrar.register(definition).await.unwrap();
    cancel.cancel();
    task.await.unwrap().unwrap();
    assert_eq!(
        check.check().await.status,
        toolkit::HealthcheckStatus::Unhealthy
    );
    assert!(
        gear.init(&ctx)
            .await
            .unwrap_err()
            .to_string()
            .contains("initialized twice")
    );
}
#[tokio::test]
async fn initialization_rejects_invalid_config_missing_db_and_uninitialized_post_init() {
    let gear = DurableExecutionGear::default();
    assert!(gear.post_init(&system_context()).await.is_err());
    let invalid = context(serde_json::json!({"heartbeat_secs":40})).await;
    assert!(
        gear.init(&invalid)
            .await
            .unwrap_err()
            .to_string()
            .contains("heartbeat")
    );
    let missing_db = GearCtx::new(
        "durable-execution",
        uuid::Uuid::new_v4(),
        Arc::new(Configuration(serde_json::json!({"config":{}}))),
        Arc::new(toolkit::ClientHub::new()),
        CancellationToken::new(),
    );
    assert!(gear.init(&missing_db).await.is_err());
}
#[cfg(feature = "integration")]
#[path = "../integration/lifecycle_tests.rs"]
mod integration;

#[tokio::test]
async fn metadata_only_lifecycle_reconciles_stopping_without_activity_workers() {
    use durable_execution_sdk::registration::{
        RegistrationState, UnregisterMode, UnregisterOptions,
    };
    struct GlobalGrant;
    #[async_trait]
    impl crate::infra::authorization::WorkerAuthorization for GlobalGrant {
        async fn scope(
            &self,
            _: &str,
        ) -> Result<toolkit_security::AccessScope, crate::infra::storage::StoreError> {
            Ok(toolkit_security::AccessScope::allow_all())
        }
    }
    let ctx = context(serde_json::json!({"dispatch_interval_secs":1})).await;
    let mut gear = DurableExecutionGear::default();
    gear.init(&ctx).await.unwrap();
    let old = gear.executor.take().unwrap();
    let store = crate::infra::storage::repository::tests::store()
        .await
        .with_authorization(Arc::new(GlobalGrant));
    gear.executor
        .set(Arc::new(Executor {
            store: store.clone(),
            registry: old.registry.clone(),
            config: old.config.clone(),
        }))
        .ok()
        .unwrap();
    let registrar = crate::infra::registrar::Registrar {
        store: store.clone(),
        registry: old.registry.clone(),
    };
    let d = crate::infra::storage::repository::tests::definition();
    let registered = registrar.register(d.clone()).await.unwrap();
    store
        .insert(
            toolkit_security::AccessScope::allow_all(),
            crate::infra::storage::repository::tests::journal(),
        )
        .await
        .unwrap();
    gear.post_init(&system_context()).await.unwrap();
    let gear = Arc::new(gear);
    let cancel = CancellationToken::new();
    let serving = tokio::spawn(gear.clone().serve(cancel.clone()));
    wait_healthy(gear.healthcheck(&ctx).unwrap().as_ref()).await;
    registrar
        .unregister(
            &d.name,
            UnregisterOptions {
                mode: UnregisterMode::CancelAndRelease,
                expected_revision: registered.revision,
            },
        )
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            if registrar.registration(&d.name).await.unwrap().state == RegistrationState::Released
                && !old.registry.available(&d.name, 0)
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(!old.registry.available(&d.name, 0));
    cancel.cancel();
    serving.await.unwrap().unwrap();
}

#[tokio::test]
async fn shutdown_interrupts_a_paused_definition_reconciliation() {
    struct Paused(tokio::sync::mpsc::UnboundedSender<()>);
    #[async_trait]
    impl crate::infra::authorization::WorkerAuthorization for Paused {
        async fn scope(
            &self,
            _: &str,
        ) -> Result<toolkit_security::AccessScope, crate::infra::storage::StoreError> {
            self.0.send(()).unwrap();
            std::future::pending().await
        }
    }
    let ctx = context(serde_json::json!({})).await;
    let mut gear = DurableExecutionGear::default();
    gear.init(&ctx).await.unwrap();
    let old = gear.executor.take().unwrap();
    let (entered, mut observed) = tokio::sync::mpsc::unbounded_channel();
    gear.executor
        .set(Arc::new(Executor {
            store: old
                .store
                .clone()
                .with_authorization(Arc::new(Paused(entered))),
            registry: old.registry.clone(),
            config: old.config.clone(),
        }))
        .ok()
        .unwrap();
    gear.post_init(&system_context()).await.unwrap();
    let gear = Arc::new(gear);
    let check = gear.healthcheck(&ctx).unwrap();
    let cancel = CancellationToken::new();
    let task = tokio::spawn(gear.serve(cancel.clone()));
    tokio::time::timeout(std::time::Duration::from_secs(5), observed.recv())
        .await
        .unwrap()
        .unwrap();
    cancel.cancel();
    tokio::time::timeout(std::time::Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(
        check.check().await.status,
        toolkit::HealthcheckStatus::Unhealthy
    );
}

#[tokio::test]
async fn enabled_runtime_rejects_missing_secret_before_queue_preparation() {
    let absent = format!("DURABLE_ABSENT_{}", uuid::Uuid::new_v4().simple()).to_ascii_uppercase();
    assert!(std::env::var_os(&absent).is_none());
    for mode in ["execute_activities", "delivery_enabled"] {
        let ctx = context(serde_json::json!({
            mode: true,
            "service_client_id": "fixture",
            "service_client_secret_env": absent,
            "queue_database_url_env": absent
        }))
        .await;
        let gear = DurableExecutionGear::default();
        gear.init(&ctx).await.unwrap();
        let error = gear.post_init(&system_context()).await.unwrap_err();
        assert_eq!(
            error.to_string(),
            "durable service client secret is not configured"
        );
        assert!(gear.runtime.lock().await.is_none());
    }
    let ctx = context(serde_json::json!({
        "service_client_secret_env": absent,
        "queue_database_url_env": absent
    }))
    .await;
    let gear = DurableExecutionGear::default();
    gear.init(&ctx).await.unwrap();
    gear.post_init(&system_context()).await.unwrap();
}

struct DatabaseFault {
    persistent: bool,
    calls: std::sync::atomic::AtomicUsize,
    observed: tokio::sync::mpsc::UnboundedSender<usize>,
}
#[async_trait]
impl crate::infra::authorization::WorkerAuthorization for DatabaseFault {
    async fn scope(
        &self,
        _: &str,
    ) -> Result<toolkit_security::AccessScope, crate::infra::storage::StoreError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        self.observed.send(call).unwrap();
        if self.persistent || call == 0 {
            Err(crate::infra::storage::StoreError::Database(
                toolkit_db::DbError::from(sea_orm::DbErr::Custom(
                    "fixture database unavailable".into(),
                )),
            ))
        } else {
            Ok(toolkit_security::AccessScope::allow_all())
        }
    }
}

async fn metadata_database_fault(
    persistent: bool,
    interval: u64,
) -> (
    Arc<DurableExecutionGear>,
    Arc<dyn Healthcheck>,
    tokio::sync::mpsc::UnboundedReceiver<usize>,
) {
    let ctx = context(serde_json::json!({
        "dispatch_interval_secs": interval,
        "control_plane_failure_limit": 2
    }))
    .await;
    let mut gear = DurableExecutionGear::default();
    gear.init(&ctx).await.unwrap();
    let old = gear.executor.take().unwrap();
    let (observed, calls) = tokio::sync::mpsc::unbounded_channel();
    gear.executor
        .set(Arc::new(Executor {
            store: old
                .store
                .clone()
                .with_authorization(Arc::new(DatabaseFault {
                    persistent,
                    calls: std::sync::atomic::AtomicUsize::new(0),
                    observed,
                })),
            registry: old.registry.clone(),
            config: old.config.clone(),
        }))
        .ok()
        .unwrap();
    gear.post_init(&system_context()).await.unwrap();
    let check = gear.healthcheck(&ctx).unwrap();
    (Arc::new(gear), check, calls)
}

#[tokio::test]
async fn metadata_controller_retries_database_errors_and_enforces_failure_limit() {
    for persistent in [false, true] {
        let (gear, check, mut calls) = metadata_database_fault(persistent, 1).await;
        let cancel = CancellationToken::new();
        let task = tokio::spawn(gear.serve(cancel.clone()));
        assert_eq!(calls.recv().await, Some(0));
        assert_eq!(
            check.check().await.status,
            toolkit::HealthcheckStatus::Healthy
        );
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(5), calls.recv())
                .await
                .unwrap(),
            Some(1)
        );
        if !persistent {
            assert!(!task.is_finished());
            cancel.cancel();
        }
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), task)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result.is_err(), persistent);
        assert_eq!(
            check.check().await.status,
            toolkit::HealthcheckStatus::Unhealthy
        );
    }
}

#[tokio::test]
async fn metadata_controller_shutdown_interrupts_database_backoff() {
    let (gear, check, mut calls) = metadata_database_fault(true, 60).await;
    let cancel = CancellationToken::new();
    let task = tokio::spawn(gear.serve(cancel.clone()));
    assert_eq!(calls.recv().await, Some(0));
    cancel.cancel();
    tokio::time::timeout(std::time::Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(
        check.check().await.status,
        toolkit::HealthcheckStatus::Unhealthy
    );
}
