//! Framework composition root. The catalog and local bindings remain available while serving.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use authz_resolver_sdk::{AuthZResolverApi, PolicyEnforcer};
use durable_execution_sdk::{DurableExecution, WorkflowRegistry};
use sea_orm_migration::MigrationTrait;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use toolkit::{DatabaseCapability, Gear, GearCtx, RestApiCapability, SystemCapability};
use toolkit::{Healthcheck, HealthcheckResult};

use crate::{
    config::Config, domain::registry::Registry, infra::executor::Executor, infra::runtime::Runtime,
    infra::service::Service, infra::storage::JournalStore,
};

#[toolkit::gear(
    name = "durable-execution",
    deps = [authn_resolver, authz_resolver],
    capabilities = [db, stateful, system, rest],
    lifecycle(entry = "serve", stop_timeout_fn = "shutdown_timeout")
)]
#[derive(Default)]
pub struct DurableExecutionGear {
    executor: OnceLock<Arc<Executor>>,
    runtime: Mutex<Option<Runtime>>,
    ready: Arc<AtomicBool>,
}

#[async_trait]
impl Gear for DurableExecutionGear {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        let config: Config = ctx.config_or_default()?;
        config.validate().map_err(anyhow::Error::msg)?;
        let db = Arc::new(ctx.db_required()?);
        let registry = Arc::new(Registry::default());
        let hub = ctx.client_hub();
        let enforcer = PolicyEnforcer::new(hub.get::<dyn AuthZResolverApi>()?);
        let store = JournalStore::new(db).with_authorization(Arc::new(
            crate::infra::authorization::ServiceAuthorization {
                authn: hub.get::<dyn authn_resolver_sdk::AuthNResolverClient>()?,
                enforcer: PolicyEnforcer::new(hub.get::<dyn AuthZResolverApi>()?),
                config: config.clone(),
            },
        ));
        let service = Arc::new(Service {
            store: store.clone(),
            enforcer,
        });
        hub.register::<dyn DurableExecution>(service.clone());
        hub.register::<dyn durable_execution_sdk::ExecutionInspector>(service);
        hub.register::<dyn WorkflowRegistry>(Arc::new(crate::infra::registrar::Registrar {
            store: store.clone(),
            registry: registry.clone(),
        }));
        self.executor
            .set(Arc::new(Executor {
                store,
                registry,
                config,
            }))
            .map_err(|_| anyhow::anyhow!("durable execution initialized twice"))?;
        Ok(())
    }
}

#[async_trait]
impl SystemCapability for DurableExecutionGear {
    async fn post_init(&self, _ctx: &toolkit::runtime::SystemContext) -> anyhow::Result<()> {
        let executor = self
            .executor
            .get()
            .ok_or_else(|| anyhow::anyhow!("durable execution is not initialized"))?;
        // API publishes the same authorized SDK but never consumes deliveries.
        // The SQLx connection belongs exclusively to the official queue adapter.
        if executor.config.execute_activities || executor.config.delivery_enabled {
            if std::env::var_os(&executor.config.service_client_secret_env).is_none() {
                anyhow::bail!("durable service client secret is not configured");
            }
            let url = std::env::var(&executor.config.queue_database_url_env)
                .map_err(|_| anyhow::anyhow!("durable queue database URL is not configured"))?;
            let runtime = Runtime::prepare(executor.clone(), &url).await?;
            *self.runtime.lock().await = Some(runtime);
        } else {
            executor.registry.validate_bindings()?;
        }
        Ok(())
    }
}

impl DurableExecutionGear {
    fn shutdown_timeout(&self) -> std::time::Duration {
        std::time::Duration::from_secs(
            self.executor
                .get()
                .map_or(Config::default().shutdown_timeout_secs, |executor| {
                    executor.config.shutdown_timeout_secs
                }),
        )
    }

