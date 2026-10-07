#![allow(clippy::unwrap_used, clippy::expect_used, clippy::use_debug)]
//! Notifications with split roles: the public API accepts invoice emails, a worker sends them.
//!
//! Mechanism: the API host registers a serialized contract it received from the
//! workflow owner. It needs no Rust handlers and no SMTP credentials, yet it can start runs.
//! Runs wait without consuming attempts until a worker that holds the credentials
//! binds the local handler for the same contract.
//!
//! Prerequisites: disposable PostgreSQL, `DURABLE_EXAMPLE_PG_URL` and `DURABLE_EXAMPLE_SECRET`.
//! Run: `cargo run -p cf-gears-durable-execution --example contract_and_bindings`
mod support;
use durable_execution_sdk::prelude::*;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use support::fake::IdempotentProvider;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct InvoiceEmail {
    invoice_id: String,
    to: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    support::run(async |app: &support::App| {
        // API role: load the exported contract and accept requests.
        let api_contract: durable_execution_sdk::contracts::ExecutionContract =
            serde_json::from_str(include_str!("contracts/bindings.v1.json"))?;
        let reference = WorkflowRef::<InvoiceEmail, String>::from_contract(api_contract.clone())?;
        app.registry.register_contract(api_contract).await?;
        let started = app
            .client
            .start(
                &app.owner,
                &reference,
                InvoiceEmail {
                    invoice_id: "inv-2026-10-77".into(),
                    to: "billing@customer.example".into(),
                },
                StartOptions {
                    idempotency_key: Some("invoice:inv-2026-10-77".into()),
                    ..Default::default()
                },
            )
            .await?;
        let queued = app.client.get(&app.owner, started.run_id).await?;
        assert_eq!(queued.steps[0].attempts, 0);

        // Worker role: owns the SMTP client and restores the binding (here: later).
        let smtp = Arc::new(IdempotentProvider::<String>::default());
        let mailer = smtp.clone();
        let workflow = WorkflowBuilder::<InvoiceEmail>::new("notifications.invoice-email.v1")
            .then(support::step(
                "send_invoice",
                move |ctx, email: InvoiceEmail| {
                    let mailer = mailer.clone();
                    async move {
                        Ok(mailer.call(&ctx.idempotency_key, || {
                            format!("smtp-{}-{}", email.invoice_id, email.to)
                        }))
                    }
                },
            ))
            .build()?;
        assert_eq!(workflow.contract(), *reference.contract());
        app.registry.register(workflow.into_definition()).await?;

        app.wait(started.run_id).await?;
        assert_eq!(
            app.client
                .result(&app.owner, started.run_id, &reference)
                .await?,
            "smtp-inv-2026-10-77-billing@customer.example"
        );
        assert_eq!(smtp.effects(), 1);
        Ok(())
    })
    .await
}
