//! Orders typed events through the Event Broker managed producer (DESIGN §4.4, §4.7; D-200).
//!
//! The transition transaction only enqueues: [`TxEvents::enqueue`] writes the SDK's producer
//! envelope into the toolkit outbox on the caller's transaction runner, so a rollback erases the
//! event with the transition and a failed enqueue aborts it. The outbox [`Wake`] is held until
//! the transaction ends: [`transaction`] fires it after commit and discards it on rollback and
//! before every retried attempt. Toolkit workers own sequencing, leases, retries and dead
//! letters; Orders owns no drain SQL, relay or re-drive path.
//!
//! The runner follows the Pricing precedent
//! (`gears/bss/pricing/pricing/src/infra/events.rs`, D-455), without its interim holding queue:
//! an [`EventSink`] exists only once the real producer is bound ([`super::broker`]).
use event_broker_sdk::{EventBrokerError, ProducerOutbox};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};
use toolkit_db::outbox::Wake;
use toolkit_db::secure::TxConfig;

use super::storage::entity::order;
use super::storage::repo::{LockedOrder, TransactionRunner};
use toolkit_db::{Db, DbError, DbTx};
use uuid::Uuid;

/// `gts.cf.core.events.event.v1~cf.bss.orders.event.v1~cf.bss.orders.<name>.v1~`.
macro_rules! concat_type {
    ($name:literal) => {
        concat!(
            "gts.cf.core.events.event.v1~cf.bss.orders.event.v1~cf.bss.orders.",
            $name,
            ".v1~"
        )
    };
}

pub mod payload;
pub mod schema;

pub use payload::{EventContractError, EventKind, OrderEvent, OrderSummary};

/// Producer identity and wire `source` (§3.7, §4.4).
pub const SOURCE: &str = "bss-orders-lifecycle";
/// The toolkit producer queue (§3.7 *Producer queue*).
pub const QUEUE: &str = "bss-orders-events";
/// Producer outbox partitions (§3.7: `Partitions::of(16)`).
pub const OUTBOX_PARTITIONS: u16 = 16;
/// Stable managed producer-registration key (§3.7).
pub const PRODUCER_KEY: &str = "bss-orders-events-v1";
/// The Orders topic instance; the identifier is allocated in the Orders-owned namespace (§4.7).
pub const TOPIC: &str = "gts.cf.core.events.topic.v1~cf.bss.orders.events.v1";
/// The abstract Orders event family base (`x-gts-abstract`).
pub const EVENT_BASE: &str = "gts.cf.core.events.event.v1~cf.bss.orders.event.v1~";
/// The producer's declared event-type pattern: exactly the Orders family.
pub const EVENT_TYPE_WILDCARD: &str =
    "gts.cf.core.events.event.v1~cf.bss.orders.event.v1~cf.bss.orders.*";
/// The order subject type, registered by Orders before its event schemas (§4.7).
pub const ORDER_SUBJECT_TYPE: &str = crate::gts::permissions::labels::ORDER;
/// Partition routing: the order UUID subject, never the root tenant (§4.7, D-95).
pub const PARTITION_KEY: &str = "/subject";
/// The toolkit outbox payload limit the fully serialized producer envelope must fit (§4.4).
/// Toolkit keeps its constant crate-private; a test pins this value against its record builder.
pub const MAX_ENVELOPE_BYTES: usize = 64 * 1024;
/// Envelope members outside `data` (identifiers, type, topic, tenancy, producer metadata),
/// reserved before admitting `data` (DESIGN §6 synthetic sizing: a 2 KiB envelope reserve).
pub const ENVELOPE_RESERVE_BYTES: usize = 2 * 1024;

/// The canonical platform-root tenant, resolved from the authoritative platform source before
/// readiness and outside every transition transaction (D-95). Only the producer binding builds
/// one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RootTenant(Uuid);
impl RootTenant {
    pub(in crate::infra) fn resolved(id: Uuid) -> Option<Self> {
        (!id.is_nil()).then_some(Self(id))
    }
    #[must_use]
    pub fn id(self) -> Uuid {
        self.0
    }
}

/// The bound managed producer: the only way to enqueue an Orders event.
#[derive(Clone)]
pub struct EventSink {
    outbox: ProducerOutbox,
    root: RootTenant,
}
impl EventSink {
    pub(in crate::infra) fn new(outbox: ProducerOutbox, root: RootTenant) -> Self {
        Self { outbox, root }
    }
    #[must_use]
    pub fn root(&self) -> RootTenant {
        self.root
    }
}

