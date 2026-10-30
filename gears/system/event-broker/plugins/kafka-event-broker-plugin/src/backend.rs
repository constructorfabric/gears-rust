//! The [`Backend`] over one Kafka registry entry.

use std::sync::Arc;

use async_trait::async_trait;
use event_broker_sdk::backend::{
    AppendRequest, Appended, Backend, BackendError, Dedup, LogClaim, Provisioning, TopicPartition,
    TopicSpec, Wait, Watch,
};
use event_broker_sdk::{Event, PartitionRange, Position, Sequence, TopicSegment};
use tokio_util::sync::CancellationToken;

use crate::shared::Shared;
use crate::{append, provision, read};

/// A backend over one Kafka cluster and topic prefix. Instances built from
/// the same settings share nothing in memory; they meet in the cluster, as
/// instances in different processes do.
pub struct KafkaBackend {
    shared: Arc<Shared>,
}

impl KafkaBackend {
    /// A backend over the registry entry's shared state; nothing connects
    /// until an operation needs the cluster.
    pub(crate) fn new(shared: Arc<Shared>) -> Self {
        Self { shared }
    }
}

#[async_trait]
impl Backend for KafkaBackend {
    async fn provision(
        &self,
        topic: &TopicSpec,
        claim: &LogClaim,
    ) -> Result<Provisioning, BackendError> {
        provision::provision(&self.shared, topic, claim).await
    }

    async fn append(&self, request: &AppendRequest<'_>) -> Result<Appended, BackendError> {
        append::append(&self.shared, request).await
    }

    fn dedup(&self) -> Dedup {
        Dedup::Fenced
    }

    fn max_events_per_append(&self) -> usize {
        append::MAX_EVENTS_PER_APPEND
    }

    async fn read(
        &self,
        at: &TopicPartition,
        offset: Sequence,
        max_events: usize,
        wait: Wait,
    ) -> Result<Vec<Event>, BackendError> {
        read::read(&self.shared, at, offset, max_events, wait).await
    }

    async fn resolve(
        &self,
        at: &TopicPartition,
        position: Position,
    ) -> Result<Sequence, BackendError> {
        read::resolve(&self.shared, at, &position).await
    }

    /// Kafka pushes nothing a client could watch heads by; the gear polls
    /// `resolve(Latest)`, which costs one offsets request per partition while
    /// the head stands still (`docs/DESIGN.md` §5.6).
    fn watch(&self) -> Watch<'_> {
        Watch::Poll(self.shared.settings.poll_interval)
    }

    async fn query(
        &self,
        at: &TopicPartition,
        range: PartitionRange,
    ) -> Result<Vec<TopicSegment>, BackendError> {
        read::query(&self.shared, at, &range).await
    }

    /// Nothing to start: retention is the brokers', and clients are created
    /// on first need, so an unreachable cluster is `Unavailable` per
    /// operation.
    async fn start(&self) -> Result<(), BackendError> {
        Ok(())
    }

    /// Waits for the appends in flight until `deadline`, then drops every
    /// client; every operation after it is `Unavailable`.
    async fn stop(&self, deadline: CancellationToken) {
        self.shared.stop(&deadline).await;
    }
}
