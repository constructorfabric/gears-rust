//! Transactional producers, one per fence key (`docs/DESIGN.md` §3.2).
//!
//! A fence key's producer has the transactional id
//! `<prefix>.fence.<key>`. Opening it (`init_transactions`) fences every
//! earlier producer of that id, in this process or another, and settles the
//! transaction it left open; reading the key's fence-topic partition after
//! that yields the last seq any of them committed. From then on, while the
//! producer is not fenced, no one else writes under the key, so the seq it
//! keeps in memory is the truth.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use event_broker_sdk::backend::{BackendError, FenceKey};
use rdkafka::error::KafkaResult;
use rdkafka::producer::{FutureProducer, Producer};
use tokio::sync::Notify;

use crate::clients;
use crate::error::{self, Operation};
use crate::names;
use crate::records;
use crate::shared::Shared;

/// A producer whose last reference is dropped on the blocking pool:
/// destroying a librdkafka producer flushes and joins its threads.
pub struct Handle(Option<Arc<FutureProducer>>);

impl Handle {
    /// The held producer.
    pub fn producer(&self) -> &Arc<FutureProducer> {
        self.0
            .as_ref()
            .unwrap_or_else(|| unreachable!("a handle holds its producer until dropped"))
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        if let Some(producer) = self.0.take() {
            clients::drop_blocking(producer);
        }
    }
}

/// An open transaction that is aborted, off the async workers, if it is
/// dropped before [`end`](Self::end): a caller that gives up mid-append
/// leaves nothing holding back `read_committed` readers.
pub struct Transaction {
    producer: Option<Arc<FutureProducer>>,
    timeout: Duration,
}

impl Transaction {
    /// Begins a transaction on `producer`.
    pub fn begin(producer: &Arc<FutureProducer>, timeout: Duration) -> KafkaResult<Self> {
        producer.begin_transaction()?;
        Ok(Self {
            producer: Some(Arc::clone(producer)),
            timeout,
        })
    }

    /// The transaction was committed or aborted by its owner.
    pub fn end(mut self) {
        self.producer = None;
    }
}

impl Drop for Transaction {
    fn drop(&mut self) {
        let Some(producer) = self.producer.take() else {
            return;
        };
        let timeout = self.timeout;
        let abort = move || {
            if let Err(err) = producer.abort_transaction(timeout) {
                tracing::debug!(error = %err, "kafka backend: aborting a dropped transaction failed");
            }
        };
        clients::drop_blocking(DropRuns(Some(abort)));
    }
}

/// An opened producer, the fences partition its key lives in, and the last
/// seq applied under the key; `None` before the first.
pub struct Open {
    /// The transactional producer of the key.
    pub producer: Handle,
    /// The fences-topic partition the key lives in.
    pub partition: i32,
    /// The last seq applied under the key; `None` before the first.
    pub last: Option<i64>,
}

/// One fence key's producer, opened on first use; appends under one key run
/// one at a time.
#[derive(Default)]
pub struct Slot {
    /// The opened producer; `None` until first use or after it is fenced or
    /// fails.
    pub open: Option<Open>,
}

type Slots = HashMap<FenceKey, (u64, Arc<tokio::sync::Mutex<Slot>>)>;

/// At most `capacity` fence keys with a slot, each holding at most one open
/// producer. A key past it takes the least recently used idle key's place,
/// or waits for one to become idle.
pub struct Producers {
    capacity: usize,
    slots: Mutex<(u64, Slots)>,
    closed: AtomicBool,
    released: Notify,
}

/// A key's slot, held by one call; the key is idle once every lease of it is
/// dropped.
pub struct Lease<'a> {
    slot: Option<Arc<tokio::sync::Mutex<Slot>>>,
    producers: &'a Producers,
}

impl Lease<'_> {
    /// The held slot.
    pub fn slot(&self) -> &Arc<tokio::sync::Mutex<Slot>> {
        self.slot
            .as_ref()
            .unwrap_or_else(|| unreachable!("a lease holds its slot until dropped"))
    }
}

impl Drop for Lease<'_> {
    fn drop(&mut self) {
        // Idle once only the pool's own reference is left: drop this one
        // before waking whoever waits for an idle key.
        drop(self.slot.take());
        self.producers.released.notify_waiters();
    }
}

impl Producers {
    /// An empty pool of at most `capacity` keys, at least one.
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            slots: Mutex::default(),
            closed: AtomicBool::new(false),
            released: Notify::new(),
        }
    }

    /// The key's slot, once the pool has room for it; `None` once closed.
    pub async fn slot(&self, key: FenceKey) -> Option<Lease<'_>> {
        loop {
            let released = self.released.notified();
            tokio::pin!(released);
            released.as_mut().enable();
            if self.is_closed() {
                return None;
            }
            if let Some(slot) = self.try_slot(key) {
                return Some(Lease {
                    slot: Some(slot),
                    producers: self,
                });
            }
            released.await;
        }
    }

    fn try_slot(&self, key: FenceKey) -> Option<Arc<tokio::sync::Mutex<Slot>>> {
        let mut guard = self.slots.lock().unwrap_or_else(PoisonError::into_inner);
        let (tick, slots) = &mut *guard;
        *tick += 1;
        let now = *tick;
        if let Some((used, slot)) = slots.get_mut(&key) {
            *used = now;
            return Some(Arc::clone(slot));
        }
        if slots.len() >= self.capacity {
            let oldest_idle = slots
                .iter()
                .filter(|(_, (_, slot))| Arc::strong_count(slot) == 1)
                .min_by_key(|(_, (used, _))| *used)
                .map(|(key, _)| *key)?;
            if let Some((_, slot)) = slots.remove(&oldest_idle) {
                clients::drop_blocking(slot);
            }
        }
        let slot = Arc::new(tokio::sync::Mutex::new(Slot::default()));
        slots.insert(key, (now, Arc::clone(&slot)));
        Some(slot)
    }

    /// Keys holding a slot.
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.slots
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .1
            .len()
    }

    /// Whether [`close`](Self::close) has run.
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    /// Closes the pool: no slot after it, every producer dropped, every
    /// waiter woken.
    pub fn close(&self) {
        self.closed.store(true, Ordering::Release);
        let slots =
            std::mem::take(&mut self.slots.lock().unwrap_or_else(PoisonError::into_inner).1);
        clients::drop_blocking(slots);
        self.released.notify_waiters();
    }
}

