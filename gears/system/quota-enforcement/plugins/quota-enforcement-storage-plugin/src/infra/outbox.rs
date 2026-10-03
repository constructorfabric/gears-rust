//! The notification outbox (invariant I11): every mutation enqueues its
//! `NotificationEvent`s in the transaction that writes its rows, through the
//! toolkit outbox under the plugin's own table prefix.
//!
//! The plugin owns the one pipeline of the notification queue:
//! [`start_notification_pipeline`] declares the queue with its leased handler
//! (the gear dispatcher's delivery callback behind it) before the outbox
//! starts, then binds that outbox through [`QeOutbox::bind`]. Until then a
//! non-empty enqueue fails [`EnqueueError::NotBound`] and its transaction
//! rolls back, so no mutation commits an event that nothing would deliver; an
//! empty enqueue succeeds unbound, so bootstrap can run before delivery.
//!
//! Every enqueue hands back its outbox `Wake`. A store collects the wakes of
//! one transaction attempt in an [`AttemptWakes`] and fires them only once the
//! attempt commits; a rolled-back or abandoned attempt discards them.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::task::{Context, Poll};

use async_trait::async_trait;
use quota_enforcement_sdk::{
    NotificationDeliveryHandle, NotificationDeliveryV1, NotificationEvent, NotificationScope,
    StorageError, TenantId,
};
use toolkit_db::Db;
use toolkit_db::outbox::{Outbox, OutboxError, OutboxHandle, Partitions, Records, Wake};
use toolkit_db::secure::DBRunner;

use super::outbox_handler::NotificationHandler;
use crate::domain::ports::NotificationPipeline;

pub use crate::infra::storage::migrations::OUTBOX_TABLE_PREFIX;

/// The queue every notification event is enqueued on.
pub const NOTIFICATION_QUEUE: &str = "qe_notifications";

/// Partitions of [`NOTIFICATION_QUEUE`]; a tenant always lands on one
/// partition, so its events stay ordered.
pub const NOTIFICATION_PARTITIONS: u16 = 8;

/// Why an enqueue failed inside the caller's transaction.
#[derive(Debug, thiserror::Error)]
pub enum EnqueueError {
    /// No outbox handle was bound yet.
    #[error("notification outbox is not bound")]
    NotBound,
    /// An event did not serialize.
    #[error("notification event does not serialize: {0}")]
    Serialize(String),
    /// The toolkit outbox rejected the batch.
    #[error(transparent)]
    Outbox(#[from] OutboxError),
}

/// The outbox a store enqueues into. Every enqueue returns the `Wake` that
/// announces it, for the transaction attempt that made it to fire after
/// commit.
#[async_trait]
pub trait NotificationOutbox: Send + Sync {
    /// Enqueue `events` in order on the caller's runner. Nothing is written
    /// when any event fails; an empty slice writes nothing and needs no bound
    /// outbox.
    async fn enqueue(
        &self,
        runner: &(dyn DBRunner + Sync),
        events: &[NotificationEvent],
    ) -> Result<Wake, EnqueueError>;
}

/// Enqueues notification events inside one transaction attempt, so they
/// commit with the rows that caused them.
#[async_trait]
pub trait NotificationEnqueuer: Send + Sync {
    /// Enqueue `events` in order. Nothing is written when any event fails.
    async fn enqueue_all(
        &self,
        runner: &(dyn DBRunner + Sync),
        events: &[NotificationEvent],
    ) -> Result<(), EnqueueError>;
}

/// The wakes of one transaction attempt's enqueues, fired only if the
/// attempt commits.
///
/// A store begins one per attempt and hands its [`Self::enqueuer`] into the
/// transaction; the attempt's future is wrapped with [`SettleWakes::settling`],
/// which fires the collected wakes on `Ok` and discards them on `Err` (a
/// rollback, a lost race, a refused lock that is retried). A retried
/// operation therefore fires the wakes of its committing attempt only.
pub struct AttemptWakes {
    outbox: Arc<dyn NotificationOutbox>,
    wake: Mutex<Wake>,
}

impl AttemptWakes {
    /// A collector for one attempt over `outbox`.
    #[must_use]
    pub fn begin(outbox: &Arc<dyn NotificationOutbox>) -> Arc<Self> {
        Arc::new(Self {
            outbox: Arc::clone(outbox),
            wake: Mutex::new(Wake::empty()),
        })
    }

    /// The enqueuer the attempt's transaction uses.
    #[must_use]
    pub fn enqueuer(self: &Arc<Self>) -> Arc<dyn NotificationEnqueuer> {
        Arc::clone(self) as Arc<dyn NotificationEnqueuer>
    }

