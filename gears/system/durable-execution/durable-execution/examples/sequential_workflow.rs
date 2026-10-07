#![allow(clippy::unwrap_used, clippy::expect_used, clippy::use_debug)]
//! Order saga: authorize payment, reserve stock, activate the subscription.
//!
//! Mechanism: a typed `.then` chain where each step receives the previous
//! result, plus an earlier checkpoint (`payment`) that a later step reads
//! through `.uses`. After a crash the chain continues from the last committed
//! step; the payment is never authorized twice.
//!
//! Prerequisites: disposable PostgreSQL, `DURABLE_EXAMPLE_PG_URL` and `DURABLE_EXAMPLE_SECRET`.
//! Run: `cargo run -p cf-gears-durable-execution --example sequential_workflow`
mod support;
use durable_execution_sdk::prelude::*;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use support::fake::IdempotentProvider;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Order {
    id: String,
    sku: String,
    amount_cents: u64,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct PaymentAuthorization {
    order_id: String,
    sku: String,
    auth_id: String,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Reservation {
    reservation_id: String,
    order_id: String,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Activation {
    subscription_id: String,
    payment_ref: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    support::run(async |app: &support::App| {
        let gateway = Arc::new(IdempotentProvider::<String>::default());
        let payments = gateway.clone();
        let builder = WorkflowBuilder::<Order>::new("orders.fulfil.v1").then(support::step(
            "authorize_payment",
            move |ctx, order: Order| {
                let payments = payments.clone();
                async move {
                    let auth_id = payments.call(&ctx.idempotency_key, || {
                        format!("auth-{}-{}", order.id, order.amount_cents)
                    });
                    Ok(PaymentAuthorization {
                        order_id: order.id,
                        sku: order.sku,
                        auth_id,
                    })
                }
            },
        ));
        let payment = builder.checkpoint();
        let payment_dependency = payment.clone();
        let workflow = builder
            .then(support::step(
                "reserve_stock",
                |_, auth: PaymentAuthorization| async move {
                    Ok(Reservation {
                        reservation_id: format!("res-{}-{}", auth.order_id, auth.sku),
                        order_id: auth.order_id,
                    })
                },
            ))
            .then(
                support::step(
                    "activate_subscription",
                    move |ctx, reserved: Reservation| {
                        let payment = payment.clone();
                        async move {
                            // Billing needs the authorization from two steps back.
                            let authorization = ctx.checkpoint(&payment)?;
                            Ok(Activation {
                                subscription_id: format!("sub-{}", reserved.order_id),
                                payment_ref: authorization.auth_id,
                            })
                        }
                    },
                )
                .uses(&payment_dependency),
            )
            .build()?;
        let reference = workflow.reference();
        app.registry.register(workflow.into_definition()).await?;

        let order = Order {
            id: "ord-2001".into(),
            sku: "pro-plan".into(),
            amount_cents: 4_900,
        };
        let started = app
            .client
            .start(
                &app.owner,
                &reference,
                order,
                StartOptions {
                    idempotency_key: Some("checkout:ord-2001".into()),
                    ..Default::default()
                },
            )
            .await?;
        app.wait(started.run_id).await?;

        let activation = app
            .client
            .result(&app.owner, started.run_id, &reference)
            .await?;
        assert_eq!(
            activation,
            Activation {
                subscription_id: "sub-ord-2001".into(),
                payment_ref: "auth-ord-2001-4900".into(),
            }
        );
        let authorization = app
            .client
            .step_result(&app.owner, started.run_id, &payment_dependency)
            .await?;
        assert_eq!(authorization.auth_id, activation.payment_ref);
        assert_eq!(gateway.effects(), 1);
        Ok(())
    })
    .await
}