    async fn serve(self: Arc<Self>, cancel: CancellationToken) -> anyhow::Result<()> {
        let runtime = self.runtime.lock().await.take();
        let _readiness = ReadyGuard(self.ready.clone());
        self.ready.store(true, Ordering::Release);
        let executor = self
            .executor
            .get()
            .ok_or_else(|| anyhow::anyhow!("durable execution not initialized"))?;
        let local_stop = cancel.child_token();
        // Runtime owns catalog reconciliation when delivery is enabled.
        // Metadata-only lifecycle supplies the same controller otherwise.
        let runtime_controls_catalog = runtime.is_some();
        let controller = async {
            if runtime_controls_catalog {
                local_stop.cancelled().await;
                return Ok::<(), anyhow::Error>(());
            }
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(
                executor.config.dispatch_interval_secs,
            ));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            let mut retry = crate::infra::retry::RetryPolicy::new(&executor.config);
            loop {
                tokio::select! {biased; ()=local_stop.cancelled()=>return Ok::<(),anyhow::Error>(()), _=tick.tick()=>{}}
                let result = tokio::select! {
                    biased;
                    ()=local_stop.cancelled()=>return Ok(()),
                    result=executor.reconcile_definitions()=>result,
                };
                match result {
                    Ok(()) => retry.reset(),
                    Err(
                        crate::infra::storage::StoreError::AuthorizationUnavailable
                        | crate::infra::storage::StoreError::Authorization(_)
                        | crate::infra::storage::StoreError::Forbidden
                        | crate::infra::storage::StoreError::Conflict,
                    ) => {}
                    Err(
                        error @ (crate::infra::storage::StoreError::Invariant(_)
                        | crate::infra::storage::StoreError::Domain(_)),
                    ) => {
                        tracing::error!(error = %error, "durable definition reconciliation skipped");
                    }
                    Err(error) => {
                        let Some(delay) = retry.retry_delay(&error) else {
                            return Err(error.into());
                        };
                        if !crate::infra::retry::wait_retry(delay, &local_stop).await {
                            return Ok(());
                        }
                    }
                }
            }
        };
        let execution = async {
            if let Some(runtime) = runtime {
                runtime.run(local_stop.clone()).await
            } else {
                local_stop.cancelled().await;
                Ok(())
            }
        };
        tokio::pin!(controller, execution);
        let (control_done, result) = tokio::select! {result=&mut controller=>(true,result),result=&mut execution=>(false,result)};
        local_stop.cancel();
        if control_done {
            execution.await?;
        } else {
            controller.await?;
        }
        result
    }
}

struct ReadyGuard(Arc<AtomicBool>);
impl Drop for ReadyGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

struct Readiness(Arc<AtomicBool>);
#[async_trait]
impl Healthcheck for Readiness {
    fn name(&self) -> &'static str {
        "durable-execution"
    }
    async fn check(&self) -> HealthcheckResult {
        if self.0.load(Ordering::Acquire) {
            HealthcheckResult::healthy()
        } else {
            HealthcheckResult::unhealthy("durable execution runtime is not serving")
        }
    }
}

impl RestApiCapability for DurableExecutionGear {
    fn register_rest(
        &self,
        _: &GearCtx,
        router: axum::Router,
        _: &dyn toolkit::api::OpenApiRegistry,
    ) -> anyhow::Result<axum::Router> {
        Ok(router)
    }
    fn healthcheck(&self, _: &GearCtx) -> Option<Arc<dyn Healthcheck>> {
        Some(Arc::new(Readiness(self.ready.clone())))
    }
}

impl DatabaseCapability for DurableExecutionGear {
    fn migrations(&self) -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(crate::infra::storage::migration::Migration),
            Box::new(crate::infra::storage::delivery_migration::Migration),
            Box::new(crate::infra::storage::normalized_migration::Migration),
            Box::new(crate::infra::storage::events_migration::Migration),
            Box::new(crate::infra::storage::definition_migration::Migration),
        ]
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "../tests/unit/gear_tests.rs"]
pub(crate) mod tests;
