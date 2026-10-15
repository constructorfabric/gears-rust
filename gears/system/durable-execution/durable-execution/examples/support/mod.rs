//! Shared standalone bootstrap. Use a disposable PostgreSQL database.
#![allow(dead_code)]
pub mod fake;
use async_trait::async_trait;
use authn_resolver_sdk::{
    AuthNResolverClient, AuthNResolverError, AuthenticationResult, ClientCredentialsRequest,
};
use authz_resolver_sdk::{
    AuthZResolverApi,
    constraints::{Constraint, InPredicate, Predicate},
    models::{EvaluationRequest, EvaluationResponse, EvaluationResponseContext},
};
use cf_gears_durable_execution::DurableExecutionGear;
use durable_execution_sdk::{
    observation::{RunProgress, RunStatus},
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
    other_owner: Uuid,
    other_tenant: Uuid,
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
            || ((request.subject.id == self.owner || request.subject.id == self.other_owner)
                && !definition
                && request.action.name != "cancel_definition");
        let constraints = if service && (definition || request.action.name == "cancel_definition") {
            vec![]
        } else {
            vec![Constraint {
                predicates: vec![Predicate::In(InPredicate::new(
                    pep_properties::OWNER_TENANT_ID,
                    if service {
                        vec![self.tenant, self.other_tenant]
                    } else if request.subject.id == self.other_owner {
                        vec![self.other_tenant]
                    } else {
                        vec![self.tenant]
                    },
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
    pub hub: Arc<ClientHub>,
    pub client: DurableExecutionClient,
    pub registry: Arc<dyn WorkflowRegistry>,
    pub inspector: Arc<dyn ExecutionInspector>,
    pub owner: SecurityContext,
    pub other_owner: SecurityContext,
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
            other_owner: Uuid::from_u128(0xda22),
            other_tenant: Uuid::from_u128(0xda21),
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
        // Apalis uses a shared private schema: distinct journal schemas need distinct queues.
        // Restarting the same fixture retains its queue identity.
        let queue = url
            .split("search_path%3D")
            .nth(1)
            .and_then(|value| value.split("%2C").next())
            .filter(|schema| {
                schema.starts_with("durable_test_")
                    && schema
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
            })
            .map_or_else(
                || "examples".to_owned(),
                |schema| format!("examples-{schema}"),
            );
        let ctx=GearCtx::new("durable-execution",Uuid::new_v4(),Arc::new(Configuration(serde_json::json!({"config":{
            "execute_activities":true,"dispatch_interval_secs":1,"service_client_id":"durable-examples",
            "service_client_secret_env":"DURABLE_EXAMPLE_SECRET","queue_database_url_env":"DURABLE_EXAMPLE_PG_URL",
            "shutdown_timeout_secs":6,"lease_secs":30,"heartbeat_secs":2,"default_queue":queue,"queues":{(queue.clone()):4}
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
            hub: hub.clone(),
            client: DurableExecutionClient::resolve(&hub)?
                .with_poll_interval(Duration::from_millis(25)),
            registry: hub.get::<dyn WorkflowRegistry>()?,
            inspector: hub.get::<dyn ExecutionInspector>()?,
            owner,
            other_owner: SecurityContext::builder()
                .subject_id(Uuid::from_u128(0xda22))
                .subject_tenant_id(Uuid::from_u128(0xda21))
                .subject_type("user")
                .build()?,
            stop,
            lifecycle,
        })
    }
    /// Wait until the run is terminal, failed compensation or blocked.
    pub async fn wait(&self, id: RunId) -> anyhow::Result<RunProgress> {
        Ok(
            tokio::time::timeout(Duration::from_secs(60), self.client.wait(&self.owner, id))
                .await??,
        )
    }
    /// Wait until the run reaches `status`.
    pub async fn wait_status(&self, id: RunId, status: RunStatus) -> anyhow::Result<RunProgress> {
        let reached = self
            .client
            .wait_until(&self.owner, id, |p| p.state.status() == status);
        Ok(tokio::time::timeout(Duration::from_secs(30), reached).await??)
    }
    /// Context for a consumer gear hosted next to the durable-execution gear.
    pub fn gear_ctx(&self, name: &str) -> GearCtx {
        GearCtx::new(
            name,
            Uuid::new_v4(),
            Arc::new(Configuration(serde_json::json!({}))),
            self.hub.clone(),
            self.stop.child_token(),
        )
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
/// A step with the 60-second timeout used by every example.
pub fn step<I, O, F, Fut>(id: &str, handler: F) -> Step<I, O>
where
    I: durable_execution_sdk::workflow::Payload,
    O: durable_execution_sdk::workflow::Payload,
    F: Fn(ActivityContext, I) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<O, ActivityError>> + Send + 'static,
{
    Step::new(id, Duration::from_secs(60), handler)
}
