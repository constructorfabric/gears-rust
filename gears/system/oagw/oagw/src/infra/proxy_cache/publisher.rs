//! The publisher: appends this replica's changes to the cluster change log
//! (ADR-0020).
//!
//! One task per replica. Each write hands it its change; the task drains
//! everything queued into one append, so a burst, or a registry reconcile
//! pass whose writes do not wait for their appends, is one cluster write. An
//! append reads the log, adds the changes, and writes it back with a
//! compare-and-swap on the entry's version; another replica's append in
//! between is a conflict, and the append is retried.
//!
//! Cancelled, the task finishes the append under way, appends what is
//! queued, and ends; `ChangeFeed::stop` aborts it at the shutdown deadline.
//! Every change that does not reach the log, aborted ones included, is
//! counted in `proxy_cache_publish_failures_total` and logged with its IDs.

use std::sync::Arc;
use std::time::Duration;

use cluster_sdk::cache::{PutRequest, Ttl};
use cluster_sdk::{ClusterCacheV1, ClusterError};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio_util::sync::{CancellationToken, WaitForCancellationFutureOwned};
use tracing::{debug, warn};
use uuid::Uuid;

use super::change::Changes;
use super::change_log::{ChangeLog, LOG_KEY, Unreadable};
use super::write_hook::PUBLISH_TIMEOUT;
use super::{Change, WatchStatus};
use crate::domain::ports::OagwMetricsPort;
use crate::domain::ports::metric_labels::cache_publish_failure;
use crate::domain::services::registry_state::jitter;

/// How many times an append is tried.
const MAX_ATTEMPTS: u32 = 5;

/// The wait before the second attempt; it doubles for each further one.
const FIRST_BACKOFF: Duration = Duration::from_millis(20);

/// How long one attempt (a read and a conditional write of the log) may
/// take before it fails and is retried, so a hung cluster fails the append
/// instead of stalling the task. At most half of [`PUBLISH_TIMEOUT`], so a
/// write's wait leaves room for a second attempt after one that hangs.
const ATTEMPT_TIMEOUT: Duration = Duration::from_secs(1);

const _: () = assert!(2 * ATTEMPT_TIMEOUT.as_millis() <= PUBLISH_TIMEOUT.as_millis());

/// Why an append failed.
#[derive(Debug, Clone, thiserror::Error)]
pub(crate) enum PublishError {
    /// Other replicas' appends came first on every attempt.
    #[error("other replicas' appends came first {MAX_ATTEMPTS} times")]
    Contended,
    /// The cluster failed the read or the write.
    #[error(transparent)]
    Cluster(#[from] ClusterError),
    /// The last attempt took longer than [`ATTEMPT_TIMEOUT`].
    #[error("the last attempt took longer than {ATTEMPT_TIMEOUT:?}")]
    TimedOut,
    /// The log has this newer version, written by a newer build during a
    /// rolling upgrade; this build leaves it alone.
    #[error("the change log is left alone: its version {0} is newer than this build's")]
    NewerLog(u64),
}

impl PublishError {
    /// The `reason` label of the changes the failed append carried.
    pub(crate) fn reason(&self) -> &'static str {
        match self {
            Self::NewerLog(_) => cache_publish_failure::NEWER_LOG,
            Self::Contended | Self::Cluster(_) | Self::TimedOut => {
                cache_publish_failure::APPEND_FAILED
            }
        }
    }
}

/// Hears how the append carrying a change went.
pub(crate) type Ack = oneshot::Sender<Result<(), PublishError>>;

/// A change for the next append, and who waits for it.
type Request = (Change, Option<Ack>);

/// The handle writes hand their changes to.
#[derive(Clone)]
pub(crate) struct Publisher {
    requests: mpsc::UnboundedSender<Request>,
    cancel: CancellationToken,
}

