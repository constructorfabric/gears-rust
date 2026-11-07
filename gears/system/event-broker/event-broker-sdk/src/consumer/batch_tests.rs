use crate::sequence::Sequence;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use toolkit_gts::gts_id;
use uuid::Uuid;

use super::types::{DeliverBatch, Delivered, EventHandlerAdapter};
use super::{
    BatchHandler, BatchHandlerOutcome, Delivery, EventBatch, EventHandler, HandlerOutcome,
};
use crate::error::ConsumerError;
use crate::gts::{EventV1, StoredPosition};

const TOPIC: &str = gts_id!("cf.core.events.topic.v1~example.orders.x.x.v1");

fn position_at(offset: i64) -> StoredPosition {
    StoredPosition {
        partition: 7,
        sequence: Sequence::assigned(offset),
        sequence_time: "2026-01-02T03:04:06Z".parse().unwrap(),
    }
}

fn raw_event(offset: i64) -> EventV1 {
    EventV1::builder_for(
        gts::GtsTypeId::new(gts_id!(
            "cf.core.events.event.v1~example.orders.order_created.x.v1~"
        )),
        serde_json::json!({ "offset": offset }),
    )
    .id(Uuid::from_u128(u128::try_from(offset).unwrap()))
    .tenant_id(Uuid::nil())
    .subject(format!("order-{offset}"))
    .subject_type_id(gts::GtsTypeId::new("gts.x.eb.test.subject.v1~"))
    .occurred_at("2026-01-02T03:04:05Z".parse().unwrap())
    .build()
    .unwrap()
    .stamp_position(position_at(offset))
}

fn positions_of(events: &[EventV1]) -> Vec<StoredPosition> {
    events
        .iter()
        .map(|event| StoredPosition {
            partition: event.partition().expect("a delivered event is stamped"),
            sequence: event.sequence().expect("a delivered event is stamped"),
            sequence_time: event.sequence_time().expect("a delivered event is stamped"),
        })
        .collect()
}

fn delivery(attempts: u16) -> Delivery {
    Delivery::new(gts::GtsInstanceId::try_new(TOPIC).unwrap(), 7, attempts)
}

fn sequence_of(event: &EventV1) -> i64 {
    event
        .sequence()
        .expect("a delivered event is stamped")
        .as_i64()
}

#[test]
fn event_batch_reports_empty_state() {
    let events: Vec<EventV1> = Vec::new();
    let batch = EventBatch::new(&events);

    assert!(batch.is_empty());
    assert_eq!(batch.len(), 0);
    assert!(batch.next_event().is_none());
    assert!(batch.next_chunk(10).is_empty());
    assert!(batch.iter().next().is_none());
}

#[test]
fn event_batch_reads_without_mutating_progress() {
    let events = vec![raw_event(10), raw_event(11), raw_event(12)];
    let batch = EventBatch::new(&events);

    assert_eq!(batch.len(), 3);
    assert_eq!(
        batch.next_event().and_then(EventV1::sequence),
        Some(Sequence::assigned(10))
    );
    assert_eq!(
        batch
            .next_chunk(2)
            .iter()
            .map(sequence_of)
            .collect::<Vec<_>>(),
        vec![10, 11]
    );
    assert_eq!(
        batch.iter().map(sequence_of).collect::<Vec<_>>(),
        vec![10, 11, 12]
    );
    assert_eq!(
        batch.next_event().and_then(EventV1::sequence),
        Some(Sequence::assigned(10))
    );
}

struct RecordingSingleHandler {
    calls: Arc<Mutex<Vec<i64>>>,
    outcome: HandlerOutcome,
}

#[async_trait::async_trait]
impl EventHandler<serde_json::Value> for RecordingSingleHandler {
    async fn handle(
        &self,
        event: EventV1,
        _delivery: &Delivery,
    ) -> Result<HandlerOutcome, ConsumerError> {
        self.calls.lock().unwrap().push(sequence_of(&event));
        Ok(self.outcome.clone())
    }
}

#[tokio::test]
async fn single_handler_adapter_reports_one_event_processed_on_success() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let handler = RecordingSingleHandler {
        calls: calls.clone(),
        outcome: HandlerOutcome::Success,
    };
    let adapter = EventHandlerAdapter::new(handler);
    let events = vec![raw_event(40)];
    let batch = EventBatch::new(&events);

    let Delivered { outcome, skipped } = adapter
        .deliver(&batch, &positions_of(&events), &delivery(1))
        .await
        .unwrap();

    assert!(matches!(
        outcome,
        BatchHandlerOutcome::AdvanceThrough { offset } if offset == Sequence::assigned(40)
    ));
    assert_eq!(skipped, None);
    assert_eq!(
        batch.next_event().and_then(EventV1::sequence),
        Some(Sequence::assigned(40))
    );
    assert_eq!(*calls.lock().unwrap(), vec![40]);
}

#[tokio::test]
async fn single_handler_adapter_reports_retry_without_progress() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let handler = RecordingSingleHandler {
        calls: calls.clone(),
        outcome: HandlerOutcome::Retry {
            reason: "not yet".to_owned(),
        },
    };
    let adapter = EventHandlerAdapter::new(handler);
    let events = vec![raw_event(41)];
    let batch = EventBatch::new(&events);

    let Delivered { outcome, skipped } = adapter
        .deliver(&batch, &positions_of(&events), &delivery(1))
        .await
        .unwrap();

    assert!(matches!(outcome, BatchHandlerOutcome::Retry { .. }));
    assert_eq!(skipped, None);
    assert_eq!(
        batch.next_event().and_then(EventV1::sequence),
        Some(Sequence::assigned(41))
    );
    assert_eq!(*calls.lock().unwrap(), vec![41]);
}

