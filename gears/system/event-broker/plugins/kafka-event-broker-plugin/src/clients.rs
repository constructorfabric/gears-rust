//! The librdkafka clients one instance holds (`docs/DESIGN.md` §3): an admin
//! client, a pool of `read_committed` consumers, and the configuration every
//! client starts from.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use rdkafka::ClientConfig;
use rdkafka::admin::AdminClient;
use rdkafka::client::DefaultClientContext;
use rdkafka::consumer::{BaseConsumer, Consumer, StreamConsumer};
use rdkafka::error::{KafkaError, KafkaResult};
use rdkafka::message::{Message, Timestamp};
use rdkafka::topic_partition_list::{Offset, TopicPartitionList};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::time::Instant;

use crate::settings::KafkaSettings;

/// The admin client every instance holds.
pub type Admin = AdminClient<DefaultClientContext>;

/// The configuration every client of this entry starts from: the cluster, the
/// pass-through properties, and the request timeout.
pub fn base_config(settings: &KafkaSettings) -> ClientConfig {
    let mut config = ClientConfig::new();
    config.set("bootstrap.servers", &settings.bootstrap_servers);
    for (name, value) in &settings.client {
        config.set(name, value);
    }
    let timeout = settings.request_timeout.as_millis().to_string();
    config.set("socket.timeout.ms", &timeout);
    config
}

/// A consumer that only ever reads assigned partitions: it never joins its
/// group, commits or stores an offset.
fn consumer_config(settings: &KafkaSettings, isolation: &str) -> ClientConfig {
    let mut config = base_config(settings);
    config
        .set("group.id", format!("{}.reader", settings.topic_prefix))
        .set("enable.auto.commit", "false")
        .set("enable.auto.offset.store", "false")
        .set("isolation.level", isolation)
        // Below the log start offset (retention removed it) reads from the
        // oldest stored record.
        .set("auto.offset.reset", "earliest")
        .set("enable.partition.eof", "true");
    // A read past the head should not wait for data that is not coming.
    set_default(&mut config, "fetch.wait.max.ms", "10");
    set_default(&mut config, "queued.max.messages.kbytes", "8192");
    config
}

/// Sets `name` unless the entry's `client` settings already did.
pub fn set_default(config: &mut ClientConfig, name: &str, value: &str) {
    if config.get(name).is_none() {
        config.set(name, value);
    }
}

/// A pooled consumer, back in the pool on drop.
pub struct Lease {
    consumer: Option<Arc<StreamConsumer>>,
    pool: Arc<Mutex<Vec<Arc<StreamConsumer>>>>,
    _permit: OwnedSemaphorePermit,
}

impl Lease {
    /// The leased consumer.
    pub fn consumer(&self) -> &Arc<StreamConsumer> {
        self.consumer
            .as_ref()
            .unwrap_or_else(|| unreachable!("a lease holds its consumer until dropped"))
    }

    /// Drops the consumer instead of returning it: after an error its state
    /// is not to be trusted.
    pub fn discard(mut self) {
        if let Some(consumer) = self.consumer.take() {
            drop_blocking(consumer);
        }
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        if let Some(consumer) = self.consumer.take() {
            // Stops the background fetching of what the lease assigned.
            if consumer.unassign().is_ok() {
                self.pool
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(consumer);
            } else {
                drop_blocking(consumer);
            }
        }
    }
}

/// Destroying a librdkafka client flushes and joins its threads: off the
/// async workers, on a thread of its own (see [`detached`]).
pub fn drop_blocking<T: Send + 'static>(client: T) {
    run_detached(move || drop(client));
}

/// Runs a blocking librdkafka call on a detached thread and awaits its
/// answer. Not the runtime's blocking pool: a runtime shutting down waits for
/// every task on that pool, and a librdkafka call may block for a whole
/// request timeout; a detached thread holds nothing up.
pub async fn detached<F, R>(call: F) -> Result<R, KafkaError>
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    let (tx, rx) = tokio::sync::oneshot::channel();
    run_detached(move || {
        // The caller gave up waiting: nobody wants the answer.
        drop(tx.send(call()));
    });
    rx.await.map_err(|_| KafkaError::Canceled)
}

fn run_detached<F: FnOnce() + Send + 'static>(call: F) {
    let slot = Arc::new(Mutex::new(Some(call)));
    let inner = Arc::clone(&slot);
    let spawned = std::thread::Builder::new()
        .name("evbk-kafka".to_owned())
        .spawn(move || take_and_run(&inner));
    if let Err(err) = spawned {
        tracing::warn!(error = %err, "kafka backend: no thread for a blocking call; running it in place");
        take_and_run(&slot);
    }
}

fn take_and_run<F: FnOnce()>(slot: &Mutex<Option<F>>) {
    let call = slot.lock().unwrap_or_else(PoisonError::into_inner).take();
    if let Some(call) = call {
        call();
    }
}

/// At most `consumer_pool` consumers, created on first need and kept.
pub struct ConsumerPool {
    config: ClientConfig,
    idle: Arc<Mutex<Vec<Arc<StreamConsumer>>>>,
    permits: Arc<Semaphore>,
}

impl ConsumerPool {
    /// An empty pool; consumers are created on first lease.
    pub fn new(settings: &KafkaSettings) -> Self {
        Self {
            config: consumer_config(settings, "read_committed"),
            idle: Arc::default(),
            permits: Arc::new(Semaphore::new(settings.consumer_pool)),
        }
    }

