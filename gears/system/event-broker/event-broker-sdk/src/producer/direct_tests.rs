//! Chain stamping of the direct producer. The first two tests drive the
//! cursor helpers alone. The rest run a `DirectProducer` over a
//! `RecordingBroker` stub: it records every published event's source, producer
//! metadata and `Ack` in arrival order, keeps the head of what it recorded on
//! the orders topic, and answers each publish call with the next scripted
//! outcome.

use std::collections::VecDeque;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::{Notify, oneshot};
use tokio::task::JoinHandle;
use toolkit_gts::gts_id;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use ::gts::{GtsInstanceId, GtsSchema, GtsTypeId};

use crate::api::{Ack, IngestOutcome, ProducerCursors, ProducerMode, TopicCursors};
use crate::broker_stub::BrokerStub;
use crate::error::EventBrokerError;
use crate::gts::{EventMeta, EventV1};
use crate::ids::ProducerId;
use crate::models::{EventType, ResetScope, Topic};
use crate::sdk::EventBrokerSdk;

use super::direct::{
    DirectProducer, ProducerSequenceCursor, advance_cursor, producer_meta_for_last,
};
use super::event_factory::PreparedEvent;

const TOPIC: &str = gts_id!("cf.core.events.topic.v1~example.sdk.producer.unit_orders.v1");
const EVENT_TYPE: &str =
    gts_id!("cf.core.events.event.v1~example.sdk.producer.unit_order_created.v1~");
const SUBJECT_TYPE: &str = gts_id!("cf.core.events.subject.v1~example.sdk.producer.unit_order.v1~");
/// A second topic, whose head the stub never lists.
const INVOICE_TOPIC: &str =
    gts_id!("cf.core.events.topic.v1~example.sdk.producer.unit_invoices.v1");
const INVOICE_TYPE: &str =
    gts_id!("cf.core.events.event.v1~example.sdk.producer.unit_invoice_issued.v1~");
/// A second event type on the orders topic.
const SHIPPED_TYPE: &str =
    gts_id!("cf.core.events.event.v1~example.sdk.producer.unit_order_shipped.v1~");
const PRODUCER: ProducerId = ProducerId(Uuid::from_u128(0x0190_f1a2_0000_7000_8000_0000_0000_0d01));
const TENANT: Uuid = Uuid::from_u128(0x0190_f1a2_0000_7000_8000_0000_0000_7e01);
const READ_ONLY_DETAIL: &str = "broker-stamped and read-only on publish";

#[test]
fn monotonic_batch_advances_sequence_between_prepared_events() {
    let second = second_batch_meta(ProducerMode::Monotonic);

    assert_eq!(second.sequence, Some(1));
    assert_eq!(second.previous, None);
}

#[test]
fn chained_batch_advances_previous_between_prepared_events() {
    let second = second_batch_meta(ProducerMode::Chained);

    assert_eq!(second.sequence, Some(1));
    assert_eq!(second.previous, Some(0));
}

fn second_batch_meta(mode: ProducerMode) -> EventMeta {
    let producer_id = ProducerId(Uuid::from_u128(1));
    let first = producer_meta_for_last(mode, producer_id, -1).unwrap();
    let mut cursor = ProducerSequenceCursor::new();
    advance_cursor(&mut cursor, TOPIC, Some(&first));
    let last = cursor
        .get(&(producer_id, TOPIC.to_owned()))
        .copied()
        .unwrap();

    producer_meta_for_last(mode, producer_id, last).unwrap()
}

/// `(version, producer_id, previous, sequence)` of one published event.
type Stamp = (u32, Option<Uuid>, Option<i64>, Option<i64>);

