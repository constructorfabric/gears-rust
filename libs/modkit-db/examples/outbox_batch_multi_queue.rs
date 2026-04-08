#![allow(clippy::unwrap_used, clippy::expect_used, clippy::use_debug)]

//! Batch processing with multiple queues in a single pipeline.
//!
//! "orders" uses `batch_decoupled` (handler receives up to 5 messages at once).
//! "notifications" uses decoupled (single-message handler).
//! Both queues process independently within the same `OutboxBuilder`.
//!
//! Run:
//!   cargo run -p cf-modkit-db --example `outbox_batch_multi_queue` --features sqlite,preview-outbox

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use modkit_db::outbox::{
    Handler, HandlerResult, MessageHandler, Outbox, OutboxMessage, Partitions, WorkerTuning,
    outbox_migrations,
};
use modkit_db::{ConnectOpts, connect_db, migration_runner::run_migrations_for_testing};

struct OrderBatchHandler {
    count: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl Handler for OrderBatchHandler {
    async fn handle(
        &self,
        msgs: &[OutboxMessage],
        _cancel: tokio_util::sync::CancellationToken,
    ) -> HandlerResult {
        // batch handler receives multiple messages per call
        println!("  orders: batch of {} messages", msgs.len());
        self.count.fetch_add(msgs.len(), Ordering::Relaxed);
        HandlerResult::Success
    }
}

struct NotificationHandler {
    count: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl MessageHandler for NotificationHandler {
    async fn handle(
        &self,
        msg: &OutboxMessage,
        _cancel: tokio_util::sync::CancellationToken,
    ) -> HandlerResult {
        let payload = String::from_utf8_lossy(&msg.payload);
        println!("  notifs: seq={} payload={payload}", msg.seq);
        self.count.fetch_add(1, Ordering::Relaxed);
        HandlerResult::Success
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let db = connect_db(
        "sqlite:file:outbox_batch?mode=memory&cache=shared",
        ConnectOpts {
            max_conns: Some(1),
            ..Default::default()
        },
    )
    .await?;
    run_migrations_for_testing(&db, outbox_migrations()).await?;

    let order_count = Arc::new(AtomicUsize::new(0));
    let notif_count = Arc::new(AtomicUsize::new(0));

    let handle = Outbox::builder(db.clone())
        .processor_tuning(
            WorkerTuning::processor_default().idle_interval(Duration::from_millis(50)),
        )
        .sequencer_tuning(
            WorkerTuning::sequencer_default().idle_interval(Duration::from_millis(50)),
        )
        // orders: 2 partitions for parallelism, batch handler processes up to 5 at once
        .queue("orders", Partitions::of(2))
        .batch_decoupled(OrderBatchHandler {
            count: order_count.clone(),
        })
        // notifications: single partition, single-message handler
        .queue("notifications", Partitions::of(1))
        .decoupled(NotificationHandler {
            count: notif_count.clone(),
        })
        .start()
        .await?;

    let conn = db.conn()?;
    for i in 0..8u32 {
        let payload = format!(r#"{{"order_id": {i}}}"#);
        handle
            .outbox()
            .enqueue(
                &conn,
                "orders",
                i % 2,
                payload.into_bytes(),
                "application/json;orders.placed.v1",
            )
            .await?;
    }
    for i in 0..3u32 {
        let payload = format!("user_{i}_welcome");
        handle
            .outbox()
            .enqueue(
                &conn,
                "notifications",
                0,
                payload.into_bytes(),
                "text/plain;notifications.welcome.v1",
            )
            .await?;
    }
    handle.outbox().flush();
    println!("Enqueued 8 orders + 3 notifications");

    for _ in 0..100 {
        let done = order_count.load(Ordering::Relaxed) + notif_count.load(Ordering::Relaxed);
        if done >= 11 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let orders = order_count.load(Ordering::Relaxed);
    let notifs = notif_count.load(Ordering::Relaxed);
    println!("Orders processed: {orders}/8");
    println!("Notifications processed: {notifs}/3");
    assert_eq!(orders, 8);
    assert_eq!(notifs, 3);

    handle.stop().await;
    println!("Done.");
    Ok(())
}
