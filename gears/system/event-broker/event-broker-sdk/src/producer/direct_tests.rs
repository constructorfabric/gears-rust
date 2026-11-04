//! Chain stamping of the direct producer. The first two tests drive the
//! cursor helpers alone. The rest run a `Producer` over a `RecordingBroker`
//! stub: it records every published event's producer metadata in arrival
//! order and answers each publish call with the next scripted outcome.

use std::borrow::Cow;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::{Notify, oneshot};
use toolkit_gts::gts_id;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use ::gts::{GtsInstanceId, GtsTypeId};

use crate::api::{IngestOutcome, ProducerCursors, ProducerMode, TopicCursors};
use crate::broker_stub::BrokerStub;
use crate::error::EventBrokerError;
use crate::ids::ProducerId;
use crate::models::{Event, EventType, ProducerMeta, Topic};
use crate::typed_event::TypedEvent;

use super::direct::{Producer, ProducerSequenceCursor, advance_cursor, producer_meta_for_last};
use super::event_factory::PreparedEvent;
use super::types::{DirectDeduplication, ProducerIdentity};

const TOPIC: &str = gts_id!("cf.core.events.topic.v1~example.sdk.producer.unit_orders.v1");
const EVENT_TYPE: &str =
    gts_id!("cf.core.events.event.v1~example.sdk.producer.unit_order_created.v1~");
const SUBJECT_TYPE: &str = gts_id!("cf.core.events.subject.v1~example.sdk.producer.unit_order.v1~");
const PRODUCER: ProducerId = ProducerId(Uuid::from_u128(0x0190_f1a2_0000_7000_8000_0000_0000_0d01));

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

fn second_batch_meta(mode: ProducerMode) -> crate::models::ProducerMeta {
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
type Stamp = (u8, Option<Uuid>, Option<i64>, Option<i64>);

fn stamp(meta: &ProducerMeta) -> Stamp {
    (meta.version, meta.producer_id, meta.previous, meta.sequence)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OrderCreated {
    order_id: Uuid,
}

impl TypedEvent for OrderCreated {
    const TYPE_ID: &'static str = EVENT_TYPE;
    const SUBJECT_TYPE: &'static str = SUBJECT_TYPE;
    const SOURCE: &'static str = "order-service";

    fn subject(&self) -> Cow<'_, str> {
        Cow::Owned(self.order_id.to_string())
    }
}

fn order(n: u128) -> OrderCreated {
    OrderCreated {
        order_id: Uuid::from_u128(n),
    }
}

/// Serves the orders topic and its event type, primes the producer at
/// `last_sequence`, and answers each `publish` / `publish_batch` call with the
/// next scripted outcome (`Accepted` once the script runs out). A held publish
/// reports on `held_reached` and waits for its release before it answers. A
/// failed publish is recorded, then answers a transport fault.
struct RecordingBroker {
    last_sequence: i64,
    fail_next: AtomicBool,
    batch_calls: AtomicUsize,
    answers: StdMutex<VecDeque<IngestOutcome>>,
    stamps: StdMutex<Vec<Option<Stamp>>>,
    hold: StdMutex<Option<oneshot::Receiver<()>>>,
    held_reached: Notify,
}

impl RecordingBroker {
    fn new(last_sequence: i64, answers: impl IntoIterator<Item = IngestOutcome>) -> Arc<Self> {
        Arc::new(Self {
            last_sequence,
            fail_next: AtomicBool::new(false),
            batch_calls: AtomicUsize::new(0),
            answers: StdMutex::new(answers.into_iter().collect()),
            stamps: StdMutex::new(Vec::new()),
            hold: StdMutex::new(None),
            held_reached: Notify::new(),
        })
    }

    /// Holds the next single publish until the returned sender fires or drops.
    fn hold_next_publish(&self) -> oneshot::Sender<()> {
        let (release, gate) = oneshot::channel();
        *self.hold.lock().expect("hold lock") = Some(gate);
        release
    }

    /// Fails the next single publish after recording it.
    fn fail_next_publish(&self) {
        self.fail_next.store(true, Ordering::SeqCst);
    }

    fn batch_calls(&self) -> usize {
        self.batch_calls.load(Ordering::SeqCst)
    }

    fn stamps(&self) -> Vec<Option<Stamp>> {
        self.stamps.lock().expect("stamps lock").clone()
    }

    fn record(&self, events: &[Event]) -> IngestOutcome {
        self.stamps
            .lock()
            .expect("stamps lock")
            .extend(events.iter().map(|event| event.meta.as_ref().map(stamp)));
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
        _mode: ProducerMode,
        _client_agent: &str,
    ) -> Result<ProducerId, EventBrokerError> {
        unimplemented!("the producer reuses a fixed id")
    }
    async fn publish(
        &self,
        _ctx: &SecurityContext,
        event: &Event,
    ) -> Result<IngestOutcome, EventBrokerError> {
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
        events: &[Event],
    ) -> Result<IngestOutcome, EventBrokerError> {
        self.batch_calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.record(events))
    }
    async fn get_producer_cursors(
        &self,
        _ctx: &SecurityContext,
        producer_id: ProducerId,
    ) -> Result<ProducerCursors, EventBrokerError> {
        Ok(ProducerCursors {
            producer_id,
            client_agent: "order-service/1.0".to_owned(),
            topics: vec![TopicCursors {
                topic: GtsInstanceId::try_new(TOPIC).expect("a valid topic id"),
                last_sequence: self.last_sequence,
            }],
        })
    }
    async fn list_topics(&self, _ctx: &SecurityContext) -> Result<Vec<Topic>, EventBrokerError> {
        Ok(vec![Topic {
            id: GtsInstanceId::try_new(TOPIC).expect("a valid topic id"),
            description: "orders".to_owned(),
            retention: None,
        }])
    }
    async fn list_event_types(
        &self,
        _ctx: &SecurityContext,
    ) -> Result<Vec<EventType>, EventBrokerError> {
        Ok(vec![EventType {
            id: GtsTypeId::try_new(EVENT_TYPE).expect("a valid event type id"),
            topic: GtsInstanceId::try_new(TOPIC).expect("a valid topic id"),
            description: None,
            allowed_subject_types: Vec::new(),
            partition_key: "/tenant_id".to_owned(),
            data_schema: json!({ "type": "object" }),
        }])
    }
    async fn get_event_type(
        &self,
        _ctx: &SecurityContext,
        _id: &str,
    ) -> Result<EventType, EventBrokerError> {
        unimplemented!("the producer prepares every type up front")
    }
}