/// Batch handler that reads a fixed-size chunk from the front of the batch,
/// records the offsets it saw on each call, and returns a scripted outcome per call.
struct ChunkingHandler {
    chunk: usize,
    seen: Arc<Mutex<Vec<Vec<i64>>>>,
    scripted: Arc<Mutex<VecDeque<BatchHandlerOutcome>>>,
}

#[async_trait::async_trait]
impl BatchHandler<serde_json::Value> for ChunkingHandler {
    async fn handle_batch(
        &self,
        batch: &EventBatch<'_, serde_json::Value>,
        _delivery: &Delivery,
    ) -> Result<BatchHandlerOutcome, ConsumerError> {
        let chunk: Vec<i64> = batch
            .next_chunk(self.chunk)
            .iter()
            .map(sequence_of)
            .collect();
        self.seen.lock().unwrap().push(chunk);
        Ok(self
            .scripted
            .lock()
            .unwrap()
            .pop_front()
            .expect("scripted outcome"))
    }
}

/// Drive a handler through the dispatcher's redelivery model: each round hands the
/// handler a fresh `EventBatch` over the not-yet-advanced tail; the committed frontier
/// advances by `BatchHandlerOutcome::processed_count` (Retry advances nothing). Returns the
/// per-round outcome tags once the batch is fully processed.
async fn drive_redelivery(events: &[EventV1], handler: &ChunkingHandler, max_rounds: usize) {
    let mut remaining: Vec<EventV1> = events.to_vec();
    let mut rounds = 0;
    while !remaining.is_empty() {
        rounds += 1;
        assert!(rounds <= max_rounds, "redelivery did not converge");
        let batch = EventBatch::new(&remaining);
        let outcome = handler
            .handle_batch(&batch, &delivery(u16::try_from(rounds).unwrap()))
            .await
            .unwrap();
        match outcome.processed_count(&positions_of(&remaining)).unwrap() {
            None => { /* Retry: frontier unchanged, redeliver the same tail */ }
            Some(n) => {
                remaining.drain(0..n);
            }
        }
    }
}

// chunk -> error(retry) -> retry(readvance from front) -> partial advance -> success.
// Demonstrates that "advance" is carried by the outcome offset + the committed frontier
// (redelivery re-anchors a fresh batch), NOT by any per-batch cursor.
#[tokio::test]
async fn chunk_retry_then_partial_advance_then_success_redelivers_from_front() {
    let events = vec![raw_event(10), raw_event(11), raw_event(12), raw_event(13)];
    let seen = Arc::new(Mutex::new(Vec::new()));
    let handler = ChunkingHandler {
        chunk: 2,
        seen: seen.clone(),
        scripted: Arc::new(Mutex::new(VecDeque::from(vec![
            BatchHandlerOutcome::Retry {
                reason: "downstream unavailable".to_owned(),
            },
            BatchHandlerOutcome::AdvanceThrough {
                offset: Sequence::assigned(12),
            },
            BatchHandlerOutcome::Success,
        ]))),
    };

    drive_redelivery(&events, &handler, 5).await;

    // Round 1: front chunk [10,11], Retry -> frontier unchanged.
    // Round 2: SAME front chunk [10,11] (fresh batch, cursor re-anchored), advance through 12
    //          -> tail becomes [13].
    // Round 3: front chunk [13], Success -> done.
    assert_eq!(
        *seen.lock().unwrap(),
        vec![vec![10, 11], vec![10, 11], vec![13]],
    );
}

// A partial AdvanceThrough redelivers only the un-acked tail.
#[tokio::test]
async fn partial_advance_redelivers_only_unacked_tail() {
    let events = vec![raw_event(20), raw_event(21), raw_event(22)];
    let seen = Arc::new(Mutex::new(Vec::new()));
    let handler = ChunkingHandler {
        chunk: 8,
        seen: seen.clone(),
        scripted: Arc::new(Mutex::new(VecDeque::from(vec![
            BatchHandlerOutcome::AdvanceThrough {
                offset: Sequence::assigned(21),
            },
            BatchHandlerOutcome::Success,
        ]))),
    };

    drive_redelivery(&events, &handler, 5).await;

    // Round 1 sees the whole batch; advancing through 21 leaves [22] for round 2.
    assert_eq!(*seen.lock().unwrap(), vec![vec![20, 21, 22], vec![22]]);
}

// A Retry advances nothing: the committed frontier is unchanged, so the same tail redelivers.
#[tokio::test]
async fn retry_outcome_leaves_frontier_unchanged() {
    let events = vec![raw_event(30), raw_event(31)];
    let batch = EventBatch::new(&events);
    let outcome = BatchHandlerOutcome::Retry {
        reason: "not yet".to_owned(),
    };

    let processed = outcome.processed_count(&positions_of(&events)).unwrap();

    assert_eq!(processed, None);
    // Nothing advanced -> the full batch would be redelivered from the front.
    assert_eq!(batch.next_chunk(events.len()).len(), 2);
    assert_eq!(
        batch.next_event().and_then(EventV1::sequence),
        Some(Sequence::assigned(30))
    );
}
