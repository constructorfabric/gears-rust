# cyberware-event-broker-sdk

High-level Rust SDK for the Cyberfabric Event Broker.

Wire concerns (JSON serialisation, partition selection, producer-chain bookkeeping,
subscription lifecycle recovery, 410/404/409 error handling) are handled inside the
SDK. Callers work with their own typed event structs, a single `EventBroker` trait,
and structured `Producer` / `Consumer` builders.

## Design source

Sourced from `modules/system/event-broker/docs/DESIGN.md` on the `event-broker-design`
branch.

```
DESIGN_PIN = bb8e169ee04eb40bdda18ff3a01da980c86fa546
```

---

## Producer quick-start

### Sync (direct to broker)

```rust
use event_broker_sdk::{ChainMode, EventBroker, ProducerBuilder, TypedEvent};
use std::borrow::Cow;

#[derive(Serialize, Deserialize)]
struct OrderCreated { order_id: Uuid, total_cents: i64 }

impl TypedEvent for OrderCreated {
    const TYPE_ID: &'static str = "gts.cf.core.events.event.v1~orders.created.v1";
    const TOPIC:   &'static str = "gts.cf.core.events.topic.v1~orders.v1";
    const SUBJECT_TYPE: &'static str = "gts.cf.core.events.subject.v1~order.v1";
    const SOURCE:  &'static str = "order-service";
    fn subject(&self) -> Cow<'_, str> { Cow::Owned(self.order_id.to_string()) }
}

// Obtain from ClientHub.
let broker = hub.get::<dyn EventBroker>()?;

let producer = broker
    .producer_builder()
    .topics(["gts.cf.core.events.topic.v1~orders.v1"])
    .event_type_patterns(["gts.cf.core.events.event.v1~orders.*"])
    .source("order-service")
    .chain_mode(ChainMode::Chained)  // default
    .build_sync(&ctx)
    .await?;

producer.publish(&ctx, OrderCreated { order_id, total_cents: 4299 }).await?;
```

### Async (transactional outbox, feature `outbox`)

```rust
let producer = broker
    .producer_builder()
    .topics([...])
    .event_type_patterns([...])
    .source("order-service")
    .build_async(&ctx, db.clone(), "order_producer_outbox")
    .await?;

// Pre-warm schemas outside the txn (lazy validation only).
// producer.prepare::<OrderCreated>(&ctx).await?;

let mut txn = db.begin().await?;
write_business_state(&txn).await?;
producer.publish(&ctx, &txn, OrderCreated { ... }).await?;
txn.commit().await?;
```

### Producer chain-mode table

| Mode | producer_id | Broker dedup | Use when |
|---|---|---|---|
| `Chained` (default) | Required | Strict sequence + previous check | At-most-once via chain |
| `Monotonic` | Required | Sequence-only; gaps allowed | Idempotent, non-strict order |
| `Stateless` | None | None | Consumer is fully idempotent |

---

## Consumer quick-start

Consumers use a **typestate builder** for commit mode:

| Builder state | Outcome enum | Commit handle | Use when |
|---|---|---|---|
| `offset_manager(InMemoryOffsetManager)` | `HandlerOutcome` or `BatchHandlerOutcome` | None | Simple, in-process cursor |
| `offset_manager(custom CommitOffset)` | `HandlerOutcome` or `BatchHandlerOutcome` | None | Remote or custom cursor |
| `offset_manager(LocalDbOffsetManager)` | `HandlerOutcome` | `TxCommitHandle<LocalDbOffsetManager>` | Atomic DB cursor |

Dead-letter behavior is handler-owned policy. If a permanent failure should be parked, the
handler writes it through `event_broker_sdk::dlq` helpers or application code, then returns
`Success` only after the parking operation is durable. If parking fails, return retry or an
error so the source offset does not advance.

### Single-event consumer

```rust
use event_broker_sdk::{
    ConsumerBuilder, ConsumerError, ConsumerGroupRef, ConsumerProfile, EventTypeRef,
    Fallback, HandlerOutcome, InMemoryOffsetManager, RawEvent, SingleEventHandler,
    SubscriptionInterest, TopicRef,
};

struct BillingProjector;

#[async_trait]
impl SingleEventHandler for BillingProjector {
    async fn handle(&self, event: RawEvent, attempts: u16)
        -> Result<HandlerOutcome, ConsumerError>
    {
        // match on event.type_id and process
        Ok(HandlerOutcome::Success)
    }
}

let handle = broker
    .consumer_builder()
    .group(ConsumerGroupRef::auto_anonymous("billing-projector"))
    .subscription_interests([SubscriptionInterest::builder()
        .topic(TopicRef::gts("gts.cf.core.events.topic.v1~orders.v1"))
        .types([EventTypeRef::gts_pattern("gts.cf.core.events.event.v1~orders.*")])
        .build()?])
    .profile(ConsumerProfile::low_latency())
    .offset_manager(InMemoryOffsetManager::new(Fallback::Earliest))
    .handler(BillingProjector)
    .start()
    .await?;

handle.stop().await?;
```

