//! What every operation of one instance shares: settings, clients, the
//! in-memory copies of the compacted topics, and the heads it knows.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use event_broker_sdk::GtsInstanceId;
use event_broker_sdk::Sequence;
use event_broker_sdk::backend::support::{LocalHeads, topic_uuid};
use event_broker_sdk::backend::{BackendError, TopicPartition};
use rdkafka::admin::{AdminOptions, NewTopic, TopicReplication};
use rdkafka::consumer::BaseConsumer;
use rdkafka::error::RDKafkaErrorCode;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::clients::{self, Admin, ConsumerPool, Lease};
use crate::error::{self, Operation};
use crate::fence::Producers;
use crate::names;
use crate::records::{LogRecord, log_key};
use crate::settings::KafkaSettings;
use crate::tail::{SharedTail, Tail};

/// A provisioned topic, as this instance last learned it. A topic is never
/// removed, so what is learned stays true.
#[derive(Debug, Clone)]
pub struct KnownTopic {
    /// The Kafka topic holding the log.
    pub name: Arc<str>,
    /// The partition count recorded when the topic was provisioned.
    pub partitions: u32,
}

/// The highest sequence a partition's last scan found, and the high
/// watermark it was found under: the same watermark needs no new scan.
#[derive(Debug, Clone, Copy)]
pub struct Latest {
    /// The high watermark the scan ran under.
    pub high: i64,
    /// The highest sequence the scan found.
    pub sequence: Sequence,
}

/// The state one plugin instance shares across its operations.
pub struct Shared {
    /// The compacted topic holding every topic's log record.
    pub logs_topic: String,
    /// The compacted topic holding every fence key's last applied seq.
    pub fences_topic: String,
    stopped: AtomicBool,
    admin: Mutex<Option<Arc<Admin>>>,
    watermark: Mutex<Option<Arc<BaseConsumer>>>,
    stable: Mutex<Option<Arc<BaseConsumer>>>,
    consumers: ConsumerPool,
    /// The fences topic's partition count, once both internal topics exist.
    internal: tokio::sync::OnceCell<u32>,
    logs: SharedTail,
    known: Mutex<HashMap<Uuid, KnownTopic>>,
    fence_tails: Mutex<HashMap<i32, SharedTail>>,
    /// Appends between their start and their end; `stop` waits for none.
    in_flight: AtomicUsize,
    drained: Notify,
    /// Provisioning attempts, run as tasks of their own.
    pub provisions: crate::provision::Jobs,
    /// The transactional producers, one per fence key in use.
    pub producers: Producers,
    /// The heads this instance has appended, known locally before the
    /// cluster reports them.
    pub heads: LocalHeads,
    latest: Mutex<HashMap<TopicPartition, Latest>>,
    /// The entry's settings.
    pub settings: KafkaSettings,
}

impl Shared {
    /// A running instance; every client is created on first need.
    pub fn new(settings: KafkaSettings) -> Arc<Self> {
        Arc::new(Self {
            logs_topic: names::logs_topic(&settings.topic_prefix),
            fences_topic: names::fences_topic(&settings.topic_prefix),
            stopped: AtomicBool::new(false),
            admin: Mutex::default(),
            watermark: Mutex::default(),
            stable: Mutex::default(),
            consumers: ConsumerPool::new(&settings),
            internal: tokio::sync::OnceCell::new(),
            logs: Arc::new(tokio::sync::Mutex::new(Tail::default())),
            known: Mutex::default(),
            fence_tails: Mutex::default(),
            in_flight: AtomicUsize::new(0),
            drained: Notify::new(),
            provisions: Mutex::default(),
            producers: Producers::new(settings.producer_pool),
            heads: LocalHeads::new(),
            latest: Mutex::default(),
            settings,
        })
    }

    /// `Unavailable` once [`stop`](Self::stop) has begun.
    pub fn check_running(&self, operation: Operation) -> Result<(), BackendError> {
        if self.stopped.load(Ordering::Acquire) {
            Err(error::stopped(operation))
        } else {
            Ok(())
        }
    }

