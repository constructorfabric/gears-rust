//! The heads of the partitions this instance serves, from the backend's own
//! watch (feature 0007 §4.2).
//!
//! The loader fetches a tail only once a head above it is known
//! (`super::scheduler`), so a reader parked at the frontier costs no fetch and
//! holds no permit while it waits. One watch per topic, over the topic's live
//! partitions: reopened when that set changes, and resubscribed with backoff
//! when it fails or ends. A backend that pushes heads is watched natively; one
//! that cannot is polled through `resolve(Latest)` behind the same stream.
//!
//! Heads are a hint, not the authority: a lost or late head costs a late wake,
//! and every reopen starts with the current head of each partition.

use std::collections::{BTreeSet, HashMap};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use tokio::task::JoinHandle;
use tokio_stream::{Stream, StreamExt};
use tokio_util::sync::CancellationToken;
use toolkit_gts::GtsInstanceId;

use crate::domain::model::Sequence;
use crate::domain::streaming::source::PartitionKey;

use super::source::SourceError;
use super::topics::TopicManager;

/// How often the watched partition sets are compared with the live ones.
pub const DEFAULT_RECONCILE: Duration = Duration::from_millis(50);

/// The first wait before a failed or ended watch is reopened; doubles per
/// failure in a row.
const RESUBSCRIBE_FLOOR: Duration = Duration::from_millis(250);

/// The longest wait before a watch is reopened.
const RESUBSCRIBE_CEILING: Duration = Duration::from_secs(30);

/// One partition's head, as the watch reported it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartitionHead {
    pub key: PartitionKey,
    pub head: Sequence,
}

pub type HeadStream = Pin<Box<dyn Stream<Item = Result<PartitionHead, SourceError>> + Send>>;

/// What the head watcher needs from a backend.
pub trait HeadSource: Send + Sync {
    /// First the current head of each named partition of `topic`, then every
    /// change. Errors arrive as stream items; the caller reopens the watch.
    fn watch(
        &self,
        topic: &GtsInstanceId,
        partitions: &[i32],
    ) -> impl Future<Output = Result<HeadStream, SourceError>> + Send;
}

pub struct HeadWatcher<H> {
    source: Arc<H>,
    topics: Arc<TopicManager>,
    reconcile: Duration,
}

impl<H: HeadSource + 'static> HeadWatcher<H> {
    /// Three arguments, all mutually distinguishable.
    #[must_use]
    pub fn new(source: Arc<H>, topics: Arc<TopicManager>, reconcile: Duration) -> Self {
        Self {
            source,
            topics,
            reconcile,
        }
    }

    /// Runs until cancelled. Every watch it opened ends with it.
    pub async fn run(self, shutdown: CancellationToken) {
        let mut watches: HashMap<GtsInstanceId, TopicWatch> = HashMap::new();
        loop {
            self.reconcile(&mut watches);
            tokio::select! {
                () = tokio::time::sleep(self.reconcile) => {}
                () = shutdown.cancelled() => return,
            }
        }
    }

    /// Closes the watch of every topic whose live partitions changed or went,
    /// and opens one for every topic without a watch.
    fn reconcile(&self, watches: &mut HashMap<GtsInstanceId, TopicWatch>) {
        let wanted = self.topics.live_by_topic();
        watches.retain(|topic, watch| wanted.get(topic) == Some(&watch.partitions));
        for (topic, partitions) in wanted {
            if watches.contains_key(&topic) {
                continue;
            }
            let task = tokio::spawn(follow(
                Arc::clone(&self.source),
                Arc::clone(&self.topics),
                topic.clone(),
                partitions.iter().copied().collect(),
            ));
            watches.insert(topic, TopicWatch { partitions, task });
        }
    }
}

/// One topic's open watch. Dropping it ends the watch.
struct TopicWatch {
    partitions: BTreeSet<i32>,
    task: JoinHandle<()>,
}

impl Drop for TopicWatch {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Follows one topic's heads until aborted, reopening the watch with backoff
/// whenever it fails, ends, or cannot be opened.
async fn follow<H: HeadSource>(
    source: Arc<H>,
    topics: Arc<TopicManager>,
    topic: GtsInstanceId,
    partitions: Vec<i32>,
) {
    let mut backoff = RESUBSCRIBE_FLOOR;
    loop {
        if watch_once(source.as_ref(), &topics, &topic, &partitions).await {
            backoff = RESUBSCRIBE_FLOOR;
        }
        tokio::time::sleep(jittered(backoff)).await;
        backoff = backoff.saturating_mul(2).min(RESUBSCRIBE_CEILING);
    }
}

/// Opens one watch and records its heads until it fails or ends; `true` when
/// it reported at least one, so the next failure starts the backoff over.
async fn watch_once<H: HeadSource>(
    source: &H,
    topics: &TopicManager,
    topic: &GtsInstanceId,
    partitions: &[i32],
) -> bool {
    let mut heads = match source.watch(topic, partitions).await {
        Ok(heads) => heads,
        Err(error) => {
            tracing::warn!(topic = %topic, ?error, "head watch could not be opened");
            return false;
        }
    };
    let mut reported = false;
    while let Some(item) = heads.next().await {
        match item {
            Ok(head) => {
                reported = true;
                topics.publish_head(&head.key, head.head);
            }
            Err(error) => {
                tracing::warn!(topic = %topic, ?error, "head watch failed; reopening");
                break;
            }
        }
    }
    reported
}

/// Somewhere in the upper half of `backoff`, so the watches of many topics
/// failing together do not reopen together.
fn jittered(backoff: Duration) -> Duration {
    backoff.mul_f64(rand::random_range(0.5..=1.0))
}