### Batch consumer

```rust
use event_broker_sdk::{BatchHandlerOutcome, ConsumerBatching, ConsumerError, ConsumerHandler, EventBatch};
use std::time::Duration;

struct BatchProjector;

#[async_trait]
impl ConsumerHandler for BatchProjector {
    async fn handle_batch(&self, batch: &EventBatch<'_>, attempts: u16)
        -> Result<BatchHandlerOutcome, ConsumerError>
    {
        let chunk = batch.next_chunk(batch.len());
        for event in chunk {
            // process events from one topic partition
        }
        Ok(BatchHandlerOutcome::AdvanceThrough {
            offset: chunk.last().expect("non-empty batch").offset,
        })
    }
}

let handle = broker
    .consumer_builder()
    .group(ConsumerGroupRef::auto_anonymous("billing-batch"))
    .subscription_interests([orders_interest])
    .batching(ConsumerBatching { max_events: 128, max_wait: Duration::from_millis(250) })
    .offset_manager(InMemoryOffsetManager::new(Fallback::Earliest))
    .batch_handler(BatchProjector)
    .start()
    .await?;
```

### Routed handlers

```rust
let handle = broker
    .consumer_builder()
    .group(ConsumerGroupRef::auto_anonymous("commerce-router"))
    .subscription_interests([orders_interest])
    .offset_manager(InMemoryOffsetManager::new(Fallback::Earliest))
    .default_handler(DefaultProjector)
    .route()
    .topic(TopicRef::gts("gts.cf.core.events.topic.v1~orders.v1"))
    .event_type(EventTypeRef::gts("gts.cf.core.events.event.v1~orders.created.v1"))
    .handler(OrderCreatedProjector)
    .start()
    .await?;
```

### Consumer with DB cursor and outbox-backed DLQ (`outbox` feature)

```rust
use std::sync::Arc;

use event_broker_sdk::dlq::{ConsumerDlqOutbox, DeadLetterRecord};
use event_broker_sdk::{
    ConsumerError, Fallback, HandlerOutcome, LocalDbOffsetManager, RawEvent,
    TxCommitHandle, TxSingleEventHandler,
};

#[async_trait]
impl TxSingleEventHandler<LocalDbOffsetManager> for MyHandler {
    async fn handle(
        &self,
        event: RawEvent,
        attempts: u16,
        commit: TxCommitHandle<LocalDbOffsetManager>,
    )
        -> Result<HandlerOutcome, ConsumerError>
    {
        if attempts > 5 {
            let record = DeadLetterRecord::builder(&event, "too many retries")
                .attempts(attempts)
                .build();

            self.db.transaction_ref(|tx| {
                Box::pin(async move {
                    self.dlq.enqueue(tx, record).await?;
                    commit.commit_offset_in_tx(tx, event.offset).await?;
                    Ok(())
                })
            }).await?;

            return Ok(HandlerOutcome::Success);
        }

        self.db.transaction_ref(|tx| {
            Box::pin(async move {
                self.project(tx, &event).await?;
                commit.commit_offset_in_tx(tx, event.offset).await?; // offset + business state atomic
                Ok(())
            })
        }).await?;

        Ok(HandlerOutcome::Success)
    }
}

// This starts the service-owned DLQ outbox queue. The SDK helper only enqueues
// a durable handoff record; your outbox processor owns final DLQ delivery.
let outbox_handle = toolkit_db::outbox::Outbox::builder(db.clone())
    .queue("consumer-dlq", toolkit_db::outbox::Partitions::of(4))
    .leased(MyDlqProcessor)
    .start()
    .await?;

let dlq = ConsumerDlqOutbox::builder(Arc::clone(outbox_handle.outbox()))
    .queue("consumer-dlq")
    .partitions(4)
    .build();

let handle = broker
    .consumer_builder()
    .group(...)
    .subscription_interests([...])
    .offset_manager(LocalDbOffsetManager::new(db.clone(), Fallback::Earliest))
    .handler(MyHandler { db, dlq })
    .start()
    .await?;
```

If the main business transaction already rolled back, open a new transaction for
the DLQ handoff and offset skip. If that DLQ transaction fails, return an error
from the handler so the source offset is not advanced. Services that need a
custom table, remote sink, or in-memory parking can still implement
`DeadLetterSink` directly.

---

## Features

| Feature | Enables | Extra deps |
|---|---|---|
| (default) | Sync producer, consumer | — |
| `db` | `LocalDbOffsetManager`, `TxCommitHandle` | `modkit-db` |
| `outbox` | Async producer via modkit-db outbox | `db`, `modkit-db/preview-outbox` |
| `integration` | Integration tests (gated) | — |
