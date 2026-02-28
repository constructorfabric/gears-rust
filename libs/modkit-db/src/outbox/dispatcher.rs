//! Generic outbox dispatcher loop.
//!
//! [`OutboxDispatcher`] encapsulates the poll→claim→publish→ack/nack cycle
//! so that consuming modules only need to supply a publish callback and wire
//! the dispatcher into their lifecycle entry point.

use std::future::Future;
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use super::store::OutboxStore;
use super::types::{ClaimCfg, ClaimedMessage};
use crate::DbError;

/// Generic outbox dispatcher that polls for pending rows and delivers them
/// via a module-supplied publish callback.
///
/// # Usage
///
/// ```ignore
/// use modkit_db::outbox::{OutboxDispatcher, OutboxStore, ClaimCfg, RetryCfg};
/// use std::time::Duration;
///
/// let store = OutboxStore::new(db_provider, worker_id, "my-ns".into(), retry_cfg);
/// let dispatcher = OutboxDispatcher::new(
///     store,
///     ClaimCfg { batch_size: 10, lease_duration: Duration::from_secs(60) },
///     Duration::from_secs(5),
/// );
///
/// // In your module's serve() lifecycle method:
/// dispatcher.run(cancel, |msg| async move {
///     publish_to_downstream(msg).await
/// }).await;
/// ```
pub struct OutboxDispatcher<E> {
    store: OutboxStore<E>,
    claim_cfg: ClaimCfg,
    poll_interval: Duration,
}

impl<E> OutboxDispatcher<E>
where
    E: From<DbError> + Send + std::fmt::Display + 'static,
{
    /// Create a new dispatcher.
    ///
    /// - `store`: the `OutboxStore` configured with namespace and retry policy.
    /// - `claim_cfg`: batch size and lease duration for each poll cycle.
    /// - `poll_interval`: how often to poll for pending rows. Should be
    ///   significantly less than `claim_cfg.lease_duration` (recommended:
    ///   `poll_interval <= lease_duration / 3`).
    ///
    /// # Panics
    ///
    /// Panics if `poll_interval` is zero.
    #[must_use]
    pub fn new(store: OutboxStore<E>, claim_cfg: ClaimCfg, poll_interval: Duration) -> Self {
        assert!(
            !poll_interval.is_zero(),
            "poll_interval must be greater than zero"
        );
        Self {
            store,
            claim_cfg,
            poll_interval,
        }
    }

    /// Run the dispatcher loop until `cancel` is triggered.
    ///
    /// For each claimed message, calls `publish_fn`:
    /// - On publish success → `ack`. If ack fails (lease expired, reclaimed
    ///   by another worker), the error is logged and no further action is taken.
    /// - On publish failure → `nack` with the error string, scheduling a retry
    ///   (or dead-lettering if `attempts >= max_attempts`).
    ///
    /// Transient DB errors during `claim_batch` / `ack` / `nack` are logged
    /// and retried on the next tick. The loop only exits when `cancel` fires.
    pub async fn run<F, Fut, PubErr>(&self, cancel: CancellationToken, publish_fn: F)
    where
        F: Fn(&ClaimedMessage) -> Fut + Send + Sync,
        Fut: Future<Output = Result<(), PubErr>> + Send,
        PubErr: std::fmt::Display,
    {
        if self.poll_interval.is_zero() {
            tracing::error!(
                namespace = %self.store.namespace(),
                "outbox dispatcher poll_interval must be > 0; exiting run loop"
            );
            return;
        }

        // Perform an immediate first poll so pending messages aren't delayed
        // by a full poll_interval on startup.
        if !cancel.is_cancelled() {
            self.poll_once(&cancel, &publish_fn).await;
        }

        let mut interval = tokio::time::interval(self.poll_interval);
        // Consume the first (immediate) tick so the loop starts with a clean delay.
        interval.tick().await;

        loop {
            tokio::select! {
                () = cancel.cancelled() => {
                    tracing::info!(
                        namespace = %self.store.namespace(),
                        "outbox dispatcher shutting down"
                    );
                    break;
                }
                _ = interval.tick() => {
                    self.poll_once(&cancel, &publish_fn).await;
                }
            }
        }
    }

    /// Execute a single poll cycle: claim a batch and process each message.
    async fn poll_once<F, Fut, PubErr>(&self, cancel: &CancellationToken, publish_fn: &F)
    where
        F: Fn(&ClaimedMessage) -> Fut + Send + Sync,
        Fut: Future<Output = Result<(), PubErr>> + Send,
        PubErr: std::fmt::Display,
    {
        let batch = match self.store.claim_batch(self.claim_cfg).await {
            Ok(b) => b,
            Err(e) => {
                tracing::error!(
                    namespace = %self.store.namespace(),
                    error = %e,
                    "outbox claim_batch failed, will retry next tick"
                );
                return;
            }
        };

        if !batch.is_empty() {
            tracing::debug!(
                namespace = %self.store.namespace(),
                count = batch.len(),
                "claimed outbox batch"
            );
        }

        for msg in &batch {
            let publish_res = tokio::select! {
                () = cancel.cancelled() => {
                    tracing::info!(
                        namespace = %self.store.namespace(),
                        "outbox dispatcher cancellation observed during publish"
                    );
                    return;
                }
                res = publish_fn(msg) => res,
            };

            match publish_res {
                Ok(()) => {
                    if let Err(e) = self.store.ack(msg.id).await {
                        // Spec: "dispatcher MUST log the error and MUST NOT
                        // treat this as a publish failure requiring nack."
                        tracing::warn!(
                            namespace = %self.store.namespace(),
                            id = %msg.id,
                            error = %e,
                            "outbox ack failed (lease may have expired)"
                        );
                    }
                }
                Err(pub_err) => {
                    let err_str = pub_err.to_string();
                    tracing::warn!(
                        namespace = %self.store.namespace(),
                        id = %msg.id,
                        error = %err_str,
                        attempts = msg.attempts,
                        "outbox publish failed, nacking"
                    );
                    if let Err(e) = self.store.nack(msg.id, &err_str).await {
                        tracing::warn!(
                            namespace = %self.store.namespace(),
                            id = %msg.id,
                            error = %e,
                            "outbox nack failed"
                        );
                    }
                }
            }
        }
    }
}
