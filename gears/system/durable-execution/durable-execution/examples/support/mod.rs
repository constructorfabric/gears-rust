//! Shared standalone bootstrap. Use a disposable PostgreSQL database.
#![allow(dead_code)]
pub mod fake;
#[cfg(test)]
use crate::DurableExecutionGear;
use async_trait::async_trait;
use authn_resolver_sdk::{
    AuthNResolverClient, AuthNResolverError, AuthenticationResult, ClientCredentialsRequest,
};
use authz_resolver_sdk::{
    AuthZResolverApi,
    constraints::{Constraint, InPredicate, Predicate},
    models::{EvaluationRequest, EvaluationResponse, EvaluationResponseContext},
};
#[cfg(not(test))]
use cf_gears_durable_execution::DurableExecutionGear;
use durable_execution_sdk::{
    observation::{RunProgress, RunState, RunStatus},
    prelude::*,
};
use std::{sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;
use toolkit::{
    ClientHub, DatabaseCapability, Gear, GearCtx, RestApiCapability, RunnableCapability,
    SystemCapability,
};
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::{PlatformSecurityContext, SecurityContext, pep_properties};
use uuid::Uuid;

struct Configuration(serde_json::Value);
impl toolkit::config::ConfigProvider for Configuration {
    fn get_gear_config(&self, _: &str) -> Option<&serde_json::Value> {
        Some(&self.0)
    }
}
struct LocalIdentity {
    service: SecurityContext,
    secret: secrecy::SecretString,
}
#[async_trait]
impl AuthNResolverClient for LocalIdentity {
    async fn authenticate(&self, _: &str) -> Result<AuthenticationResult, AuthNResolverError> {
        Err(AuthNResolverError::NoPluginAvailable)
    }
    async fn exchange_client_credentials(
        &self,
        request: &ClientCredentialsRequest,
    ) -> Result<AuthenticationResult, AuthNResolverError> {
        use secrecy::ExposeSecret;
        if request.client_secret.expose_secret() != self.secret.expose_secret()
            || request.client_id != "durable-examples"
        {
            return Err(AuthNResolverError::NoPluginAvailable);
        }
        Ok(AuthenticationResult {
            security_context: self.service.clone(),
        })
    }
}
struct LocalPolicy {
    tenant: Uuid,
    service: Uuid,
    owner: Uuid,
}
#[async_trait]
impl AuthZResolverApi for LocalPolicy {
    async fn evaluate(
        &self,
        _: PlatformSecurityContext,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        let service = request.subject.id == self.service;
        let definition = request.resource.resource_type == "durable_execution.definition";
        let decision = service
            || (request.subject.id == self.owner
                && !definition
                && request.action.name != "cancel_definition");
        let constraints = if service && (definition || request.action.name == "cancel_definition") {
            vec![]
        } else {
            vec![Constraint {
                predicates: vec![Predicate::In(InPredicate::new(
                    pep_properties::OWNER_TENANT_ID,
                    [self.tenant],
                ))],
            }]
        };
        Ok(EvaluationResponse {
            decision,
            context: EvaluationResponseContext {
                constraints,
                ..Default::default()
            },
        })
    }
}
pub struct App {
    pub client: DurableExecutionClient,
    pub registry: Arc<dyn WorkflowRegistry>,
    pub inspector: Arc<dyn ExecutionInspector>,
    pub owner: SecurityContext,
    stop: CancellationToken,
    lifecycle: Arc<dyn RunnableCapability>,
}
impl App {
    pub async fn start() -> anyhow::Result<Self> {
        let url = std::env::var("DURABLE_EXAMPLE_PG_URL")?;
        anyhow::ensure!(
            url.starts_with("postgres:") || url.starts_with("postgresql:"),
            "examples require PostgreSQL"
        );
        let secret = std::env::var("DURABLE_EXAMPLE_SECRET")?;
        let tenant = Uuid::from_u128(0xda11);
        let owner_id = Uuid::from_u128(0xda12);
        let service_id = Uuid::from_u128(0xda13);
        let owner = SecurityContext::builder()
            .subject_id(owner_id)
            .subject_tenant_id(tenant)
            .subject_type("user")
            .build()?;
        let service = SecurityContext::builder()
            .subject_id(service_id)
            .subject_tenant_id(tenant)
            .subject_type("service")
            .build()?;
        let hub = Arc::new(ClientHub::new());
        hub.register::<dyn AuthNResolverClient>(Arc::new(LocalIdentity {
            service,
            secret: secrecy::SecretString::from(secret),
        }));
        hub.register::<dyn AuthZResolverApi>(Arc::new(LocalPolicy {
            tenant,
            service: service_id,
            owner: owner_id,
        }));
        let db = toolkit_db::connect_db(&url, toolkit_db::ConnectOpts::default()).await?;
        let gear = DurableExecutionGear::default();
        toolkit_db::migration_runner::run_migrations_for_gear(
            &db,
            "durable-execution",
            gear.migrations(),
        )
        .await?;
        let stop = CancellationToken::new();
        let ctx=GearCtx::new("durable-execution",Uuid::new_v4(),Arc::new(Configuration(serde_json::json!({"config":{
            "execute_activities":true,"dispatch_interval_secs":1,"service_client_id":"durable-examples",
            "service_client_secret_env":"DURABLE_EXAMPLE_SECRET","queue_database_url_env":"DURABLE_EXAMPLE_PG_URL",
            "shutdown_timeout_secs":6,"lease_secs":30,"heartbeat_secs":2,"default_queue":"examples","queues":{"examples":4}
        }}))),hub.clone(),stop.clone()).with_db(toolkit_db::DBProvider::new(db));
        gear.init(&ctx).await?;
        let health = gear
            .healthcheck(&ctx)
            .ok_or_else(|| anyhow::anyhow!("durable readiness is not registered"))?;
        let system = toolkit::runtime::SystemContext::new(
            Uuid::new_v4(),
            Arc::new(toolkit::runtime::GearManager::new()),
            Arc::new(toolkit::runtime::GrpcInstallerStore::new()),
        );
        gear.post_init(&system).await?;
        let lifecycle: Arc<dyn RunnableCapability> = Arc::new(gear.into_gear());
        lifecycle.start(stop.clone()).await?;
        tokio::time::timeout(Duration::from_secs(15), async {
            while health.check().await.status != toolkit::HealthcheckStatus::Healthy {
                tokio::task::yield_now().await;
            }
        })
        .await?;
        Ok(Self {
            client: DurableExecutionClient::resolve(&hub)?,
            registry: hub.get::<dyn WorkflowRegistry>()?,
            inspector: hub.get::<dyn ExecutionInspector>()?,
            owner,
            stop,
            lifecycle,
        })
    }
    pub async fn wait(&self, id: RunId) -> anyhow::Result<RunProgress> {
        tokio::time::timeout(Duration::from_secs(60), async {
            loop {
                let progress = self.client.get(&self.owner, id).await?;
                if progress.state.is_terminal()
                    || matches!(progress.state, RunState::Blocked { .. })
                {
                    return Ok::<_, CanonicalError>(progress);
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await?
        .map_err(Into::into)
    }
    pub async fn wait_status(&self, id: RunId, status: RunStatus) -> anyhow::Result<RunProgress> {
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let progress = self.client.get(&self.owner, id).await?;
                if progress.state.status() == status {
                    return Ok::<_, CanonicalError>(progress);
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await?
        .map_err(Into::into)
    }
    pub async fn shutdown(&self) -> anyhow::Result<()> {
        self.stop.cancel();
        self.lifecycle.stop(CancellationToken::new()).await
    }
}
pub async fn run<F>(scenario: F) -> anyhow::Result<()>
where
    F: for<'a> AsyncFnOnce(&'a App) -> anyhow::Result<()>,
{
    let app = App::start().await?;
    let result = tokio::time::timeout(Duration::from_secs(120), scenario(&app)).await;
    let stopped = app.shutdown().await;
    result??;
    stopped
}
pub fn step<
    I: durable_execution_sdk::workflow::Payload,
    O: durable_execution_sdk::workflow::Payload,
    F,
    Fut,
>(
    id: &str,
    handler: F,
) -> Step<I, O>
where
    F: Fn(durable_execution_sdk::workflow::StepContext, I) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<O, ActivityError>> + Send + 'static,
{
    Step::new(id, handler).timeout(Duration::from_secs(60))
}
