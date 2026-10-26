//! Shared standalone bootstrap. Use a disposable PostgreSQL database.
#![allow(dead_code, reason = "shared by examples that each use a subset")]
pub mod fake;
use async_trait::async_trait;
use secrecy::SecretString;
use std::{num::NonZeroUsize, sync::Arc, time::Duration};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use toolkit_durable_execution::{
    AuthorizationError, Authorizer, CallerAction, ConstraintPolicy, Engine, ExecutionInspector,
    ExecutionMode, LifecycleError, QueueConfig, ServiceAction,
    observation::{RunProgress, RunStatus},
    prelude::*,
};
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

pub const TENANT: Uuid = Uuid::from_u128(0xda11);
pub const OWNER: Uuid = Uuid::from_u128(0xda12);
pub const OTHER_TENANT: Uuid = Uuid::from_u128(0xda21);
pub const OTHER_OWNER: Uuid = Uuid::from_u128(0xda22);

/// Stand-in for the platform policy: owners see their tenant's runs; the service
/// identity works across both example tenants and manages the catalog.
///
/// A production gear implements [`Authorizer`] with its policy enforcer and a
/// cached service identity instead; see the crate README.
pub struct ExampleAuthorizer;

#[async_trait]
impl Authorizer for ExampleAuthorizer {
    async fn caller_scope(
        &self,
        ctx: &SecurityContext,
        _: CallerAction,
    ) -> Result<AccessScope, AuthorizationError> {
        match ctx.subject_id() {
            OWNER => Ok(AccessScope::for_tenant(TENANT)),
            OTHER_OWNER => Ok(AccessScope::for_tenant(OTHER_TENANT)),
            _ => Err(AuthorizationError::denied()),
        }
    }
    async fn service_scope(
        &self,
        action: ServiceAction,
    ) -> Result<AccessScope, AuthorizationError> {
        Ok(match action.constraints() {
            ConstraintPolicy::Required => AccessScope::for_tenants(vec![TENANT, OTHER_TENANT]),
            _ => AccessScope::allow_all(),
        })
    }
}

/// The PostgreSQL URL of the disposable example database.
pub fn database_url() -> anyhow::Result<String> {
    let url = std::env::var("DURABLE_EXAMPLE_PG_URL")?;
    anyhow::ensure!(
        url.starts_with("postgres:") || url.starts_with("postgresql:"),
        "examples require PostgreSQL"
    );
    Ok(url)
}

/// Apalis uses a shared private schema: distinct journal schemas need distinct queues.
/// Restarting the same fixture retains its queue identity.
pub fn queue_name(url: &str) -> String {
    url.split("search_path%3D")
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
        )
}

/// Connect and apply the durable-execution migrations, as the host does for a gear.
pub async fn database(url: &str) -> anyhow::Result<toolkit_db::Db> {
    let db = toolkit_db::connect_db(url, toolkit_db::ConnectOpts::default()).await?;
    toolkit_db::migration_runner::run_migrations_for_gear(
        &db,
        "durable-examples",
        toolkit_durable_execution::migrations(),
    )
    .await?;
    Ok(db)
}

/// A worker engine with the timings used by the examples. `queue` must be unique
/// among the engines that share the queue database.
pub fn engine(db: toolkit_db::Db, dsn: SecretString, queue: String) -> anyhow::Result<Engine> {
    let queue =
        QueueConfig::new(dsn, queue).concurrency(NonZeroUsize::new(4).unwrap_or(NonZeroUsize::MIN));
    Ok(Engine::builder(db, Arc::new(ExampleAuthorizer))
        .mode(ExecutionMode::Worker(queue))
        .dispatch_interval(Duration::from_secs(1))
        .shutdown_timeout(Duration::from_secs(6))
        .lease(Duration::from_secs(30))
        .heartbeat(Duration::from_secs(2))
        .build()?)
}

pub fn caller(subject: Uuid, tenant: Uuid) -> anyhow::Result<SecurityContext> {
    Ok(SecurityContext::builder()
        .subject_id(subject)
        .subject_tenant_id(tenant)
        .subject_type("user")
        .build()?)
}

pub struct App {
    pub client: ExecutionClient,
    pub registry: WorkflowRegistry,
    pub inspector: ExecutionInspector,
    pub owner: SecurityContext,
    pub other_owner: SecurityContext,
    stop: CancellationToken,
    serving: parking_lot::Mutex<Option<JoinHandle<Result<(), LifecycleError>>>>,
}

impl App {
    pub async fn start() -> anyhow::Result<Self> {
        let url = database_url()?;
        let queue = queue_name(&url);
        let engine = engine(database(&url).await?, SecretString::from(url), queue)?;
        let (client, registry, inspector) =
            (engine.client(), engine.registry(), engine.inspector());
        let readiness = engine.readiness();
        let runner = engine.prepare().await?;
        let stop = CancellationToken::new();
        let mut serving = tokio::spawn(runner.run(stop.clone()));
        // Wait for readiness, but surface the runner's own error if it stops first,
        // and never leave it running detached.
        let ready = async {
            while !readiness.is_ready() {
                tokio::task::yield_now().await;
            }
        };
        tokio::select! {
            outcome = tokio::time::timeout(Duration::from_secs(15), ready) => {
                if outcome.is_err() {
                    stop.cancel();
                    serving.await??;
                    anyhow::bail!("durable engine did not become ready");
                }
            }
            finished = &mut serving => {
                finished??;
                anyhow::bail!("durable engine stopped before it became ready");
            }
        }
        Ok(Self {
            client: client.with_poll_interval(Duration::from_millis(25)),
            registry,
            inspector,
            owner: caller(OWNER, TENANT)?,
            other_owner: caller(OTHER_OWNER, OTHER_TENANT)?,
            stop,
            serving: parking_lot::Mutex::new(Some(serving)),
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
    pub async fn shutdown(&self) -> anyhow::Result<()> {
        self.stop.cancel();
        let serving = self.serving.lock().take();
        if let Some(serving) = serving {
            serving.await??;
        }
        Ok(())
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
    I: toolkit_durable_execution::workflow::Payload,
    O: toolkit_durable_execution::workflow::Payload,
    F: Fn(ActivityContext, I) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<O, ActivityError>> + Send + 'static,
{
    Step::new(id, Duration::from_secs(60), handler)
}
