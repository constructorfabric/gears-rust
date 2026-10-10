#![allow(clippy::unwrap_used, clippy::expect_used, clippy::use_debug)]
//! Order status page: a UI polls checkout progress; a support agent inspects it.
//!
//! Mechanism:
//! - `get` returns payload-free progress plus a cursor; `events` after that cursor tells the
//!   page when to reread progress.
//! - `history` lists attempts.
//! - `ExecutionInspector` lets support staff list and inspect runs under a separate permission,
//!   without seeing the card token or step results.
//!
//! Prerequisites: disposable PostgreSQL, `DURABLE_EXAMPLE_PG_URL` and `DURABLE_EXAMPLE_SECRET`.
//! Run: `cargo run -p cf-gears-durable-execution --example progress_projection`
mod support;
use durable_execution_sdk::{observation::RunStatus, prelude::*};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Checkout {
    order_id: String,
    card_token: String,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Charge {
    charge_id: String,
    order_id: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    support::run(async |app: &support::App| {
        let workflow =
            WorkflowBuilder::<Checkout>::new("orders.checkout.v1")
                .then(support::step(
                    "charge_card",
                    |_, checkout: Checkout| async move {
                        Ok(Charge {
                            charge_id: format!("ch-{}", checkout.card_token.len()),
                            order_id: checkout.order_id,
                        })
                    },
                ))
                .then(support::step(
                    "email_receipt",
                    |_, charge: Charge| async move {
                        Ok(format!("receipt sent for {}", charge.order_id))
                    },
                ))
                .build()?;
        let reference = workflow.reference();
        app.registry.register(workflow.into_definition()).await?;
        let started = app
            .client
            .start(
                &app.owner,
                &reference,
                Checkout {
                    order_id: "ord-3001".into(),
                    card_token: "tok_visa_4242".into(),
                },
                StartOptions::default(),
            )
            .await?;

        // First render of the status page.
        let initial = app.client.get(&app.owner, started.run_id).await?;
        let finished = app.wait(started.run_id).await?;
        // Later polls fetch only events after the rendered cursor, then reread progress.
        let events = app
            .client
            .events(&app.owner, started.run_id, initial.cursor, 100)
            .await?;
        assert!(events.iter().all(|event| event.sequence > initial.cursor));
        // Replace the rendered page only when both its epoch and cursor are current.
        assert!(finished.cursor >= initial.cursor);
        assert_eq!(finished.epoch, initial.epoch);

        // What the browser receives never contains the card token or step results.
        let page = serde_json::to_value(&finished)?;
        assert!(page.get("input").is_none());
        assert!(page["steps"][0].get("result").is_none());
        assert!(!page.to_string().contains("tok_visa_4242"));

        let history = app.client.history(&app.owner, started.run_id).await?;
        assert_eq!(history.steps.len(), 2);
        assert_eq!(history.steps[0].attempts.len(), 1);

        // Support desk: separate inspect/list permission, same payload-free view.
        let inspected = app.inspector.inspect(&app.owner, started.run_id).await?;
        assert_eq!(inspected.cursor, finished.cursor);
        assert!(!serde_json::to_string(&inspected)?.contains("tok_visa_4242"));
        let recent_checkouts = app
            .inspector
            .list(
                &app.owner,
                durable_execution_sdk::observation::WorkflowQuery {
                    since: chrono::Utc::now() - chrono::Duration::hours(1),
                    status: Some(RunStatus::Succeeded),
                    limit: 100,
                    offset: 0,
                },
            )
            .await?;
        assert!(
            recent_checkouts
                .items
                .iter()
                .any(|run| run.id == started.run_id)
        );
        Ok(())
    })
    .await
}
