#![allow(clippy::unwrap_used, clippy::expect_used, clippy::use_debug)]

//! The recommended way to integrate the transactional outbox into a gear.
//!
//! A gear should not let every layer depend on `toolkit_db::outbox`. Instead it
//! owns **two small traits** and keeps the outbox behind one infra seam:
//!
//! - [`OutboxEnqueuer`] — the port the gear's services call. Each method
//!   writes a row inside the caller's transaction and returns a [`Wake`].
//! - [`Wake`] — the post-commit signal. It is *implemented for*
//!   `toolkit_db::outbox::Wake` (a local trait on a foreign type, which the
//!   orphan rule allows) and for a no-op, so services name no outbox type and a
//!   test can drop in [`NoopEnqueuer`].
//!
//! Everything is static dispatch: `impl Wake` / `&impl OutboxEnqueuer`, no `dyn`
//! and no boxing. `toolkit_db::outbox` appears only in the [`Wake`] impl, the
//! [`DbEnqueuer`], [`start_outbox`], and the handler — never in the port, the
//! records, the no-op, or the service.
//!
//! No `anyhow`: every fallible surface returns a typed error — [`EnqueueError`],
//! [`OrderError`], or `toolkit_db`'s own error types.
//!
//! Run:
//!   cargo run -p cf-gears-toolkit-db --example `outbox_gear_integration` --features sqlite

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use sea_orm::DatabaseExecutor;
use serde::{Deserialize, Serialize};
use toolkit_db::outbox::{
    HandlerResult, Outbox, OutboxError, OutboxHandle, OutboxMessage, Partitions, Record,
    TransactionalMessageHandler, WorkerTuning, outbox_migrations_with_prefix,
};
use toolkit_db::{
    ConnectOpts, Db, DbError, DbTx, connect_db, migration_runner::run_migrations_for_testing,
};

// ─────────────────────────── the gear's domain ───────────────────────────

/// One queue carries every event of this gear; the payload type distinguishes
/// the records. Partition by a stable key so one entity's events stay ordered.
const QUEUE: &str = "shop.events";
const TABLE_PREFIX: &str = "shop";
const PARTITIONS: u16 = 4;

const ORDER_PLACED: &str = "shop.order_placed.v1";
const PAYMENT_CAPTURED: &str = "shop.payment_captured.v1";
const ORDER_SHIPPED: &str = "shop.order_shipped.v1";

#[derive(Serialize, Deserialize)]
struct OrderPlaced {
    order_id: u64,
    customer_id: u64,
    total_cents: i64,
}

#[derive(Serialize, Deserialize)]
struct PaymentCaptured {
    order_id: u64,
    amount_cents: i64,
}

#[derive(Serialize, Deserialize)]
struct OrderShipped {
    order_id: u64,
    carrier: String,
}

/// Route an order's events to a stable partition, so they are processed in
/// order relative to each other.
fn partition(order_id: u64) -> u32 {
    #[allow(clippy::cast_possible_truncation)]
    {
        (order_id % u64::from(PARTITIONS)) as u32
    }
}

// ──────────────────────── trait 1: the wake token ────────────────────────

/// The post-commit signal a gear's services hand back to whoever owns the
/// transaction: fired *after* the transaction commits. On rollback the caller
/// simply drops the wake — the rows never committed, so there is nothing to
/// signal. Consuming `self` by value lets each enqueue method return its wake by
/// value (static dispatch, no boxing).
trait Wake: Send + 'static {
    /// Wake the sequencer for the rows the committed transaction wrote.
    fn fire(self);
}

// A LOCAL trait implemented for a FOREIGN type — allowed because `Wake` is ours.
// `Type::method(self)` resolves to the inherent method, so this forwards to
// toolkit's own `fire` rather than recursing into the trait.
impl Wake for toolkit_db::outbox::Wake {
    fn fire(self) {
        toolkit_db::outbox::Wake::fire(self);
    }
}

// ─────────────────────── trait 2: the enqueuer port ──────────────────────

/// What the gear enqueue can fail with, domainised so the port never names
/// `toolkit_db::outbox::OutboxError`.
#[derive(Debug, thiserror::Error)]
enum EnqueueError {
    #[error("serialize {event}")]
    Serialize {
        event: &'static str,
        #[source]
        source: serde_json::Error,
    },
    #[error("outbox enqueue: {0}")]
    Outbox(String),
}