impl Publisher {
    /// Start the task appending to the log in `cache` as `origin`, reporting
    /// to `status` whether its appends skip a newer log.
    pub(crate) fn spawn(
        cache: ClusterCacheV1,
        origin: Uuid,
        metrics: Arc<dyn OagwMetricsPort>,
        status: WatchStatus,
        cancel: CancellationToken,
    ) -> (Self, JoinHandle<()>) {
        let (requests, queue) = mpsc::unbounded_channel();
        let task = tokio::spawn(run(cache, origin, metrics, status, queue, cancel.clone()));
        (Self { requests, cancel }, task)
    }

    /// Completes once the publisher is told to stop: what waits on it to
    /// publish later ends then.
    pub(crate) fn stopping(&self) -> WaitForCancellationFutureOwned {
        self.cancel.clone().cancelled_owned()
    }

    /// Queue `change` for the next append, without waiting; `ack`, if any,
    /// hears how the append went.
    ///
    /// # Errors
    ///
    /// The change, back, when the task takes no more changes: it was
    /// cancelled or has ended. The caller counts and logs it.
    pub(crate) fn submit(&self, change: Change, ack: Option<Ack>) -> Result<(), Change> {
        self.requests
            .send((change, ack))
            .map_err(|refused| refused.0.0)
    }
}

/// The queue and the changes of the append under way. Dropped with changes
/// left (the task was aborted), it counts and logs them as not published.
struct Backlog {
    queue: mpsc::UnboundedReceiver<Request>,
    in_hand: Vec<Change>,
    metrics: Arc<dyn OagwMetricsPort>,
}

impl Drop for Backlog {
    fn drop(&mut self) {
        self.queue.close();
        let mut lost = std::mem::take(&mut self.in_hand);
        while let Ok((change, _)) = self.queue.try_recv() {
            lost.push(change);
        }
        if lost.is_empty() {
            return;
        }
        self.metrics.record_proxy_cache_publish_failures(
            cache_publish_failure::PUBLISHER_STOPPED,
            lost.len() as u64,
        );
        warn!(
            count = lost.len(),
            changes = %Changes(&lost),
            "The proxy configuration change publisher stopped before appending these changes; \
             other replicas see them when their cache entries expire"
        );
    }
}

/// Append each batch of queued changes. Once `cancel` fires, take no more,
/// append what is queued, and end.
async fn run(
    cache: ClusterCacheV1,
    origin: Uuid,
    metrics: Arc<dyn OagwMetricsPort>,
    status: WatchStatus,
    queue: mpsc::UnboundedReceiver<Request>,
    cancel: CancellationToken,
) {
    let mut backlog = Backlog {
        queue,
        in_hand: Vec::new(),
        metrics,
    };
    let mut cancelled = false;
    loop {
        let first = tokio::select! {
            biased;
            () = cancel.cancelled(), if !cancelled => {
                // A closed queue still yields what it holds, then `None`.
                cancelled = true;
                backlog.queue.close();
                continue;
            }
            next = backlog.queue.recv() => match next {
                Some(next) => next,
                None => return,
            },
        };
        let mut acks = Vec::new();
        for (change, ack) in
            std::iter::once(first).chain(std::iter::from_fn(|| backlog.queue.try_recv().ok()))
        {
            backlog.in_hand.push(change);
            acks.extend(ack);
        }
        // Not raced against `cancel`: the append finishes, or the stop
        // deadline aborts the task and `Backlog` counts its changes.
        let result = append(&cache, origin, &backlog.in_hand).await;
        let changes = std::mem::take(&mut backlog.in_hand);
        // Only a landed append shows the log is no longer newer.
        match &result {
            Ok(()) => status.set_newer_log(None),
            Err(PublishError::NewerLog(version)) => status.set_newer_log(Some(*version)),
            Err(PublishError::Contended | PublishError::Cluster(_) | PublishError::TimedOut) => {}
        }
        if let Err(e) = &result {
            backlog
                .metrics
                .record_proxy_cache_publish_failures(e.reason(), changes.len() as u64);
            warn!(
                error = %e,
                count = changes.len(),
                changes = %Changes(&changes),
                "Could not append proxy configuration changes to the cluster change log; \
                 other replicas see them when their cache entries expire"
            );
        }
        for ack in acks {
            // A writer that stopped waiting has gone on.
            let _ = ack.send(result.clone());
        }
    }
}

