//! A [`Backend`] whose `append` never completes, for exercising the
//! synchronous-publish timeout (`Prefer: wait=N` -> `202`).

use std::sync::Arc;

use async_trait::async_trait;
use event_broker_sdk::api::Position;
use event_broker_sdk::backend::{
    AppendRequest, Appended, Backend, BackendError, Dedup, LogClaim, Provisioning, TopicPartition,
    TopicSpec, Wait, Watch,
};
use event_broker_sdk::models::{Event, PartitionRange, TopicSegment};
use event_broker_sdk::sequence::Sequence;
use tokio_util::sync::CancellationToken;

/// Delegates everything to a real backend except `append`, which never
/// completes - so a `Prefer: wait=N` publish deterministically times out to
/// `202` (the event is admitted and enqueued, just never stored).
pub struct NeverStoringBackend {
    inner: Arc<dyn Backend>,
}

impl NeverStoringBackend {
    #[must_use]
    pub fn new(inner: Arc<dyn Backend>) -> Self {
        Self { inner }
    }
}

#[async_trait]
impl Backend for NeverStoringBackend {
    async fn provision(
        &self,
        topic: &TopicSpec,
        claim: &LogClaim,
    ) -> Result<Provisioning, BackendError> {
        self.inner.provision(topic, claim).await
    }

    async fn append(&self, _request: &AppendRequest<'_>) -> Result<Appended, BackendError> {
        // Never resolves: the outbox worker blocks here, so the event is never
        // stored and a synchronous publish waiting on it only ever times out.
        std::future::pending::<Result<Appended, BackendError>>().await
    }

    fn dedup(&self) -> Dedup {
        self.inner.dedup()
    }

    fn max_events_per_append(&self) -> usize {
        self.inner.max_events_per_append()
    }

    async fn read(
        &self,
        at: &TopicPartition,
        offset: Sequence,
        max_events: usize,
        wait: Wait,
    ) -> Result<Vec<Event>, BackendError> {
        self.inner.read(at, offset, max_events, wait).await
    }

    async fn resolve(
        &self,
        at: &TopicPartition,
        position: Position,
    ) -> Result<Sequence, BackendError> {
        self.inner.resolve(at, position).await
    }

    fn watch(&self) -> Watch<'_> {
        self.inner.watch()
    }

    async fn query(
        &self,
        at: &TopicPartition,
        range: PartitionRange,
    ) -> Result<Vec<TopicSegment>, BackendError> {
        self.inner.query(at, range).await
    }

    async fn start(&self) -> Result<(), BackendError> {
        self.inner.start().await
    }

    async fn stop(&self, deadline: CancellationToken) {
        self.inner.stop(deadline).await;
    }
}
