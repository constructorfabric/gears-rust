#![allow(clippy::unwrap_used, clippy::expect_used, clippy::use_debug)]
//! Notifications with split roles: the public API accepts invoice emails, a worker sends them.
//!
//! Mechanism: the API host registers a serialized contract it received from the
//! workflow owner. It needs no Rust handlers and no SMTP credentials, yet it can start runs.
//! Runs wait without consuming attempts until a worker that holds the credentials
//! binds the local handler for the same contract.
//!
//! Prerequisites: disposable PostgreSQL, `DURABLE_EXAMPLE_PG_URL`.
//! Run: `cargo run -p cf-gears-toolkit-durable-execution --example contract_and_bindings`
//!
//! The exported contract in `contracts/bindings.v1.json` must match the worker code.
//! `cargo test -p cf-gears-toolkit-durable-execution --example contract_and_bindings` checks it;
//! set `UPDATE_CONTRACTS=1` to rewrite the file after an intentional change.
mod support;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use support::fake::IdempotentProvider;
use toolkit_durable_execution::{DefinitionError, prelude::*};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct InvoiceEmail {
    invoice_id: String,
    to: String,
}

const CONTRACT: &str = include_str!("contracts/bindings.v1.json");

/// Worker-side definition. Owns the SMTP client; the exported contract carries none of it.
fn invoice_workflow(
    smtp: Arc<IdempotentProvider<String>>,
) -> Result<Workflow<InvoiceEmail, String>, DefinitionError> {
    WorkflowBuilder::<InvoiceEmail>::new("notifications.invoice-email.v1")
        .then(support::step(
            "send_invoice",
            move |ctx, email: InvoiceEmail| {
                let smtp = Arc::clone(&smtp);
                async move {
                    Ok(smtp.call(ctx.idempotency_key(), || {
                        format!("smtp-{}-{}", email.invoice_id, email.to)
                    }))
                }
            },
        ))
        .build()
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    support::run(async |app: &support::App| {
        // API role: load the exported contract and accept requests.
        let api_contract: toolkit_durable_execution::contracts::ExecutionContract =
            serde_json::from_str(CONTRACT)?;
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
                StartOptions::new().idempotency_key("invoice:inv-2026-10-77"),
            )
            .await?;
        let queued = app.client.get(&app.owner, started.run_id()).await?;
        assert_eq!(queued.steps[0].attempts, 0);

        // Worker role: owns the SMTP client and restores the binding (here: later).
        let smtp = Arc::new(IdempotentProvider::<String>::default());
        let workflow = invoice_workflow(Arc::clone(&smtp))?;
        assert_eq!(workflow.contract(), reference.contract());
        app.registry.register_workflow(workflow).await?;

        app.wait(started.run_id()).await?;
        assert_eq!(
            app.client
                .result(&app.owner, &reference.attach(started.run_id()))
                .await?,
            "smtp-inv-2026-10-77-billing@customer.example"
        );
        assert_eq!(smtp.effects(), 1);
        Ok(())
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exported_contract_matches_worker_definition() {
        let workflow = invoice_workflow(Arc::default()).unwrap();
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/examples/contracts/bindings.v1.json"
        );
        if std::env::var_os("UPDATE_CONTRACTS").is_some() {
            let json = serde_json::to_string_pretty(workflow.contract()).unwrap();
            std::fs::write(
                path,
                json + "
",
            )
            .unwrap();
            return;
        }
        let exported: toolkit_durable_execution::contracts::ExecutionContract =
            serde_json::from_str(CONTRACT).unwrap();
        assert_eq!(
            &exported,
            workflow.contract(),
            "contract changed: rerun with UPDATE_CONTRACTS=1 and use a new .vN name if it is incompatible"
        );
    }
}
