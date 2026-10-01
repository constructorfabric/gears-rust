//! The [`Backend`] over one `PostgreSQL` registry entry.

use std::sync::Arc;

use async_trait::async_trait;
use event_broker_sdk::backend::{
    AppendRequest, Appended, Backend, BackendError, Dedup, Head, HeadWatch, LogClaim, Provisioning,
    TopicPartition, TopicSpec, Wait, Watch,
};
use event_broker_sdk::{Event, PartitionRange, Position, Sequence, TopicSegment};
use futures_util::stream::BoxStream;

use crate::shared::Shared;
use crate::{append, provision, read};

/// A backend over one database. Instances built from the same settings share
/// nothing in memory; they meet in the database, as instances in different
/// processes do.
pub struct PostgresBackend {
    shared: Arc<Shared>,
}

impl PostgresBackend {
    pub(crate) fn new(shared: Arc<Shared>) -> Self {
        Self { shared }
    }
}

#[async_trait]
impl Backend for PostgresBackend {
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

    fn watch(&self) -> Watch<'_> {
        Watch::Native(self)
    }

    async fn query(
        &self,
        at: &TopicPartition,
        range: PartitionRange,
    ) -> Result<Vec<TopicSegment>, BackendError> {
        read::query(&self.shared, at, &range).await
    }
}

impl HeadWatch for PostgresBackend {
    fn watch(
        &self,
        partitions: &[TopicPartition],
    ) -> BoxStream<'static, Result<Head, BackendError>> {
        let shared = Arc::clone(&self.shared);
        let named = partitions.to_vec();
        self.shared.heads.watch(partitions, async move {
            read::current_heads(&shared, &named).await
        })
    }
}