/// The gear's outbox port. Services depend on THIS, not on `toolkit_db::outbox`.
///
/// Each method enqueues within the caller's transaction and returns `impl Wake`
/// to be fired after that transaction commits. Callers take `&impl
/// OutboxEnqueuer`, so the real and no-op impls monomorphise with no `dyn`.
trait OutboxEnqueuer: Send + Sync {
    /// The wake each enqueue method yields. A named associated type (rather than a
    /// per-method `impl Wake`) so the value can be moved past the commit
    /// boundary without capturing the enqueue's `&self`/`tx` lifetimes.
    type Wake: Wake;

    fn order_placed(
        &self,
        tx: &DbTx<'_>,
        event: OrderPlaced,
    ) -> impl Future<Output = Result<Self::Wake, EnqueueError>> + Send;

    fn payment_captured(
        &self,
        tx: &DbTx<'_>,
        event: PaymentCaptured,
    ) -> impl Future<Output = Result<Self::Wake, EnqueueError>> + Send;

    fn order_shipped(
        &self,
        tx: &DbTx<'_>,
        event: OrderShipped,
    ) -> impl Future<Output = Result<Self::Wake, EnqueueError>> + Send;
}

// ─────────────────── no-op impl (tests / outbox disabled) ─────────────────

struct NoopWake;
impl Wake for NoopWake {
    fn fire(self) {}
}

/// Drop-in for the port when the real outbox is not wanted (unit tests, dry
/// runs). Zero cost: the generic call sites monomorphise to no-ops.
struct NoopEnqueuer;

impl OutboxEnqueuer for NoopEnqueuer {
    type Wake = NoopWake;
    async fn order_placed(
        &self,
        _tx: &DbTx<'_>,
        _event: OrderPlaced,
    ) -> Result<NoopWake, EnqueueError> {
        Ok(NoopWake)
    }
    async fn payment_captured(
        &self,
        _tx: &DbTx<'_>,
        _event: PaymentCaptured,
    ) -> Result<NoopWake, EnqueueError> {
        Ok(NoopWake)
    }
    async fn order_shipped(
        &self,
        _tx: &DbTx<'_>,
        _event: OrderShipped,
    ) -> Result<NoopWake, EnqueueError> {
        Ok(NoopWake)
    }
}

// ──────────────────── the one infra seam: the real impl ───────────────────

/// The real port impl. The ONLY service-visible place bound to
/// `toolkit_db::outbox`; built once by [`start`].
struct DbEnqueuer {
    outbox: Arc<Outbox>,
}

impl DbEnqueuer {
    fn new(outbox: Arc<Outbox>) -> Self {
        Self { outbox }
    }

    /// Serialize an event and enqueue it on the shop queue, returning the raw
    /// outbox wake (which satisfies `impl Wake`).
    async fn enqueue<E: Serialize>(
        &self,
        tx: &DbTx<'_>,
        order_id: u64,
        payload_type: &'static str,
        event_name: &'static str,
        event: &E,
    ) -> Result<toolkit_db::outbox::Wake, EnqueueError> {
        let bytes = serde_json::to_vec(event).map_err(|source| EnqueueError::Serialize {
            event: event_name,
            source,
        })?;
        let record = Record::to(QUEUE, partition(order_id))
            .payload(bytes, payload_type)
            .build()
            .map_err(|e| EnqueueError::Outbox(e.to_string()))?;
        self.outbox
            .enqueue(tx, record)
            .await
            .map_err(|e| EnqueueError::Outbox(e.to_string()))
    }
}

impl OutboxEnqueuer for DbEnqueuer {
    type Wake = toolkit_db::outbox::Wake;
    async fn order_placed(
        &self,
        tx: &DbTx<'_>,
        event: OrderPlaced,
    ) -> Result<toolkit_db::outbox::Wake, EnqueueError> {
        self.enqueue(tx, event.order_id, ORDER_PLACED, "OrderPlaced", &event)
            .await
    }
    async fn payment_captured(
        &self,
        tx: &DbTx<'_>,
        event: PaymentCaptured,
    ) -> Result<toolkit_db::outbox::Wake, EnqueueError> {
        self.enqueue(
            tx,
            event.order_id,
            PAYMENT_CAPTURED,
            "PaymentCaptured",
            &event,
        )
        .await
    }
    async fn order_shipped(
        &self,
        tx: &DbTx<'_>,
        event: OrderShipped,
    ) -> Result<toolkit_db::outbox::Wake, EnqueueError> {
        self.enqueue(tx, event.order_id, ORDER_SHIPPED, "OrderShipped", &event)
            .await
    }
}