async fn producer_over(broker: Arc<RecordingBroker>, mode: ProducerMode) -> Producer {
    Producer::builder()
        .broker(broker)
        .security_context(SecurityContext::anonymous())
        .identity(ProducerIdentity::new().source("order-service"))
        .deduplication(DirectDeduplication::reuse(mode, PRODUCER))
        .topics([TOPIC])
        .event_type_patterns([EVENT_TYPE])
        .prepare_all()
        .await
        .expect("the producer builds")
}

#[tokio::test]
async fn a_duplicate_ack_advances_the_local_chain() {
    let broker = RecordingBroker::new(-1, [IngestOutcome::Duplicate]);
    let producer = producer_over(Arc::clone(&broker), ProducerMode::Chained).await;

    let a = producer.publish(order(1)).await.expect("A publishes");
    let b = producer.publish(order(2)).await.expect("B publishes");

    assert_eq!((a, b), (IngestOutcome::Duplicate, IngestOutcome::Accepted));
    assert_eq!(
        broker.stamps(),
        vec![
            Some((1, Some(PRODUCER.0), Some(-1), Some(0))),
            Some((1, Some(PRODUCER.0), Some(0), Some(1))),
        ],
        "B chains onto the sequence the broker already holds"
    );
}

#[tokio::test]
async fn a_duplicate_ack_advances_a_monotonic_producer() {
    let broker = RecordingBroker::new(-1, [IngestOutcome::Duplicate]);
    let producer = producer_over(Arc::clone(&broker), ProducerMode::Monotonic).await;

    let a = producer.publish(order(1)).await.expect("A publishes");
    let b = producer.publish(order(2)).await.expect("B publishes");

    assert_eq!((a, b), (IngestOutcome::Duplicate, IngestOutcome::Accepted));
    assert_eq!(
        broker.stamps(),
        vec![
            Some((1, Some(PRODUCER.0), None, Some(0))),
            Some((1, Some(PRODUCER.0), None, Some(1))),
        ],
        "B moves past the sequence the broker already holds"
    );
}

#[tokio::test]
async fn an_empty_batch_publishes_nothing_and_leaves_the_chain_unmoved() {
    let broker = RecordingBroker::new(-1, []);
    let producer = producer_over(Arc::clone(&broker), ProducerMode::Chained).await;

    let batch = producer
        .publish_batch(Vec::<OrderCreated>::new())
        .await
        .expect("an empty batch publishes nothing");
    let next = producer
        .publish(order(1))
        .await
        .expect("the next publishes");

    assert_eq!(
        (batch, next, broker.batch_calls()),
        (Vec::<IngestOutcome>::new(), IngestOutcome::Accepted, 0)
    );
    assert_eq!(
        broker.stamps(),
        vec![Some((1, Some(PRODUCER.0), Some(-1), Some(0)))],
        "the next event takes the chain's first position: the empty batch moved nothing"
    );
}

