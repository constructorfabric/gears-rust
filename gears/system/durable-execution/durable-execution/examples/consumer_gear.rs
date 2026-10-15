//! A consumer gear that owns an order-confirmation workflow.
//!
//! Mechanism: the gear resolves `WorkflowRegistry` and `DurableExecutionClient` from
//! `ClientHub` in `init`, registers its workflow when it starts, and keeps the typed
//! `WorkflowRef` for its own API. Callers never see contracts or run handles' internals.
//!
//! A real gear declares the dependency so the host initializes durable execution first:
//! `#[toolkit::gear(name = "orders", deps = [durable_execution], capabilities = [stateful])]`.
//! Here the host is the shared example bootstrap, so the gear is driven by hand.
//!
//! Prerequisites: disposable PostgreSQL, `DURABLE_EXAMPLE_PG_URL` and `DURABLE_EXAMPLE_SECRET`.
//! Run: `cargo run -p cf-gears-durable-execution --example consumer_gear`
mod support;
use async_trait::async_trait;
use durable_execution_sdk::{
    observation::{RunState, RunStatus},
    prelude::*,
};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use support::fake::IdempotentProvider;
use tokio_util::sync::CancellationToken;
use toolkit::{Gear, GearCtx, RunnableCapability};
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

/// Services resolved during `init`.
struct Wiring {
    registry: Arc<dyn WorkflowRegistry>,
    client: DurableExecutionClient,
}

#[derive(Default)]
struct OrdersGear {
    mailer: Arc<IdempotentProvider<String>>,
    wiring: OnceLock<Wiring>,
    confirmations: OnceLock<WorkflowRef<Order, Confirmation>>,
}

#[async_trait]
impl Gear for OrdersGear {
    // Dependency injection only: resolve what the gear needs.
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        let hub = ctx.client_hub();
        let wiring = Wiring {
            registry: hub.get::<dyn WorkflowRegistry>()?,
            client: DurableExecutionClient::resolve(&hub)?,
        };
        self.wiring
            .set(wiring)
            .map_err(|_| anyhow::anyhow!("orders gear initialized twice"))
    }
}

#[async_trait]
impl RunnableCapability for OrdersGear {
    // Register on start, after every gear finished init and post_init.
    async fn start(&self, _cancel: CancellationToken) -> anyhow::Result<()> {
        let workflow = WorkflowBuilder::<Order>::new("orders.confirmation.v1")
            .then(Step::activity(
                "send_confirmation",
                Duration::from_secs(30),
                SendConfirmation {
                    mailer: Arc::clone(&self.mailer),
                },
            ))
            .build()?;
        let registered = self.wiring()?.registry.register_workflow(workflow).await?;
        self.confirmations
            .set(registered.reference)
            .map_err(|_| anyhow::anyhow!("orders gear started twice"))
    }
    async fn stop(&self, _deadline: CancellationToken) -> anyhow::Result<()> {
        Ok(())
    }
}

impl OrdersGear {
    fn wiring(&self) -> anyhow::Result<&Wiring> {
        self.wiring
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
            .ok_or_else(|| anyhow::anyhow!("orders gear is not started"))?;
        let options = StartOptions::new().idempotency_key(format!("confirm:{}", order.id));
        let started = self
            .wiring()?
            .client
            .start(ctx, workflow, order, options)
            .await?;
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
        let client = &self.wiring()?.client;
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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    support::run(async |app: &support::App| {
        let gear = OrdersGear::default();
        gear.init(&app.gear_ctx("orders")).await?;
        gear.start(CancellationToken::new()).await?;

        let order = Order {
            id: "ord-5001".into(),
            email: "buyer@example.com".into(),
        };
        let handle = gear.confirm(&app.owner, order.clone()).await?;
        let retried = gear.confirm(&app.owner, order).await?;
        assert_eq!(handle.id(), retried.id());

        let confirmation = gear.confirmation(&app.owner, &handle).await?;
        anyhow::ensure!(
            confirmation.message_id == "msg-ord-5001",
            "unexpected confirmation receipt"
        );
        assert_eq!(gear.mailer.effects(), 1);
        gear.stop(CancellationToken::new()).await?;
        Ok(())
    })
    .await
}
