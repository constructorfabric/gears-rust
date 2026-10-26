//! Order saga: reverse stock and payment after activation fails.
//! Retry resumes a failed stock compensator before refunding the payment.
mod support;
use serde::{Deserialize, Serialize};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use support::fake::IdempotentProvider;
use toolkit_durable_execution::{observation::RunState, prelude::*};
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Order {
    id: String,
    amount: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Payment {
    order: String,
    authorization: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Reservation {
    order: String,
    reservation: String,
}
/// Owns the payment gateway client, so the step needs no per-call cloning.
struct AuthorizePayment {
    gateway: Arc<IdempotentProvider<String>>,
}
impl Activity for AuthorizePayment {
    type Input = Order;
    type Output = Payment;
    async fn execute(&self, ctx: ActivityContext, order: Order) -> Result<Payment, ActivityError> {
        let authorization = self.gateway.call(ctx.idempotency_key(), || {
            format!("auth-{}-{}", order.id, order.amount)
        });
        Ok(Payment {
            order: order.id,
            authorization,
        })
    }
}
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    support::run(async |app: &support::App| {
        let payment = Arc::new(IdempotentProvider::<String>::default());
        let payment_cleanup = Arc::new(IdempotentProvider::<()>::default());
        let stock_cleanup = Arc::new(IdempotentProvider::<()>::default());
        let blocked = Arc::new(AtomicBool::new(true));
        let refund = payment_cleanup.clone();
        let stock = stock_cleanup.clone();
        let gate = blocked.clone();
        let workflow = WorkflowBuilder::<Order>::new("orders.fulfil.v1")
            .saga()
            .then(
                Step::activity(
                    "authorize_payment",
                    Duration::from_secs(10),
                    AuthorizePayment {
                        gateway: payment.clone(),
                    },
                )
                .compensate(Compensation::new(
                    Duration::from_secs(10),
                    move |ctx, _: Order, _: Payment| {
                        let refund = refund.clone();
                        async move {
                            refund.call(ctx.idempotency_key(), || ());
                            Ok(())
                        }
                    },
                )),
            )
            .then(
                Step::new(
                    "reserve_stock",
                    Duration::from_secs(10),
                    |_, payment: Payment| async move {
                        let reservation = format!("stock-{}", payment.order);
                        Ok(Reservation {
                            order: payment.order,
                            reservation,
                        })
                    },
                )
                .compensate(Compensation::new(
                    Duration::from_secs(10),
                    move |ctx, _: Payment, _: Reservation| {
                        let stock = stock.clone();
                        let gate = gate.clone();
                        async move {
                            if gate.load(Ordering::SeqCst) {
                                return Err(ActivityError::permanent("stock_cleanup_unavailable"));
                            }
                            stock.call(ctx.idempotency_key(), || ());
                            Ok(())
                        }
                    },
                )),
            )
            .then(Step::new(
                "activate",
                Duration::from_secs(10),
                |_, _: Reservation| async move {
                    Err::<String, _>(ActivityError::permanent("activation_rejected"))
                },
            ))
            .build()?;
        let reference = app.registry.register_workflow(workflow).await?.reference;
        let started = app
            .client
            .start(
                &app.owner,
                &reference,
                Order {
                    id: "ord-2001".into(),
                    amount: 4_900,
                },
                StartOptions::new().idempotency_key("checkout:ord-2001"),
            )
            .await?;
        let stopped = app.wait(started.run_id()).await?;
        assert!(matches!(stopped.state, RunState::CompensationFailed { .. }));
        assert_eq!(payment_cleanup.effects(), 0);
        assert_eq!(payment.effects(), 1);
        blocked.store(false, Ordering::SeqCst);
        app.client
            .retry(
                &app.owner,
                started.run_id(),
                ContinueOptions::new(stopped.epoch),
            )
            .await?;
        assert!(matches!(
            app.wait(started.run_id()).await?.state,
            RunState::Compensated { .. }
        ));
        assert_eq!(stock_cleanup.effects(), 1);
        assert_eq!(payment_cleanup.effects(), 1);
        assert_eq!(payment.effects(), 1);
        assert!(
            app.client
                .result(&app.owner, started.handle())
                .await
                .is_err()
        );
        Ok(())
    })
    .await
}
