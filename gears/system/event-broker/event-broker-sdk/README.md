# cf-gears-event-broker-sdk

High-level Rust SDK for the `event-broker` gear.

Wire concerns (JSON serialisation, producer-chain bookkeeping,
subscription lifecycle recovery, canonical error handling) are handled inside the
SDK. Callers work with their own typed event structs, a single `EventBrokerApi` trait,
and structured producer and consumer builders.

## Error model

The SDK traits and wrappers use domain-readable local errors in-process:
`EventBrokerApi` returns `EventBrokerError`, `backend::Backend` returns
`BackendError` (feature 0007), and producer/consumer wrappers return `EventBrokerError`
for local typed validation, retry, and dispatch. Those errors are canonical-error
compatible: before errors cross an API or transport boundary they convert to the
canonical categories and RFC 9457 `Problem` representation from
`toolkit-canonical-errors`.

`Problem.instance` and `Problem.trace_id` are boundary-owned fields. SDK domain
errors do not treat them as business data; HTTP or RPC middleware attaches them
when converting a `CanonicalError` into the final problem response.

## Design source

Sourced from `modules/system/event-broker/docs/DESIGN.md` on the `event-broker-design`
branch.

```
DESIGN_PIN = bb8e169ee04eb40bdda18ff3a01da980c86fa546
```

---

## Events

An event is an `EventV1<P>`, generic over its payload type `P`; `EventV1` alone is
`EventV1<serde_json::Value>`, the untyped form. A gear declares each event type as a
derived type schema under `EventV1` and names the event with a type alias. It can declare
the subject kinds it owns under `SubjectV1` the same way.

```rust
use event_broker_sdk::{EventV1, SubjectV1, gts_id};
use toolkit_gts::gts_type_schema;

#[gts_type_schema(
    dir_path = "schemas",
    base = EventV1,
    type_id = gts_id!("cf.core.events.event.v1~acme.orders.created.v1~"),
    description = "An order was created",
    properties = "order_id,total_cents",
    traits = serde_json::json!({ "topic": gts_id!("cf.core.events.topic.v1~acme.orders.v1") }),
)]
pub struct OrderCreatedV1 {
    pub order_id: Uuid,
    pub total_cents: i64,
}
pub type OrderCreated = EventV1<OrderCreatedV1>;

#[gts_type_schema(
    dir_path = "schemas",
    base = SubjectV1,
    type_id = gts_id!("cf.core.events.subject.v1~acme.orders.order.v1~"),
    description = "An order",
    properties = "",
)]
pub struct OrderSubjectV1;

let event = OrderCreated::builder(OrderCreatedV1 { order_id, total_cents: 4299 })
    .tenant_id(tenant_id)
    .subject(order_id.to_string())
    .subject_type::<OrderSubjectV1>()
    .build()?;
```

`build()` checks the envelope fields and returns an `EventBrokerError` naming the first one
that is missing or invalid. The event's `type` comes from the payload type. `id` defaults to a
new UUID v7 and `occurred_at` to the current time, both fixed when `build()` runs; set them
with `.id(..)` and `.occurred_at(..)`. `.source(..)` and `.trace_parent(..)` are optional; a
producer stamps its own `source` and refuses an event that already has one.
`.subject_type_id(..)` takes a subject type id that is not declared in Rust, such as a
vendor's. `EventV1::builder_for(type_id, data)` and `EventV1::builder_without_data(type_id)`
build untyped events. A payload declared with `#[gts_type_schema]` carries every field: an
unset `Option` field travels as `null` (see `EventPayload` for the schema attribute that says
so).

For a new gear, name the payload struct `<Name>Payload` and the alias for the full event
`<Name>`: `pub struct OrderCreatedPayload { .. }` with
`pub type OrderCreated = EventV1<OrderCreatedPayload>;`. This is a recommendation for new
gears only: the examples here keep their `V1` names, and existing gears keep the names they
have.

`OrderCreated::EVENT_TYPE_ID` is the event's type id. With `gts::GtsSchema` in scope,
`OrderCreated::TYPE_ID` also compiles, but it is the base type's id
(`gts.cf.core.events.event.v1~`), from `EventV1`'s own `GtsSchema` impl.

## Producer quick-start

### DB-free direct producer

```rust
use std::sync::Arc;

use event_broker_sdk::{Ack, DirectProducer, EventBrokerApi};

// Obtain from ClientHub.
let broker = hub.get::<dyn EventBrokerApi>()?;

let producer = DirectProducer::builder()
    .broker(Arc::clone(&broker))
    .security_context(ctx.clone())
    .source("order-service")
    .topics(["gts.cf.core.events.topic.v1~acme.orders.v1"])
    .event_type_patterns(["gts.cf.core.events.event.v1~acme.orders.*"])
    .build()
    .await?;

// `event` is the `OrderCreated` built in the Events section above.
producer.publish(event, Ack::Accepted).await?;
```