    /// Fire the collected wakes when the attempt committed, else discard them.
    fn settle(&self, committed: bool) {
        let wake = std::mem::replace(
            &mut *self.wake.lock().unwrap_or_else(PoisonError::into_inner),
            Wake::empty(),
        );
        if committed {
            wake.fire();
        } else {
            wake.discard();
        }
    }
}

#[async_trait]
impl NotificationEnqueuer for AttemptWakes {
    async fn enqueue_all(
        &self,
        runner: &(dyn DBRunner + Sync),
        events: &[NotificationEvent],
    ) -> Result<(), EnqueueError> {
        let wake = self.outbox.enqueue(runner, events).await?;
        *self.wake.lock().unwrap_or_else(PoisonError::into_inner) += wake;
        Ok(())
    }
}

/// A transaction attempt that settles its [`AttemptWakes`] when it resolves.
pub struct Settling<F> {
    attempt: Pin<Box<F>>,
    wakes: Option<Arc<AttemptWakes>>,
}

impl<F, T, E> Future for Settling<F>
where
    F: Future<Output = Result<T, E>>,
{
    type Output = Result<T, E>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let outcome = std::task::ready!(self.attempt.as_mut().poll(cx));
        if let Some(wakes) = self.wakes.take() {
            wakes.settle(outcome.is_ok());
        }
        Poll::Ready(outcome)
    }
}

/// Settle an attempt's wakes when its transaction resolves.
pub trait SettleWakes<T, E>: Future<Output = Result<T, E>> + Sized {
    /// This attempt, firing `wakes` if it commits and discarding them if not.
    fn settling(self, wakes: Arc<AttemptWakes>) -> Settling<Self> {
        Settling {
            attempt: Box::pin(self),
            wakes: Some(wakes),
        }
    }
}

impl<F, T, E> SettleWakes<T, E> for F where F: Future<Output = Result<T, E>> {}

/// The outbox handle is bound once the pipeline starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("notification outbox is already bound")]
pub struct AlreadyBound;

/// Late-bound handle to the plugin's outbox.
#[derive(Default)]
pub struct QeOutbox {
    outbox: OnceLock<Arc<Outbox>>,
}

impl QeOutbox {
    /// An unbound handle.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Bind the started outbox. Once.
    ///
    /// # Errors
    ///
    /// [`AlreadyBound`] on a second call.
    pub fn bind(&self, outbox: Arc<Outbox>) -> Result<(), AlreadyBound> {
        self.outbox.set(outbox).map_err(|_| AlreadyBound)
    }

    /// True once [`Self::bind`] ran.
    #[must_use]
    pub fn is_bound(&self) -> bool {
        self.outbox.get().is_some()
    }

    /// The partition a tenant's events land on.
    #[must_use]
    pub fn partition_for(tenant_id: TenantId) -> u32 {
        // The remainder of a `u128` by a `u16` fits a `u32`.
        u32::try_from(tenant_id.as_uuid().as_u128() % u128::from(NOTIFICATION_PARTITIONS))
            .unwrap_or(0)
    }
}

#[async_trait]
// @cpt-flow:cpt-cf-quota-enforcement-flow-sink-delivery:p1
impl NotificationOutbox for QeOutbox {
    async fn enqueue(
        &self,
        runner: &(dyn DBRunner + Sync),
        events: &[NotificationEvent],
    ) -> Result<Wake, EnqueueError> {
        // @cpt-begin:cpt-cf-quota-enforcement-flow-sink-delivery:p1:inst-del-enqueue
        // Nothing to write needs no pipeline: bootstrap seeds before delivery
        // starts.
        let Some(first) = events.first() else {
            return Ok(Wake::empty());
        };
        let outbox = self.outbox.get().ok_or(EnqueueError::NotBound)?;
        // Every event names its own kind, so the batch default is only the
        // fallback each entity overrides. Nothing traces the batch: a
        // notification is already identified by its own event id.
        let mut batch = Records::to(NOTIFICATION_QUEUE).payload_type(first.kind.as_str());
        for event in events {
            let payload =
                serde_json::to_vec(event).map_err(|e| EnqueueError::Serialize(e.to_string()))?;
            let partition = match event.scope {
                NotificationScope::Tenant { tenant_id } => Self::partition_for(tenant_id),
                // All policy transitions share one ordered platform stream.
                NotificationScope::Platform => 0,
            };
            batch = batch.push_with_type(partition, payload, event.kind.as_str());
        }
        // Fired by the caller's attempt once its transaction commits.
        Ok(outbox.enqueue_batch(runner, batch.build()?).await?)
        // @cpt-end:cpt-cf-quota-enforcement-flow-sink-delivery:p1:inst-del-enqueue
    }
}