// ─────────────────────── the consumer (in this file) ──────────────────────

/// Demo-only: lets `main` observe delivery. A real gear's handler does real
/// work instead of counting.
static PROCESSED: AtomicUsize = AtomicUsize::new(0);

/// Handles delivered events. Concrete and private to this file — nothing else
/// depends on it, so it needs no trait of its own.
struct OrderEventsHandler;

#[async_trait::async_trait]
impl TransactionalMessageHandler for OrderEventsHandler {
    async fn handle(&self, _txn: &DatabaseExecutor<'_>, msg: &OutboxMessage) -> HandlerResult {
        match msg.payload_type.as_str() {
            ORDER_PLACED => match serde_json::from_slice::<OrderPlaced>(&msg.payload) {
                Ok(e) => println!(
                    "  order placed:     order={} customer={} total={}c",
                    e.order_id, e.customer_id, e.total_cents
                ),
                Err(err) => {
                    return HandlerResult::Reject {
                        reason: err.to_string(),
                    };
                }
            },
            PAYMENT_CAPTURED => match serde_json::from_slice::<PaymentCaptured>(&msg.payload) {
                Ok(e) => println!(
                    "  payment captured: order={} amount={}c",
                    e.order_id, e.amount_cents
                ),
                Err(err) => {
                    return HandlerResult::Reject {
                        reason: err.to_string(),
                    };
                }
            },
            ORDER_SHIPPED => match serde_json::from_slice::<OrderShipped>(&msg.payload) {
                Ok(e) => println!(
                    "  order shipped:    order={} carrier={}",
                    e.order_id, e.carrier
                ),
                Err(err) => {
                    return HandlerResult::Reject {
                        reason: err.to_string(),
                    };
                }
            },
            other => {
                return HandlerResult::Reject {
                    reason: format!("unknown payload type: {other}"),
                };
            }
        }
        PROCESSED.fetch_add(1, Ordering::Relaxed);
        HandlerResult::Success
    }
}

// ───────────────────────── build + start the outbox ──────────────────────

/// Build and start the gear's outbox pipeline, returning the pipeline handle
/// (for shutdown) and the concrete enqueuer the gear hands to its services.
///
/// A gear calls this once during its own start-up; keeping the return concrete
/// preserves the static-dispatch story. In a real gear this lives in the gear's
/// own module and reads `outbox::start` / `start_outbox` — not a bare `start`.
async fn start_outbox(db: Db) -> Result<(OutboxHandle, DbEnqueuer), OutboxError> {
    let handle = Outbox::builder(db)
        .table_prefix(TABLE_PREFIX)?
        .processor_tuning(
            WorkerTuning::processor_default().idle_interval(Duration::from_millis(50)),
        )
        .sequencer_tuning(
            WorkerTuning::sequencer_default().idle_interval(Duration::from_millis(50)),
        )
        .queue(QUEUE, Partitions::of(PARTITIONS))
        .transactional(OrderEventsHandler)
        .start()
        .await?;
    let enqueuer = DbEnqueuer::new(Arc::clone(handle.outbox()));
    Ok((handle, enqueuer))
}

// ───────────────────── the domain service (no outbox in sight) ─────────────