#[tokio::test]
async fn an_all_duplicate_batch_advances_the_local_chain_to_its_highest_sequence() {
    let broker = RecordingBroker::new(-1, [IngestOutcome::Duplicate]);
    let producer = producer_over(Arc::clone(&broker), ProducerMode::Chained).await;

    let batch = producer
        .publish_batch(vec![order(1), order(2)])
        .await
        .expect("the batch publishes");
    let next = producer
        .publish(order(3))
        .await
        .expect("the next publishes");

    assert_eq!(
        (batch, next),
        (
            vec![IngestOutcome::Duplicate, IngestOutcome::Duplicate],
            IngestOutcome::Accepted
        )
    );
    assert_eq!(
        broker.stamps(),
        vec![
            Some((1, Some(PRODUCER.0), Some(-1), Some(0))),
            Some((1, Some(PRODUCER.0), Some(0), Some(1))),
            Some((1, Some(PRODUCER.0), Some(1), Some(2))),
        ],
        "the next event chains onto the batch's highest sequence"
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
        async move { producer.publish(order(1)).await }
    });
    tokio::time::timeout(HOLD_TIMEOUT, broker.held_reached.notified())
        .await
        .expect("A reaches the broker");

    let b = tokio::spawn({
        let producer = Arc::clone(&producer);
        async move { producer.publish(order(2)).await }
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

/// The broker fails A, so the chain stays at 4 and the topic's publish lock is
/// released: B is stamped with A's numbers and goes through.
#[tokio::test]
async fn a_failed_publish_releases_the_topic_and_leaves_the_chain_where_it_was() {
    let broker = RecordingBroker::new(4, []);
    let producer = producer_over(Arc::clone(&broker), ProducerMode::Chained).await;
    broker.fail_next_publish();

    let a = producer.publish(order(1)).await;
    let b = tokio::time::timeout(HOLD_TIMEOUT, producer.publish(order(2)))
        .await
        .expect("B is not blocked by A's lock");

    assert!(
        matches!(a, Err(EventBrokerError::Transport(ref detail)) if detail == "connection reset"),
        "A surfaces the transport fault, got {a:?}"
    );
    assert_eq!(b.expect("B answers"), IngestOutcome::Accepted);
    assert_eq!(
        broker.stamps(),
        vec![
            Some((1, Some(PRODUCER.0), Some(4), Some(5))),
            Some((1, Some(PRODUCER.0), Some(4), Some(5))),
        ],
        "B reuses the numbers of the failed A"
    );
}

/// Local state stands at 2. A single publish stamped 3 is held at the broker
/// while a batch is started on the same topic. The batch waits on the topic's
/// publish lock, so it cannot be stamped 3..=5 behind the held publish and
/// race its answer. The held publish answers `Duplicate` for 3 and advances
/// the chain to 3; the batch is stamped 4..=6 after it. The batch can park on
/// a contended await before it reaches the lock; the 16 yields bound how many
/// times it may park and still be held behind the held publish.
#[tokio::test]
async fn a_batch_started_behind_a_held_publish_is_stamped_after_it() {
    let broker = RecordingBroker::new(2, [IngestOutcome::Duplicate, IngestOutcome::Accepted]);
    let producer = Arc::new(producer_over(Arc::clone(&broker), ProducerMode::Chained).await);
    let release = broker.hold_next_publish();
    let held = tokio::spawn({
        let producer = Arc::clone(&producer);
        async move { producer.publish(order(1)).await }
    });
    tokio::time::timeout(HOLD_TIMEOUT, broker.held_reached.notified())
        .await
        .expect("the held publish reaches the broker");

    let batch = tokio::spawn({
        let producer = Arc::clone(&producer);
        async move {
            producer
                .publish_batch(vec![order(2), order(3), order(4)])
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
        .publish(order(5))
        .await
        .expect("the next publishes");

    assert_eq!(
        (held, batch, next),
        (
            IngestOutcome::Duplicate,
            vec![IngestOutcome::Accepted; 3],
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
        ],
        "the batch continues from the held publish's 3"
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
        .publish(order(1))
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
    PreparedEvent {
        event: Event {
            id: Uuid::from_u128(0x0190_f1a2_0000_7000_8000_0000_0000_e001),
            type_id: GtsTypeId::try_new(EVENT_TYPE).expect("a valid event type id"),
            tenant_id: Uuid::from_u128(0x0190_f1a2_0000_7000_8000_0000_0000_7e01),
            source: "order-service".to_owned(),
            subject: "order-stale".to_owned(),
            subject_type: GtsTypeId::try_new(SUBJECT_TYPE).expect("a valid subject type id"),
            occurred_at: chrono::DateTime::UNIX_EPOCH,
            trace_parent: None,
            data: Some(json!({})),
            partition: None,
            sequence: None,
            sequence_time: None,
            meta: producer_meta_for_last(ProducerMode::Chained, PRODUCER, sequence - 1),
        },
        topic: TOPIC.to_owned(),
        key: "00000000-0000-0000-0000-000000000000".to_owned(),
    }
}