/// Why the notification pipeline did not start.
#[derive(Debug, thiserror::Error)]
pub enum PipelineError {
    /// The toolkit outbox failed to start.
    #[error(transparent)]
    Outbox(#[from] OutboxError),
    /// A pipeline was already bound to this plugin's outbox handle.
    #[error(transparent)]
    AlreadyBound(#[from] AlreadyBound),
}

/// Start the one pipeline of the notification queue: declare the queue with
/// its leased handler, which hands every claimed event to `delivery`, start
/// the outbox, and bind it to `outbox` for enqueueing.
///
/// # Errors
///
/// [`PipelineError::AlreadyBound`] when `outbox` is already bound: the plugin
/// never runs a second pipeline. The toolkit error of the start otherwise.
// @cpt-dod:cpt-cf-quota-enforcement-dod-dispatcher:p1
// @cpt-state:cpt-cf-quota-enforcement-state-outbox-event:p1
pub async fn start_notification_pipeline(
    db: Db,
    outbox: &QeOutbox,
    delivery: Arc<dyn NotificationDeliveryV1>,
) -> Result<OutboxHandle, PipelineError> {
    if outbox.is_bound() {
        return Err(AlreadyBound.into());
    }
    // @cpt-begin:cpt-cf-quota-enforcement-flow-sink-delivery:p1:inst-del-alo
    // @cpt-begin:cpt-cf-quota-enforcement-state-outbox-event:p1:inst-obst-reclaim
    // The default lease drops a handler future at its cancel point, so another
    // processor re-claims what an expired holder was delivering; the
    // framework's vacuum workers reclaim delivered rows.
    let handle = Outbox::builder(db)
        .table_prefix(OUTBOX_TABLE_PREFIX)?
        .queue(NOTIFICATION_QUEUE, Partitions::of(NOTIFICATION_PARTITIONS))
        .leased(NotificationHandler::new(delivery))
        .start()
        .await?;
    // @cpt-end:cpt-cf-quota-enforcement-state-outbox-event:p1:inst-obst-reclaim
    // @cpt-end:cpt-cf-quota-enforcement-flow-sink-delivery:p1:inst-del-alo
    outbox.bind(Arc::clone(handle.outbox()))?;
    Ok(handle)
}

/// The SQL plugin's one notification pipeline: its database and the outbox
/// handle every store enqueues through.
pub struct SqlNotificationPipeline {
    db: Db,
    outbox: Arc<QeOutbox>,
    started: AtomicBool,
}

impl SqlNotificationPipeline {
    /// The pipeline over `db`, binding `outbox` when it starts.
    #[must_use]
    pub fn new(db: Db, outbox: Arc<QeOutbox>) -> Self {
        Self {
            db,
            outbox,
            started: AtomicBool::new(false),
        }
    }
}

#[async_trait]
impl NotificationPipeline for SqlNotificationPipeline {
    async fn start(
        &self,
        delivery: Arc<dyn NotificationDeliveryV1>,
    ) -> Result<Box<dyn NotificationDeliveryHandle>, StorageError> {
        // Claimed before anything starts, so two concurrent calls can never
        // both run a pipeline.
        if self.started.swap(true, Ordering::SeqCst) || self.outbox.is_bound() {
            return Err(StorageError::Internal(
                "the notification pipeline is already started".to_owned(),
            ));
        }
        match start_notification_pipeline(self.db.clone(), &self.outbox, delivery).await {
            Ok(handle) => Ok(Box::new(PipelineHandle(handle))),
            Err(error) => {
                self.started.store(false, Ordering::SeqCst);
                tracing::warn!(
                    target: "qe.storage",
                    error = %error,
                    "the notification pipeline failed to start"
                );
                Err(StorageError::Unavailable(
                    "the notification pipeline failed to start".to_owned(),
                ))
            }
        }
    }
}

/// A started pipeline; stopping it cancels its workers and waits for them.
struct PipelineHandle(OutboxHandle);

#[async_trait]
impl NotificationDeliveryHandle for PipelineHandle {
    async fn stop(self: Box<Self>) {
        self.0.stop().await;
    }
}

/// Start the outbox on `db` with the notification queue registered and **no
/// handler**, for tests that inspect enqueued rows: nothing ever drains them.
/// Production starts the queue only through [`start_notification_pipeline`].
///
/// # Errors
///
/// The toolkit error of the start or the queue registration.
#[cfg(any(test, feature = "test-util"))]
pub async fn start_undelivered_outbox(db: Db) -> Result<OutboxHandle, OutboxError> {
    let handle = Outbox::builder(db.clone())
        .table_prefix(OUTBOX_TABLE_PREFIX)?
        .start()
        .await?;
    handle
        .outbox()
        .register_queue(&db, NOTIFICATION_QUEUE, NOTIFICATION_PARTITIONS)
        .await?;
    Ok(handle)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "outbox_tests.rs"]
mod outbox_tests;