/// What an order operation can fail with. A typed error rather than `anyhow`, so
/// the transaction closure carries `Result<_, OrderError>` and both the database
/// and the enqueue failures convert into it with `?`.
#[derive(Debug, thiserror::Error)]
enum OrderError {
    #[error(transparent)]
    Db(#[from] DbError),
    #[error(transparent)]
    Enqueue(#[from] EnqueueError),
}

/// A gear's domain service, shaped like any other (`TurnService`,
/// `RegistryService` …): a struct that holds its dependencies and exposes the
/// business operations as methods. Its only outbox dependency is the
/// `OutboxEnqueuer` port, kept generic so it monomorphises to the real or the
/// no-op impl with no `dyn`. Each method writes its business rows and enqueues
/// events in one transaction, then fires the wakes after the commit lands.
struct OrderService<E: OutboxEnqueuer + 'static> {
    db: Db,
    outbox: Arc<E>,
}

impl<E: OutboxEnqueuer + 'static> OrderService<E> {
    fn new(db: Db, outbox: Arc<E>) -> Self {
        Self { db, outbox }
    }

    /// Place an order and capture its payment atomically, emitting one event
    /// each.
    async fn place_order(&self, order_id: u64) -> Result<(), OrderError> {
        let outbox = Arc::clone(&self.outbox);
        // `transaction_ref_mapped` lets the closure return `Result<_, OrderError>`,
        // so the enqueue's `EnqueueError` and any `DbError` both convert with `?` -
        // no `anyhow`, no manual wrapping.
        let (placed, captured) = self
            .db
            .transaction_ref_mapped(|tx| {
                // The closure's future owns an `Arc` clone (not a borrow), so it
                // satisfies the transaction's lifetime; the calls still
                // monomorphise to the concrete enqueuer, no `dyn`.
                let outbox = Arc::clone(&outbox);
                Box::pin(async move {
                    // ... write the order + payment business rows on `tx` here ...
                    let placed = outbox
                        .order_placed(
                            tx,
                            OrderPlaced {
                                order_id,
                                customer_id: 42,
                                total_cents: 9_900,
                            },
                        )
                        .await?;
                    let captured = outbox
                        .payment_captured(
                            tx,
                            PaymentCaptured {
                                order_id,
                                amount_cents: 9_900,
                            },
                        )
                        .await?;
                    Ok::<_, OrderError>((placed, captured))
                })
            })
            .await?;
        // Committed: wake the sequencer. Each enqueue returns its own `impl
        // Wake`, so they are fired individually.
        placed.fire();
        captured.fire();
        Ok(())
    }

    /// Ship an order. A single-enqueue flow: for this shape the
    /// `toolkit_db::outbox::in_transaction` convenience wrapper is an option
    /// too; a service is free to run its own transaction as here.
    async fn ship_order(&self, order_id: u64) -> Result<(), OrderError> {
        let outbox = Arc::clone(&self.outbox);
        let shipped = self
            .db
            .transaction_ref_mapped(|tx| {
                let outbox = Arc::clone(&outbox);
                Box::pin(async move {
                    Ok::<_, OrderError>(
                        outbox
                            .order_shipped(
                                tx,
                                OrderShipped {
                                    order_id,
                                    carrier: "acme-express".to_owned(),
                                },
                            )
                            .await?,
                    )
                })
            })
            .await?;
        shipped.fire();
        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // shared-cache so every pool connection sees the same in-memory database.
    let db = connect_db(
        "sqlite:file:shop_outbox?mode=memory&cache=shared",
        ConnectOpts {
            max_conns: Some(1),
            ..Default::default()
        },
    )
    .await?;
    run_migrations_for_testing(&db, outbox_migrations_with_prefix(TABLE_PREFIX)?).await?;

    let (handle, enqueuer) = start_outbox(db.clone()).await?;
    // A gear wires the enqueuer into its services once, at start-up.
    let orders = OrderService::new(db.clone(), Arc::new(enqueuer));

    orders.place_order(1001).await?;
    orders.ship_order(1001).await?;
    println!("Enqueued the order lifecycle for order 1001");

    for _ in 0..100 {
        if PROCESSED.load(Ordering::Relaxed) >= 3 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let processed = PROCESSED.load(Ordering::Relaxed);
    println!("Processed: {processed}/3");
    assert_eq!(processed, 3);

    // The very same service, constructed against a no-op enqueuer — as a unit
    // test would — with no outbox and no dynamic dispatch.
    let test_orders = OrderService::new(db.clone(), Arc::new(NoopEnqueuer));
    test_orders.place_order(2002).await?;
    println!("order 2002 handled with NoopEnqueuer (no outbox wired)");

    handle.stop().await;
    println!("Done.");

    drop(db);
    Ok(())
}