fn stamp(meta: &EventMeta) -> Stamp {
    (meta.version, meta.producer_id, meta.previous, meta.sequence)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OrderCreated {
    order_id: Uuid,
}

impl GtsSchema for OrderCreated {
    const TYPE_ID: &'static str = EVENT_TYPE;

    fn gts_schema_with_refs() -> serde_json::Value {
        json!({ "type": "object" })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct InvoiceIssued {
    invoice_id: Uuid,
}

impl GtsSchema for InvoiceIssued {
    const TYPE_ID: &'static str = INVOICE_TYPE;

    fn gts_schema_with_refs() -> serde_json::Value {
        json!({ "type": "object" })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OrderShipped {
    order_id: Uuid,
}

impl GtsSchema for OrderShipped {
    const TYPE_ID: &'static str = SHIPPED_TYPE;

    fn gts_schema_with_refs() -> serde_json::Value {
        json!({ "type": "object" })
    }
}

fn order(n: u128) -> EventV1<OrderCreated> {
    let order_id = Uuid::from_u128(n);
    EventV1::builder(OrderCreated { order_id })
        .tenant_id(TENANT)
        .subject(order_id.to_string())
        .subject_type_id(GtsTypeId::new(SUBJECT_TYPE))
        .build()
        .expect("a valid order event")
}

fn invoice(n: u128) -> EventV1<InvoiceIssued> {
    let invoice_id = Uuid::from_u128(n);
    EventV1::builder(InvoiceIssued { invoice_id })
        .tenant_id(TENANT)
        .subject(invoice_id.to_string())
        .subject_type_id(GtsTypeId::new(SUBJECT_TYPE))
        .build()
        .expect("a valid invoice event")
}

fn shipment(n: u128) -> EventV1<OrderShipped> {
    let order_id = Uuid::from_u128(n);
    EventV1::builder(OrderShipped { order_id })
        .tenant_id(TENANT)
        .subject(order_id.to_string())
        .subject_type_id(GtsTypeId::new(SUBJECT_TYPE))
        .build()
        .expect("a valid shipment event")
}

/// The fixed id of the event `order_with_id(n)` builds.
fn event_id(n: u128) -> Uuid {
    Uuid::from_u128(0x0190_f1a2_0000_7000_8000_0000_0000_a000 + n)
}

/// `order(n)` with the fixed id `event_id(n)`, so a test can publish the same
/// event again.
fn order_with_id(n: u128) -> EventV1<OrderCreated> {
    let order_id = Uuid::from_u128(n);
    EventV1::builder(OrderCreated { order_id })
        .id(event_id(n))
        .tenant_id(TENANT)
        .subject(order_id.to_string())
        .subject_type_id(GtsTypeId::new(SUBJECT_TYPE))
        .build()
        .expect("a valid order event")
}

/// An order event as a caller reads it with serde, before the producer stamps
/// it: no source, no `meta`, no broker-stamped member.
fn order_body() -> serde_json::Value {
    json!({
        "id": Uuid::from_u128(0x0190_f1a2_0000_7000_8000_0000_0000_e101),
        "type": EVENT_TYPE,
        "tenant_id": TENANT,
        "subject": "order-1",
        "subject_type": SUBJECT_TYPE,
        "occurred_at": "2026-11-06T10:00:00Z",
        "data": { "order_id": Uuid::from_u128(1) },
    })
}

/// `order_body()` with `member` set to `value`, decoded.
fn decoded_order_with(member: &str, value: serde_json::Value) -> EventV1 {
    let mut body = order_body();
    body[member] = value;
    serde_json::from_value(body).expect("the order body decodes")
}

/// Serves the orders and invoices topics and their event types, counting the
/// lookups of one event type, and answers each `publish` / `publish_batch`
/// call with the next scripted outcome (`Accepted` once the script runs out).
/// It records each sent event's id and stamp, and stores what it records: the
/// producer's orders head starts at `last_sequence` and rises to the highest
/// recorded orders sequence, and every cursor read reports it, or no row once
/// the head is forgotten; it never lists an invoices head. Registering answers
/// `PRODUCER` and records the mode and client agent. A held publish reports on
/// `held_reached` and waits for its release before it answers. A failed
/// publish or batch is recorded, then answers a transport fault; a rejected
/// publish answers one without being recorded. A violated publish or batch
/// is recorded as sent but not stored, and answers a `SequenceViolation`
/// against the head the test moved. A failed cursor read answers a
/// transport fault. A chain reset of every topic or of the orders topic drops
/// the head, as a real reset does; a reset of any other topic leaves it. A
/// held reset drops the head, reports on `held_reached` and waits for its
/// release; a failed reset drops the head, then answers a transport fault. A
/// held cursor read or event-type lookup is counted, reports on
/// `held_reached` and waits for its release before it answers.
struct RecordingBroker {
    head: StdMutex<Option<i64>>,
    fail_next: AtomicBool,
    fail_next_batch: AtomicBool,
    reject_next: AtomicBool,
    violate_next: AtomicBool,
    fail_next_cursor_read: AtomicBool,
    fail_next_reset: AtomicBool,
    batch_calls: AtomicUsize,
    cursor_reads: AtomicUsize,
    resets: AtomicUsize,
    event_type_lookups: AtomicUsize,
    registrations: StdMutex<Vec<(ProducerMode, String)>>,
    answers: StdMutex<VecDeque<IngestOutcome>>,
    sent: StdMutex<Vec<(Uuid, Option<Stamp>)>>,
    sources: StdMutex<Vec<Option<String>>>,
    acks: StdMutex<Vec<Ack>>,
    hold: StdMutex<Option<oneshot::Receiver<()>>>,
    reset_hold: StdMutex<Option<oneshot::Receiver<()>>>,
    cursor_hold: StdMutex<Option<oneshot::Receiver<()>>>,
    lookup_hold: StdMutex<Option<oneshot::Receiver<()>>>,
    held_reached: Notify,
}

impl RecordingBroker {
    fn new(last_sequence: i64, answers: impl IntoIterator<Item = IngestOutcome>) -> Arc<Self> {
        Arc::new(Self {
            head: StdMutex::new(Some(last_sequence)),
            fail_next: AtomicBool::new(false),
            fail_next_batch: AtomicBool::new(false),
            reject_next: AtomicBool::new(false),
            violate_next: AtomicBool::new(false),
            fail_next_cursor_read: AtomicBool::new(false),
            fail_next_reset: AtomicBool::new(false),
            batch_calls: AtomicUsize::new(0),
            cursor_reads: AtomicUsize::new(0),
            resets: AtomicUsize::new(0),
            event_type_lookups: AtomicUsize::new(0),
            registrations: StdMutex::new(Vec::new()),
            answers: StdMutex::new(answers.into_iter().collect()),
            sent: StdMutex::new(Vec::new()),
            sources: StdMutex::new(Vec::new()),
            acks: StdMutex::new(Vec::new()),
            hold: StdMutex::new(None),
            reset_hold: StdMutex::new(None),
            cursor_hold: StdMutex::new(None),
            lookup_hold: StdMutex::new(None),
            held_reached: Notify::new(),
        })
    }

    /// Holds the next single publish until the returned sender fires or drops.
    fn hold_next_publish(&self) -> oneshot::Sender<()> {
        let (release, gate) = oneshot::channel();
        *self.hold.lock().expect("hold lock") = Some(gate);
        release
    }

    /// Holds the next chain reset until the returned sender fires or drops.
    fn hold_next_reset(&self) -> oneshot::Sender<()> {
        let (release, gate) = oneshot::channel();
        *self.reset_hold.lock().expect("reset hold lock") = Some(gate);
        release
    }

    /// Holds the next cursor read until the returned sender fires or drops.
    fn hold_next_cursor_read(&self) -> oneshot::Sender<()> {
        let (release, gate) = oneshot::channel();
        *self.cursor_hold.lock().expect("cursor hold lock") = Some(gate);
        release
    }

    /// Holds the next event-type lookup until the returned sender fires or
    /// drops.
    fn hold_next_lookup(&self) -> oneshot::Sender<()> {
        let (release, gate) = oneshot::channel();
        *self.lookup_hold.lock().expect("lookup hold lock") = Some(gate);
        release
    }

    /// Fails the next chain reset after it dropped the head.
    fn fail_next_reset(&self) {
        self.fail_next_reset.store(true, Ordering::SeqCst);
    }

    /// Fails the next single publish after recording it.
    fn fail_next_publish(&self) {
        self.fail_next.store(true, Ordering::SeqCst);
    }

    /// Fails the next batch after recording it.
    fn fail_next_batch(&self) {
        self.fail_next_batch.store(true, Ordering::SeqCst);
    }

    /// Fails the next single publish without recording it: the event is not
    /// stored.
    fn reject_next_publish(&self) {
        self.reject_next.store(true, Ordering::SeqCst);
    }

    /// Moves the producer's head to `head`, as an earlier request whose outcome
    /// the producer did not see would, and answers the next single publish or
    /// batch `SequenceViolation` against it, without storing its events.
    fn violate_next_publish(&self, head: i64) {
        *self.head.lock().expect("head lock") = Some(head);
        self.violate_next.store(true, Ordering::SeqCst);
    }

    /// Records `events` as sent without storing them, and answers the
    /// `SequenceViolation` the broker returns for numbers behind its head.
    fn violate(&self, events: &[EventV1]) -> EventBrokerError {
        self.record_sent(events);
        let head = self.head.lock().expect("head lock").unwrap_or(-1);
        EventBrokerError::SequenceViolation {
            expected_previous: head,
            detail: "stale previous".to_owned(),
        }
    }

    fn fail_next_cursor_read(&self) {
        self.fail_next_cursor_read.store(true, Ordering::SeqCst);
    }

    /// Drops the producer's head: cursor reads list no row for the topic, as
    /// after a reset.
    fn forget_head(&self) {
        *self.head.lock().expect("head lock") = None;
    }

    fn batch_calls(&self) -> usize {
        self.batch_calls.load(Ordering::SeqCst)
    }

    /// Cursor reads answered or failed, the read at build included.
    fn cursor_reads(&self) -> usize {
        self.cursor_reads.load(Ordering::SeqCst)
    }

    fn resets(&self) -> usize {
        self.resets.load(Ordering::SeqCst)
    }

    /// Lookups of one event type, the way a lazy producer prepares a type.
    fn event_type_lookups(&self) -> usize {
        self.event_type_lookups.load(Ordering::SeqCst)
    }

    /// The mode and client agent of each registration, in order.
    fn registrations(&self) -> Vec<(ProducerMode, String)> {
        self.registrations
            .lock()
            .expect("registrations lock")
            .clone()
    }

    fn stamps(&self) -> Vec<Option<Stamp>> {
        self.sent().into_iter().map(|(_, stamp)| stamp).collect()
    }

    /// The id and stamp of each sent event, in order.
    fn sent(&self) -> Vec<(Uuid, Option<Stamp>)> {
        self.sent.lock().expect("sent lock").clone()
    }

    fn sources(&self) -> Vec<Option<String>> {
        self.sources.lock().expect("sources lock").clone()
    }

    /// The `Ack` of each publish or batch call, in order.
    fn acks(&self) -> Vec<Ack> {
        self.acks.lock().expect("acks lock").clone()
    }

    fn record_sent(&self, events: &[EventV1]) {
        self.sent.lock().expect("sent lock").extend(
            events
                .iter()
                .map(|event| (event.id(), event.meta().map(stamp))),
        );
        self.sources
            .lock()
            .expect("sources lock")
            .extend(events.iter().map(|event| event.source().map(str::to_owned)));
    }

    fn record(&self, events: &[EventV1]) -> IngestOutcome {
        self.record_sent(events);
        let mut head = self.head.lock().expect("head lock");
        for sequence in events
            .iter()
            .filter(|event| matches!(event.event_type().as_ref(), EVENT_TYPE | SHIPPED_TYPE))
            .filter_map(|event| event.meta().and_then(|meta| meta.sequence))
        {
            *head = Some(head.map_or(sequence, |last| last.max(sequence)));
        }
        drop(head);
        self.answers
            .lock()
            .expect("answers lock")
            .pop_front()
            .unwrap_or(IngestOutcome::Accepted)
    }
}

#[async_trait]
impl BrokerStub for RecordingBroker {
    async fn register_producer(
        &self,
        _ctx: &SecurityContext,
        mode: ProducerMode,
        client_agent: &str,
    ) -> Result<ProducerId, EventBrokerError> {
        self.registrations
            .lock()
            .expect("registrations lock")
            .push((mode, client_agent.to_owned()));
        Ok(PRODUCER)
    }
    async fn publish(
        &self,
        _ctx: &SecurityContext,
        event: &EventV1,
        ack: Ack,
    ) -> Result<IngestOutcome, EventBrokerError> {
        self.acks.lock().expect("acks lock").push(ack);
        if self.reject_next.swap(false, Ordering::SeqCst) {
            return Err(EventBrokerError::Transport("connection refused".to_owned()));
        }
        if self.violate_next.swap(false, Ordering::SeqCst) {
            return Err(self.violate(std::slice::from_ref(event)));
        }
        let outcome = self.record(std::slice::from_ref(event));
        if self.fail_next.swap(false, Ordering::SeqCst) {
            return Err(EventBrokerError::Transport("connection reset".to_owned()));
        }
        let gate = self.hold.lock().expect("hold lock").take();
        if let Some(gate) = gate {
            self.held_reached.notify_one();
            // A dropped sender releases the call too.
            let _ = gate.await;
        }
        Ok(outcome)
    }
    async fn publish_batch(
        &self,
        _ctx: &SecurityContext,
        events: &[EventV1],
        ack: Ack,
    ) -> Result<IngestOutcome, EventBrokerError> {
        self.acks.lock().expect("acks lock").push(ack);
        self.batch_calls.fetch_add(1, Ordering::SeqCst);
        if self.violate_next.swap(false, Ordering::SeqCst) {
            return Err(self.violate(events));
        }
        let outcome = self.record(events);
        if self.fail_next_batch.swap(false, Ordering::SeqCst) {
            return Err(EventBrokerError::Transport("connection reset".to_owned()));
        }
        Ok(outcome)
    }
    async fn get_producer_cursors(
        &self,
        _ctx: &SecurityContext,
        producer_id: ProducerId,
    ) -> Result<ProducerCursors, EventBrokerError> {
        self.cursor_reads.fetch_add(1, Ordering::SeqCst);
        if self.fail_next_cursor_read.swap(false, Ordering::SeqCst) {
            return Err(EventBrokerError::Transport("cursor read reset".to_owned()));
        }
        let gate = self.cursor_hold.lock().expect("cursor hold lock").take();
        if let Some(gate) = gate {
            self.held_reached.notify_one();
            // A dropped sender releases the call too.
            let _ = gate.await;
        }
        let head = *self.head.lock().expect("head lock");
        Ok(ProducerCursors {
            producer_id,
            client_agent: "order-service/1.0".to_owned(),
            topics: head
                .map(|last_sequence| TopicCursors {
                    topic: GtsInstanceId::try_new(TOPIC).expect("a valid topic id"),
                    last_sequence,
                })
                .into_iter()
                .collect(),
        })
    }
    async fn reset_producer_chain(
        &self,
        _ctx: &SecurityContext,
        _producer_id: ProducerId,
        scope: ResetScope<'_>,
    ) -> Result<(), EventBrokerError> {
        self.resets.fetch_add(1, Ordering::SeqCst);
        if matches!(scope, ResetScope::AllTopics | ResetScope::Topic(TOPIC)) {
            self.forget_head();
        }
        if self.fail_next_reset.swap(false, Ordering::SeqCst) {
            return Err(EventBrokerError::Transport("connection reset".to_owned()));
        }
        let gate = self.reset_hold.lock().expect("reset hold lock").take();
        if let Some(gate) = gate {
            self.held_reached.notify_one();
            // A dropped sender releases the call too.
            let _ = gate.await;
        }
        Ok(())
    }
    async fn list_topics(&self, _ctx: &SecurityContext) -> Result<Vec<Topic>, EventBrokerError> {
        Ok(vec![
            Topic {
                id: GtsInstanceId::try_new(TOPIC).expect("a valid topic id"),
                description: "orders".to_owned(),
                retention: None,
            },
            Topic {
                id: GtsInstanceId::try_new(INVOICE_TOPIC).expect("a valid topic id"),
                description: "invoices".to_owned(),
                retention: None,
            },
        ])
    }
    async fn list_event_types(
        &self,
        _ctx: &SecurityContext,
    ) -> Result<Vec<EventType>, EventBrokerError> {
        Ok(Self::event_types())
    }
    async fn get_event_type(
        &self,
        _ctx: &SecurityContext,
        id: &str,
    ) -> Result<EventType, EventBrokerError> {
        self.event_type_lookups.fetch_add(1, Ordering::SeqCst);
        let gate = self.lookup_hold.lock().expect("lookup hold lock").take();
        if let Some(gate) = gate {
            self.held_reached.notify_one();
            // A dropped sender releases the call too.
            let _ = gate.await;
        }
        Self::event_types()
            .into_iter()
            .find(|event_type| event_type.id == id)
            .ok_or_else(|| EventBrokerError::EventTypeUnknown {
                type_id: id.to_owned(),
                detail: "not served".to_owned(),
            })
    }
}

impl RecordingBroker {
    fn event_types() -> Vec<EventType> {
        vec![
            EventType {
                id: GtsTypeId::try_new(EVENT_TYPE).expect("a valid event type id"),
                topic: GtsInstanceId::try_new(TOPIC).expect("a valid topic id"),
                description: None,
                allowed_subject_types: Vec::new(),
                partition_key: "/tenant_id".to_owned(),
                data_schema: json!({ "type": "object" }),
            },
            EventType {
                id: GtsTypeId::try_new(SHIPPED_TYPE).expect("a valid event type id"),
                topic: GtsInstanceId::try_new(TOPIC).expect("a valid topic id"),
                description: None,
                allowed_subject_types: Vec::new(),
                partition_key: "/tenant_id".to_owned(),
                data_schema: json!({ "type": "object" }),
            },
            EventType {
                id: GtsTypeId::try_new(INVOICE_TYPE).expect("a valid event type id"),
                topic: GtsInstanceId::try_new(INVOICE_TOPIC).expect("a valid topic id"),
                description: None,
                allowed_subject_types: Vec::new(),
                partition_key: "/tenant_id".to_owned(),
                data_schema: json!({ "type": "object" }),
            },
        ]
    }
}

async fn producer_over(broker: Arc<RecordingBroker>, mode: ProducerMode) -> DirectProducer {
    DirectProducer::builder()
        .broker(broker)
        .security_context(SecurityContext::anonymous())
        .source("order-service")
        .mode(mode)
        .topics([TOPIC])
        .event_type_patterns([EVENT_TYPE])
        .build()
        .await
        .expect("the producer builds")
}

/// A's fresh numbers are answered `Duplicate`: the broker already holds that
/// sequence, so the producer advances past it and resends A with the next
/// numbers. B chains onto the resent A.
#[tokio::test]
async fn a_duplicate_ack_advances_the_local_chain() {
    let broker = RecordingBroker::new(-1, [IngestOutcome::Duplicate]);
    let producer = producer_over(Arc::clone(&broker), ProducerMode::Chained).await;

    let a = producer
        .publish(order(1), Ack::Accepted)
        .await
        .expect("A publishes");
    let b = producer
        .publish(order(2), Ack::Accepted)
        .await
        .expect("B publishes");

    assert_eq!((a, b), (IngestOutcome::Accepted, IngestOutcome::Accepted));
    assert_eq!(
        broker.stamps(),
        vec![
            Some((1, Some(PRODUCER.0), Some(-1), Some(0))),
            Some((1, Some(PRODUCER.0), Some(0), Some(1))),
            Some((1, Some(PRODUCER.0), Some(1), Some(2))),
        ],
        "A is resent onto the sequence the broker already holds; B chains onto A"
    );
}

/// As above for a monotonic producer: A is resent past the sequence the
/// broker already holds, and B moves past A.
#[tokio::test]
async fn a_duplicate_ack_advances_a_monotonic_producer() {
    let broker = RecordingBroker::new(-1, [IngestOutcome::Duplicate]);
    let producer = producer_over(Arc::clone(&broker), ProducerMode::Monotonic).await;

    let a = producer
        .publish(order(1), Ack::Accepted)
        .await
        .expect("A publishes");
    let b = producer
        .publish(order(2), Ack::Accepted)
        .await
        .expect("B publishes");

    assert_eq!((a, b), (IngestOutcome::Accepted, IngestOutcome::Accepted));
    assert_eq!(
        broker.stamps(),
        vec![
            Some((1, Some(PRODUCER.0), None, Some(0))),
            Some((1, Some(PRODUCER.0), None, Some(1))),
            Some((1, Some(PRODUCER.0), None, Some(2))),
        ],
        "A is resent past the sequence the broker already holds; B moves past A"
    );
}

#[tokio::test]
async fn an_empty_batch_is_refused_and_leaves_the_chain_unmoved() {
    let broker = RecordingBroker::new(-1, []);
    let producer = producer_over(Arc::clone(&broker), ProducerMode::Chained).await;

    let batch = producer
        .publish_batch(Vec::<EventV1<OrderCreated>>::new(), Ack::Accepted)
        .await;
    let next = producer
        .publish(order(1), Ack::Accepted)
        .await
        .expect("the next publishes");

    let Err(EventBrokerError::InvalidEventField { field, detail }) = batch else {
        panic!("an empty batch must be refused, got {batch:?}");
    };
    assert_eq!((field, detail.as_str()), ("events", "empty batch"));
    assert_eq!((next, broker.batch_calls()), (IngestOutcome::Accepted, 0));
    assert_eq!(
        broker.stamps(),
        vec![Some((1, Some(PRODUCER.0), Some(-1), Some(0)))],
        "the next event takes the chain's first position: the empty batch moved nothing"
    );
}

/// The batch's fresh numbers are answered `Duplicate`: the producer advances
/// to the batch's highest sequence and resends the batch from there; the next
/// event chains onto the resent batch.
#[tokio::test]
async fn an_all_duplicate_batch_advances_the_local_chain_to_its_highest_sequence() {
    let broker = RecordingBroker::new(-1, [IngestOutcome::Duplicate]);
    let producer = producer_over(Arc::clone(&broker), ProducerMode::Chained).await;

    let batch = producer
        .publish_batch(vec![order(1), order(2)], Ack::Accepted)
        .await
        .expect("the batch publishes");
    let next = producer
        .publish(order(3), Ack::Accepted)
        .await
        .expect("the next publishes");

    assert_eq!(
        (batch, next),
        (IngestOutcome::Accepted, IngestOutcome::Accepted)
    );
    assert_eq!(
        broker.stamps(),
        vec![
            Some((1, Some(PRODUCER.0), Some(-1), Some(0))),
            Some((1, Some(PRODUCER.0), Some(0), Some(1))),
            Some((1, Some(PRODUCER.0), Some(1), Some(2))),
            Some((1, Some(PRODUCER.0), Some(2), Some(3))),
            Some((1, Some(PRODUCER.0), Some(3), Some(4))),
        ],
        "the batch is resent past its highest sequence; the next event chains onto it"
    );
}

/// Bounds every await on the stub's hold, so a lock regression fails fast
/// instead of hanging the run.
const HOLD_TIMEOUT: Duration = Duration::from_secs(10);

/// A is held at the broker while B is started on the same producer and
/// topic. The yields let B run as far as it can before A is released: it
/// waits on the topic's publish lock, so it is stamped only after A advanced
/// the chain. Without that lock B is stamped with A's numbers and the broker
/// would store one event and answer `Duplicate` to the other. B can park on
/// a contended await before it reaches the lock; the 16 yields bound how many
/// times it may park and still be held behind A.
async fn publish_two_concurrently(
    mode: ProducerMode,
) -> (Arc<RecordingBroker>, [IngestOutcome; 2]) {
    let broker = RecordingBroker::new(4, []);
    let producer = Arc::new(producer_over(Arc::clone(&broker), mode).await);
    let release = broker.hold_next_publish();
    let a = tokio::spawn({
        let producer = Arc::clone(&producer);
        async move { producer.publish(order(1), Ack::Accepted).await }
    });
    tokio::time::timeout(HOLD_TIMEOUT, broker.held_reached.notified())
        .await
        .expect("A reaches the broker");

    let b = tokio::spawn({
        let producer = Arc::clone(&producer);
        async move { producer.publish(order(2), Ack::Accepted).await }
    });
    for _ in 0..16 {
        tokio::task::yield_now().await;
    }
    release.send(()).expect("A is still held");
    let a = tokio::time::timeout(HOLD_TIMEOUT, a)
        .await
        .expect("A finishes in time")
        .expect("A completes")
        .expect("A answers");
    let b = tokio::time::timeout(HOLD_TIMEOUT, b)
        .await
        .expect("B finishes in time")
        .expect("B completes")
        .expect("B answers");
    (broker, [a, b])
}

#[tokio::test]
async fn concurrent_publishes_on_one_producer_store_both_events() {
    let (broker, outcomes) = publish_two_concurrently(ProducerMode::Chained).await;

    assert_eq!(outcomes, [IngestOutcome::Accepted, IngestOutcome::Accepted]);
    assert_eq!(
        broker.stamps(),
        vec![
            Some((1, Some(PRODUCER.0), Some(4), Some(5))),
            Some((1, Some(PRODUCER.0), Some(5), Some(6))),
        ],
        "B chains onto A instead of repeating A's numbers"
    );
}

#[tokio::test]
async fn concurrent_publishes_on_one_monotonic_producer_store_both_events() {
    let (broker, outcomes) = publish_two_concurrently(ProducerMode::Monotonic).await;

    assert_eq!(outcomes, [IngestOutcome::Accepted, IngestOutcome::Accepted]);
    assert_eq!(
        broker.stamps(),
        vec![
            Some((1, Some(PRODUCER.0), None, Some(5))),
            Some((1, Some(PRODUCER.0), None, Some(6))),
        ],
        "B moves past A's sequence instead of repeating it"
    );
}

/// The broker stores A but fails its answer, so the producer cannot tell
/// whether A is stored and its head is uncertain. B re-reads the head from the
/// broker before it is stamped and chains onto A; C follows a seen outcome and
/// reads nothing.
#[tokio::test]
async fn a_publish_after_a_failed_one_is_stamped_from_the_brokers_head() {
    let broker = RecordingBroker::new(4, []);
    let producer = producer_over(Arc::clone(&broker), ProducerMode::Chained).await;
    broker.fail_next_publish();

    let a = producer.publish(order(1), Ack::Accepted).await;
    let b = tokio::time::timeout(HOLD_TIMEOUT, producer.publish(order(2), Ack::Accepted))
        .await
        .expect("B is not blocked by A's lock");
    let c = producer.publish(order(3), Ack::Accepted).await;

    assert!(
        matches!(a, Err(EventBrokerError::Transport(ref detail)) if detail == "connection reset"),
        "A surfaces the transport fault, got {a:?}"
    );
    assert_eq!(
        (b.expect("B answers"), c.expect("C answers")),
        (IngestOutcome::Accepted, IngestOutcome::Accepted)
    );
    assert_eq!(
        broker.stamps(),
        vec![
            Some((1, Some(PRODUCER.0), Some(4), Some(5))),
            Some((1, Some(PRODUCER.0), Some(5), Some(6))),
            Some((1, Some(PRODUCER.0), Some(6), Some(7))),
        ],
        "B chains onto the stored A instead of reusing its numbers"
    );
    assert_eq!(
        broker.cursor_reads(),
        2,
        "the read at build and one re-read before B; C reads nothing"
    );
}

/// As above for a monotonic producer: B moves past the stored A's sequence.
#[tokio::test]
async fn a_monotonic_publish_after_a_failed_one_is_stamped_from_the_brokers_head() {
    let broker = RecordingBroker::new(4, []);
    let producer = producer_over(Arc::clone(&broker), ProducerMode::Monotonic).await;
    broker.fail_next_publish();

    let a = producer.publish(order(1), Ack::Accepted).await;
    let b = producer.publish(order(2), Ack::Accepted).await;

    assert!(
        matches!(a, Err(EventBrokerError::Transport(ref detail)) if detail == "connection reset"),
        "A surfaces the transport fault, got {a:?}"
    );
    assert_eq!(b.expect("B answers"), IngestOutcome::Accepted);
    assert_eq!(
        broker.stamps(),
        vec![
            Some((1, Some(PRODUCER.0), None, Some(5))),
            Some((1, Some(PRODUCER.0), None, Some(6))),
        ],
        "B moves past the stored A's sequence instead of reusing it"
    );
    assert_eq!(
        broker.cursor_reads(),
        2,
        "the read at build and one re-read"
    );
}

/// A reaches the broker, which stores it, and the caller drops A's future while
/// the broker call is in flight. B re-reads the head and chains onto A.
#[tokio::test]
async fn a_publish_after_a_dropped_one_is_stamped_from_the_brokers_head() {
    let broker = RecordingBroker::new(4, []);
    let producer = Arc::new(producer_over(Arc::clone(&broker), ProducerMode::Chained).await);
    let release = broker.hold_next_publish();
    let a = tokio::spawn({
        let producer = Arc::clone(&producer);
        async move { producer.publish(order(1), Ack::Accepted).await }
    });
    tokio::time::timeout(HOLD_TIMEOUT, broker.held_reached.notified())
        .await
        .expect("A reaches the broker");

    a.abort();
    let a = tokio::time::timeout(HOLD_TIMEOUT, a)
        .await
        .expect("A ends in time");
    drop(release);
    let b = tokio::time::timeout(HOLD_TIMEOUT, producer.publish(order(2), Ack::Accepted))
        .await
        .expect("B is not blocked by the dropped A");

    assert!(
        matches!(a, Err(ref error) if error.is_cancelled()),
        "A is cancelled, got {a:?}"
    );
    assert_eq!(b.expect("B answers"), IngestOutcome::Accepted);
    assert_eq!(
        broker.stamps(),
        vec![
            Some((1, Some(PRODUCER.0), Some(4), Some(5))),
            Some((1, Some(PRODUCER.0), Some(5), Some(6))),
        ],
        "B chains onto the stored A instead of reusing its numbers"
    );
    assert_eq!(
        broker.cursor_reads(),
        2,
        "the read at build and one re-read"
    );
}

/// The broker stores batch A but fails its answer. Batch B re-reads the head
/// once and chains onto A's last event.
#[tokio::test]
async fn a_batch_after_a_failed_batch_is_stamped_from_the_brokers_head() {
    let broker = RecordingBroker::new(4, []);
    let producer = producer_over(Arc::clone(&broker), ProducerMode::Chained).await;
    broker.fail_next_batch();

    let a = producer
        .publish_batch(vec![order(1), order(2)], Ack::Accepted)
        .await;
    let b = producer
        .publish_batch(vec![order(3), order(4)], Ack::Accepted)
        .await;

    assert!(
        matches!(a, Err(EventBrokerError::Transport(ref detail)) if detail == "connection reset"),
        "batch A surfaces the transport fault, got {a:?}"
    );
    assert_eq!(b.expect("batch B answers"), IngestOutcome::Accepted);
    assert_eq!(
        broker.stamps(),
        vec![
            Some((1, Some(PRODUCER.0), Some(4), Some(5))),
            Some((1, Some(PRODUCER.0), Some(5), Some(6))),
            Some((1, Some(PRODUCER.0), Some(6), Some(7))),
            Some((1, Some(PRODUCER.0), Some(7), Some(8))),
        ],
        "batch B chains onto the stored batch A instead of reusing its numbers"
    );
    assert_eq!(
        (broker.batch_calls(), broker.cursor_reads()),
        (2, 2),
        "two batches; the read at build and one re-read"
    );
}

/// After batch A fails, batch B re-reads the head and its outcome is seen, so
/// batch C is stamped from the local head and reads nothing.
#[tokio::test]
async fn a_batch_after_a_seen_batch_outcome_reads_no_head() {
    let broker = RecordingBroker::new(4, []);
    let producer = producer_over(Arc::clone(&broker), ProducerMode::Chained).await;
    broker.fail_next_batch();

    let a = producer
        .publish_batch(vec![order(1), order(2)], Ack::Accepted)
        .await;
    let b = producer.publish_batch(vec![order(3)], Ack::Accepted).await;
    let c = producer.publish_batch(vec![order(4)], Ack::Accepted).await;

    assert!(
        matches!(a, Err(EventBrokerError::Transport(ref detail)) if detail == "connection reset"),
        "batch A surfaces the transport fault, got {a:?}"
    );
    assert_eq!(
        (b.expect("batch B answers"), c.expect("batch C answers")),
        (IngestOutcome::Accepted, IngestOutcome::Accepted)
    );
    assert_eq!(
        broker.stamps(),
        vec![
            Some((1, Some(PRODUCER.0), Some(4), Some(5))),
            Some((1, Some(PRODUCER.0), Some(5), Some(6))),
            Some((1, Some(PRODUCER.0), Some(6), Some(7))),
            Some((1, Some(PRODUCER.0), Some(7), Some(8))),
        ],
        "batches B and C chain onto the stored batch A"
    );
    assert_eq!(
        (broker.batch_calls(), broker.cursor_reads()),
        (3, 2),
        "three batches; the read at build and one re-read before B; C reads nothing"
    );
}

/// A failed batch leaves its topic's head uncertain for the single path too.
#[tokio::test]
async fn a_monotonic_publish_after_a_failed_batch_is_stamped_from_the_brokers_head() {
    let broker = RecordingBroker::new(4, []);
    let producer = producer_over(Arc::clone(&broker), ProducerMode::Monotonic).await;
    broker.fail_next_batch();

    let a = producer
        .publish_batch(vec![order(1), order(2)], Ack::Accepted)
        .await;
    let b = producer.publish(order(3), Ack::Accepted).await;

    assert!(
        matches!(a, Err(EventBrokerError::Transport(ref detail)) if detail == "connection reset"),
        "batch A surfaces the transport fault, got {a:?}"
    );
    assert_eq!(b.expect("B answers"), IngestOutcome::Accepted);
    assert_eq!(
        broker.stamps(),
        vec![
            Some((1, Some(PRODUCER.0), None, Some(5))),
            Some((1, Some(PRODUCER.0), None, Some(6))),
            Some((1, Some(PRODUCER.0), None, Some(7))),
        ],
        "B moves past the stored batch A instead of reusing its sequences"
    );
    assert_eq!(
        broker.cursor_reads(),
        2,
        "the read at build and one re-read"
    );
}

/// A fails, so B must re-read the head, and that read fails: B returns the
/// read's error and sends nothing. The head stays uncertain, so C reads again
/// and chains onto A.
#[tokio::test]
async fn a_failed_head_reread_fails_the_publish_and_is_retried_by_the_next_one() {
    let broker = RecordingBroker::new(4, []);
    let producer = producer_over(Arc::clone(&broker), ProducerMode::Chained).await;
    broker.fail_next_publish();

    let a = producer.publish(order(1), Ack::Accepted).await;
    broker.fail_next_cursor_read();
    let b = producer.publish(order(2), Ack::Accepted).await;
    let c = producer.publish(order(3), Ack::Accepted).await;

    assert!(
        matches!(a, Err(EventBrokerError::Transport(ref detail)) if detail == "connection reset"),
        "A surfaces the transport fault, got {a:?}"
    );
    assert!(
        matches!(b, Err(EventBrokerError::Transport(ref detail)) if detail == "cursor read reset"),
        "B surfaces the failed re-read, got {b:?}"
    );
    assert_eq!(c.expect("C answers"), IngestOutcome::Accepted);
    assert_eq!(
        broker.stamps(),
        vec![
            Some((1, Some(PRODUCER.0), Some(4), Some(5))),
            Some((1, Some(PRODUCER.0), Some(5), Some(6))),
        ],
        "B sends nothing; C chains onto the stored A"
    );
    assert_eq!(
        broker.cursor_reads(),
        3,
        "the read at build, B's failed re-read and C's re-read"
    );
}

/// The broker lists no head for the topic any more, and A is refused without
/// being stored. B re-reads the head, finds no row, and restarts the chain.
#[tokio::test]
async fn a_head_the_broker_no_longer_lists_restarts_the_chain() {
    let broker = RecordingBroker::new(4, []);
    let producer = producer_over(Arc::clone(&broker), ProducerMode::Chained).await;
    broker.forget_head();
    broker.reject_next_publish();

    let a = producer.publish(order(1), Ack::Accepted).await;
    let b = producer.publish(order(2), Ack::Accepted).await;

    assert!(
        matches!(a, Err(EventBrokerError::Transport(ref detail)) if detail == "connection refused"),
        "A surfaces the transport fault, got {a:?}"
    );
    assert_eq!(b.expect("B answers"), IngestOutcome::Accepted);
    assert_eq!(
        broker.stamps(),
        vec![Some((1, Some(PRODUCER.0), Some(-1), Some(0)))],
        "B takes the chain's first position: the broker lists no head"
    );
    assert_eq!(
        broker.cursor_reads(),
        2,
        "the read at build and one re-read"
    );
}

/// Local state stands at 2. A single publish stamped 3 is held at the broker
/// while a batch is started on the same topic. The batch waits on the topic's
/// publish lock, so it cannot be stamped 3..=5 behind the held publish and
/// race its answer. The held publish is answered `Duplicate` for 3, advances
/// the chain to 3 and is resent as 4 under the same lock; the batch is stamped
/// 5..=7 after it. The batch can park on
/// a contended await before it reaches the lock; the 16 yields bound how many
/// times it may park and still be held behind the held publish.
#[tokio::test]
async fn a_batch_started_behind_a_held_publish_is_stamped_after_it() {
    let broker = RecordingBroker::new(2, [IngestOutcome::Duplicate, IngestOutcome::Accepted]);
    let producer = Arc::new(producer_over(Arc::clone(&broker), ProducerMode::Chained).await);
    let release = broker.hold_next_publish();
    let held = tokio::spawn({
        let producer = Arc::clone(&producer);
        async move { producer.publish(order(1), Ack::Accepted).await }
    });
    tokio::time::timeout(HOLD_TIMEOUT, broker.held_reached.notified())
        .await
        .expect("the held publish reaches the broker");

    let batch = tokio::spawn({
        let producer = Arc::clone(&producer);
        async move {
            producer
                .publish_batch(vec![order(2), order(3), order(4)], Ack::Accepted)
                .await
        }
    });
    // A batch that skipped the lock would reach the broker here, before the
    // release.
    for _ in 0..16 {
        tokio::task::yield_now().await;
    }
    release.send(()).expect("the publish is still held");
    let held = tokio::time::timeout(HOLD_TIMEOUT, held)
        .await
        .expect("the held publish finishes in time")
        .expect("the held publish completes")
        .expect("the held publish answers");
    let batch = tokio::time::timeout(HOLD_TIMEOUT, batch)
        .await
        .expect("the batch finishes in time")
        .expect("the batch completes")
        .expect("the batch publishes");
    let next = producer
        .publish(order(5), Ack::Accepted)
        .await
        .expect("the next publishes");

    assert_eq!(
        (held, batch, next),
        (
            IngestOutcome::Accepted,
            IngestOutcome::Accepted,
            IngestOutcome::Accepted
        )
    );
    assert_eq!(
        broker.stamps(),
        vec![
            Some((1, Some(PRODUCER.0), Some(2), Some(3))),
            Some((1, Some(PRODUCER.0), Some(3), Some(4))),
            Some((1, Some(PRODUCER.0), Some(4), Some(5))),
            Some((1, Some(PRODUCER.0), Some(5), Some(6))),
            Some((1, Some(PRODUCER.0), Some(6), Some(7))),
            Some((1, Some(PRODUCER.0), Some(7), Some(8))),
        ],
        "the batch continues from the held publish's resent 4"
    );
}

/// A reset of every topic drops the broker's head, so the local head of 5 is
/// stale: B re-reads the head, finds no row, and restarts the chain.
#[tokio::test]
async fn a_publish_after_a_full_chain_reset_restarts_the_chain() {
    let broker = RecordingBroker::new(4, []);
    let producer = producer_over(Arc::clone(&broker), ProducerMode::Chained).await;

    let a = producer
        .publish(order(1), Ack::Accepted)
        .await
        .expect("A publishes");
    producer
        .reset_chain(ResetScope::AllTopics)
        .await
        .expect("the reset succeeds");
    let b = producer
        .publish(order(2), Ack::Accepted)
        .await
        .expect("B publishes");

    assert_eq!((a, b), (IngestOutcome::Accepted, IngestOutcome::Accepted));
    assert_eq!(
        broker.stamps(),
        vec![
            Some((1, Some(PRODUCER.0), Some(4), Some(5))),
            Some((1, Some(PRODUCER.0), Some(-1), Some(0))),
        ],
        "B takes the chain's first position after the reset"
    );
    assert_eq!(
        (broker.cursor_reads(), broker.resets()),
        (2, 1),
        "the read at build and one re-read before B; one reset"
    );
}

/// As above for a monotonic producer and a reset of the orders topic alone.
#[tokio::test]
async fn a_publish_after_its_topics_chain_reset_restarts_the_chain() {
    let broker = RecordingBroker::new(4, []);
    let producer = producer_over(Arc::clone(&broker), ProducerMode::Monotonic).await;

    let a = producer
        .publish(order(1), Ack::Accepted)
        .await
        .expect("A publishes");
    producer
        .reset_chain(ResetScope::Topic(TOPIC))
        .await
        .expect("the reset succeeds");
    let b = producer
        .publish(order(2), Ack::Accepted)
        .await
        .expect("B publishes");

    assert_eq!((a, b), (IngestOutcome::Accepted, IngestOutcome::Accepted));
    assert_eq!(
        broker.stamps(),
        vec![
            Some((1, Some(PRODUCER.0), None, Some(5))),
            Some((1, Some(PRODUCER.0), None, Some(0))),
        ],
        "B takes the chain's first position after the reset"
    );
    assert_eq!(
        (broker.cursor_reads(), broker.resets()),
        (2, 1),
        "the read at build and one re-read before B; one reset"
    );
}

/// The broker drops the head on the reset and then fails its answer, so the
/// producer cannot tell whether the reset happened: B re-reads the head and
/// restarts the chain.
#[tokio::test]
async fn a_publish_after_a_failed_chain_reset_is_stamped_from_the_brokers_head() {
    let broker = RecordingBroker::new(4, []);
    let producer = producer_over(Arc::clone(&broker), ProducerMode::Chained).await;

    let a = producer
        .publish(order(1), Ack::Accepted)
        .await
        .expect("A publishes");
    broker.fail_next_reset();
    let reset = producer.reset_chain(ResetScope::AllTopics).await;
    let b = producer
        .publish(order(2), Ack::Accepted)
        .await
        .expect("B publishes");

    assert!(
        matches!(
            reset,
            Err(EventBrokerError::Transport(ref detail)) if detail == "connection reset"
        ),
        "the reset surfaces the transport fault, got {reset:?}"
    );
    assert_eq!((a, b), (IngestOutcome::Accepted, IngestOutcome::Accepted));
    assert_eq!(
        broker.stamps(),
        vec![
            Some((1, Some(PRODUCER.0), Some(4), Some(5))),
            Some((1, Some(PRODUCER.0), Some(-1), Some(0))),
        ],
        "B takes the chain's first position: the broker reset the chain"
    );
    assert_eq!(
        (broker.cursor_reads(), broker.resets()),
        (2, 1),
        "the read at build and one re-read before B; one reset"
    );
}

/// The producer does not declare the other topic, so it has no local head for
/// it: the reset reaches the broker and leaves the orders chain alone.
#[tokio::test]
async fn a_reset_of_an_undeclared_topic_leaves_the_declared_chain_alone() {
    let broker = RecordingBroker::new(4, []);
    let producer = producer_over(Arc::clone(&broker), ProducerMode::Chained).await;

    let a = producer
        .publish(order(1), Ack::Accepted)
        .await
        .expect("A publishes");
    producer
        .reset_chain(ResetScope::Topic(gts_id!(
            "cf.core.events.topic.v1~example.sdk.producer.other.v1"
        )))
        .await
        .expect("the reset succeeds");
    let b = producer
        .publish(order(2), Ack::Accepted)
        .await
        .expect("B publishes");

    assert_eq!((a, b), (IngestOutcome::Accepted, IngestOutcome::Accepted));
    assert_eq!(
        broker.stamps(),
        vec![
            Some((1, Some(PRODUCER.0), Some(4), Some(5))),
            Some((1, Some(PRODUCER.0), Some(5), Some(6))),
        ],
        "B chains onto A: the orders chain was not reset"
    );
    assert_eq!(
        (broker.cursor_reads(), broker.resets()),
        (1, 1),
        "the read at build only; one reset"
    );
}

/// A reset of every topic is held at the broker while B is started on the
/// orders topic. B waits on the topic's publish lock, which the reset holds,
/// so it is stamped only after the reset, from the re-read head. Without that
/// lock B is stamped from the stale local head while the reset is in flight. B
/// can park on a contended await before it reaches the lock; the 16 yields
/// bound how many times it may park and still be held behind the reset.
#[tokio::test]
async fn a_publish_waiting_behind_a_reset_is_stamped_after_it() {
    let broker = RecordingBroker::new(4, []);
    let producer = Arc::new(producer_over(Arc::clone(&broker), ProducerMode::Chained).await);
    let a = producer
        .publish(order(1), Ack::Accepted)
        .await
        .expect("A publishes");
    let release = broker.hold_next_reset();
    let reset = tokio::spawn({
        let producer = Arc::clone(&producer);
        async move { producer.reset_chain(ResetScope::AllTopics).await }
    });
    tokio::time::timeout(HOLD_TIMEOUT, broker.held_reached.notified())
        .await
        .expect("the reset reaches the broker");

    let b = tokio::spawn({
        let producer = Arc::clone(&producer);
        async move { producer.publish(order(2), Ack::Accepted).await }
    });
    for _ in 0..16 {
        tokio::task::yield_now().await;
    }
    let stamps_while_held = broker.stamps();
    release.send(()).expect("the reset is still held");
    let reset = tokio::time::timeout(HOLD_TIMEOUT, reset)
        .await
        .expect("the reset finishes in time")
        .expect("the reset completes");
    let b = tokio::time::timeout(HOLD_TIMEOUT, b)
        .await
        .expect("B finishes in time")
        .expect("B completes")
        .expect("B answers");

    assert_eq!(
        stamps_while_held,
        vec![Some((1, Some(PRODUCER.0), Some(4), Some(5)))],
        "B does not reach the broker while the reset is held"
    );
    assert!(reset.is_ok(), "the reset succeeds, got {reset:?}");
    assert_eq!((a, b), (IngestOutcome::Accepted, IngestOutcome::Accepted));
    assert_eq!(
        broker.stamps(),
        vec![
            Some((1, Some(PRODUCER.0), Some(4), Some(5))),
            Some((1, Some(PRODUCER.0), Some(-1), Some(0))),
        ],
        "B takes the chain's first position after the reset"
    );
}

/// A direct producer always deduplicates, so a stateless mode is refused at
/// build, before it registers or reads anything.
#[tokio::test]
async fn a_stateless_mode_is_refused_at_build() {
    let broker = RecordingBroker::new(4, []);

    let built = DirectProducer::builder()
        .broker(Arc::<RecordingBroker>::clone(&broker))
        .security_context(SecurityContext::anonymous())
        .source("order-service")
        .mode(ProducerMode::Stateless)
        .topics([TOPIC])
        .event_type_patterns([EVENT_TYPE])
        .build()
        .await;

    let Err(EventBrokerError::InvalidProducerOptions { detail }) = built else {
        panic!("a stateless direct producer must be refused, got {built:?}");
    };
    assert_eq!(
        detail,
        "a direct producer deduplicates: mode must be chained or monotonic"
    );
    assert_eq!(
        (broker.registrations(), broker.cursor_reads()),
        (Vec::new(), 0),
        "nothing reaches the broker"
    );
}

/// No publish path delivers an older sequence after a newer one - the topic's
/// publish lock makes every outcome carry the head's next sequence - so the
/// advance is driven directly.
#[tokio::test]
async fn a_stale_outcome_never_moves_the_local_head_back() {
    let broker = RecordingBroker::new(4, []);
    let producer = producer_over(Arc::clone(&broker), ProducerMode::Chained).await;

    producer
        .advance_after_acceptance(&prepared_at(7), IngestOutcome::Accepted)
        .await;
    producer
        .advance_after_acceptance(&prepared_at(5), IngestOutcome::Duplicate)
        .await;
    let next = producer
        .publish(order(1), Ack::Accepted)
        .await
        .expect("the next publishes");

    assert_eq!(next, IngestOutcome::Accepted);
    assert_eq!(
        broker.stamps(),
        vec![Some((1, Some(PRODUCER.0), Some(7), Some(8)))],
        "the next event chains onto 7, not onto the stale 5"
    );
}

/// An event of the orders topic stamped `sequence` by `PRODUCER`.
fn prepared_at(sequence: i64) -> PreparedEvent {
    let mut event = EventV1::builder_for(
        GtsTypeId::try_new(EVENT_TYPE).expect("a valid event type id"),
        json!({}),
    )
    .id(Uuid::from_u128(0x0190_f1a2_0000_7000_8000_0000_0000_e001))
    .tenant_id(TENANT)
    .source("order-service")
    .subject("order-stale")
    .subject_type_id(GtsTypeId::try_new(SUBJECT_TYPE).expect("a valid subject type id"))
    .occurred_at(chrono::DateTime::UNIX_EPOCH)
    .build()
    .expect("a valid event");
    if let Some(meta) = producer_meta_for_last(ProducerMode::Chained, PRODUCER, sequence - 1) {
        event.stamp_meta(meta);
    }
    PreparedEvent {
        event,
        topic: TOPIC.to_owned(),
        key: "00000000-0000-0000-0000-000000000000".to_owned(),
    }
}

/// The builder refuses a malformed trace parent, so the event is read with
/// serde, which takes any string.
#[tokio::test]
async fn a_malformed_trace_parent_is_refused_before_any_publish() {
    let broker = RecordingBroker::new(4, []);
    let producer = producer_over(Arc::clone(&broker), ProducerMode::Chained).await;

    let published = producer
        .publish(
            decoded_order_with("trace_parent", json!("00-abc-def-01")),
            Ack::Accepted,
        )
        .await;

    let Err(EventBrokerError::InvalidEventField { field, detail }) = published else {
        panic!("a malformed trace parent must be refused, got {published:?}");
    };
    assert_eq!(
        (field, detail.as_str()),
        (
            "trace_parent",
            "must be a W3C traceparent: four lowercase hex groups of 2, 32, 16 and 2 digits \
             joined by '-'"
        )
    );
    assert_eq!(broker.stamps(), vec![], "nothing is published");
}

/// A decoded event skips the builder; the producer applies the builder's
/// envelope rules itself, to a single publish and to every event of a batch,
/// before any broker call.
#[tokio::test]
async fn a_decoded_event_that_breaks_an_envelope_rule_is_refused_before_any_call() {
    let broker = RecordingBroker::new(4, []);
    let producer = producer_over(Arc::clone(&broker), ProducerMode::Chained).await;
    let well_formed = || EventV1::try_from(&order(1)).expect("the order converts");
    let ill_formed = || decoded_order_with("subject", json!("PLANTED-\u{e9}"));

    let single = producer.publish(ill_formed(), Ack::Accepted).await;
    let batch = producer
        .publish_batch(vec![well_formed(), ill_formed()], Ack::Accepted)
        .await;

    for (path, outcome) in [("single", single), ("batch", batch)] {
        let Err(EventBrokerError::InvalidTextField {
            field,
            detail,
            reason,
        }) = outcome
        else {
            panic!("{path}: a non-ASCII subject must be refused, got {outcome:?}");
        };
        assert_eq!(
            (field, detail.as_str(), reason),
            (
                "subject",
                "must contain only printable ASCII (0x20-0x7E)",
                "ascii_only"
            ),
            "{path}"
        );
    }
    assert_eq!(
        (broker.stamps(), broker.batch_calls(), broker.cursor_reads()),
        (vec![], 0, 1),
        "nothing is sent and nothing read beyond the read at build"
    );
}

/// The producer stamps its own `source` on every event it sends, so an event
/// that already carries one is refused before any call.
#[tokio::test]
async fn a_producer_stamps_its_source_and_refuses_an_event_that_carries_one() {
    let broker = RecordingBroker::new(4, []);
    let producer = producer_over(Arc::clone(&broker), ProducerMode::Chained).await;
    let sourced = EventV1::builder(OrderCreated {
        order_id: Uuid::from_u128(2),
    })
    .tenant_id(TENANT)
    .source("x")
    .subject("order-2")
    .subject_type_id(GtsTypeId::new(SUBJECT_TYPE))
    .build()
    .expect("a valid order event");

    let unsourced = producer
        .publish(order(1), Ack::Accepted)
        .await
        .expect("an event without a source publishes");
    let refused = producer.publish(sourced, Ack::Accepted).await;

    assert_eq!(unsourced, IngestOutcome::Accepted);
    let Err(EventBrokerError::InvalidEventField { field, detail }) = refused else {
        panic!("an event carrying a source must be refused, got {refused:?}");
    };
    assert_eq!(
        (field, detail.as_str()),
        ("source", "the producer stamps it")
    );
    assert_eq!(
        (broker.sources(), broker.stamps()),
        (
            vec![Some("order-service".to_owned())],
            vec![Some((1, Some(PRODUCER.0), Some(4), Some(5)))]
        ),
        "only the event without a source is sent, carrying the producer's source"
    );
}

/// Each member the broker or the producer stamps is refused on input, on a
/// single publish and in a batch, before any call.
#[tokio::test]
async fn a_producer_refuses_stamped_members_and_meta_before_any_call() {
    let broker = RecordingBroker::new(4, []);
    let producer = producer_over(Arc::clone(&broker), ProducerMode::Chained).await;
    let cases = [
        ("partition", json!(3), READ_ONLY_DETAIL),
        ("sequence", json!(7), READ_ONLY_DETAIL),
        (
            "sequence_time",
            json!("2026-11-06T10:00:01Z"),
            READ_ONLY_DETAIL,
        ),
        (
            "meta",
            json!({ "version": 1, "producer_id": null, "previous": null, "sequence": null }),
            "the producer stamps it",
        ),
    ];

    for (member, value, expected) in cases {
        let single = producer
            .publish(decoded_order_with(member, value.clone()), Ack::Accepted)
            .await;
        let batch = producer
            .publish_batch(
                vec![
                    EventV1::try_from(&order(1)).expect("the order converts"),
                    decoded_order_with(member, value),
                ],
                Ack::Accepted,
            )
            .await;

        for (path, outcome) in [("single", single), ("batch", batch)] {
            let Err(EventBrokerError::InvalidEventField { field, detail }) = outcome else {
                panic!("{path}: an event carrying {member} must be refused, got {outcome:?}");
            };
            assert_eq!((field, detail.as_str()), (member, expected), "{path}");
        }
    }
    assert_eq!(
        (broker.stamps(), broker.batch_calls(), broker.cursor_reads()),
        (vec![], 0, 1),
        "nothing is sent and nothing read beyond the read at build"
    );
}

/// The caller's `Ack` reaches the transport unchanged, on a single publish and
/// on a batch. Both ask for a persisted answer, so neither can pass as the
/// default `Ack::Accepted`.
#[tokio::test]
async fn the_ack_reaches_the_transport() {
    let broker = RecordingBroker::new(4, []);
    let producer = producer_over(Arc::clone(&broker), ProducerMode::Chained).await;
    let single = Ack::Persisted {
        within: Duration::from_secs(5),
    };
    let batch = Ack::Persisted {
        within: Duration::from_secs(7),
    };

    producer
        .publish(order(1), single)
        .await
        .expect("the persisted publish answers");
    producer
        .publish_batch(vec![order(2), order(3)], batch)
        .await
        .expect("the persisted batch answers");

    assert_eq!(broker.acks(), vec![single, batch]);
}

/// The orders head is uncertain after a failed publish; the invoices head is
/// certain. A batch over both re-reads the producer's cursors once and takes
/// only the orders head from them: the invoice continues its local chain. The
/// stub lists no invoices head, so taking the invoices head from the re-read
/// would restart that chain.
#[tokio::test]
async fn a_two_topic_batch_rereads_only_the_uncertain_topics_head() {
    let broker = RecordingBroker::new(4, []);
    let producer = DirectProducer::builder()
        .broker(Arc::<RecordingBroker>::clone(&broker))
        .security_context(SecurityContext::anonymous())
        .source("order-service")
        .topics([TOPIC, INVOICE_TOPIC])
        .event_type_patterns([EVENT_TYPE, INVOICE_TYPE])
        .build()
        .await
        .expect("the producer builds");

    let first_invoice = producer
        .publish(invoice(1), Ack::Accepted)
        .await
        .expect("the first invoice publishes");
    broker.fail_next_publish();
    let failed = producer.publish(order(2), Ack::Accepted).await;
    let batch = producer
        .publish_batch(
            vec![
                EventV1::try_from(&order(3)).expect("the order converts"),
                EventV1::try_from(&invoice(4)).expect("the invoice converts"),
            ],
            Ack::Accepted,
        )
        .await
        .expect("the batch publishes");

    assert!(
        matches!(failed, Err(EventBrokerError::Transport(ref detail)) if detail == "connection reset"),
        "the order surfaces the transport fault, got {failed:?}"
    );
    assert_eq!(
        (first_invoice, batch),
        (IngestOutcome::Accepted, IngestOutcome::Accepted)
    );
    assert_eq!(
        broker.stamps(),
        vec![
            Some((1, Some(PRODUCER.0), Some(-1), Some(0))),
            Some((1, Some(PRODUCER.0), Some(4), Some(5))),
            Some((1, Some(PRODUCER.0), Some(5), Some(6))),
            Some((1, Some(PRODUCER.0), Some(0), Some(1))),
        ],
        "the batch's order chains onto the stored failed order; its invoice onto the first"
    );
    assert_eq!(
        broker.cursor_reads(),
        2,
        "the read at build and one re-read before the batch"
    );
}

/// `build()` registers a fresh producer id with the builder's mode (chained
/// when none is set) and client agent (the SDK's default when none is set).
#[tokio::test]
async fn a_producer_registers_its_mode_at_build() {
    let broker = RecordingBroker::new(4, []);

    let chained = DirectProducer::builder()
        .broker(Arc::<RecordingBroker>::clone(&broker))
        .security_context(SecurityContext::anonymous())
        .source("order-service")
        .topics([TOPIC])
        .event_type_patterns([EVENT_TYPE])
        .build()
        .await
        .expect("the chained producer builds");
    let monotonic = DirectProducer::builder()
        .broker(Arc::<RecordingBroker>::clone(&broker))
        .security_context(SecurityContext::anonymous())
        .source("order-service")
        .mode(ProducerMode::Monotonic)
        .client_agent("order-service/1.0")
        .topics([TOPIC])
        .event_type_patterns([EVENT_TYPE])
        .build()
        .await
        .expect("the monotonic producer builds");

    assert_eq!(
        broker.registrations(),
        vec![
            (
                ProducerMode::Chained,
                EventBrokerSdk::default_client_agent().to_owned()
            ),
            (ProducerMode::Monotonic, "order-service/1.0".to_owned()),
        ]
    );
    assert_eq!(
        (chained.producer_id(), monotonic.producer_id()),
        (PRODUCER, PRODUCER)
    );
}

/// `Debug` prints the producer's mode, id and topics, and of the builder which
/// fields are set, with the security context reduced to its subject id.
#[tokio::test]
async fn the_debug_output_shows_no_security_context_secret() {
    let broker = RecordingBroker::new(4, []);
    let producer = producer_over(Arc::clone(&broker), ProducerMode::Monotonic).await;
    let builder = DirectProducer::builder()
        .security_context(SecurityContext::anonymous())
        .source("order-service")
        .topics([TOPIC]);

    assert_eq!(
        format!("{producer:?}"),
        format!(
            "DirectProducer {{ mode: Monotonic, producer_id: ProducerId({}), topics: [{TOPIC:?}], .. }}",
            PRODUCER.0
        )
    );
    assert_eq!(
        format!("{builder:?}"),
        format!(
            "DirectProducerBuilder {{ broker: unset, security_context: Some({}), \
             source: Some(\"order-service\"), topics: [{TOPIC:?}], event_type_patterns: [], \
             mode: Chained, client_agent: None, lazy_schema_loading: false, \
             max_waiting_per_topic: 64 }}",
            Uuid::nil()
        )
    );
}

/// A blank `source`, or one that breaks the envelope's source rule, is refused
/// at build, before the producer registers or reads anything.
#[tokio::test]
async fn a_blank_or_invalid_source_is_refused_at_build() {
    let broker = RecordingBroker::new(4, []);

    for (source, expected) in [
        ("   ", "source is required"),
        (
            "order-\u{e9}",
            "source must contain only printable ASCII (0x20-0x7E)",
        ),
    ] {
        let built = DirectProducer::builder()
            .broker(Arc::<RecordingBroker>::clone(&broker))
            .security_context(SecurityContext::anonymous())
            .source(source)
            .topics([TOPIC])
            .event_type_patterns([EVENT_TYPE])
            .build()
            .await;

        let Err(EventBrokerError::InvalidProducerOptions { detail }) = built else {
            panic!("source {source:?} must be refused, got {built:?}");
        };
        assert_eq!(detail, expected, "{source:?}");
    }
    assert_eq!(
        (broker.registrations(), broker.cursor_reads()),
        (Vec::new(), 0),
        "nothing reaches the broker"
    );
}

/// A single publish on the orders topic is held at the broker. Batch B, over
/// orders then invoices, starts first; batch A, over invoices then orders,
/// starts after it. Both take their topic locks in ascending topic order
/// (invoices first), so B holds invoices while it waits for orders, and A
/// waits for invoices behind it. Taking them in input order instead, B would
/// queue for orders first and A would take invoices, and once the held publish
/// answers each would hold the lock the other waits for. Each batch can park
/// on a contended await before it reaches its locks; the 16 yields bound how
/// many times it may park and still queue in this order.
#[tokio::test]
async fn two_batches_over_two_topics_in_opposite_orders_both_publish() {
    let broker = RecordingBroker::new(4, []);
    let producer = Arc::new(
        DirectProducer::builder()
            .broker(Arc::<RecordingBroker>::clone(&broker))
            .security_context(SecurityContext::anonymous())
            .source("order-service")
            .topics([TOPIC, INVOICE_TOPIC])
            .event_type_patterns([EVENT_TYPE, INVOICE_TYPE])
            .build()
            .await
            .expect("the producer builds"),
    );
    let release = broker.hold_next_publish();
    let held = tokio::spawn({
        let producer = Arc::clone(&producer);
        async move { producer.publish(order(1), Ack::Accepted).await }
    });
    tokio::time::timeout(HOLD_TIMEOUT, broker.held_reached.notified())
        .await
        .expect("the held publish reaches the broker");

    let b = tokio::spawn({
        let producer = Arc::clone(&producer);
        async move {
            let events = vec![
                EventV1::try_from(&order(2)).expect("the order converts"),
                EventV1::try_from(&invoice(3)).expect("the invoice converts"),
            ];
            producer.publish_batch(events, Ack::Accepted).await
        }
    });
    for _ in 0..16 {
        tokio::task::yield_now().await;
    }
    let a = tokio::spawn({
        let producer = Arc::clone(&producer);
        async move {
            let events = vec![
                EventV1::try_from(&invoice(4)).expect("the invoice converts"),
                EventV1::try_from(&order(5)).expect("the order converts"),
            ];
            producer.publish_batch(events, Ack::Accepted).await
        }
    });
    for _ in 0..16 {
        tokio::task::yield_now().await;
    }
    release.send(()).expect("the publish is still held");
    let held = tokio::time::timeout(HOLD_TIMEOUT, held)
        .await
        .expect("the held publish finishes in time")
        .expect("the held publish completes")
        .expect("the held publish answers");
    let b = tokio::time::timeout(HOLD_TIMEOUT, b)
        .await
        .expect("batch B finishes in time")
        .expect("batch B completes")
        .expect("batch B publishes");
    let a = tokio::time::timeout(HOLD_TIMEOUT, a)
        .await
        .expect("batch A finishes in time")
        .expect("batch A completes")
        .expect("batch A publishes");

    assert_eq!(
        (held, b, a),
        (
            IngestOutcome::Accepted,
            IngestOutcome::Accepted,
            IngestOutcome::Accepted
        )
    );
    assert_eq!(
        broker.stamps(),
        vec![
            Some((1, Some(PRODUCER.0), Some(4), Some(5))),
            Some((1, Some(PRODUCER.0), Some(5), Some(6))),
            Some((1, Some(PRODUCER.0), Some(-1), Some(0))),
            Some((1, Some(PRODUCER.0), Some(0), Some(1))),
            Some((1, Some(PRODUCER.0), Some(6), Some(7))),
        ],
        "B follows the held publish; A follows B on both topics"
    );
}

/// The broker stores X but fails its answer. Re-publishing X resends the
/// numbers X carried without re-reading the head, so the broker answers
/// `Duplicate` instead of storing X twice; Y then continues after X.
#[tokio::test]
async fn a_republished_event_after_a_lost_ack_reuses_its_numbers() {
    let broker = RecordingBroker::new(4, [IngestOutcome::Accepted, IngestOutcome::Duplicate]);
    let producer = producer_over(Arc::clone(&broker), ProducerMode::Chained).await;
    broker.fail_next_publish();

    let lost = producer.publish(order_with_id(1), Ack::Accepted).await;
    let again = producer.publish(order_with_id(1), Ack::Accepted).await;
    let next = producer.publish(order_with_id(2), Ack::Accepted).await;

    assert!(
        matches!(lost, Err(EventBrokerError::Transport(ref detail)) if detail == "connection reset"),
        "X surfaces the transport fault, got {lost:?}"
    );
    assert_eq!(
        (again.expect("X answers"), next.expect("Y answers")),
        (IngestOutcome::Duplicate, IngestOutcome::Accepted)
    );
    assert_eq!(
        broker.sent(),
        vec![
            (event_id(1), Some((1, Some(PRODUCER.0), Some(4), Some(5)))),
            (event_id(1), Some((1, Some(PRODUCER.0), Some(4), Some(5)))),
            (event_id(2), Some((1, Some(PRODUCER.0), Some(5), Some(6)))),
        ],
        "X is resent with its numbers; Y continues after X"
    );
    assert_eq!(broker.cursor_reads(), 1, "the read at build only");
}

/// As above with X's future dropped while the broker call is in flight.
#[tokio::test]
async fn a_republished_event_after_a_dropped_publish_reuses_its_numbers() {
    let broker = RecordingBroker::new(4, [IngestOutcome::Accepted, IngestOutcome::Duplicate]);
    let producer = Arc::new(producer_over(Arc::clone(&broker), ProducerMode::Chained).await);
    let release = broker.hold_next_publish();
    let dropped = tokio::spawn({
        let producer = Arc::clone(&producer);
        async move { producer.publish(order_with_id(1), Ack::Accepted).await }
    });
    tokio::time::timeout(HOLD_TIMEOUT, broker.held_reached.notified())
        .await
        .expect("X reaches the broker");

    dropped.abort();
    let dropped = tokio::time::timeout(HOLD_TIMEOUT, dropped)
        .await
        .expect("X ends in time");
    drop(release);
    let again = tokio::time::timeout(
        HOLD_TIMEOUT,
        producer.publish(order_with_id(1), Ack::Accepted),
    )
    .await
    .expect("X is not blocked by the dropped publish");
    let next = producer.publish(order_with_id(2), Ack::Accepted).await;

    assert!(
        matches!(dropped, Err(ref error) if error.is_cancelled()),
        "X is cancelled, got {dropped:?}"
    );
    assert_eq!(
        (again.expect("X answers"), next.expect("Y answers")),
        (IngestOutcome::Duplicate, IngestOutcome::Accepted)
    );
    assert_eq!(
        broker.sent(),
        vec![
            (event_id(1), Some((1, Some(PRODUCER.0), Some(4), Some(5)))),
            (event_id(1), Some((1, Some(PRODUCER.0), Some(4), Some(5)))),
            (event_id(2), Some((1, Some(PRODUCER.0), Some(5), Some(6)))),
        ],
        "X is resent with its numbers; Y continues after X"
    );
    assert_eq!(broker.cursor_reads(), 1, "the read at build only");
}

/// The broker stores X but fails its answer. A different event Y is not a
/// re-publish of X: it re-reads the head and chains onto the stored X.
#[tokio::test]
async fn a_different_event_after_a_lost_ack_rereads_the_head() {
    let broker = RecordingBroker::new(4, []);
    let producer = producer_over(Arc::clone(&broker), ProducerMode::Chained).await;
    broker.fail_next_publish();

    let lost = producer.publish(order_with_id(1), Ack::Accepted).await;
    let other = producer.publish(order_with_id(2), Ack::Accepted).await;

    assert!(
        matches!(lost, Err(EventBrokerError::Transport(ref detail)) if detail == "connection reset"),
        "X surfaces the transport fault, got {lost:?}"
    );
    assert_eq!(other.expect("Y answers"), IngestOutcome::Accepted);
    assert_eq!(
        broker.sent(),
        vec![
            (event_id(1), Some((1, Some(PRODUCER.0), Some(4), Some(5)))),
            (event_id(2), Some((1, Some(PRODUCER.0), Some(5), Some(6)))),
        ],
        "Y chains onto the stored X"
    );
    assert_eq!(
        broker.cursor_reads(),
        2,
        "the read at build and one re-read before Y"
    );
}

/// X's fresh numbers are answered `Duplicate`: an earlier request the producer
/// did not see took them, so X is not stored. The producer resends X once
/// with the next numbers, and Y continues after the resent X.
#[tokio::test]
async fn a_fresh_duplicate_is_resent_once_with_the_next_numbers() {
    let broker = RecordingBroker::new(4, [IngestOutcome::Duplicate, IngestOutcome::Accepted]);
    let producer = producer_over(Arc::clone(&broker), ProducerMode::Chained).await;

    let first = producer.publish(order_with_id(1), Ack::Accepted).await;
    let next = producer.publish(order_with_id(2), Ack::Accepted).await;

    assert_eq!(
        (first.expect("X answers"), next.expect("Y answers")),
        (IngestOutcome::Accepted, IngestOutcome::Accepted)
    );
    assert_eq!(
        broker.sent(),
        vec![
            (event_id(1), Some((1, Some(PRODUCER.0), Some(4), Some(5)))),
            (event_id(1), Some((1, Some(PRODUCER.0), Some(5), Some(6)))),
            (event_id(2), Some((1, Some(PRODUCER.0), Some(6), Some(7)))),
        ],
        "X is resent once with the next numbers; Y continues after it"
    );
    assert_eq!(broker.cursor_reads(), 1, "the read at build only");
}

/// X's fresh numbers and its resend's next numbers are both answered
/// `Duplicate`: the publish fails with `SequenceTaken` after one resend.
/// Re-publishing X then re-reads the head and is stamped fresh from it.
#[tokio::test]
async fn a_second_fresh_duplicate_is_sequence_taken() {
    let broker = RecordingBroker::new(4, [IngestOutcome::Duplicate, IngestOutcome::Duplicate]);
    let producer = producer_over(Arc::clone(&broker), ProducerMode::Chained).await;

    let taken = producer.publish(order_with_id(1), Ack::Accepted).await;
    let again = producer.publish(order_with_id(1), Ack::Accepted).await;

    let Err(EventBrokerError::SequenceTaken { topic }) = taken else {
        panic!("a second fresh duplicate must be SequenceTaken, got {taken:?}");
    };
    assert_eq!(topic, TOPIC);
    assert_eq!(again.expect("X answers"), IngestOutcome::Accepted);
    assert_eq!(
        broker.sent(),
        vec![
            (event_id(1), Some((1, Some(PRODUCER.0), Some(4), Some(5)))),
            (event_id(1), Some((1, Some(PRODUCER.0), Some(5), Some(6)))),
            (event_id(1), Some((1, Some(PRODUCER.0), Some(6), Some(7)))),
        ],
        "one resend, then the re-published X is stamped from the re-read head"
    );
    assert_eq!(
        broker.cursor_reads(),
        2,
        "the read at build and one re-read before the re-published X"
    );
}

/// Batch A fails after the broker stored it. The same batch again resends its
/// numbers without a re-read. Batch C fails after it was stored too, and
/// batch D shares only its first event with C: it is stamped fresh after a
/// re-read.
#[tokio::test]
async fn a_republished_batch_after_a_lost_ack_reuses_its_numbers() {
    let broker = RecordingBroker::new(4, [IngestOutcome::Accepted, IngestOutcome::Duplicate]);
    let producer = producer_over(Arc::clone(&broker), ProducerMode::Chained).await;
    broker.fail_next_batch();

    let a = producer
        .publish_batch(vec![order_with_id(1), order_with_id(2)], Ack::Accepted)
        .await;
    let b = producer
        .publish_batch(vec![order_with_id(1), order_with_id(2)], Ack::Accepted)
        .await;
    broker.fail_next_batch();
    let c = producer
        .publish_batch(vec![order_with_id(3), order_with_id(4)], Ack::Accepted)
        .await;
    let d = producer
        .publish_batch(vec![order_with_id(3), order_with_id(5)], Ack::Accepted)
        .await;

    for (name, failed) in [("A", a), ("C", c)] {
        assert!(
            matches!(failed, Err(EventBrokerError::Transport(ref detail)) if detail == "connection reset"),
            "batch {name} surfaces the transport fault, got {failed:?}"
        );
    }
    assert_eq!(
        (b.expect("batch B answers"), d.expect("batch D answers")),
        (IngestOutcome::Duplicate, IngestOutcome::Accepted)
    );
    assert_eq!(
        broker.sent(),
        vec![
            (event_id(1), Some((1, Some(PRODUCER.0), Some(4), Some(5)))),
            (event_id(2), Some((1, Some(PRODUCER.0), Some(5), Some(6)))),
            (event_id(1), Some((1, Some(PRODUCER.0), Some(4), Some(5)))),
            (event_id(2), Some((1, Some(PRODUCER.0), Some(5), Some(6)))),
            (event_id(3), Some((1, Some(PRODUCER.0), Some(6), Some(7)))),
            (event_id(4), Some((1, Some(PRODUCER.0), Some(7), Some(8)))),
            (event_id(3), Some((1, Some(PRODUCER.0), Some(8), Some(9)))),
            (event_id(5), Some((1, Some(PRODUCER.0), Some(9), Some(10)))),
        ],
        "B resends A's numbers; D is stamped from the re-read head"
    );
    assert_eq!(
        (broker.batch_calls(), broker.cursor_reads()),
        (4, 2),
        "four batches; the read at build and one re-read before D"
    );
}

/// Batch A fails after the broker stored it. Publishing A's first event alone
/// is not a re-publish of A: it re-reads the head and is stamped fresh.
#[tokio::test]
async fn one_event_of_an_unseen_batch_is_stamped_fresh() {
    let broker = RecordingBroker::new(4, []);
    let producer = producer_over(Arc::clone(&broker), ProducerMode::Chained).await;
    broker.fail_next_batch();

    let a = producer
        .publish_batch(vec![order_with_id(1), order_with_id(2)], Ack::Accepted)
        .await;
    let alone = producer.publish(order_with_id(1), Ack::Accepted).await;

    assert!(
        matches!(a, Err(EventBrokerError::Transport(ref detail)) if detail == "connection reset"),
        "batch A surfaces the transport fault, got {a:?}"
    );
    assert_eq!(alone.expect("X answers"), IngestOutcome::Accepted);
    assert_eq!(
        broker.sent(),
        vec![
            (event_id(1), Some((1, Some(PRODUCER.0), Some(4), Some(5)))),
            (event_id(2), Some((1, Some(PRODUCER.0), Some(5), Some(6)))),
            (event_id(1), Some((1, Some(PRODUCER.0), Some(6), Some(7)))),
        ],
        "X alone chains onto the stored batch A"
    );
    assert_eq!(
        broker.cursor_reads(),
        2,
        "the read at build and one re-read before X"
    );
}

/// As `a_fresh_duplicate_is_resent_once_with_the_next_numbers` for a batch:
/// the batch is resent once from its highest sequence.
#[tokio::test]
async fn a_fresh_duplicate_batch_is_resent_once() {
    let broker = RecordingBroker::new(4, [IngestOutcome::Duplicate, IngestOutcome::Accepted]);
    let producer = producer_over(Arc::clone(&broker), ProducerMode::Chained).await;

    let batch = producer
        .publish_batch(vec![order_with_id(1), order_with_id(2)], Ack::Accepted)
        .await;
    let next = producer.publish(order_with_id(3), Ack::Accepted).await;

    assert_eq!(
        (batch.expect("the batch answers"), next.expect("Z answers")),
        (IngestOutcome::Accepted, IngestOutcome::Accepted)
    );
    assert_eq!(
        broker.sent(),
        vec![
            (event_id(1), Some((1, Some(PRODUCER.0), Some(4), Some(5)))),
            (event_id(2), Some((1, Some(PRODUCER.0), Some(5), Some(6)))),
            (event_id(1), Some((1, Some(PRODUCER.0), Some(6), Some(7)))),
            (event_id(2), Some((1, Some(PRODUCER.0), Some(7), Some(8)))),
            (event_id(3), Some((1, Some(PRODUCER.0), Some(8), Some(9)))),
        ],
        "the batch is resent once with the next numbers; Z continues after it"
    );
    assert_eq!(
        (broker.batch_calls(), broker.cursor_reads()),
        (2, 1),
        "two batch calls; the read at build only"
    );
}

/// A monotonic producer reuses X's sequence after a lost ack, and resends Y
/// once after a fresh `Duplicate`, as a chained one does.
#[tokio::test]
async fn a_monotonic_producer_reuses_and_resends_alike() {
    let broker = RecordingBroker::new(
        4,
        [
            IngestOutcome::Accepted,
            IngestOutcome::Duplicate,
            IngestOutcome::Duplicate,
            IngestOutcome::Accepted,
        ],
    );
    let producer = producer_over(Arc::clone(&broker), ProducerMode::Monotonic).await;
    broker.fail_next_publish();

    let lost = producer.publish(order_with_id(1), Ack::Accepted).await;
    let again = producer.publish(order_with_id(1), Ack::Accepted).await;
    let resent = producer.publish(order_with_id(2), Ack::Accepted).await;

    assert!(
        matches!(lost, Err(EventBrokerError::Transport(ref detail)) if detail == "connection reset"),
        "X surfaces the transport fault, got {lost:?}"
    );
    assert_eq!(
        (again.expect("X answers"), resent.expect("Y answers")),
        (IngestOutcome::Duplicate, IngestOutcome::Accepted)
    );
    assert_eq!(
        broker.sent(),
        vec![
            (event_id(1), Some((1, Some(PRODUCER.0), None, Some(5)))),
            (event_id(1), Some((1, Some(PRODUCER.0), None, Some(5)))),
            (event_id(2), Some((1, Some(PRODUCER.0), None, Some(6)))),
            (event_id(2), Some((1, Some(PRODUCER.0), None, Some(7)))),
        ],
        "X is resent with its sequence; Y is resent once with the next one"
    );
    assert_eq!(broker.cursor_reads(), 1, "the read at build only");
}

/// The broker stores X but fails its answer, then the producer's chain is
/// reset. X's numbers belong to the chain before the reset, so re-publishing
/// X re-reads the head and takes the chain's first position.
#[tokio::test]
async fn a_republished_event_after_a_chain_reset_is_stamped_fresh() {
    let broker = RecordingBroker::new(4, []);
    let producer = producer_over(Arc::clone(&broker), ProducerMode::Chained).await;
    broker.fail_next_publish();

    let lost = producer.publish(order_with_id(1), Ack::Accepted).await;
    producer
        .reset_chain(ResetScope::AllTopics)
        .await
        .expect("the reset succeeds");
    let again = producer.publish(order_with_id(1), Ack::Accepted).await;

    assert!(
        matches!(lost, Err(EventBrokerError::Transport(ref detail)) if detail == "connection reset"),
        "X surfaces the transport fault, got {lost:?}"
    );
    assert_eq!(again.expect("X answers"), IngestOutcome::Accepted);
    assert_eq!(
        broker.sent(),
        vec![
            (event_id(1), Some((1, Some(PRODUCER.0), Some(4), Some(5)))),
            (event_id(1), Some((1, Some(PRODUCER.0), Some(-1), Some(0)))),
        ],
        "the re-published X takes the chain's first position after the reset"
    );
    assert_eq!(
        (broker.cursor_reads(), broker.resets()),
        (2, 1),
        "the read at build and one re-read before X; one reset"
    );
}

/// A lazy producer prepares nothing at build. Its first publish of a type
/// looks the type up once; the second publish of that type looks up nothing.
#[tokio::test]
async fn a_lazy_producer_prepares_a_type_on_its_first_publish_only() {
    let broker = RecordingBroker::new(4, []);
    let producer = DirectProducer::builder()
        .broker(Arc::<RecordingBroker>::clone(&broker))
        .security_context(SecurityContext::anonymous())
        .source("order-service")
        .topics([TOPIC])
        .event_type_patterns([EVENT_TYPE])
        .lazy_schema_loading()
        .build()
        .await
        .expect("the lazy producer builds");
    let at_build = broker.event_type_lookups();

    let first = producer.publish(order(1), Ack::Accepted).await;
    let after_first = broker.event_type_lookups();
    let second = producer.publish(order(2), Ack::Accepted).await;

    assert_eq!(
        (
            first.expect("the first answers"),
            second.expect("the second answers")
        ),
        (IngestOutcome::Accepted, IngestOutcome::Accepted)
    );
    assert_eq!(
        (at_build, after_first, broker.event_type_lookups()),
        (0, 1, 1),
        "no lookup at build, one on the first publish, none on the second"
    );
}

/// The broker's head moved to 6 behind the producer's back, so X's numbers are
/// stale: the broker answers `SequenceViolation` and does not store X.
/// Re-publishing X does not resend those numbers: it re-reads the head once and
/// is stamped from it.
#[tokio::test]
async fn a_republish_after_a_sequence_violation_rereads_the_head() {
    let broker = RecordingBroker::new(4, []);
    let producer = producer_over(Arc::clone(&broker), ProducerMode::Chained).await;
    broker.violate_next_publish(6);

    let violated = producer.publish(order_with_id(1), Ack::Accepted).await;
    let reads_before = broker.cursor_reads();
    let again = producer.publish(order_with_id(1), Ack::Accepted).await;

    let Err(EventBrokerError::SequenceViolation {
        expected_previous,
        detail,
    }) = violated
    else {
        panic!("X must be a SequenceViolation, got {violated:?}");
    };
    assert_eq!((expected_previous, detail.as_str()), (6, "stale previous"));
    assert_eq!(again.expect("X answers"), IngestOutcome::Accepted);
    assert_eq!(
        broker.sent(),
        vec![
            (event_id(1), Some((1, Some(PRODUCER.0), Some(4), Some(5)))),
            (event_id(1), Some((1, Some(PRODUCER.0), Some(6), Some(7)))),
        ],
        "the re-published X is stamped from the re-read head"
    );
    assert_eq!(
        (reads_before, broker.cursor_reads()),
        (1, 2),
        "the read at build, then one re-read before the re-published X"
    );
}

/// As `a_republish_after_a_sequence_violation_rereads_the_head` for a batch.
#[tokio::test]
async fn a_republished_batch_after_a_sequence_violation_rereads_the_head() {
    let broker = RecordingBroker::new(4, []);
    let producer = producer_over(Arc::clone(&broker), ProducerMode::Chained).await;
    broker.violate_next_publish(6);

    let violated = producer
        .publish_batch(vec![order_with_id(1), order_with_id(2)], Ack::Accepted)
        .await;
    let reads_before = broker.cursor_reads();
    let again = producer
        .publish_batch(vec![order_with_id(1), order_with_id(2)], Ack::Accepted)
        .await;

    let Err(EventBrokerError::SequenceViolation {
        expected_previous,
        detail,
    }) = violated
    else {
        panic!("the batch must be a SequenceViolation, got {violated:?}");
    };
    assert_eq!((expected_previous, detail.as_str()), (6, "stale previous"));
    assert_eq!(again.expect("the batch answers"), IngestOutcome::Accepted);
    assert_eq!(
        broker.sent(),
        vec![
            (event_id(1), Some((1, Some(PRODUCER.0), Some(4), Some(5)))),
            (event_id(2), Some((1, Some(PRODUCER.0), Some(5), Some(6)))),
            (event_id(1), Some((1, Some(PRODUCER.0), Some(6), Some(7)))),
            (event_id(2), Some((1, Some(PRODUCER.0), Some(7), Some(8)))),
        ],
        "the re-published batch is stamped from the re-read head"
    );
    assert_eq!(
        (broker.batch_calls(), reads_before, broker.cursor_reads()),
        (2, 1, 2),
        "two batch calls; the read at build, then one re-read before the re-published batch"
    );
}

/// X's fresh numbers are answered `Duplicate`. While that first send is held,
/// the broker's head moves to 7, so the resend's next numbers are stale and
/// answered `SequenceViolation`. Re-publishing X re-reads the head once and is
/// stamped from it, instead of resending the resend's numbers.
#[tokio::test]
async fn a_republish_after_a_resend_sequence_violation_rereads_the_head() {
    let broker = RecordingBroker::new(4, [IngestOutcome::Duplicate]);
    let producer = Arc::new(producer_over(Arc::clone(&broker), ProducerMode::Chained).await);
    let release = broker.hold_next_publish();
    let first = tokio::spawn({
        let producer = Arc::clone(&producer);
        async move { producer.publish(order_with_id(1), Ack::Accepted).await }
    });
    tokio::time::timeout(HOLD_TIMEOUT, broker.held_reached.notified())
        .await
        .expect("X's first send reaches the broker");

    broker.violate_next_publish(7);
    release.send(()).expect("X's first send is still held");
    let violated = tokio::time::timeout(HOLD_TIMEOUT, first)
        .await
        .expect("X finishes in time")
        .expect("X completes");
    let reads_before = broker.cursor_reads();
    let again = producer.publish(order_with_id(1), Ack::Accepted).await;

    let Err(EventBrokerError::SequenceViolation {
        expected_previous,
        detail,
    }) = violated
    else {
        panic!("the resend must be a SequenceViolation, got {violated:?}");
    };
    assert_eq!((expected_previous, detail.as_str()), (7, "stale previous"));
    assert_eq!(again.expect("X answers"), IngestOutcome::Accepted);
    assert_eq!(
        broker.sent(),
        vec![
            (event_id(1), Some((1, Some(PRODUCER.0), Some(4), Some(5)))),
            (event_id(1), Some((1, Some(PRODUCER.0), Some(5), Some(6)))),
            (event_id(1), Some((1, Some(PRODUCER.0), Some(7), Some(8)))),
        ],
        "one resend, then the re-published X is stamped from the re-read head"
    );
    assert_eq!(
        (reads_before, broker.cursor_reads()),
        (1, 2),
        "the read at build, then one re-read before the re-published X"
    );
}

/// X's fresh numbers are answered `Duplicate`. The broker stores the resend
/// with the next numbers, but its answer fails. Re-publishing X resends the
/// resend's numbers, not the first send's, with no re-read, and the broker
/// answers `Duplicate`.
#[tokio::test]
async fn a_resend_that_fails_leaves_its_numbers_for_a_republish() {
    let broker = RecordingBroker::new(
        4,
        [
            IngestOutcome::Duplicate,
            IngestOutcome::Accepted,
            IngestOutcome::Duplicate,
        ],
    );
    let producer = Arc::new(producer_over(Arc::clone(&broker), ProducerMode::Chained).await);
    let release = broker.hold_next_publish();
    let first = tokio::spawn({
        let producer = Arc::clone(&producer);
        async move { producer.publish(order_with_id(1), Ack::Accepted).await }
    });
    tokio::time::timeout(HOLD_TIMEOUT, broker.held_reached.notified())
        .await
        .expect("X's first send reaches the broker");

    broker.fail_next_publish();
    release.send(()).expect("X's first send is still held");
    let lost = tokio::time::timeout(HOLD_TIMEOUT, first)
        .await
        .expect("X finishes in time")
        .expect("X completes");
    let again = producer.publish(order_with_id(1), Ack::Accepted).await;

    assert!(
        matches!(lost, Err(EventBrokerError::Transport(ref detail)) if detail == "connection reset"),
        "the resend surfaces the transport fault, got {lost:?}"
    );
    assert_eq!(again.expect("X answers"), IngestOutcome::Duplicate);
    assert_eq!(
        broker.sent(),
        vec![
            (event_id(1), Some((1, Some(PRODUCER.0), Some(4), Some(5)))),
            (event_id(1), Some((1, Some(PRODUCER.0), Some(5), Some(6)))),
            (event_id(1), Some((1, Some(PRODUCER.0), Some(5), Some(6)))),
        ],
        "the re-published X carries the resend's numbers"
    );
    assert_eq!(broker.cursor_reads(), 1, "the read at build only");
}

/// As `a_resend_that_fails_leaves_its_numbers_for_a_republish` with X's future
/// dropped while the resend is in flight.
#[tokio::test]
async fn a_resend_whose_future_is_dropped_leaves_its_numbers_for_a_republish() {
    let broker = RecordingBroker::new(
        4,
        [
            IngestOutcome::Duplicate,
            IngestOutcome::Accepted,
            IngestOutcome::Duplicate,
        ],
    );
    let producer = Arc::new(producer_over(Arc::clone(&broker), ProducerMode::Chained).await);
    let release = broker.hold_next_publish();
    let dropped = tokio::spawn({
        let producer = Arc::clone(&producer);
        async move { producer.publish(order_with_id(1), Ack::Accepted).await }
    });
    tokio::time::timeout(HOLD_TIMEOUT, broker.held_reached.notified())
        .await
        .expect("X's first send reaches the broker");

    let release_resend = broker.hold_next_publish();
    release.send(()).expect("X's first send is still held");
    tokio::time::timeout(HOLD_TIMEOUT, broker.held_reached.notified())
        .await
        .expect("X's resend reaches the broker");
    dropped.abort();
    let dropped = tokio::time::timeout(HOLD_TIMEOUT, dropped)
        .await
        .expect("X ends in time");
    drop(release_resend);
    let again = tokio::time::timeout(
        HOLD_TIMEOUT,
        producer.publish(order_with_id(1), Ack::Accepted),
    )
    .await
    .expect("X is not blocked by the dropped publish");

    assert!(
        matches!(dropped, Err(ref error) if error.is_cancelled()),
        "X is cancelled, got {dropped:?}"
    );
    assert_eq!(again.expect("X answers"), IngestOutcome::Duplicate);
    assert_eq!(
        broker.sent(),
        vec![
            (event_id(1), Some((1, Some(PRODUCER.0), Some(4), Some(5)))),
            (event_id(1), Some((1, Some(PRODUCER.0), Some(5), Some(6)))),
            (event_id(1), Some((1, Some(PRODUCER.0), Some(5), Some(6)))),
        ],
        "the re-published X carries the resend's numbers"
    );
    assert_eq!(broker.cursor_reads(), 1, "the read at build only");
}

/// A batch over orders and invoices is answered `Duplicate` on its fresh
/// numbers and again on the resend's: `SequenceTaken` names the invoices
/// topic, the first of the two in ascending order.
#[tokio::test]
async fn sequence_taken_names_the_first_topic_of_a_two_topic_batch() {
    let broker = RecordingBroker::new(4, [IngestOutcome::Duplicate, IngestOutcome::Duplicate]);
    let producer = DirectProducer::builder()
        .broker(Arc::<RecordingBroker>::clone(&broker))
        .security_context(SecurityContext::anonymous())
        .source("order-service")
        .topics([TOPIC, INVOICE_TOPIC])
        .event_type_patterns([EVENT_TYPE, INVOICE_TYPE])
        .build()
        .await
        .expect("the producer builds");

    let taken = producer
        .publish_batch(
            vec![
                EventV1::try_from(&order(1)).expect("the order converts"),
                EventV1::try_from(&invoice(2)).expect("the invoice converts"),
            ],
            Ack::Accepted,
        )
        .await;

    let Err(EventBrokerError::SequenceTaken { topic }) = taken else {
        panic!("a second fresh duplicate must be SequenceTaken, got {taken:?}");
    };
    assert_eq!(topic, INVOICE_TOPIC);
    assert_eq!(
        broker.stamps(),
        vec![
            Some((1, Some(PRODUCER.0), Some(4), Some(5))),
            Some((1, Some(PRODUCER.0), Some(-1), Some(0))),
            Some((1, Some(PRODUCER.0), Some(5), Some(6))),
            Some((1, Some(PRODUCER.0), Some(0), Some(1))),
        ],
        "the batch is resent once with the next numbers on both topics"
    );
    assert_eq!(broker.batch_calls(), 2, "the batch and its one resend");
}

/// Bounds a publish that must be refused at once with `ProducerBusy`: far
/// over the time a refusal takes, which awaits no held lock or broker call,
/// and well under `HOLD_TIMEOUT`. A publish that waits for a permit or a lock
/// instead runs into it, since every holder ahead of it is held until the test
/// releases it.
const AT_ONCE: Duration = Duration::from_millis(500);

fn limit(n: usize) -> NonZeroUsize {
    NonZeroUsize::new(n).expect("a non-zero limit")
}

/// A chained producer over the orders and invoices topics that lets at most
/// `limit` publishes wait on one topic.
async fn producer_with_limit(broker: &Arc<RecordingBroker>, limit: usize) -> Arc<DirectProducer> {
    Arc::new(
        DirectProducer::builder()
            .broker(Arc::<RecordingBroker>::clone(broker))
            .security_context(SecurityContext::anonymous())
            .source("order-service")
            .topics([TOPIC, INVOICE_TOPIC])
            .event_type_patterns([EVENT_TYPE, INVOICE_TYPE])
            .max_waiting_per_topic(self::limit(limit))
            .build()
            .await
            .expect("the producer builds"),
    )
}

type PublishTask = JoinHandle<Result<IngestOutcome, EventBrokerError>>;

/// Starts a publish of `order(n)` and returns once the broker holds it, with
/// the sender that releases it.
async fn spawn_held_order(
    producer: &Arc<DirectProducer>,
    broker: &RecordingBroker,
    n: u128,
) -> (oneshot::Sender<()>, PublishTask) {
    let release = broker.hold_next_publish();
    let task = tokio::spawn({
        let producer = Arc::clone(producer);
        async move { producer.publish(order(n), Ack::Accepted).await }
    });
    tokio::time::timeout(HOLD_TIMEOUT, broker.held_reached.notified())
        .await
        .expect("the held publish reaches the broker");
    (release, task)
}

/// With room for one publish on the orders topic, A1 is held at the broker
/// with the topic's only permit. A2 on the same topic is refused at once with
/// `ProducerBusy` and sends nothing; A1 then completes with its outcome.
#[tokio::test]
async fn a_second_publish_on_a_full_topic_is_busy_at_once() {
    let broker = RecordingBroker::new(4, []);
    let producer = producer_with_limit(&broker, 1).await;
    let (release, a1) = spawn_held_order(&producer, &broker, 1).await;

    let a2 = tokio::time::timeout(AT_ONCE, producer.publish(order(2), Ack::Accepted))
        .await
        .expect("A2 answers at once");
    let sent_while_held = broker.stamps();
    release.send(()).expect("A1 is still held");
    let a1 = tokio::time::timeout(HOLD_TIMEOUT, a1)
        .await
        .expect("A1 finishes in time")
        .expect("A1 completes");

    let Err(EventBrokerError::ProducerBusy { limit }) = a2 else {
        panic!("A2 must be refused as busy, got {a2:?}");
    };
    assert_eq!(limit, 1);
    assert_eq!(a1.expect("A1 answers"), IngestOutcome::Accepted);
    assert_eq!(
        sent_while_held,
        vec![Some((1, Some(PRODUCER.0), Some(4), Some(5)))],
        "only A1 reached the broker while it was held"
    );
    assert_eq!(
        (broker.stamps(), broker.cursor_reads()),
        (vec![Some((1, Some(PRODUCER.0), Some(4), Some(5)))], 1),
        "A2 sent nothing and read nothing; the read at build only"
    );
}

/// With room for one publish on the orders topic, A1's fresh numbers are
/// answered `Duplicate` and its resend is held at the broker. A1 keeps the
/// topic's only permit across the resend: a probe is refused at once with
/// `ProducerBusy` and sends nothing; A1 then completes with the resend's
/// outcome.
#[tokio::test]
async fn a_publish_held_at_its_resend_keeps_its_permit() {
    let broker = RecordingBroker::new(4, [IngestOutcome::Duplicate, IngestOutcome::Accepted]);
    let producer = producer_with_limit(&broker, 1).await;
    let (release, a1) = spawn_held_order(&producer, &broker, 1).await;
    let release_resend = broker.hold_next_publish();
    release.send(()).expect("A1's first send is still held");
    tokio::time::timeout(HOLD_TIMEOUT, broker.held_reached.notified())
        .await
        .expect("A1's resend reaches the broker");

    let probe = tokio::time::timeout(AT_ONCE, producer.publish(order(9), Ack::Accepted))
        .await
        .expect("the probe answers at once");
    let sent_while_held = broker.stamps();
    release_resend.send(()).expect("A1's resend is still held");
    let a1 = tokio::time::timeout(HOLD_TIMEOUT, a1)
        .await
        .expect("A1 finishes in time")
        .expect("A1 completes");

    let Err(EventBrokerError::ProducerBusy { limit }) = probe else {
        panic!("the probe must be refused as busy, got {probe:?}");
    };
    assert_eq!(limit, 1);
    assert_eq!(a1.expect("A1 answers"), IngestOutcome::Accepted);
    assert_eq!(
        sent_while_held,
        vec![
            Some((1, Some(PRODUCER.0), Some(4), Some(5))),
            Some((1, Some(PRODUCER.0), Some(5), Some(6))),
        ],
        "only A1's send and resend reached the broker while the resend was held"
    );
    assert_eq!(
        (broker.stamps(), broker.cursor_reads()),
        (
            vec![
                Some((1, Some(PRODUCER.0), Some(4), Some(5))),
                Some((1, Some(PRODUCER.0), Some(5), Some(6))),
            ],
            1
        ),
        "the probe sent nothing and read nothing; the read at build only"
    );
}

/// As `a_second_publish_on_a_full_topic_is_busy_at_once`: while A1 holds the
/// orders topic's only permit and A2 is refused, B publishes on the invoices
/// topic, whose permit is its own.
#[tokio::test]
async fn a_full_topic_does_not_block_another_topic() {
    let broker = RecordingBroker::new(4, []);
    let producer = producer_with_limit(&broker, 1).await;
    let (release, a1) = spawn_held_order(&producer, &broker, 1).await;

    let a2 = tokio::time::timeout(AT_ONCE, producer.publish(order(2), Ack::Accepted))
        .await
        .expect("A2 answers at once");
    let b = tokio::time::timeout(HOLD_TIMEOUT, producer.publish(invoice(3), Ack::Accepted))
        .await
        .expect("B is not held behind the orders topic");
    release.send(()).expect("A1 is still held");
    let a1 = tokio::time::timeout(HOLD_TIMEOUT, a1)
        .await
        .expect("A1 finishes in time")
        .expect("A1 completes");

    let Err(EventBrokerError::ProducerBusy { limit }) = a2 else {
        panic!("A2 must be refused as busy, got {a2:?}");
    };
    assert_eq!(limit, 1);
    assert_eq!(
        (a1.expect("A1 answers"), b.expect("B answers")),
        (IngestOutcome::Accepted, IngestOutcome::Accepted)
    );
    assert_eq!(
        broker.stamps(),
        vec![
            Some((1, Some(PRODUCER.0), Some(4), Some(5))),
            Some((1, Some(PRODUCER.0), Some(-1), Some(0))),
        ],
        "B is sent while A1 is held; A2 sent nothing"
    );
}

/// A1 holds the orders topic's only permit. A batch over orders and invoices
/// takes the invoices permit first (ascending topic order), misses the orders
/// one and is refused at once with `ProducerBusy`, giving the invoices permit
/// back: B then publishes on invoices while A1 is still held.
#[tokio::test]
async fn a_batch_takes_every_topics_permit_or_none() {
    let broker = RecordingBroker::new(4, []);
    let producer = producer_with_limit(&broker, 1).await;
    let (release, a1) = spawn_held_order(&producer, &broker, 1).await;

    let batch = tokio::time::timeout(
        AT_ONCE,
        producer.publish_batch(
            vec![
                EventV1::try_from(&order(2)).expect("the order converts"),
                EventV1::try_from(&invoice(3)).expect("the invoice converts"),
            ],
            Ack::Accepted,
        ),
    )
    .await
    .expect("the batch answers at once");
    let b = tokio::time::timeout(HOLD_TIMEOUT, producer.publish(invoice(4), Ack::Accepted))
        .await
        .expect("B is not held behind the refused batch");
    release.send(()).expect("A1 is still held");
    let a1 = tokio::time::timeout(HOLD_TIMEOUT, a1)
        .await
        .expect("A1 finishes in time")
        .expect("A1 completes");

    let Err(EventBrokerError::ProducerBusy { limit }) = batch else {
        panic!("the batch must be refused as busy, got {batch:?}");
    };
    assert_eq!(limit, 1);
    assert_eq!(
        (a1.expect("A1 answers"), b.expect("B answers")),
        (IngestOutcome::Accepted, IngestOutcome::Accepted)
    );
    assert_eq!(
        (broker.stamps(), broker.batch_calls()),
        (
            vec![
                Some((1, Some(PRODUCER.0), Some(4), Some(5))),
                Some((1, Some(PRODUCER.0), Some(-1), Some(0))),
            ],
            0
        ),
        "the batch sent nothing; B takes the invoices chain's first position"
    );
}

/// With room for two publishes on the orders topic, A1 is held at the broker
/// and A2 waits for the topic's lock: a probe is refused, so A2 holds the
/// second permit. A2's future is dropped while it waits. Its permit goes with
/// it: A3 is not refused, waits behind A1, and publishes once A1 is released.
/// A2 and A3 can park on a contended await before they reach the lock; the 16
/// yields bound how many times each may park and still queue behind A1.
#[tokio::test]
async fn a_dropped_waiting_publish_releases_its_permit() {
    let broker = RecordingBroker::new(4, []);
    let producer = producer_with_limit(&broker, 2).await;
    let (release, a1) = spawn_held_order(&producer, &broker, 1).await;
    let a2 = tokio::spawn({
        let producer = Arc::clone(&producer);
        async move { producer.publish(order(2), Ack::Accepted).await }
    });
    for _ in 0..16 {
        tokio::task::yield_now().await;
    }

    let probe = tokio::time::timeout(AT_ONCE, producer.publish(order(9), Ack::Accepted))
        .await
        .expect("the probe answers at once");
    a2.abort();
    let a2 = tokio::time::timeout(HOLD_TIMEOUT, a2)
        .await
        .expect("A2 ends in time");
    let a3 = tokio::spawn({
        let producer = Arc::clone(&producer);
        async move { producer.publish(order(3), Ack::Accepted).await }
    });
    for _ in 0..16 {
        tokio::task::yield_now().await;
    }
    release.send(()).expect("A1 is still held");
    let a1 = tokio::time::timeout(HOLD_TIMEOUT, a1)
        .await
        .expect("A1 finishes in time")
        .expect("A1 completes");
    let a3 = tokio::time::timeout(HOLD_TIMEOUT, a3)
        .await
        .expect("A3 finishes in time")
        .expect("A3 completes");

    let Err(EventBrokerError::ProducerBusy { limit }) = probe else {
        panic!("the probe must be refused as busy, got {probe:?}");
    };
    assert_eq!(limit, 2);
    assert!(
        matches!(a2, Err(ref error) if error.is_cancelled()),
        "A2 is cancelled, got {a2:?}"
    );
    assert_eq!(
        (a1.expect("A1 answers"), a3.expect("A3 answers")),
        (IngestOutcome::Accepted, IngestOutcome::Accepted)
    );
    assert_eq!(
        broker.stamps(),
        vec![
            Some((1, Some(PRODUCER.0), Some(4), Some(5))),
            Some((1, Some(PRODUCER.0), Some(5), Some(6))),
        ],
        "A2 and the probe sent nothing; A3 chains onto A1"
    );
}

/// With room for one publish on the orders topic, A's answer fails, so B
/// re-reads the head before it is stamped. B's re-read is held at the broker
/// and B's future is dropped there. Its permit goes with it: C is not refused,
/// re-reads the head and chains onto the stored A.
#[tokio::test]
async fn a_publish_dropped_during_its_reread_releases_its_permit() {
    let broker = RecordingBroker::new(4, []);
    let producer = producer_with_limit(&broker, 1).await;
    broker.fail_next_publish();
    let a = producer.publish(order(1), Ack::Accepted).await;
    let release = broker.hold_next_cursor_read();
    let b = tokio::spawn({
        let producer = Arc::clone(&producer);
        async move { producer.publish(order(2), Ack::Accepted).await }
    });
    tokio::time::timeout(HOLD_TIMEOUT, broker.held_reached.notified())
        .await
        .expect("B's re-read reaches the broker");

    b.abort();
    let b = tokio::time::timeout(HOLD_TIMEOUT, b)
        .await
        .expect("B ends in time");
    drop(release);
    let c = tokio::time::timeout(HOLD_TIMEOUT, producer.publish(order(3), Ack::Accepted))
        .await
        .expect("C is not blocked by the dropped B");

    assert!(
        matches!(a, Err(EventBrokerError::Transport(ref detail)) if detail == "connection reset"),
        "A surfaces the transport fault, got {a:?}"
    );
    assert!(
        matches!(b, Err(ref error) if error.is_cancelled()),
        "B is cancelled, got {b:?}"
    );
    assert_eq!(c.expect("C answers"), IngestOutcome::Accepted);
    assert_eq!(
        broker.stamps(),
        vec![
            Some((1, Some(PRODUCER.0), Some(4), Some(5))),
            Some((1, Some(PRODUCER.0), Some(5), Some(6))),
        ],
        "B sent nothing; C chains onto the stored A"
    );
    assert_eq!(
        broker.cursor_reads(),
        3,
        "the read at build, B's dropped re-read and C's re-read"
    );
}

/// A lazy producer with room for one publish on the orders topic. An order
/// prepares its type on its first publish. A shipment, a type on the same
/// topic not yet prepared, is held inside its event-type lookup; a second
/// order publishes meanwhile, so the lookup holds no permit.
#[tokio::test]
async fn a_lazy_schema_lookup_holds_no_permit() {
    let broker = RecordingBroker::new(4, []);
    let producer = Arc::new(
        DirectProducer::builder()
            .broker(Arc::<RecordingBroker>::clone(&broker))
            .security_context(SecurityContext::anonymous())
            .source("order-service")
            .topics([TOPIC])
            .event_type_patterns([EVENT_TYPE, SHIPPED_TYPE])
            .lazy_schema_loading()
            .max_waiting_per_topic(limit(1))
            .build()
            .await
            .expect("the lazy producer builds"),
    );
    let first = producer
        .publish(order(1), Ack::Accepted)
        .await
        .expect("the first order publishes");
    let release = broker.hold_next_lookup();
    let shipment = tokio::spawn({
        let producer = Arc::clone(&producer);
        async move { producer.publish(shipment(2), Ack::Accepted).await }
    });
    tokio::time::timeout(HOLD_TIMEOUT, broker.held_reached.notified())
        .await
        .expect("the shipment's lookup reaches the broker");

    let second = tokio::time::timeout(HOLD_TIMEOUT, producer.publish(order(3), Ack::Accepted))
        .await
        .expect("the second order is not held behind the lookup");
    release.send(()).expect("the lookup is still held");
    let shipment = tokio::time::timeout(HOLD_TIMEOUT, shipment)
        .await
        .expect("the shipment finishes in time")
        .expect("the shipment completes");

    assert_eq!(
        (
            first,
            second.expect("the second order answers"),
            shipment.expect("the shipment answers")
        ),
        (
            IngestOutcome::Accepted,
            IngestOutcome::Accepted,
            IngestOutcome::Accepted
        )
    );
    assert_eq!(
        broker.stamps(),
        vec![
            Some((1, Some(PRODUCER.0), Some(4), Some(5))),
            Some((1, Some(PRODUCER.0), Some(5), Some(6))),
            Some((1, Some(PRODUCER.0), Some(6), Some(7))),
        ],
        "the second order is sent while the shipment's lookup is held"
    );
    assert_eq!(
        broker.event_type_lookups(),
        2,
        "one lookup per type: the order's and the shipment's"
    );
}

/// The default limit lets 64 publishes wait on one topic: A1 held at the
/// broker and 63 waiting for its lock. The 65th is refused at once; the 64
/// publish once A1 is released. The waiters can park on a contended await
/// before they reach the lock; the 16 yields bound how many times each may
/// park and still hold its permit when the 65th is tried.
#[tokio::test]
async fn the_default_limit_lets_64_publishes_wait_on_one_topic() {
    let broker = RecordingBroker::new(4, []);
    let producer = Arc::new(producer_over(Arc::clone(&broker), ProducerMode::Chained).await);
    let (release, a1) = spawn_held_order(&producer, &broker, 1).await;
    let waiters = (2..=64)
        .map(|n| {
            let producer = Arc::clone(&producer);
            tokio::spawn(async move { producer.publish(order(n), Ack::Accepted).await })
        })
        .collect::<Vec<_>>();
    for _ in 0..16 {
        tokio::task::yield_now().await;
    }

    let refused = tokio::time::timeout(AT_ONCE, producer.publish(order(65), Ack::Accepted))
        .await
        .expect("the 65th answers at once");
    release.send(()).expect("A1 is still held");
    let mut outcomes = vec![
        tokio::time::timeout(HOLD_TIMEOUT, a1)
            .await
            .expect("A1 finishes in time")
            .expect("A1 completes")
            .expect("A1 answers"),
    ];
    for waiter in waiters {
        outcomes.push(
            tokio::time::timeout(HOLD_TIMEOUT, waiter)
                .await
                .expect("a waiter finishes in time")
                .expect("a waiter completes")
                .expect("a waiter answers"),
        );
    }

    let Err(EventBrokerError::ProducerBusy { limit }) = refused else {
        panic!("the 65th must be refused as busy, got {refused:?}");
    };
    assert_eq!(limit, 64);
    assert_eq!(outcomes, vec![IngestOutcome::Accepted; 64]);
    assert_eq!(
        broker.stamps(),
        (5..=68)
            .map(|sequence| Some((1, Some(PRODUCER.0), Some(sequence - 1), Some(sequence))))
            .collect::<Vec<_>>(),
        "the 64 publishes are stamped one after another; the 65th sent nothing"
    );
}

/// A limit past `tokio::sync::Semaphore::MAX_PERMITS`, which a semaphore
/// cannot hold, is refused at build, before the producer registers or reads
/// anything; a limit of exactly `MAX_PERMITS` builds.
#[tokio::test]
async fn a_limit_past_the_semaphore_maximum_is_refused_at_build() {
    let broker = RecordingBroker::new(4, []);
    let builder = || {
        DirectProducer::builder()
            .broker(Arc::<RecordingBroker>::clone(&broker))
            .security_context(SecurityContext::anonymous())
            .source("order-service")
            .topics([TOPIC])
            .event_type_patterns([EVENT_TYPE])
    };

    let refused = builder()
        .max_waiting_per_topic(limit(tokio::sync::Semaphore::MAX_PERMITS + 1))
        .build()
        .await;
    let nothing_reached = (broker.registrations(), broker.cursor_reads());
    let at_max = builder()
        .max_waiting_per_topic(limit(tokio::sync::Semaphore::MAX_PERMITS))
        .build()
        .await;

    let Err(EventBrokerError::InvalidProducerOptions { detail }) = refused else {
        panic!("a limit past the maximum must be refused, got {refused:?}");
    };
    assert_eq!(
        detail,
        "max_waiting_per_topic must be at most 2305843009213693951"
    );
    assert_eq!(
        nothing_reached,
        (Vec::new(), 0),
        "nothing reaches the broker"
    );
    assert_eq!(
        at_max
            .expect("a limit of exactly the maximum builds")
            .producer_id(),
        PRODUCER
    );
}
