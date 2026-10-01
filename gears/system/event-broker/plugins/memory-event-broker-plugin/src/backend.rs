//! The [`Backend`] over one memory store.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use event_broker_sdk::backend::{
    AppendRequest, Appended, Backend, BackendError, Dedup, Head, HeadWatch, LogClaim, Provisioning,
    TopicPartition, TopicSpec, Wait, Watch,
};
use event_broker_sdk::{Event, PartitionRange, Position, Sequence, TopicSegment};
use futures_util::stream::BoxStream;
use tokio_util::sync::CancellationToken;

use crate::store::Store;

const MAX_EVENTS_PER_APPEND: usize = 1000;

/// A backend over one memory store. Clones, and instances the provider builds
/// for the same store, share its events, fence state and heads.
#[derive(Clone, Default)]
pub struct MemoryBackend {
    store: Arc<Store>,
}

impl MemoryBackend {
    /// A backend over a fresh store of its own, with no retention worker:
    /// what a test that drives retention itself wants.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn over(store: Arc<Store>) -> Self {
        Self { store }
    }

    /// Removes every event of `at` up to and including `through`, as
    /// retention would, leaving the head where it is. For tests that need a
    /// retention floor without waiting for one.
    ///
    /// # Errors
    /// [`BackendError::NotProvisioned`] for a topic never provisioned here;
    /// [`BackendError::PartitionNotFound`] for a partition it does not have.
    pub fn truncate(&self, at: &TopicPartition, through: Sequence) -> Result<(), BackendError> {
        self.store.truncate(at, through)
    }

    /// Runs one retention pass now, as the worker does on its interval.
    pub fn enforce_retention(&self) {
        self.store.enforce_retention(chrono::Utc::now());
    }

    /// Runs a retention pass every `interval` until `stop` is cancelled.
    pub fn spawn_retention(&self, interval: Duration, stop: CancellationToken) {
        let store = Arc::clone(&self.store);
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tokio::select! {
                    () = stop.cancelled() => return,
                    _ = ticker.tick() => store.enforce_retention(chrono::Utc::now()),
                }
            }
        });
    }
}

#[async_trait]
impl Backend for MemoryBackend {
    async fn provision(
        &self,
        topic: &TopicSpec,
        claim: &LogClaim,
    ) -> Result<Provisioning, BackendError> {
        self.store.provision(topic, claim)
    }

    async fn append(&self, request: &AppendRequest<'_>) -> Result<Appended, BackendError> {
        if request.events.len() > MAX_EVENTS_PER_APPEND {
            return Err(BackendError::append_failed(format!(
                "an append carries at most {MAX_EVENTS_PER_APPEND} events"
            )));
        }
        let appended = self.store.append(request, chrono::Utc::now())?;
        if let Appended::Heads(heads) = &appended {
            for head in heads {
                self.store.heads.publish(head.clone());
            }
        }
        Ok(appended)
    }

    fn dedup(&self) -> Dedup {
        Dedup::Fenced
    }

    fn max_events_per_append(&self) -> usize {
        MAX_EVENTS_PER_APPEND
    }

    async fn read(
        &self,
        at: &TopicPartition,
        offset: Sequence,
        max_events: usize,
        wait: Wait,
    ) -> Result<Vec<Event>, BackendError> {
        match wait {
            Wait::No => self.store.read(at, offset, max_events),
            // Every writer of this store is in this process and publishes its
            // heads here, so the wait needs no poll of its own.
            Wait::For(wait) => {
                self.store
                    .heads
                    .read_waiting(at, offset, wait, wait, || async {
                        self.store.read(at, offset, max_events)
                    })
                    .await
            }
        }
    }

    async fn resolve(
        &self,
        at: &TopicPartition,
        position: Position,
    ) -> Result<Sequence, BackendError> {
        self.store.resolve(at, &position)
    }

    fn watch(&self) -> Watch<'_> {
        Watch::Native(self)
    }

    async fn query(
        &self,
        at: &TopicPartition,
        range: PartitionRange,
    ) -> Result<Vec<TopicSegment>, BackendError> {
        self.store.query(at, &range)
    }
}

impl HeadWatch for MemoryBackend {
    fn watch(
        &self,
        partitions: &[TopicPartition],
    ) -> BoxStream<'static, Result<Head, BackendError>> {
        let store = Arc::clone(&self.store);
        let named = partitions.to_vec();
        self.store
            .heads
            .watch(partitions, async move { store.heads(&named) })
    }
}