/// A transactional producer for `transactional_id`, its transactions opened:
/// every earlier producer of the id is fenced.
///
/// `init_transactions` is tried again, with backoff, for as long as `budget`
/// allows while the coordinator is not ready - loading, moving, or still
/// settling an earlier producer's transaction (`ConcurrentTransactions`), as
/// on a broker that has just created its transaction state log - and a
/// producer left fatal by a failed attempt is replaced.
pub async fn open_producer(
    shared: &Shared,
    transactional_id: &str,
    operation: Operation,
    budget: Duration,
) -> Result<Handle, BackendError> {
    let deadline = tokio::time::Instant::now() + budget;
    let mut pause = FIRST_RETRY;
    let mut producer = None;
    loop {
        shared.check_running(operation)?;
        let current = match producer.take() {
            Some(current) => current,
            None => create_producer(shared, transactional_id, operation)?,
        };
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        let timeout = shared.settings.request_timeout.min(left).max(MIN_ATTEMPT);
        match blocking(current.producer(), move |p| p.init_transactions(timeout)).await {
            Ok(()) => return Ok(current),
            Err(err)
                if error::is_coordinator_busy(&err)
                    && tokio::time::Instant::now() + pause < deadline =>
            {
                tracing::debug!(error = %err, "kafka backend: transaction coordinator not ready; retrying");
                if current.producer().client().fatal_error().is_none() {
                    producer = Some(current);
                }
                tokio::time::sleep(pause).await;
                pause = (pause * 2).min(LAST_RETRY);
            }
            Err(err) => return Err(error::map(operation, &err)),
        }
    }
}

/// Between two `init_transactions` attempts: from the first, doubling to the
/// last.
const FIRST_RETRY: Duration = Duration::from_millis(100);
const LAST_RETRY: Duration = Duration::from_secs(2);
/// The shortest `init_transactions` attempt worth making.
const MIN_ATTEMPT: Duration = Duration::from_millis(500);

fn create_producer(
    shared: &Shared,
    transactional_id: &str,
    operation: Operation,
) -> Result<Handle, BackendError> {
    let settings = &shared.settings;
    let txn_timeout = settings.transaction_timeout.as_millis().to_string();
    let mut config = clients::base_config(settings);
    config
        .set("transactional.id", transactional_id)
        .set("transaction.timeout.ms", &txn_timeout)
        .set("message.timeout.ms", &txn_timeout)
        .set("enable.idempotence", "true")
        .set("acks", "all");
    // The transaction commits right after its last record: lingering only
    // delays it.
    clients::set_default(&mut config, "linger.ms", "0");
    let producer: FutureProducer = config.create().map_err(|e| error::map(operation, &e))?;
    Ok(Handle(Some(Arc::new(producer))))
}

/// Runs a blocking producer call off the async workers.
pub async fn blocking<F>(producer: &Arc<FutureProducer>, call: F) -> KafkaResult<()>
where
    F: FnOnce(&FutureProducer) -> KafkaResult<()> + Send + 'static,
{
    let producer = Arc::clone(producer);
    clients::detached(move || call(&producer)).await?
}

/// Aborts the open transaction if the producer still can, so it does not
/// hold back `read_committed` readers until it times out.
pub async fn abort(producer: &Arc<FutureProducer>, timeout: Duration) {
    if let Err(err) = blocking(producer, move |p| p.abort_transaction(timeout)).await {
        tracing::debug!(error = %err, "kafka backend: aborting a transaction failed");
    }
}

/// Opens the key's producer and reads the last seq applied under it.
pub async fn open_fence(shared: &Shared, key: FenceKey) -> Result<Open, BackendError> {
    const OP: Operation = Operation::Append;
    let partitions = shared.ensure_internal(OP).await?;
    let id = names::fence_transaction(&shared.settings.topic_prefix, key);
    let producer = open_producer(shared, &id, OP, shared.settings.request_timeout).await?;
    let partition = names::fence_partition(key, partitions);
    let tail = shared.fence_tail(partition);
    let mut tail = tail.lock().await;
    tail.catch_up(shared, &shared.fences_topic, partition, OP)
        .await?;
    let last = tail
        .get(&records::fence_key(key))
        .and_then(records::decode_fence);
    Ok(Open {
        producer,
        partition,
        last,
    })
}

/// Runs its call when dropped: hands a call to [`clients::drop_blocking`].
struct DropRuns<F: FnOnce()>(Option<F>);

impl<F: FnOnce()> Drop for DropRuns<F> {
    fn drop(&mut self) {
        if let Some(call) = self.0.take() {
            call();
        }
    }
}