`build()` registers a fresh producer id with the broker; the producer stamps its `source`
and chain numbers on every event, so the event itself carries neither. `Ack::Persisted {
within }` asks for the stored answer, which the broker does not serve yet (#5655:
`Unimplemented`).

A `DirectProducer` registers a fresh producer id when it is built and keeps its chain head
in memory, so deduplication is always on. It serializes its publishes per topic. At most
`max_waiting_per_topic` publishes (default 64) wait on one topic; the next fails at once
with `ProducerBusy`. After a publish whose outcome it does not see (an error or a dropped
future), re-publishing the same event (same `id`) before any other publish on that topic
resends it with the numbers it carried, so the broker answers `Duplicate` if the first
attempt was stored; any other publish on that topic re-reads the producer's head first. A
`SequenceViolation` answer means the numbers were stale: the next publish, the same event
included, re-reads the head. The producer does not retry by itself, and a re-publish after a
restart is not deduplicated.

Not covered: if an earlier request of another event, whose outcome was not seen, commits the
numbers an event was last sent with, re-publishing that event is answered `Duplicate`
although it was not stored. The broker deduplicates on the numbers, not on the event id
(#5660). Also not covered, in chained mode: a re-publish whose earlier attempt was stored is
answered `SequenceViolation` when an earlier unseen request reaches the broker later and
moves the head past it; the next publish then stores it again.

### DB-aware managed outbox producer (`outbox` feature)

```rust
use event_broker_sdk::{OutboxRegistration, OutboxSetup, ProducerMode, producer_registration_migrations};
use toolkit_db::outbox::{Outbox, Partitions};

// Run explicitly during service migration, before building the OutboxSetup.
toolkit_db::migration_runner::run_migrations_for_gear(
    &db,
    "event-broker-producer",
    producer_registration_migrations(),
)
.await?;

let setup = OutboxSetup::builder()
    .broker(Arc::clone(&broker))
    .db(db.clone())
    .security_context(ctx.clone())
    .source("order-service")
    .client_agent("order-service/1.0")
    .registration(OutboxRegistration::managed(ProducerMode::Chained, "orders.created"))
    .topics(["gts.cf.core.events.topic.v1~acme.orders.v1"])
    .event_type_patterns(["gts.cf.core.events.event.v1~acme.orders.*"])
    .queue("event-broker-producer", Partitions::of(16))
    .build()
    .await?;
let handle = setup.start(Outbox::builder(db.clone())).await?;
let producer = handle.producer().clone();

// in_transaction fires the returned wake only if the transaction commits, so
// nothing is published for a rolled-back write.
toolkit_db::outbox::in_transaction(&db, |txn| Box::pin(async move {
    write_business_state(txn).await?;
    // `event` is the `OrderCreated` built in the Events section above.
    let wake = producer.enqueue(txn, event).await?;
    Ok(((), wake))
})).await?;
```

`enqueue` refuses an event that already carries a field the producer stamps (`source`, `meta`, the broker-stamped members), then checks the envelope rules and the event type's schema before it writes the outbox row. It makes no Event Broker call, so nothing remote runs inside the transaction. `build()` prepares every event type the declared patterns match unless `.lazy_schema_loading()` is set; a lazy setup refuses an unprepared type with `SchemaNotPrepared`, so call `prepare_event_type(..)` on the setup before `start`, or on the producer before opening the transaction. The producer stamps its own `source` on every event.

A registration key is non-blank, carries no leading or trailing whitespace, and is at most 255 bytes. An outbox queue name follows the same three rules and uses only ASCII letters, digits, `.`, `_` and `-`, starting and ending with a letter or digit (toolkit-db's queue-name rule). A client agent is printable ASCII, 1-256 bytes.

### Producer configuration matrix

| Surface | Deduplication | Local DB | Producer id | Multi-instance notes |
|---|---|---|---|---|
| `DirectProducer` | Always on (`ProducerMode::Chained` by default, or `Monotonic`) | No | Fresh broker-issued id per build | Deduplication resets on restart; earlier ids remain until the registration reaper (#5651) |
| `OutboxSetup` / `OutboxProducer` | `OutboxRegistration::Stateless` | Yes | None | Uses the DB for the outbox only, not for a producer id |
| `OutboxSetup` / `OutboxProducer` | `OutboxRegistration::managed(mode, key)` | Yes | Loaded or registered broker-issued id, one per outbox partition | Preferred durable chained/monotonic outbox path |

Producer ids are always broker-issued. The SDK does not mint producer ids and
does not persist a separate last-sent-sequence table. A `DirectProducer` keeps
its sequence state in memory, read from Event Broker cursors when it is built.
Outbox producers use toolkit-db `OutboxMessage.seq` as the durable local
sequence and Event Broker cursors as the authoritative accepted sequence.

One `OutboxSetup` queue carries every topic the setup declares.
The SDK maps each event to a producer outbox partition by its own rule; it
derives no broker partition, which the backend assigns from the partition key.
A partitioned producer registers one producer id per producer partition, each a
sequence space of its own, shared by every topic that partition publishes to.

The service owns the toolkit-db outbox lifecycle: it runs the outbox migrations, configures the `OutboxBuilder` (profile, table prefix, tuning) and stops the handle. `OutboxSetup::start` registers the producer's queue with its leased processor on that builder and starts it. A service that runs queues of its own on the same outbox calls `OutboxSetup::register` instead, which returns the queue builder and an `OutboxBinder`; it starts the outbox itself and binds the handle it started (`OutboxBinder::bind`).

---

## Consumer quick-start

Consumers use a **typestate builder** for commit mode:

| Builder state | Outcome enum | Commit handle | Use when |
|---|---|---|---|
| `offset_manager(InMemoryOffsetManager)` | `HandlerOutcome` or `BatchHandlerOutcome` | None | Simple, in-process cursor |
| `offset_manager(custom CommitOffset)` | `HandlerOutcome` or `BatchHandlerOutcome` | None | Remote or custom cursor |
| `offset_manager(LocalDbOffsetManager)` | `HandlerOutcome` | `TxCommitHandle<LocalDbOffsetManager>` | Atomic DB cursor |
| `.route().topic(..).event::<P>(Undecodable)` | `HandlerOutcome` or `BatchHandlerOutcome`, payload decoded as `P` | None | Typed route; the policy for an undecodable event sits on the route |
| `.route().topic(..).event::<P>(TxUndecodable)` | `HandlerOutcome`, payload decoded as `P` | `TxCommitHandle<LocalDbOffsetManager>` | Typed transactional route (`db` feature) |

Dead-letter behavior is handler-owned policy. If a permanent failure should be parked, the
handler writes it through `event_broker_sdk::dlq` helpers or application code, then returns
`Success` only after the parking operation is durable. If parking fails, return retry or an
error so the source offset does not advance.

### Single-event consumer

```rust
use event_broker_sdk::{
    ConsumerBuilder, ConsumerError, ConsumerGroupRef, ConsumerProfile, Delivery, EventHandler,
    EventV1, Fallback, GtsIdPattern, GtsInstanceId, HandlerOutcome, InMemoryOffsetManager,
    SubscriptionInterest,
};

struct BillingProjector;

#[async_trait]
impl EventHandler<serde_json::Value> for BillingProjector {
    async fn handle(&self, event: EventV1, delivery: &Delivery)
        -> Result<HandlerOutcome, ConsumerError>
    {
        // match on event.event_type() and process; delivery.attempts() is 1 on the
        // first call and grows with each redelivery
        Ok(HandlerOutcome::Success)
    }
}

let orders_topic = GtsInstanceId::try_new("gts.cf.core.events.topic.v1~orders.v1")?;
let orders_interest = SubscriptionInterest::builder()
    .topic(orders_topic.clone())
    .types([GtsIdPattern::try_new("gts.cf.core.events.event.v1~orders.*")?])
    .build()?;

let handle = ConsumerBuilder::new(broker.clone())
    .group(ConsumerGroupRef::auto_anonymous("billing-projector"))
    .subscription_interests([orders_interest.clone()])
    .profile(ConsumerProfile::low_latency())
    .offset_manager(InMemoryOffsetManager::new(Fallback::Earliest))
    .handler(BillingProjector)
    .start()
    .await?;

handle.stop().await?;
```

### Batch consumer

```rust
use event_broker_sdk::{
    BatchHandler, BatchHandlerOutcome, ConsumerBatching, ConsumerBuilder, ConsumerError,
    ConsumerGroupRef, ConsumerRetry, Delivery, EventBatch, Fallback, InMemoryOffsetManager,
};
use std::time::Duration;

struct BatchProjector;

#[async_trait]
impl BatchHandler<serde_json::Value> for BatchProjector {
    async fn handle_batch(&self, batch: &EventBatch<'_, serde_json::Value>, delivery: &Delivery)
        -> Result<BatchHandlerOutcome, ConsumerError>
    {
        let chunk = batch.next_chunk(batch.len());
        for event in chunk {
            // process events from delivery.partition() of delivery.topic()
        }
        Ok(BatchHandlerOutcome::AdvanceThrough {
            offset: chunk
                .last()
                .expect("non-empty batch")
                .sequence()
                .expect("a delivered event is stamped"),
        })
    }
}

let handle = ConsumerBuilder::new(broker.clone())
    .group(ConsumerGroupRef::auto_anonymous("billing-batch"))
    .subscription_interests([orders_interest.clone()])
    .batching(ConsumerBatching { max_events: 128, max_wait: Duration::from_millis(250) })
    .retry(ConsumerRetry {
        base_delay: Duration::from_millis(100),
        max_delay: Duration::from_secs(5),
    })
    .offset_manager(InMemoryOffsetManager::new(Fallback::Earliest))
    .batch_handler(BatchProjector)
    .start()
    .await?;
```

`event.sequence()` is an `Option`; it is always set on a delivered event. A handler error is
reported and treated as `Retry`.

A typed batch route decodes the batch as `P` and hands the handler an
`EventBatch<'_, P>` (see the next section for the route). Its rules:

- The batch is decoded in delivery order, up to the first event that does not decode. The
  handler gets the events before it, the typed prefix. A non-empty batch whose events all
  decode goes to the handler whole, and its outcome passes through unchanged.
- The route's `Undecodable` policy runs on the first undecodable event, after the handler
  succeeded or advanced through the whole prefix. `Stall` advances through the prefix and
  stops at the event (with no prefix it retries). `Skip` advances past the event and
  reports it as `ConsumerRuntimeEvent::EventSkipped`. `Fallback(h)` hands the raw event to
  `h`: its `Success` advances past the event, its `Retry` advances through the prefix, or
  with no prefix retries.
- The events after the undecodable one are not decoded, and no outcome advances through
  them.
- An error from the handler or a fallback handler is reported and the whole batch is
  retried, so a prefix the handler already handled is handed to it again.

### Routed handlers

A route sends the events of one topic and event type to its own handler; every other event
goes to the default handler. A route names its topic with a `GtsInstanceId` and its event
type with a `GtsIdPattern`.

```rust
use event_broker_sdk::{GtsIdPattern, GtsInstanceId};

let handle = ConsumerBuilder::new(broker.clone())
    .group(ConsumerGroupRef::auto_anonymous("commerce-router"))
    .subscription_interests([orders_interest.clone()])
    .offset_manager(InMemoryOffsetManager::new(Fallback::Earliest))
    .default_handler(DefaultProjector)
    .route()
    .topic(orders_topic.clone())
    .event_type(GtsIdPattern::try_new("gts.cf.core.events.event.v1~orders.refunded.v1~")?)
    .handler(RefundProjector)
    .start()
    .await?;
```

`DefaultProjector` and `RefundProjector` implement `EventHandler<serde_json::Value>`.

A typed route decodes each event as a payload type `P` and hands its handler an
`EventV1<P>`. The `Undecodable` policy sits on the route and says what happens to an event
whose data does not decode as `P`:

- `Undecodable::Stall` retries the event; the partition does not move past it, and no
  handler is called.
- `Undecodable::Skip` advances past the event and reports it to the runtime listeners as
  `ConsumerRuntimeEvent::EventSkipped`.
- `Undecodable::Fallback(h)` hands the raw `EventV1` to `h`, an
  `EventHandler<serde_json::Value>`; its `Success` advances past the event.

The route serves the events of `P`'s type and of the types derived from it.

```rust
use std::sync::Arc;

use event_broker_sdk::{
    ConsumerRuntimeEvent, ConsumerRuntimeListener, Delivery, EventHandler, EventV1,
    HandlerOutcome, Undecodable,
};

// `OrderCreatedV1` is the payload declared in the Events section above.
struct OrderCreatedProjector;

#[async_trait]
impl EventHandler<OrderCreatedV1> for OrderCreatedProjector {
    async fn handle(&self, event: EventV1<OrderCreatedV1>, delivery: &Delivery)
        -> Result<HandlerOutcome, ConsumerError>
    {
        // event.data() is Some(&OrderCreatedV1) for an event with data
        Ok(HandlerOutcome::Success)
    }
}

struct SkipLogger;

#[async_trait]
impl ConsumerRuntimeListener for SkipLogger {
    async fn on_consumer_event(&self, event: &ConsumerRuntimeEvent) -> Result<(), ConsumerError> {
        if let ConsumerRuntimeEvent::EventSkipped { sequence, type_id, category, .. } = event {
            // count or log the skipped event; it carries no part of the data
        }
        Ok(())
    }
}

let handle = ConsumerBuilder::new(broker.clone())
    .group(ConsumerGroupRef::auto_anonymous("orders-typed"))
    .subscription_interests([orders_interest.clone()])
    .register_listener(SkipLogger)
    .offset_manager(InMemoryOffsetManager::new(Fallback::Earliest))
    .default_handler(DefaultProjector)
    .route()
    .topic(orders_topic.clone())
    .event::<OrderCreatedV1>(Undecodable::Skip)
    .handler(OrderCreatedProjector)
    .start()
    .await?;
```

`.batch_handler(..)` on the same route takes a `BatchHandler<OrderCreatedV1>` and follows the
typed batch rules in the Batch consumer section.

### Consumer with DB cursor and outbox-backed DLQ (`outbox` feature)

```rust
use std::sync::Arc;

use event_broker_sdk::dlq::{ConsumerDlqOutbox, DeadLetterRecord};
use event_broker_sdk::{
    ConsumerBuilder, ConsumerError, Delivery, EventV1, Fallback, HandlerOutcome,
    LocalDbOffsetManager, TxCommitHandle, TxEventHandler,
};

#[async_trait]
impl TxEventHandler<serde_json::Value, LocalDbOffsetManager> for MyHandler {
    async fn handle(
        &self,
        event: EventV1,
        delivery: &Delivery,
        commit: TxCommitHandle<LocalDbOffsetManager>,
    )
        -> Result<HandlerOutcome, ConsumerError>
    {
        let sequence = event.sequence().expect("a delivered event is stamped");

        if delivery.attempts() > 5 {
            // attempts defaults to delivery.attempts()
            let record = DeadLetterRecord::builder(&event, delivery, "too many retries")
                .build();

            // in_transaction fires the returned wake only if the transaction
            // commits. Enqueue last, so an error on the offset write never drops
            // an unfired wake.
            toolkit_db::outbox::in_transaction(&self.db, |tx| Box::pin(async move {
                commit.commit_offset_in_tx(tx, sequence).await?;
                let wake = self.dlq.enqueue(tx, record).await?;
                Ok(((), wake))
            })).await?;

            return Ok(HandlerOutcome::Success);
        }

        self.db.transaction_ref(|tx| {
            Box::pin(async move {
                self.project(tx, &event).await?;
                commit.commit_offset_in_tx(tx, sequence).await?; // offset + business state atomic
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

let handle = ConsumerBuilder::new(broker.clone())
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

A typed transactional route decodes each event as `P` and hands a
`TxEventHandler<P, LocalDbOffsetManager>` the `EventV1<P>` with its commit handle. Its
policy is `TxUndecodable`, which has no `Skip`: a transactional handler advances the
consumer only by committing an offset in its own transaction, and the route has none.

- `TxUndecodable::Stall` retries the event; nothing is committed and no handler is called.
  The runtime delivers one event per call, so `Stall` stops at the undecodable event.
- `TxUndecodable::Fallback(h)` hands the raw `EventV1` and the commit handle to `h`, a
  `TxEventHandler<serde_json::Value, LocalDbOffsetManager>`; its outcome passes through.

```rust
use event_broker_sdk::TxUndecodable;

// `TypedOrderHandler` implements `TxEventHandler<OrderCreatedV1, LocalDbOffsetManager>`.
let handle = ConsumerBuilder::new(broker.clone())
    .group(ConsumerGroupRef::auto_anonymous("orders-typed-tx"))
    .subscription_interests([orders_interest.clone()])
    .offset_manager(LocalDbOffsetManager::new(db.clone(), Fallback::Earliest))
    .route()
    .topic(orders_topic.clone())
    .event::<OrderCreatedV1>(TxUndecodable::Stall)
    .handler(TypedOrderHandler { db })
    .start()
    .await?;
```

A transactional handler whose transaction rolls back after `commit_offset_in_tx` must not
return `Success`: return `Retry` or an error. `commit_offset_in_tx` records the offset when
the offset manager's write returns, before the transaction commits.

---

## Features

| Feature | Enables | Extra deps |
|---|---|---|
| (default) | DB-free direct producer, consumer | — |
| `db` | `LocalDbOffsetManager`, `TxCommitHandle`, `TxUndecodable`, `TxTypedRouteBuilder` | `toolkit-db` |
| `outbox` | DB-aware producer outbox helper | `db` |
| `integration` | Integration tests (gated) | — |
