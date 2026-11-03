//! A gear that embeds durable execution for its order-confirmation workflow.
//!
//! Mechanism:
//! - `DatabaseCapability::migrations` adds the engine schema to the gear's own migrations.
//! - `Gear::init` reads the gear's configuration and builds the engine. No I/O.
//! - The stateful lifecycle entry `serve` registers the workflow, prepares the engine
//!   and awaits `Runner::run`, so a `LifecycleError` reaches the host. The host's
//!   `WithLifecycle` wrapper cancels `serve` on stop and enforces the stop timeout,
//!   which is the engine's own drain budget.
//!
//! The gear keeps the typed `WorkflowRef` for its own API; callers never see contracts.
//!
//! Prerequisites: disposable PostgreSQL and `DURABLE_EXAMPLE_PG_URL`.
//! Run: `cargo run -p cf-gears-toolkit-durable-execution --example consumer_gear`
mod support;
use async_trait::async_trait;
use parking_lot::Mutex;
use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use support::fake::IdempotentProvider;
use tokio_util::sync::CancellationToken;
use toolkit::{DatabaseCapability, Gear, GearCtx, RunnableCapability};
use toolkit_durable_execution::{
    Engine, Readiness,
    observation::{RunState, RunStatus},
    prelude::*,
};
use toolkit_security::SecurityContext;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Order {
    id: String,
    email: String,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Confirmation {
    message_id: String,
}

/// The gear's provider client; credentials stay here, never in workflow payloads.
struct SendConfirmation {
    mailer: Arc<IdempotentProvider<String>>,
}
impl Activity for SendConfirmation {
    type Input = Order;
    type Output = Confirmation;
    async fn execute(
        &self,
        ctx: ActivityContext,
        order: Order,
    ) -> Result<Confirmation, ActivityError> {
        let message_id = self
            .mailer
            .call(ctx.idempotency_key(), || format!("msg-{}", order.id));
        Ok(Confirmation { message_id })
    }
}

/// The gear's own configuration. In production take the DSN from the credential
/// store rather than plain configuration.
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct OrdersConfig {
    queue_dsn: String,
    /// Unique among all engines that share the queue database.
    queue: String,
}

#[toolkit::gear(
    name = "orders",
    capabilities = [db, stateful],
    lifecycle(entry = "serve", stop_timeout_fn = "stop_timeout")
)]
#[derive(Default)]
struct OrdersGear {
    mailer: Arc<IdempotentProvider<String>>,
    engine: Mutex<Option<Engine>>,
    client: OnceLock<ExecutionClient>,
    readiness: OnceLock<Readiness>,
    shutdown_timeout: OnceLock<Duration>,
    confirmations: OnceLock<WorkflowRef<Order, Confirmation>>,
}

impl DatabaseCapability for OrdersGear {
    fn migrations(&self) -> Vec<Box<dyn sea_orm_migration::MigrationTrait>> {
        // The gear's own migrations come first; the engine schema is independent of them.
        toolkit_durable_execution::migrations()
    }
}

#[async_trait]
impl Gear for OrdersGear {
    // Settings only: build the engine and hand out the run client.
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        let config: OrdersConfig = ctx.config()?;
        let engine = support::engine(
            ctx.db_required()?.db(),
            SecretString::from(config.queue_dsn),
            config.queue,
        )?;
        self.client
            .set(engine.client())
            .map_err(|_| anyhow::anyhow!("orders gear initialized twice"))?;
        self.readiness.get_or_init(|| engine.readiness());
        self.shutdown_timeout
            .get_or_init(|| engine.shutdown_timeout());
        *self.engine.lock() = Some(engine);
        Ok(())
    }
}

impl OrdersGear {
    /// Stateful lifecycle entry: serve until the host cancels, then drain.
    async fn serve(self: Arc<Self>, cancel: CancellationToken) -> anyhow::Result<()> {
        let engine = self
            .engine
            .lock()
            .take()
            .ok_or_else(|| anyhow::anyhow!("orders gear is not initialized"))?;
        let workflow = WorkflowBuilder::<Order>::new("orders.confirmation.v1")
            .then(Step::activity(
                "send_confirmation",
                Duration::from_secs(30),
                SendConfirmation {
                    mailer: Arc::clone(&self.mailer),
                },
            ))
            .build()?;
        let registered = engine.registry().register_workflow(workflow).await?;
        let runner = engine.prepare().await?;
        // Published once delivery is up, so the domain API never schedules into a
        // queue that is not connected yet.
        self.confirmations
            .set(registered.reference)
            .map_err(|_| anyhow::anyhow!("orders gear served twice"))?;
        // A fatal engine failure ends `serve`, so the host learns about it at once.
        Ok(runner.run(cancel).await?)
    }

