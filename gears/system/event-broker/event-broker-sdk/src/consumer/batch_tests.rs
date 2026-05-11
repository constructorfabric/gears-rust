use chrono::Utc;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

use super::{
    BatchEventHandler, CommitHandle, EventBatch, EventHandler, HandlerOutcome, RawEvent,
    SingleEventHandlerAdapter,
};
use crate::error::ConsumerError;

fn raw_event(offset: i64) -> RawEvent {
    RawEvent {
        id: Uuid::new_v4(),
        type_id: "gts.cf.core.events.event.v1~orders.OrderCreated".to_owned(),
        topic: "gts.cf.core.events.topic.v1~orders.v1".to_owned(),
        tenant_id: Uuid::nil(),
        subject: format!("order-{offset}"),
        subject_type: "order".to_owned(),
        partition: 7,
        sequence: offset,
        offset,
        occurred_at: Utc::now(),
        sequence_time: Utc::now(),
        trace_parent: None,
        data: serde_json::json!({ "offset": offset }),
    }
}

#[test]
fn event_batch_reports_empty_state() {
    let events = Vec::new();
    let batch = EventBatch::new(&events);

    assert!(batch.is_empty());
    assert_eq!(batch.len(), 0);
    assert_eq!(batch.processed(), 0);
    assert!(batch.next_event().is_none());
    assert!(batch.next_chunk(10).is_empty());
}

#[test]
fn event_batch_advances_one_event_at_a_time() {
    let events = vec![raw_event(10), raw_event(11)];
    let mut batch = EventBatch::new(&events);

    assert_eq!(batch.len(), 2);
    assert_eq!(batch.processed(), 0);
    assert_eq!(batch.next_event().map(|event| event.offset), Some(10));

    batch.ack();

    assert_eq!(batch.processed(), 1);
    assert_eq!(batch.next_event().map(|event| event.offset), Some(11));

    batch.ack();

    assert_eq!(batch.processed(), 2);
    assert!(batch.next_event().is_none());
}

#[test]
fn event_batch_chunks_are_cursor_scoped_and_clamped() {
    let events = vec![raw_event(20), raw_event(21), raw_event(22)];
    let mut batch = EventBatch::new(&events);

    assert_eq!(
        batch
            .next_chunk(2)
            .iter()
            .map(|event| event.offset)
            .collect::<Vec<_>>(),
        vec![20, 21]
    );

    batch.ack_chunk(2);

    assert_eq!(batch.processed(), 2);
    assert_eq!(
        batch
            .next_chunk(10)
            .iter()
            .map(|event| event.offset)
            .collect::<Vec<_>>(),
        vec![22]
    );

    batch.ack_chunk(10);

    assert_eq!(batch.processed(), 3);
    assert!(batch.next_chunk(1).is_empty());
}

#[test]
fn event_batch_reject_counts_current_event_as_processed() {
    let events = vec![raw_event(30), raw_event(31)];
    let mut batch = EventBatch::new(&events);

    batch.reject("bad payload");

    assert_eq!(batch.processed(), 1);
    assert_eq!(batch.next_event().map(|event| event.offset), Some(31));
}

struct RecordingSingleHandler {
    calls: Arc<Mutex<Vec<i64>>>,
    outcome: HandlerOutcome,
}

#[async_trait::async_trait]
impl EventHandler<CommitHandle, HandlerOutcome> for RecordingSingleHandler {
    async fn handle(
        &self,
        event: RawEvent,
        _attempts: u16,
        _commit: CommitHandle,
    ) -> Result<HandlerOutcome, ConsumerError> {
        self.calls.lock().unwrap().push(event.offset);
        Ok(self.outcome.clone())
    }
}

#[tokio::test]
async fn single_handler_adapter_acks_successful_event_batch() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let handler = Arc::new(RecordingSingleHandler {
        calls: calls.clone(),
        outcome: HandlerOutcome::Success,
    });
    let adapter = SingleEventHandlerAdapter::new(handler);
    let events = vec![raw_event(40)];
    let mut batch = EventBatch::new(&events);

    let outcome = adapter
        .handle_batch(&mut batch, 1, CommitHandle::new(7, 40))
        .await
        .unwrap();

    assert!(matches!(outcome, HandlerOutcome::Success));
    assert_eq!(batch.processed(), 1);
    assert!(batch.next_event().is_none());
    assert_eq!(*calls.lock().unwrap(), vec![40]);
}

#[tokio::test]
async fn single_handler_adapter_leaves_retry_unprocessed() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let handler = Arc::new(RecordingSingleHandler {
        calls: calls.clone(),
        outcome: HandlerOutcome::Retry {
            reason: "not yet".to_owned(),
        },
    });
    let adapter = SingleEventHandlerAdapter::new(handler);
    let events = vec![raw_event(41)];
    let mut batch = EventBatch::new(&events);

    let outcome = adapter
        .handle_batch(&mut batch, 1, CommitHandle::new(7, 41))
        .await
        .unwrap();

    assert!(matches!(outcome, HandlerOutcome::Retry { .. }));
    assert_eq!(batch.processed(), 0);
    assert_eq!(batch.next_event().map(|event| event.offset), Some(41));
    assert_eq!(*calls.lock().unwrap(), vec![41]);
}