/// An event the transition could not enqueue; every variant aborts the transaction.
#[derive(Debug, thiserror::Error)]
pub enum EventEnqueueError {
    #[error(transparent)]
    Contract(#[from] EventContractError),
    #[error("Orders event data is {bytes} bytes; at most {max} fit the 64 KiB envelope")]
    TooLarge { bytes: usize, max: usize },
    #[error("an Orders transition enqueues at most one event")]
    AlreadyEnqueued,
    #[error("the event must be enqueued in the transaction holding the order's lock")]
    ForeignTransaction,
    #[error("the event summary does not describe the locked order's post-state")]
    NotLockedPostState,
    #[error("Orders event serialization failed")]
    Serialize,
    #[error("the Orders event producer refused the enqueue: {0}")]
    Producer(EventBrokerError),
    /// The attempt's work returned success after one of its enqueues failed. The runner
    /// refuses to commit it: order state never commits without its producer message.
    #[error("an Orders event enqueue failed in this transaction; it cannot commit")]
    EnqueueFailed,
}

/// The largest `data` encoding admitted before the SDK builds the envelope.
#[must_use]
pub const fn max_data_bytes() -> usize {
    MAX_ENVELOPE_BYTES - ENVELOPE_RESERVE_BYTES
}

/// Admit an encoded `data` length (§4.4: enforce 64 KiB on the envelope before commit).
///
/// # Errors
/// `TooLarge` above [`max_data_bytes`].
pub fn admit_data_len(bytes: usize) -> Result<(), EventEnqueueError> {
    if bytes > max_data_bytes() {
        return Err(EventEnqueueError::TooLarge {
            bytes,
            max: max_data_bytes(),
        });
    }
    Ok(())
}

#[derive(Default)]
struct Pending {
    wake: Option<Wake>,
    /// Address of this attempt's transaction runner; identity only, never dereferenced.
    runner: usize,
    enqueued: bool,
    /// An enqueue in this attempt failed: the attempt may only roll back.
    failed: bool,
}

/// The event sink as one transition transaction sees it. Clones share one pending wake.
#[derive(Clone)]
pub struct TxEvents {
    sink: EventSink,
    pending: Arc<Mutex<Pending>>,
}
impl TxEvents {
    fn new(sink: EventSink) -> Self {
        Self {
            sink,
            pending: Arc::new(Mutex::new(Pending::default())),
        }
    }
    fn lock(&self) -> std::sync::MutexGuard<'_, Pending> {
        self.pending.lock().unwrap_or_else(PoisonError::into_inner)
    }
    fn take(&self) -> Option<Wake> {
        let mut pending = self.lock();
        pending.enqueued = false;
        pending.failed = false;
        pending.runner = 0;
        pending.wake.take()
    }
    /// Start an attempt on `tx`: anything a previous attempt enqueued rolled back with it.
    fn begin(&self, tx: &DbTx<'_>) {
        self.discard();
        self.lock().runner = std::ptr::from_ref(tx).addr();
    }
    /// Wake the sequencer for the committed event. Only after commit.
    fn fire(&self) {
        if let Some(wake) = self.take() {
            wake.fire();
        }
    }
    /// Drop the wake unfired: the attempt rolled back, and its outbox row with it.
    fn discard(&self) {
        if let Some(wake) = self.take() {
            wake.discard();
        }
    }

    /// Enqueue the transition's one event in the transaction holding `locked`, stamped with
    /// the platform-root tenant (D-95).
    ///
    /// `locked` must be this attempt's aggregate lock, taken through the sealed transaction
    /// runner `transaction` supplied, and `summary` must describe its committed post-state:
    /// the order, version, resulting state, category, contract and tenant axes. A transition
    /// enqueues at most one event (D-200: one local producer message per committed
    /// event-producing transition). Then the §4.4 contract and the 64 KiB budget are checked,
    /// and the SDK validates `data` against the prepared GTS schema, resolves the `/subject`
    /// partition and writes its producer envelope. The wake stays here until the transaction
    /// ends.
    ///
    /// # Errors
    /// [`EventEnqueueError`]; the caller aborts the transaction. Any failure also marks the
    /// attempt failed, so [`transaction`] refuses to commit it even if the error is dropped.
    pub async fn enqueue<T: TransactionRunner + Sync, K: EventKind>(
        &self,
        locked: &LockedOrder<'_, T>,
        summary: OrderSummary,
        detail: K,
    ) -> Result<(), EventEnqueueError> {
        let result = self.enqueue_checked(locked, summary, detail).await;
        self.record(result)
    }

    async fn enqueue_checked<T: TransactionRunner + Sync, K: EventKind>(
        &self,
        locked: &LockedOrder<'_, T>,
        summary: OrderSummary,
        detail: K,
    ) -> Result<(), EventEnqueueError> {
        self.bound_to(locked)?;
        matches_locked(locked.row(), &summary)?;
        let event = OrderEvent::rooted(self.sink.root, summary, detail);
        event.validate()?;
        let encoded = serde_json::to_vec(&event).map_err(|_| EventEnqueueError::Serialize)?;
        admit_data_len(encoded.len())?;
        self.enqueue_prepared(locked.transaction(), event).await
    }

    /// Mark the attempt failed on any enqueue error (D-200: the transition cannot commit
    /// without its producer message).
    fn record(&self, result: Result<(), EventEnqueueError>) -> Result<(), EventEnqueueError> {
        if result.is_err() {
            self.lock().failed = true;
        }
        result
    }

