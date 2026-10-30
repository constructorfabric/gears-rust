//! The [`Backend`] over one `MySQL` or `MariaDB` registry entry.

use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use event_broker_sdk::backend::{
    AppendRequest, Appended, Backend, BackendError, Dedup, Head, HeadWatch, LogClaim, Provisioning,
    TopicPartition, TopicSpec, Wait, Watch,
};
use event_broker_sdk::{Event, PartitionRange, Position, Sequence, TopicSegment};
use futures_util::StreamExt;
use futures_util::stream::BoxStream;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::heads::WatchGuard;
use crate::shared::{self, Shared};
use crate::{append, provision, read, startup};

/// A backend over one database. Instances built from the same settings share
/// nothing in memory; they meet in the database, as instances in different
/// processes do.
pub struct MysqlBackend {
    shared: Arc<Shared>,
    /// The startup, head poll and maintenance tasks, from `start` until
    /// `stop`.
    workers: Mutex<Option<Workers>>,
}

struct Workers {
    stop: CancellationToken,
    tasks: TaskTracker,
}

impl MysqlBackend {
    /// A backend over the registry entry's shared state; nothing runs until
    /// `start`.
    pub(crate) fn new(shared: Arc<Shared>) -> Self {
        Self {
            shared,
            workers: Mutex::default(),
        }
    }
}

#[async_trait]
impl Backend for MysqlBackend {
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

    /// Starts the startup task and returns at once: it connects, checks and
    /// migrates in the background, so an unreachable server is
    /// `Unavailable` per operation (`docs/DESIGN.md` §3.1).
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

    /// Ends the startup, head poll and maintenance tasks, waits for them, and
    /// closes both pools; every operation after it is `Unavailable`.
    async fn stop(&self, deadline: CancellationToken) {
        let workers = self
            .workers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        let Some(Workers { stop, tasks }) = workers else {
            return;
        };
        let pools = self.shared.stop();
        stop.cancel();
        tasks.close();
        tokio::select! {
            () = tasks.wait() => {}
            () = deadline.cancelled() => {}
        }
        // Out of time or not, the pools close: a task of their own finishes
        // what the deadline cuts short.
        let closing = shared::close_in_background(pools);
        tokio::select! {
            _ = closing => {}
            () = deadline.cancelled() => {}
        }
    }
}

impl HeadWatch for MysqlBackend {
    /// The current heads, then every head this instance appends or its head
    /// poll reads, for as long as the stream lives (`docs/DESIGN.md` §4.7).
    fn watch(
        &self,
        partitions: &[TopicPartition],
    ) -> BoxStream<'static, Result<Head, BackendError>> {
        let shared = Arc::clone(&self.shared);
        let named = partitions.to_vec();
        // Holds the watch's partitions in the head poll for as long as the
        // stream lives; filled when the stream first runs.
        let held: Arc<Mutex<Option<WatchGuard>>> = Arc::default();
        let fill = Arc::clone(&held);
        let heads = self.shared.heads.watch(partitions, async move {
            let keys = read::keys_of(&shared, &named).await?;
            let guard = shared
                .watched
                .enter(keys.into_iter().zip(named.iter().cloned()).collect());
            *fill.lock().unwrap_or_else(PoisonError::into_inner) = Some(guard);
            read::current_heads(&shared, &named).await
        });
        heads
            .map(move |head| {
                let _held = &held;
                head
            })
            .boxed()
    }
}