    /// A consumer once a permit is free, reusing an idle one when there is
    /// one; fails once the pool is closed.
    pub async fn lease(&self) -> KafkaResult<Lease> {
        let permit = Arc::clone(&self.permits)
            .acquire_owned()
            .await
            .map_err(|_| KafkaError::Canceled)?;
        let pooled = self
            .idle
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pop();
        let consumer = match pooled {
            Some(consumer) => consumer,
            None => Arc::new(self.config.create::<StreamConsumer>()?),
        };
        Ok(Lease {
            consumer: Some(consumer),
            pool: Arc::clone(&self.idle),
            _permit: permit,
        })
    }

    /// Closes the pool: no lease after it, and every idle consumer dropped.
    pub fn close(&self) {
        self.permits.close();
        let idle = std::mem::take(&mut *self.idle.lock().unwrap_or_else(PoisonError::into_inner));
        drop_blocking(idle);
    }
}

/// A consumer only asked for watermarks, never assigned: `read_uncommitted`
/// answers the true high watermark (what a catch-up must see below, open
/// transactions included), `read_committed` the last stable offset.
pub fn offsets_consumer(settings: &KafkaSettings, isolation: &str) -> KafkaResult<BaseConsumer> {
    consumer_config(settings, isolation).create::<BaseConsumer>()
}

/// One record, owned.
#[derive(Debug, Clone)]
pub struct Record {
    /// The record's offset in its partition.
    pub offset: i64,
    /// Milliseconds since the epoch: the broker's `LogAppendTime`.
    pub timestamp: Option<i64>,
    /// The record key; `None` for a keyless record.
    pub key: Option<Vec<u8>>,
    /// The record payload; `None` for a tombstone.
    pub value: Option<Vec<u8>>,
}

/// What [`consume`] read.
#[derive(Debug, Default)]
pub struct Consumed {
    /// The records read, in offset order.
    pub records: Vec<Record>,
    /// Every committed record below `end` was seen (or `max` was reached).
    pub complete: bool,
}

/// What [`consume`] reads.
#[derive(Debug, Clone, Copy)]
pub struct Span<'a> {
    /// The Kafka topic to read.
    pub topic: &'a str,
    /// The Kafka partition to read.
    pub partition: i32,
    /// The first offset wanted.
    pub start: i64,
    /// Reading stops once the position reaches it.
    pub end: i64,
    /// Reading stops once this many records are read.
    pub max: usize,
    /// Reading gives up, incomplete, after this long.
    pub timeout: Duration,
    /// Whether reaching the end of what is readable now also ends the read.
    /// A `read_committed` consumer reaches it at the last stable offset,
    /// before any open transaction: right for a read, too early for a
    /// catch-up that must see every transaction below `end` decided.
    pub eof_ends: bool,
}

/// How long one wait for a record lasts before the position is checked: a
/// tail of control records or aborted ones moves the position without
/// yielding a record.
const STEP: Duration = Duration::from_millis(20);

/// The committed records of `span`, in offset order.
pub async fn consume(consumer: &StreamConsumer, span: Span<'_>) -> KafkaResult<Consumed> {
    let mut out = Consumed::default();
    if span.start >= span.end || span.max == 0 {
        out.complete = true;
        return Ok(out);
    }
    let mut assignment = TopicPartitionList::new();
    assignment.add_partition_offset(span.topic, span.partition, Offset::Offset(span.start))?;
    consumer.assign(&assignment)?;
    let deadline = Instant::now() + span.timeout;
    loop {
        let now = Instant::now();
        if now >= deadline {
            return Ok(out);
        }
        match tokio::time::timeout(STEP.min(deadline - now), consumer.recv()).await {
            Ok(Ok(message)) => {
                if message.topic() != span.topic
                    || message.partition() != span.partition
                    || message.offset() < span.start
                {
                    continue;
                }
                if message.offset() >= span.end {
                    out.complete = true;
                    return Ok(out);
                }
                let offset = message.offset();
                out.records.push(Record {
                    offset,
                    timestamp: match message.timestamp() {
                        Timestamp::NotAvailable => None,
                        Timestamp::CreateTime(ms) | Timestamp::LogAppendTime(ms) => Some(ms),
                    },
                    key: message.key().map(<[u8]>::to_vec),
                    value: message.payload().map(<[u8]>::to_vec),
                });
                if out.records.len() >= span.max || offset + 1 >= span.end {
                    out.complete = true;
                    return Ok(out);
                }
            }
            Ok(Err(KafkaError::PartitionEOF(partition))) if partition == span.partition => {
                if span.eof_ends || position(consumer, span)? >= span.end {
                    out.complete = true;
                    return Ok(out);
                }
            }
            Ok(Err(KafkaError::PartitionEOF(_))) => {}
            Ok(Err(err)) => return Err(err),
            Err(_) => {
                if position(consumer, span)? >= span.end {
                    out.complete = true;
                    return Ok(out);
                }
            }
        }
    }
}

/// The consumer's position in `span`'s partition; `-1` when it has none yet.
fn position(consumer: &StreamConsumer, span: Span<'_>) -> KafkaResult<i64> {
    let positions = consumer.position()?;
    Ok(positions
        .find_partition(span.topic, span.partition)
        .and_then(|element| match element.offset() {
            Offset::Offset(offset) => Some(offset),
            _ => None,
        })
        .unwrap_or(-1))
}

/// `(low, high)` watermarks of a partition, off the async workers.
pub async fn watermarks<C>(
    consumer: Arc<C>,
    topic: String,
    partition: i32,
    timeout: Duration,
) -> KafkaResult<(i64, i64)>
where
    C: Consumer + Send + Sync + 'static,
{
    detached(move || consumer.fetch_watermarks(&topic, partition, timeout)).await?
}
