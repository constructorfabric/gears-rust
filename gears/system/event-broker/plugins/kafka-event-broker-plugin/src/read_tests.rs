use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use chrono::DateTime;
use event_broker_sdk::backend::{BackendError, Partition, TopicPartition};
use event_broker_sdk::models::{PartitionRange, TopicSegment};
use event_broker_sdk::{GtsInstanceId, Sequence};
use rdkafka::error::KafkaResult;

use crate::clients::{Consumed, Record, Span};
use crate::read::{RecordSource, Scope, query_on};

/// Milliseconds since the epoch of the record at offset 0; the record at
/// offset `n` is stored `n` ms later.
const BASE_MS: i64 = 1_700_000_000_000;

/// A scripted partition: events at the offsets `is_event` accepts, markers at
/// the rest. A read that reaches `stall_at` before its `max` times out there,
/// incomplete. Every span asked for is recorded as `(start, end, max)`.
struct Scripted {
    is_event: fn(i64) -> bool,
    stall_at: i64,
    spans: Mutex<Vec<(i64, i64, usize)>>,
}

impl Scripted {
    fn new(is_event: fn(i64) -> bool) -> Self {
        Self {
            is_event,
            stall_at: i64::MAX,
            spans: Mutex::default(),
        }
    }

    fn stalling_at(mut self, offset: i64) -> Self {
        self.stall_at = offset;
        self
    }

    fn spans(&self) -> Vec<(i64, i64, usize)> {
        self.spans
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl RecordSource for Scripted {
    fn consume(&self, span: Span<'_>) -> impl Future<Output = KafkaResult<Consumed>> + Send {
        self.spans
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((span.start, span.end, span.max));
        let consumed = if span.start >= span.end || span.max == 0 {
            Consumed {
                records: Vec::new(),
                complete: true,
            }
        } else {
            let records: Vec<Record> = (span.start..span.end.min(self.stall_at))
                .filter(|offset| (self.is_event)(*offset))
                .map(|offset| Record {
                    offset,
                    timestamp: Some(BASE_MS + offset),
                    key: None,
                    value: None,
                })
                .take(span.max)
                .collect();
            let complete = records.len() >= span.max || span.end <= self.stall_at;
            Consumed { records, complete }
        };
        std::future::ready(Ok(consumed))
    }
}

const KAFKA_TOPIC: &str = "eb.orders";

fn at() -> TopicPartition {
    TopicPartition::new(
        GtsInstanceId::try_new("gts.cf.core.events.topic.v1~acme.orders.x.topic.v1")
            .expect("valid id"),
        Partition::new(0),
    )
}

fn scope(high: i64) -> Scope<'static> {
    Scope {
        topic: KAFKA_TOPIC,
        partition: 0,
        low: 0,
        high,
        timeout: Duration::from_secs(5),
    }
}

const EVERYTHING: PartitionRange = PartitionRange {
    start_offset: None,
    end_offset: None,
    limit: u32::MAX,
};

/// The one segment a query over `[first, last]` (offsets) of a partition
/// with watermark `high` answers.
fn segment(first: i64, last: i64, high: i64) -> TopicSegment {
    TopicSegment {
        topic: "gts.cf.core.events.topic.v1~acme.orders.x.topic.v1".to_owned(),
        partition: 0,
        start_sequence: Sequence::assigned(first + 1),
        end_sequence: Sequence::assigned(last + 1),
        start_time: DateTime::from_timestamp_millis(BASE_MS + first),
        end_time: DateTime::from_timestamp_millis(BASE_MS + last),
        segments: vec![serde_json::json!({
            "kafka_topic": KAFKA_TOPIC,
            "kafka_partition": 0,
            "log_start_offset": 0,
            "last_stable_offset": high,
        })],
    }
}

fn shown(found: &Result<Vec<TopicSegment>, BackendError>) -> String {
    format!("{found:?}")
}

#[tokio::test]
async fn a_query_cut_by_the_timeout_is_an_error_not_a_partial_envelope() {
    let source = Scripted::new(|offset| offset % 2 == 0 && offset < 10).stalling_at(5);
    let found = query_on(&source, &at(), &scope(10), &EVERYTHING).await;
    let Err(err) = &found else {
        panic!("a query cut short must fail: {}", shown(&found));
    };
    assert_eq!(
        format!("{err:?}"),
        r#"Unavailable { reason: "query: kafka error (OperationTimedOut)", detail: "" }"#
    );
}

#[tokio::test]
async fn an_unbounded_query_reads_one_record_forward_and_backward_windows_only() {
    // Events at even offsets, transaction markers at odd ones.
    let source = Scripted::new(|offset| offset % 2 == 0 && offset < 1_000_000);
    let found = query_on(&source, &at(), &scope(1_000_000), &EVERYTHING).await;
    assert_eq!(
        source.spans(),
        vec![(0, 1_000_000, 1), (999_996, 1_000_000, 4)]
    );
    assert_eq!(
        shown(&found),
        shown(&Ok(vec![segment(0, 999_998, 1_000_000)]))
    );
}

#[tokio::test]
async fn a_query_over_markers_only_tail_finds_the_last_event() {
    // Events at even offsets up to 78; offsets 80 to 99 are all markers.
    let source = Scripted::new(|offset| offset % 2 == 0 && offset < 80);
    let found = query_on(&source, &at(), &scope(100), &EVERYTHING).await;
    assert_eq!(
        source.spans(),
        vec![(0, 100, 1), (96, 100, 4), (36, 100, 64)]
    );
    assert_eq!(shown(&found), shown(&Ok(vec![segment(0, 78, 100)])));
}

#[tokio::test]
async fn a_finite_limit_reads_forward_at_most_limit_records() {
    // Sequence 3 is offset 2.
    let source = Scripted::new(|offset| offset % 2 == 0 && offset < 20);
    let range = PartitionRange {
        start_offset: Some(Sequence::assigned(3)),
        end_offset: None,
        limit: 3,
    };
    let found = query_on(&source, &at(), &scope(20), &range).await;
    assert_eq!(source.spans(), vec![(2, 20, 1), (2, 20, 3)]);
    assert_eq!(shown(&found), shown(&Ok(vec![segment(2, 6, 20)])));
}

#[tokio::test]
async fn a_zero_limit_query_is_empty() {
    let source = Scripted::new(|offset| offset < 10);
    let range = PartitionRange {
        start_offset: None,
        end_offset: None,
        limit: 0,
    };
    let found = query_on(&source, &at(), &scope(10), &range).await;
    assert_eq!(shown(&found), shown(&Ok(vec![])));
    assert_eq!(source.spans(), vec![]);
}