    /// Stop timeout of the lifecycle: the engine's own drain budget.
    fn stop_timeout(&self) -> Duration {
        self.shutdown_timeout
            .get()
            .copied()
            .unwrap_or(toolkit_durable_execution::EngineBuilder::DEFAULT_SHUTDOWN_TIMEOUT)
    }

    fn client(&self) -> anyhow::Result<&ExecutionClient> {
        self.client
            .get()
            .ok_or_else(|| anyhow::anyhow!("orders gear is not initialized"))
    }

    /// Domain API: start a confirmation once per order, however often checkout retries.
    async fn confirm(
        &self,
        ctx: &SecurityContext,
        order: Order,
    ) -> anyhow::Result<RunHandle<Order, Confirmation>> {
        let workflow = self
            .confirmations
            .get()
            .ok_or_else(|| anyhow::anyhow!("orders gear is not serving"))?;
        let options = StartOptions::new().idempotency_key(format!("confirm:{}", order.id));
        let started = self.client()?.start(ctx, workflow, order, options).await?;
        Ok(started.into_handle())
    }

    /// Domain API: the confirmation once the run has succeeded.
    ///
    /// # Errors
    /// [`ConfirmationError::NotSent`] keeps the safe settled state of a run that did not
    /// succeed; other errors are infrastructure or authorization failures.
    async fn confirmation(
        &self,
        ctx: &SecurityContext,
        handle: &RunHandle<Order, Confirmation>,
    ) -> anyhow::Result<Confirmation> {
        let client = self.client()?;
        let progress =
            tokio::time::timeout(Duration::from_secs(60), client.wait(ctx, handle.id())).await??;
        if !matches!(progress.state, RunState::Succeeded { .. }) {
            return Err(ConfirmationError::NotSent(progress.state).into());
        }
        Ok(client.result(ctx, handle).await?)
    }
}

/// A settled confirmation run that did not send the email.
#[derive(Debug, thiserror::Error)]
enum ConfirmationError {
    /// Failed, blocked, cancelled or compensated; the state carries the safe cause.
    #[error("order confirmation was not sent: {}", status_name(.0.status()))]
    NotSent(RunState),
}
fn status_name(status: RunStatus) -> String {
    serde_json::to_value(status)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_else(|| "unknown".into())
}

/// Configuration provider for the hand-driven host below.
struct Configuration(serde_json::Value);
impl toolkit::config::ConfigProvider for Configuration {
    fn get_gear_config(&self, _: &str) -> Option<&serde_json::Value> {
        Some(&self.0)
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let url = support::database_url()?;
    let lifecycle = Arc::new(OrdersGear::default().into_gear());
    let gear = lifecycle.inner_arc();
    // The host applies `DatabaseCapability::migrations`, runs `init`, then starts the
    // lifecycle; `WithLifecycle` runs `serve` and implements the two-phase stop.
    let db = toolkit_db::connect_db(&url, toolkit_db::ConnectOpts::default()).await?;
    toolkit_db::migration_runner::run_migrations_for_gear(&db, "orders", gear.migrations()).await?;
    let config = serde_json::json!({
        "config": { "queue_dsn": url, "queue": format!("orders-{}", support::queue_name(&url)) }
    });
    let ctx = GearCtx::new(
        "orders",
        uuid::Uuid::new_v4(),
        Arc::new(Configuration(config)),
        Arc::new(toolkit::ClientHub::new()),
        CancellationToken::new(),
    )
    .with_db(toolkit_db::DBProvider::new(db));
    gear.init(&ctx).await?;
    let readiness = gear
        .readiness
        .get()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("orders gear is not initialized"))?;
    let stop = CancellationToken::new();
    lifecycle.start(stop.clone()).await?;
    tokio::time::timeout(Duration::from_secs(15), async {
        while !readiness.is_ready() {
            tokio::task::yield_now().await;
        }
    })
    .await?;

    let owner = support::caller(support::OWNER, support::TENANT)?;
    let order = Order {
        id: "ord-5001".into(),
        email: "buyer@example.com".into(),
    };
    let handle = gear.confirm(&owner, order.clone()).await?;
    let retried = gear.confirm(&owner, order).await?;
    assert_eq!(handle.id(), retried.id());

    let confirmation = gear.confirmation(&owner, &handle).await?;
    anyhow::ensure!(
        confirmation.message_id == "msg-ord-5001",
        "unexpected confirmation receipt"
    );
    assert_eq!(gear.mailer.effects(), 1);
    stop.cancel();
    lifecycle.stop(CancellationToken::new()).await
}