/// Append `changes` made by `origin` to the log, retrying a conflict, a
/// transient cluster error or a slow attempt with a jittered, doubling
/// backoff. A log of a newer format is not retried.
async fn append(
    cache: &ClusterCacheV1,
    origin: Uuid,
    changes: &[Change],
) -> Result<(), PublishError> {
    let mut backoff = FIRST_BACKOFF;
    let mut attempt = 1;
    loop {
        let tried = tokio::time::timeout(ATTEMPT_TIMEOUT, try_append(cache, origin, changes));
        let failed = match tried.await {
            Ok(Ok(Attempt::Appended)) => return Ok(()),
            Ok(Ok(Attempt::Conflict)) => PublishError::Contended,
            Ok(Ok(Attempt::NewerLog(version))) => return Err(PublishError::NewerLog(version)),
            Ok(Err(e)) if e.is_retryable() => PublishError::Cluster(e),
            Ok(Err(e)) => return Err(e.into()),
            // The write may have landed; the next attempt then logs the
            // changes twice, which evicts nothing more.
            Err(_) => PublishError::TimedOut,
        };
        if attempt == MAX_ATTEMPTS {
            return Err(failed);
        }
        // The last attempt's failure is logged by the task, with the changes.
        if matches!(failed, PublishError::Contended) {
            debug!(attempt, error = %failed, "Another replica appended to the proxy configuration change log first; retrying");
        } else {
            warn!(attempt, error = %failed, "An append to the proxy configuration change log failed; retrying");
        }
        tokio::time::sleep(jitter(backoff)).await;
        backoff *= 2;
        attempt += 1;
    }
}

/// How one attempt went.
enum Attempt {
    /// The changes are in the log.
    Appended,
    /// Another append came in between.
    Conflict,
    /// The log has this newer version and was left alone.
    NewerLog(u64),
}

/// One read and conditional write of the log.
async fn try_append(
    cache: &ClusterCacheV1,
    origin: Uuid,
    changes: &[Change],
) -> Result<Attempt, ClusterError> {
    let entry = cache.get(LOG_KEY).await?;
    let mut log = match &entry {
        None => ChangeLog::new(),
        Some(entry) => match ChangeLog::decode(&entry.value) {
            Ok(log) => log,
            // A newer build's log stays: replacing it would empty every
            // replica's cache on each write until the upgrade ends.
            Err(Unreadable::Newer(version)) => return Ok(Attempt::NewerLog(version)),
            // Any other value this build cannot read is replaced by a new
            // log: readers see the new ID and empty their caches.
            Err(unreadable @ (Unreadable::Malformed(_) | Unreadable::Version(_))) => {
                let log = ChangeLog::new();
                warn!(
                    log = %log.id(),
                    origin = %origin,
                    error = %unreadable,
                    "Replacing an unreadable proxy configuration change log with a new log"
                );
                log
            }
        },
    };
    let value = log.append(origin, changes);
    let Some(entry) = entry else {
        let created = cache
            .put_if_absent(PutRequest {
                key: LOG_KEY,
                value: &value,
                ttl: Ttl::Indefinite,
            })
            .await?;
        return Ok(if created.is_some() {
            Attempt::Appended
        } else {
            Attempt::Conflict
        });
    };
    match cache
        .compare_and_swap(LOG_KEY, entry.version, &value, Ttl::Indefinite)
        .await
    {
        Ok(_) => Ok(Attempt::Appended),
        Err(ClusterError::CasConflict { .. }) => Ok(Attempt::Conflict),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
#[path = "publisher_tests.rs"]
mod tests;
