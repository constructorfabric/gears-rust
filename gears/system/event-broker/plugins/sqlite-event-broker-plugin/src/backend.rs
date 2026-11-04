//! The [`Backend`] over one `SQLite` registry entry.

use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use event_broker_sdk::backend::{
    AppendRequest, Appended, Backend, BackendError, Dedup, Head, HeadWatch, LogClaim, Provisioning,
    TopicPartition, TopicSpec, Wait, Watch,
};
use event_broker_sdk::{Event, PartitionRange, Position, Sequence, TopicSegment};
use futures_util::stream::{self, BoxStream, StreamExt};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::shared::Shared;
use crate::{append, provision, read, startup};

/// A backend over one database file. Instances over the same file in one
/// process share its hub (`docs/DESIGN.md` §3.2); each has its own pool and
/// its own workers.
pub struct SqliteBackend {
    shared: Arc<Shared>,
    /// The startup task and the sweep, from `start` until `stop`.
    workers: Mutex<Option<Workers>>,
}

struct Workers {
    stop: CancellationToken,
    tasks: TaskTracker,
}

impl SqliteBackend {
    /// A backend over `shared`, its workers not started yet.
    pub(crate) fn new(shared: Arc<Shared>) -> Self {
        Self {
            shared,
            workers: Mutex::default(),
        }
    }
}

#[async_trait]
impl Backend for SqliteBackend {
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

    /// Starts the startup task and returns at once: a file that cannot be
    /// opened is `Unavailable` per operation (`docs/DESIGN.md` §3.1).
    async fn start(&self) -> Result<(), BackendError> {
        let mut workers = self.workers.lock().unwrap_or_else(PoisonError::into_inner);
        if workers.is_none() {
            let stop = CancellationToken::new();
            let tasks = TaskTracker::new();
            startup::spawn(Arc::clone(&self.shared), stop.clone(), &tasks);
            *workers = Some(Workers { stop, tasks });
        }
        Ok(())
    }

    /// Ends the startup task and the sweep, waits for them, and closes the
    /// pool; every operation after it is `Unavailable`. Other instances over
    /// the file keep their own pools and workers.
    async fn stop(&self, deadline: CancellationToken) {
        let workers = self
            .workers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        let Some(Workers { stop, tasks }) = workers else {
            return;
        };
        stop.cancel();
        tasks.close();
        tokio::select! {
            () = tasks.wait() => {}
            () = deadline.cancelled() => return,
        }
        if let Ok(opened) = self.shared.opened() {
            tokio::select! {
                () = opened.pool.close() => {}
                () = deadline.cancelled() => {}
            }
        }
    }
}

impl HeadWatch for SqliteBackend {
    fn watch(
        &self,
        partitions: &[TopicPartition],
    ) -> BoxStream<'static, Result<Head, BackendError>> {
        let opened = match self.shared.opened() {
            Ok(opened) => opened,
            Err(err) => return stream::once(async move { Err(err) }).boxed(),
        };
        let shared = Arc::clone(&self.shared);
        let named = partitions.to_vec();
        opened.hub.heads.watch(partitions, async move {
            read::current_heads(&shared, &named).await
        })
    }
}
