use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures_util::StreamExt;

use super::poll_heads;
use crate::api::Position;
use crate::backend::{
    AppendRequest, Appended, Backend, BackendError, Dedup, Head, LogClaim, Partition, Provisioning,
    TopicPartition, TopicSpec, Wait, Watch,
};
use crate::models::{Event, PartitionRange, TopicSegment};
use crate::{GtsInstanceId, Sequence};

/// Answers `resolve(Latest)` from a map the test moves, after any scripted
/// failures of the partition; counts every call. Nothing else is called.
#[derive(Default)]
struct Heads {
    heads: Mutex<HashMap<u32, i64>>,
    failures: Mutex<HashMap<u32, usize>>,
    resolves: AtomicUsize,
}

impl Heads {
    fn set(&self, partition: u32, sequence: i64) {
        self.heads.lock().expect("lock").insert(partition, sequence);
    }

    /// The next `count` resolves of `partition` fail.
    fn fail(&self, partition: u32, count: usize) {
        self.failures.lock().expect("lock").insert(partition, count);
    }

    fn resolves(&self) -> usize {
        self.resolves.load(Ordering::SeqCst)
    }
}

fn unused() -> BackendError {
    BackendError::Internal("not used by poll_heads".to_owned())
}

#[async_trait]
impl Backend for Heads {
    async fn provision(&self, _: &TopicSpec, _: &LogClaim) -> Result<Provisioning, BackendError> {
        Err(unused())
    }
    async fn append(&self, _: &AppendRequest<'_>) -> Result<Appended, BackendError> {
        Err(unused())
    }
    fn dedup(&self) -> Dedup {
        Dedup::Fenced
    }
    fn max_events_per_append(&self) -> usize {
        1
    }
    async fn read(
        &self,
        _: &TopicPartition,
        _: Sequence,
        _: usize,
        _: Wait,
    ) -> Result<Vec<Event>, BackendError> {
        Err(unused())
    }
    async fn resolve(
        &self,
        at: &TopicPartition,
        position: Position,
    ) -> Result<Sequence, BackendError> {
        assert!(
            matches!(position, Position::Latest),
            "poll_heads resolves Latest only"
        );
        self.resolves.fetch_add(1, Ordering::SeqCst);
        if let Some(left) = self
            .failures
            .lock()
            .expect("lock")
            .get_mut(&at.partition.value())
            .filter(|left| **left > 0)
        {
            *left -= 1;
            return Err(BackendError::unavailable("scripted"));
        }
        let head = self
            .heads
            .lock()
            .expect("lock")
            .get(&at.partition.value())
            .copied()
            .unwrap_or(0);
        Ok(Sequence::assigned(head))
    }
    fn watch(&self) -> Watch<'_> {
        Watch::Poll(Duration::from_millis(10))
    }
    async fn query(
        &self,
        _: &TopicPartition,
        _: PartitionRange,
    ) -> Result<Vec<TopicSegment>, BackendError> {
        Err(unused())
    }
}

fn at(partition: u32) -> TopicPartition {
    TopicPartition::new(
        GtsInstanceId::try_new("gts.cf.core.events.topic.v1~acme.orders.x.topic.v1")
            .expect("valid id"),
        Partition::new(partition),
    )
}

fn head(partition: u32, sequence: i64) -> Head {
    Head {
        at: at(partition),
        sequence: Sequence::assigned(sequence),
    }
}

#[tokio::test]
async fn current_heads_first_then_one_per_rise() {
    let backend = Arc::new(Heads::default());
    backend.set(0, 4);
    let mut stream = poll_heads(backend.clone(), &[at(0), at(1)], Duration::from_millis(10));
    assert_eq!(stream.next().await.map(Result::unwrap), Some(head(0, 4)));
    assert_eq!(stream.next().await.map(Result::unwrap), Some(head(1, 0)));

    backend.set(1, 2);
    assert_eq!(stream.next().await.map(Result::unwrap), Some(head(1, 2)));
}

#[tokio::test]
async fn no_item_without_a_change() {
    let backend = Arc::new(Heads::default());
    let mut stream = poll_heads(backend, &[at(0)], Duration::from_millis(10));
    assert_eq!(stream.next().await.map(Result::unwrap), Some(head(0, 0)));
    let next = tokio::time::timeout(Duration::from_millis(60), stream.next()).await;
    assert!(next.is_err(), "expected no item, got {next:?}");
}

#[tokio::test]
async fn a_zero_poll_interval_still_yields_heads() {
    let backend = Arc::new(Heads::default());
    backend.set(0, 4);
    let mut stream = poll_heads(backend, &[at(0)], Duration::ZERO);
    let next = tokio::time::timeout(Duration::from_secs(1), stream.next())
        .await
        .expect("an item within 1 s");
    assert_eq!(next.map(Result::unwrap), Some(head(0, 4)));
}

#[tokio::test]
async fn a_sub_millisecond_poll_interval_still_yields_heads() {
    let backend = Arc::new(Heads::default());
    backend.set(0, 4);
    let mut stream = poll_heads(backend, &[at(0)], Duration::from_micros(10));
    let next = tokio::time::timeout(Duration::from_secs(1), stream.next())
        .await
        .expect("an item within 1 s");
    assert_eq!(next.map(Result::unwrap), Some(head(0, 4)));
}

#[tokio::test(start_paused = true)]
async fn an_error_item_does_not_end_the_stream() {
    let backend = Arc::new(Heads::default());
    backend.set(0, 3);
    backend.fail(0, 1);
    let mut stream = poll_heads(backend, &[at(0)], Duration::from_millis(10));
    let first = stream.next().await;
    assert!(
        matches!(&first, Some(Err(BackendError::Unavailable { reason, detail })) if reason == "scripted" && detail.is_empty()),
        "got {first:?}"
    );
    assert_eq!(stream.next().await.map(Result::unwrap), Some(head(0, 3)));
    let next = tokio::time::timeout(Duration::from_millis(60), stream.next()).await;
    assert!(next.is_err(), "expected no item, got {next:?}");
}

#[tokio::test(start_paused = true)]
async fn dropping_the_stream_stops_polling() {
    let backend = Arc::new(Heads::default());
    let mut stream = poll_heads(backend.clone(), &[at(0)], Duration::from_millis(10));
    assert_eq!(stream.next().await.map(Result::unwrap), Some(head(0, 0)));
    tokio::time::sleep(Duration::from_millis(35)).await;
    let polling = backend.resolves();

    drop(stream);
    tokio::time::sleep(Duration::from_millis(100)).await;

    assert_eq!((polling, backend.resolves()), (4, 4));
    assert_eq!(
        Arc::strong_count(&backend),
        1,
        "the polling task exited and released its clone of the backend"
    );
}
