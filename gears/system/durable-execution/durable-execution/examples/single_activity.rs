#![allow(clippy::unwrap_used, clippy::expect_used, clippy::use_debug)]
//! Notifications: send an order confirmation email exactly once.
//!
//! Mechanism: one associated-type `Activity`, an idempotent start and an
//! explicitly read typed result. The request key deduplicates the run; the
//! activity key deduplicates the provider call if delivery repeats.
//!
//! Prerequisites: disposable PostgreSQL, `DURABLE_EXAMPLE_PG_URL` and `DURABLE_EXAMPLE_SECRET`.
//! Run: `cargo run -p cf-gears-durable-execution --example single_activity`
mod support;
use durable_execution_sdk::{observation::RunState, prelude::*};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use support::fake::IdempotentProvider;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct ConfirmationEmail {
    order_id: String,
    to: String,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct DeliveryReceipt {
    message_id: String,
}

/// Owns the mail provider client; workflow inputs never carry its credentials.
struct SendOrderConfirmation {
    mailer: Arc<IdempotentProvider<DeliveryReceipt>>,
}
#[async_trait::async_trait]
impl Activity for SendOrderConfirmation {
    type Input = ConfirmationEmail;
    type Output = DeliveryReceipt;
    async fn execute(
        &self,
        ctx: StepContext,
        email: ConfirmationEmail,
    ) -> Result<DeliveryReceipt, ActivityError> {
        // A crash after sending but before the checkpoint repeats this call;
        // the provider-side key makes the repeat a no-op.
        Ok(self.mailer.call(&ctx.idempotency_key, || DeliveryReceipt {
            message_id: format!("msg-{}-to-{}", email.order_id, email.to),
        }))
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    support::run(async |app: &support::App| {
        let mailer = Arc::new(IdempotentProvider::default());
        let workflow =
            WorkflowBuilder::<ConfirmationEmail>::new("notifications.order-confirmation.v1")
                .then(
                    Step::activity(
                        "send_confirmation",
                        SendOrderConfirmation {
                            mailer: mailer.clone(),
                        },
                    )
                    .timeout(std::time::Duration::from_secs(10)),
                )
                .build()?;
        let reference = workflow.reference();
        app.registry.register(workflow.into_definition()).await?;

        let email = ConfirmationEmail {
            order_id: "ord-1001".into(),
            to: "buyer@example.com".into(),
        };
        let options = StartOptions {
            idempotency_key: Some(format!("confirm:{}", email.order_id)),
            ..Default::default()
        };
        let started = app
            .client
            .start(&app.owner, &reference, email.clone(), options.clone())
            .await?;
        // The checkout handler retried its request: same key, same run.
        let repeated = app
            .client
            .start(&app.owner, &reference, email, options)
            .await?;
        assert_eq!(repeated.run_id, started.run_id);

        assert!(matches!(
            app.wait(started.run_id).await?.state,
            RunState::Succeeded { .. }
        ));
        let receipt = app
            .client
            .result(&app.owner, started.run_id, &reference)
            .await?;
        assert_eq!(receipt.message_id, "msg-ord-1001-to-buyer@example.com");
        assert_eq!(mailer.effects(), 1);
        Ok(())
    })
    .await
}
