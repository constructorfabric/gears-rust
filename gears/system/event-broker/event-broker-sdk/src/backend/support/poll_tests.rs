use std::collections::HashMap;
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

/// Answers `resolve(Latest)` from a map the test moves; nothing else is called.
#[derive(Default)]
struct Heads(Mutex<HashMap<u32, i64>>);

impl Heads {
    fn set(&self, partition: u32, sequence: i64) {
        self.0.lock().expect("lock").insert(partition, sequence);
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
        let head = self
            .0
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
