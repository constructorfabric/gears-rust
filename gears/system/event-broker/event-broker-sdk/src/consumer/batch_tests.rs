use chrono::Utc;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

use super::{
    BatchHandlerOutcome, ConsumerHandler, EventBatch, HandlerOutcome, RawEvent, SingleEventHandler,
    SingleEventHandlerAdapter,
};
use crate::consumer::types::PartitionFrontier;
use crate::error::ConsumerError;

fn raw_event(offset: i64) -> RawEvent {
    RawEvent {
        id: Uuid::new_v4(),
        type_id: "gts.cf.core.events.event.v1~orders.OrderCreated".to_owned(),
        topic: "gts.cf.core.events.topic.v1~orders.v1".to_owned(),
        tenant_id: Uuid::nil(),
        subject: format!("order-{offset}"),
        subject_type: "order".to_owned(),
        partition_key: None,
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
    assert!(batch.next_event().is_none());
    assert!(batch.next_chunk(10).is_empty());
    assert!(batch.iter().next().is_none());
}

#[test]
fn event_batch_reads_without_mutating_progress() {
    let events = vec![raw_event(10), raw_event(11), raw_event(12)];
    let batch = EventBatch::new(&events);

    assert_eq!(batch.len(), 3);
    assert_eq!(batch.next_event().map(|event| event.offset), Some(10));
    assert_eq!(
        batch
            .next_chunk(2)
            .iter()
            .map(|event| event.offset)
            .collect::<Vec<_>>(),
        vec![10, 11]
    );
    assert_eq!(
        batch.iter().map(|event| event.offset).collect::<Vec<_>>(),
        vec![10, 11, 12]
    );
    assert_eq!(batch.next_event().map(|event| event.offset), Some(10));
}

#[test]
fn partition_frontier_advances_through_contiguous_acked_offsets() {
    let mut frontier = PartitionFrontier::new(9);

    assert_eq!(frontier.mark_acked(10), 10);
    assert_eq!(frontier.mark_acked(11), 11);
    assert_eq!(frontier.committed(), 11);
}

#[test]
fn partition_frontier_blocks_on_skipped_or_failed_offset() {
    let mut frontier = PartitionFrontier::new(19);

    assert_eq!(frontier.mark_acked(20), 20);
    assert_eq!(frontier.mark_acked(22), 20);
    assert_eq!(frontier.committed(), 20);

    assert_eq!(frontier.mark_acked(21), 22);
    assert_eq!(frontier.committed(), 22);
}

#[test]
fn partition_frontier_ignores_duplicate_or_stale_offsets() {
    let mut frontier = PartitionFrontier::new(39);

    assert_eq!(frontier.mark_acked(40), 40);
    assert_eq!(frontier.mark_acked(38), 40);
    assert_eq!(frontier.committed(), 40);
}

struct RecordingSingleHandler {
    calls: Arc<Mutex<Vec<i64>>>,
    outcome: HandlerOutcome,
}

#[async_trait::async_trait]
impl SingleEventHandler for RecordingSingleHandler {
    async fn handle(
        &self,
        event: RawEvent,
        _attempts: u16,
    ) -> Result<HandlerOutcome, ConsumerError> {
        self.calls.lock().unwrap().push(event.offset);
        Ok(self.outcome.clone())
    }
}

#[tokio::test]
async fn single_handler_adapter_reports_one_event_processed_on_success() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let handler = Arc::new(RecordingSingleHandler {
        calls: calls.clone(),
        outcome: HandlerOutcome::Success,
    });
    let adapter = SingleEventHandlerAdapter::new(handler);
    let events = vec![raw_event(40)];
    let batch = EventBatch::new(&events);

    let outcome = adapter.handle_batch(&batch, 1).await.unwrap();

    assert!(matches!(
        outcome,
        BatchHandlerOutcome::AdvanceThrough { offset: 40 }
    ));
    assert_eq!(batch.next_event().map(|event| event.offset), Some(40));
    assert_eq!(*calls.lock().unwrap(), vec![40]);
}

#[tokio::test]
async fn single_handler_adapter_reports_retry_without_progress() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let handler = Arc::new(RecordingSingleHandler {
        calls: calls.clone(),
        outcome: HandlerOutcome::Retry {
            reason: "not yet".to_owned(),
        },
    });
    let adapter = SingleEventHandlerAdapter::new(handler);
    let events = vec![raw_event(41)];
    let batch = EventBatch::new(&events);

    let outcome = adapter.handle_batch(&batch, 1).await.unwrap();

    assert!(matches!(outcome, BatchHandlerOutcome::Retry { .. }));
    assert_eq!(batch.next_event().map(|event| event.offset), Some(41));
    assert_eq!(*calls.lock().unwrap(), vec![41]);
}