    /// The attempt may commit only if none of its enqueues failed.
    fn committable(&self) -> Result<(), EventEnqueueError> {
        if self.lock().failed {
            return Err(EventEnqueueError::EnqueueFailed);
        }
        Ok(())
    }

    fn bound_to<T: TransactionRunner>(
        &self,
        locked: &LockedOrder<'_, T>,
    ) -> Result<(), EventEnqueueError> {
        let runner = std::ptr::from_ref(locked.transaction()).addr();
        let pending = self.lock();
        if pending.runner == 0 || pending.runner != runner {
            return Err(EventEnqueueError::ForeignTransaction);
        }
        Ok(())
    }

    async fn enqueue_prepared<K: EventKind>(
        &self,
        tx: &(impl TransactionRunner + Sync),
        event: OrderEvent<K>,
    ) -> Result<(), EventEnqueueError> {
        {
            // Claimed before the await: a failed enqueue aborts the transaction, never retries
            // inside it.
            let mut pending = self.lock();
            if pending.enqueued {
                return Err(EventEnqueueError::AlreadyEnqueued);
            }
            pending.enqueued = true;
        }
        let wake = self
            .sink
            .outbox
            .enqueue(tx, event)
            .await
            .map_err(EventEnqueueError::Producer)?;
        let mut pending = self.lock();
        match pending.wake.as_mut() {
            Some(held) => *held += wake,
            None => pending.wake = Some(wake),
        }
        Ok(())
    }

    /// Test-only: skip Orders' own contract and size admission to prove the platform's checks.
    #[cfg(test)]
    pub(crate) async fn enqueue_unchecked<T: TransactionRunner + Sync, K: EventKind>(
        &self,
        locked: &LockedOrder<'_, T>,
        summary: OrderSummary,
        detail: K,
    ) -> Result<(), EventEnqueueError> {
        let result = async {
            self.bound_to(locked)?;
            let event = OrderEvent::rooted(self.sink.root, summary, detail);
            self.enqueue_prepared(locked.transaction(), event).await
        }
        .await;
        self.record(result)
    }
}

/// The summary reports the locked aggregate's committed post-state (§4.4 common summary).
fn matches_locked(row: &order::Model, summary: &OrderSummary) -> Result<(), EventEnqueueError> {
    let state = serde_json::to_value(summary.state).map_err(|_| EventEnqueueError::Serialize)?;
    let locked = (
        row.order_id,
        row.current_version,
        row.category.as_str(),
        row.contract_id,
        row.resource_tenant_id,
        row.seller_tenant_id,
        row.payer_tenant_id,
    );
    let reported = (
        summary.order_id,
        summary.order_version,
        summary.category.as_str(),
        summary.contract_id,
        summary.resource_tenant_id,
        summary.seller_tenant_id,
        summary.payer_tenant_id,
    );
    let same = locked == reported && state.as_str() == Some(row.state.as_str());
    if same {
        Ok(())
    } else {
        Err(EventEnqueueError::NotLockedPostState)
    }
}

/// Run `work` in one retrying transition transaction with a [`TxEvents`] over `sink`, firing
/// the event's wake only after commit (D-200, D-455). A retried attempt has rolled back, so its
/// wake is discarded before the next attempt; a failed transaction discards the rest. An
/// attempt whose work succeeds after a failed enqueue rolls back instead of committing.
///
/// # Errors
/// The transaction's error after its attempts, as [`Db::transaction_with_retry_max`] reports;
/// [`EventEnqueueError::EnqueueFailed`] (as `DbError::Other`, never retried) when `work`
/// returned success over a failed enqueue.
pub async fn transaction<T, E, X, F>(
    db: &Db,
    sink: &EventSink,
    config: TxConfig,
    attempts: u32,
    extract_db_err: X,
    mut work: F,
) -> Result<T, E>
where
    E: From<DbError> + Send + 'static,
    T: Send + 'static,
    X: Fn(&E) -> Option<&sea_orm::DbErr> + Send,
    F: for<'a> FnMut(
            &'a DbTx<'a>,
            TxEvents,
        ) -> Pin<Box<dyn Future<Output = Result<T, E>> + Send + 'a>>
        + Send,
{
    let events = TxEvents::new(sink.clone());
    let attempt = events.clone();
    let result = db
        .transaction_with_retry_max(config, attempts, extract_db_err, move |tx| {
            attempt.begin(tx);
            let body = work(tx, attempt.clone());
            let guard = attempt.clone();
            Box::pin(async move {
                let value = body.await?;
                guard
                    .committable()
                    .map_err(|e| E::from(DbError::Other(anyhow::Error::new(e))))?;
                Ok(value)
            })
        })
        .await;
    if result.is_ok() {
        events.fire();
    } else {
        events.discard();
    }
    result
}

#[cfg(test)]
#[path = "events_tests.rs"]
mod events_tests;
#[cfg(test)]
pub mod test_support;