    /// Marks an append in flight until the guard drops; `Unavailable` once
    /// stopped. Counted before the check, so `stop` either sees it or it sees
    /// `stop`.
    pub fn enter(&self, operation: Operation) -> Result<InFlight<'_>, BackendError> {
        self.in_flight.fetch_add(1, Ordering::AcqRel);
        let guard = InFlight { shared: self };
        self.check_running(operation)?;
        Ok(guard)
    }

    /// [`enter`](Self::enter) for a task that owns its share of the instance.
    pub fn enter_owned(
        self: &Arc<Self>,
        operation: Operation,
    ) -> Result<OwnedInFlight, BackendError> {
        self.in_flight.fetch_add(1, Ordering::AcqRel);
        let guard = OwnedInFlight {
            shared: Arc::clone(self),
        };
        self.check_running(operation)?;
        Ok(guard)
    }

    fn leave(&self) {
        if self.in_flight.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.drained.notify_waiters();
        }
    }

    /// Every operation after it answers `Unavailable`. Waits for the appends
    /// in flight to end until `deadline` is cancelled, then drops every
    /// client.
    pub async fn stop(&self, deadline: &CancellationToken) {
        self.stopped.store(true, Ordering::Release);
        loop {
            let drained = self.drained.notified();
            tokio::pin!(drained);
            drained.as_mut().enable();
            if self.in_flight.load(Ordering::Acquire) == 0 {
                break;
            }
            tokio::select! {
                () = drained => {}
                () = deadline.cancelled() => break,
            }
        }
        self.consumers.close();
        self.producers.close();
        if let Some(admin) = self
            .admin
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
            clients::drop_blocking(admin);
        }
        for slot in [&self.watermark, &self.stable] {
            if let Some(consumer) = slot.lock().unwrap_or_else(PoisonError::into_inner).take() {
                clients::drop_blocking(consumer);
            }
        }
    }

    /// The admin client, created on first need and kept.
    pub fn admin(&self, operation: Operation) -> Result<Arc<Admin>, BackendError> {
        self.check_running(operation)?;
        let mut admin = self.admin.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(admin) = admin.as_ref() {
            return Ok(Arc::clone(admin));
        }
        let created: Arc<Admin> = Arc::new(
            clients::base_config(&self.settings)
                .create()
                .map_err(|e| error::map(operation, &e))?,
        );
        *admin = Some(Arc::clone(&created));
        Ok(created)
    }

    /// The `read_uncommitted` consumer answering true high watermarks.
    pub fn watermark_consumer(
        &self,
        operation: Operation,
    ) -> Result<Arc<BaseConsumer>, BackendError> {
        self.offsets_consumer(&self.watermark, "read_uncommitted", operation)
    }

    /// The `read_committed` consumer answering last stable offsets, shared by
    /// every caller: it is never assigned anything.
    pub fn stable_consumer(&self, operation: Operation) -> Result<Arc<BaseConsumer>, BackendError> {
        self.offsets_consumer(&self.stable, "read_committed", operation)
    }

    fn offsets_consumer(
        &self,
        slot: &Mutex<Option<Arc<BaseConsumer>>>,
        isolation: &str,
        operation: Operation,
    ) -> Result<Arc<BaseConsumer>, BackendError> {
        self.check_running(operation)?;
        let mut consumer = slot.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(consumer) = consumer.as_ref() {
            return Ok(Arc::clone(consumer));
        }
        let created = Arc::new(
            clients::offsets_consumer(&self.settings, isolation)
                .map_err(|e| error::map(operation, &e))?,
        );
        *consumer = Some(Arc::clone(&created));
        Ok(created)
    }

    /// A pooled `read_committed` consumer; `Unavailable` once stopped.
    pub async fn lease(&self, operation: Operation) -> Result<Lease, BackendError> {
        self.check_running(operation)?;
        self.consumers
            .lease()
            .await
            .map_err(|_| error::stopped(operation))
    }

    /// Admin request options bounded by the entry's request timeout.
    pub fn admin_options(&self) -> AdminOptions {
        AdminOptions::new()
            .request_timeout(Some(self.settings.request_timeout))
            .operation_timeout(Some(self.settings.request_timeout))
    }

    /// Creates the logs and fence topics once per instance; an existing one
    /// is taken as it is. Answers the fences topic's partition count as the
    /// cluster holds it: `fence_partitions` only sizes a topic this call
    /// creates, and a key's partition must be the same in every instance.
    pub async fn ensure_internal(&self, operation: Operation) -> Result<u32, BackendError> {
        self.internal
            .get_or_try_init(|| async {
                let admin = self.admin(operation)?;
                let replication = i32::from(self.settings.replication_factor);
                let isr = self.settings.min_insync_replicas.to_string();
                let fences = i32::try_from(self.settings.fence_partitions).unwrap_or(i32::MAX);
                let topics = [
                    NewTopic::new(&self.logs_topic, 1, TopicReplication::Fixed(replication)),
                    NewTopic::new(
                        &self.fences_topic,
                        fences,
                        TopicReplication::Fixed(replication),
                    ),
                ]
                .map(|topic| {
                    topic
                        .set("cleanup.policy", "compact")
                        .set("min.insync.replicas", &isr)
                });
                let results = admin
                    .create_topics(&topics, &self.admin_options())
                    .await
                    .map_err(|e| error::map(operation, &e))?;
                for result in results {
                    match result {
                        Ok(_) | Err((_, RDKafkaErrorCode::TopicAlreadyExists)) => {}
                        Err((_, code)) => return Err(error::map_code(operation, code)),
                    }
                }
                let held = self.fences_partitions(&admin, operation).await?;
                if held != self.settings.fence_partitions {
                    tracing::warn!(
                        held,
                        configured = self.settings.fence_partitions,
                        "kafka backend: the fences topic has another partition count than \
                         `fence_partitions`; the topic's count is used"
                    );
                }
                Ok(held)
            })
            .await
            .copied()
    }

    /// The fences topic's partition count, every partition led; `Unavailable`
    /// until it is.
    async fn fences_partitions(
        &self,
        admin: &Arc<Admin>,
        operation: Operation,
    ) -> Result<u32, BackendError> {
        let admin = Arc::clone(admin);
        let topic = self.fences_topic.clone();
        let timeout = self.settings.request_timeout;
        let metadata =
            clients::detached(move || admin.inner().fetch_metadata(Some(&topic), timeout))
                .await
                .map_err(|_| error::stopped(operation))?
                .map_err(|e| error::map(operation, &e))?;
        let partitions = metadata
            .topics()
            .iter()
            .find(|t| t.name() == self.fences_topic && t.error().is_none())
            .map(rdkafka::metadata::MetadataTopic::partitions)
            .filter(|p| !p.is_empty() && p.iter().all(|p| p.leader() >= 0 && p.error().is_none()))
            .ok_or_else(|| {
                BackendError::unavailable(format!("{operation}: the fences topic is not ready"))
            })?;
        Ok(u32::try_from(partitions.len()).unwrap_or(u32::MAX))
    }

    /// The logs topic's copy, caught up.
    pub async fn logs(
        &self,
        operation: Operation,
    ) -> Result<tokio::sync::OwnedMutexGuard<Tail>, BackendError> {
        self.ensure_internal(operation).await?;
        let mut logs = Arc::clone(&self.logs).lock_owned().await;
        logs.catch_up(self, &self.logs_topic, 0, operation).await?;
        Ok(logs)
    }

    /// The fence-topic partition copy a key lives in, not caught up.
    pub fn fence_tail(&self, partition: i32) -> SharedTail {
        Arc::clone(
            self.fence_tails
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .entry(partition)
                .or_default(),
        )
    }

    /// Caches a topic this instance has provisioned or looked up.
    pub fn remember(&self, topic: Uuid, known: KnownTopic) {
        self.known
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(topic, known);
    }

    /// The topic, from memory or the logs topic; `NotProvisioned` when it
    /// holds no record of it.
    pub async fn topic(
        &self,
        topic: &GtsInstanceId,
        operation: Operation,
    ) -> Result<KnownTopic, BackendError> {
        self.check_running(operation)?;
        let uuid = topic_uuid(topic);
        if let Some(known) = self
            .known
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&uuid)
        {
            return Ok(known.clone());
        }
        let logs = self.logs(operation).await?;
        let record = logs
            .get(&log_key(uuid))
            .and_then(LogRecord::decode)
            .ok_or_else(|| BackendError::not_provisioned(operation.name()))?;
        let known = KnownTopic {
            name: Arc::from(names::event_topic(&self.settings.topic_prefix, uuid)),
            partitions: record.partitions,
        };
        self.remember(uuid, known.clone());
        Ok(known)
    }

    /// The topic and the Kafka partition of `at`, after checking both exist.
    pub async fn partition(
        &self,
        at: &TopicPartition,
        operation: Operation,
    ) -> Result<(KnownTopic, i32), BackendError> {
        let known = self.topic(&at.topic, operation).await?;
        if at.partition.value() >= known.partitions {
            return Err(BackendError::PartitionNotFound {
                detail: format!("{operation}: the topic has no such partition on this backend"),
            });
        }
        let partition = i32::try_from(at.partition.value()).unwrap_or(i32::MAX);
        Ok((known, partition))
    }

    /// The cached result of the partition's last head scan, if any.
    pub fn latest(&self, at: &TopicPartition) -> Option<Latest> {
        self.latest
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(at)
            .copied()
    }

    /// Keeps the higher of what is cached and `latest`: `Latest` never goes
    /// down.
    pub fn record_latest(&self, at: &TopicPartition, latest: Latest) -> Sequence {
        let mut cache = self.latest.lock().unwrap_or_else(PoisonError::into_inner);
        let entry = cache.entry(at.clone()).or_insert(latest);
        let sequence = entry.sequence.max(latest.sequence);
        *entry = Latest {
            high: latest.high,
            sequence,
        };
        sequence
    }
}

/// An append in flight; `stop` waits for every one to drop.
pub struct InFlight<'a> {
    shared: &'a Shared,
}

impl Drop for InFlight<'_> {
    fn drop(&mut self) {
        self.shared.leave();
    }
}

/// An in-flight operation run by a task that owns its share of the instance.
pub struct OwnedInFlight {
    shared: Arc<Shared>,
}

impl Drop for OwnedInFlight {
    fn drop(&mut self) {
        self.shared.leave();
    }
}
