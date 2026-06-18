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

Consumers use a **typestate builder** with two axes:

- **D** (`NoDlq` / `WithDlq`): enables `RejectableOutcome::Reject`
- **M** (`BrokerOnly<_>` / `WithTx<_>`): enables `ack.commit_in_tx(&txn)`

### Four-quadrant typestate cheatsheet

| Builder state | Outcome enum | Ack handle | Use when |
|---|---|---|---|
| `NoDlq` + `offset_manager(BrokerOffsetManager)` | `HandlerOutcome` | `AckHandle` | Simple, no DB, no DLQ |
| `WithDlq` + `offset_manager(BrokerOffsetManager)` | `RejectableOutcome` | `AckHandle` | DLQ needed, no DB cursor |
| `NoDlq` + `tx_offset_manager(LocalDbOffsetManager)` | `HandlerOutcome` | `TxAckHandle<LocalDbOffsetManager>` | Atomic DB cursor, no DLQ |
| `WithDlq` + `tx_offset_manager(LocalDbOffsetManager)` | `RejectableOutcome` | `TxAckHandle<LocalDbOffsetManager>` | Full: atomic cursor + DLQ |

### Simplest consumer (no DLQ, broker-only cursor)

```rust
use event_broker_sdk::{AckHandle, BrokerOffsetManager, ConsumerGroupRef, EventHandler,
                       HandlerOutcome, RawEvent};

struct BillingProjector;

#[async_trait]
impl EventHandler<AckHandle, HandlerOutcome> for BillingProjector {
    async fn handle(&self, ctx, event: RawEvent, attempts, ack: AckHandle)
        -> Result<HandlerOutcome, ConsumerError>
    {
        // match on event.type_id and process
        ack.commit_on_eb().await?;
        Ok(HandlerOutcome::Success)
    }
}

let consumer = broker
    .consumer_builder()
    .group(ConsumerGroupRef::auto_anonymous("billing-projector"))
    .topics(["gts.cf.core.events.topic.v1~orders.v1"])
    .event_type_patterns(["gts.cf.core.events.event.v1~orders.*"])
    .offset_manager(BrokerOffsetManager::new())
    .handler(BillingProjector)
    .build_background(&ctx)
    .await?;

// Later:
consumer.shutdown(&ctx).await?;
```

### Consumer with atomic DB cursor + DLQ (outbox feature)

```rust
use event_broker_sdk::{LocalDbOffsetManager, RejectableOutcome, TxAckHandle};

#[async_trait]
impl EventHandler<TxAckHandle<LocalDbOffsetManager>, RejectableOutcome> for MyHandler {
    async fn handle(&self, ctx, event: RawEvent, attempts, ack: TxAckHandle<LocalDbOffsetManager>)
        -> Result<RejectableOutcome, ConsumerError>
    {
        if attempts > 5 {
            return Ok(RejectableOutcome::Reject { reason: "too many retries".into() });
        }
        let mut txn = self.db.begin().await?;
        self.project(&txn, &event).await?;
        ack.commit_in_tx(&txn).await?;   // offset + business state atomic
        txn.commit().await?;
        Ok(RejectableOutcome::Success)
    }
}

let consumer = broker
    .consumer_builder()
    .group(...)
    .topics([...])
    .event_type_patterns([...])
    .on_dead_letter(|dl| async move { log_dlq(dl).await })
    .tx_offset_manager(LocalDbOffsetManager::new(db.clone(), "evbk_consumer_cursors"))
    .handler(MyHandler { db })
    .build_background(&ctx)
    .await?;
```

---

## Features

| Feature | Enables | Extra deps |
|---|---|---|
| (default) | Sync producer, consumer | — |
| `outbox` | Async producer via modkit-db outbox, TxAckHandle | `modkit-db` |
| `integration` | Integration tests (gated) | — |
