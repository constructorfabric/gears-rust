//! What the bench writes: topics, events and the stamp each event carries, and
//! the clock that stamp is read against.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use chrono::Utc;
use event_broker_sdk::backend::support::partition_for;
use event_broker_sdk::backend::{
    Backend, BackendError, KeyedEvent, LogClaim, LogId, Partition, Provisioning, Replacing,
    Retention, TopicPartition, TopicSpec,
};
use event_broker_sdk::models::Event;
use event_broker_sdk::{GtsInstanceId, GtsTypeId, Position};
use tokio::time::Instant;
use uuid::Uuid;

use crate::stats::nanos;

/// Every bench topic has this many partitions.
pub const PARTITIONS: u32 = 16;

/// Long enough that retention removes nothing a cell writes.
const KEEP: Retention = Retention {
    max_age: Some(Duration::from_hours(24)),
    max_bytes: None,
};

const PROVISION_DEADLINE: Duration = Duration::from_mins(1);
const READY_DEADLINE: Duration = Duration::from_mins(2);

/// splitmix64: cheap, seedable and good enough to spread keys.
pub struct Rng(u64);

impl Rng {
    /// Seeded from a random UUID.
    pub fn seeded() -> Self {
        Self(u64::try_from(Uuid::new_v4().as_u128() & u128::from(u64::MAX)).unwrap_or(1))
    }

    /// The next pseudo-random value.
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform-enough value in `0..bound`; a zero bound acts as 1.
    pub fn below(&mut self, bound: u32) -> u32 {
        u32::try_from(self.next_u64() % u64::from(bound.max(1))).unwrap_or(0)
    }
}

/// The phase's time base: stamps are nanoseconds since `origin`, and only
/// what falls in `[from, end)` is measured. The event cap can move both
/// earlier: `from` when warmup used its share of the budget, `end` when the
/// budget ran out. Clones share both.
#[derive(Debug, Clone)]
pub struct Clock {
    /// The instant stamps count from.
    pub origin: Instant,
    /// Where the measured window ends unless the event cap ends it earlier.
    pub to: Instant,
    /// Stamp of the measured window's start.
    from: Arc<AtomicU64>,
    /// Stamp of the measured window's end.
    end: Arc<AtomicU64>,
}

impl Clock {
    /// Starts now: `warmup` unmeasured, then `duration` measured.
    pub fn starting(warmup: Duration, duration: Duration) -> Self {
        let origin = Instant::now();
        let to = origin + warmup + duration;
        Self {
            origin,
            to,
            from: Arc::new(AtomicU64::new(nanos(warmup))),
            end: Arc::new(AtomicU64::new(nanos(to - origin))),
        }
    }

    /// `at` as nanoseconds since `origin`.
    pub fn stamp(&self, at: Instant) -> u64 {
        nanos(at.saturating_duration_since(self.origin))
    }

    /// The instant `stamp` nanoseconds after `origin`.
    pub fn instant(&self, stamp: u64) -> Instant {
        self.origin + Duration::from_nanos(stamp)
    }

    /// Starts the measured window at `at` if it is earlier than the current
    /// start.
    pub fn start_by(&self, at: Instant) {
        self.from.fetch_min(self.stamp(at), Ordering::Relaxed);
    }

    /// Start of the measured window.
    pub fn from(&self) -> Instant {
        self.instant(self.from.load(Ordering::Relaxed))
    }

    /// Ends the measured window at `at` if it is earlier than the current end.
    pub fn end_by(&self, at: Instant) {
        self.end.fetch_min(self.stamp(at), Ordering::Relaxed);
    }

    /// End of the measured window.
    pub fn end(&self) -> Instant {
        self.instant(self.end.load(Ordering::Relaxed))
    }

    /// Whether the event cap ended the window before `to`.
    pub fn capped(&self) -> bool {
        self.end() < self.to
    }

    /// Whether `at` falls in the measured window.
    pub fn measures(&self, at: Instant) -> bool {
        at >= self.from() && at < self.end()
    }

    /// Length of the measured window.
    pub fn measured(&self) -> Duration {
        self.end().saturating_duration_since(self.from())
    }
}

/// What every bench event's payload says about it.
#[derive(Debug, Clone, Copy)]
pub struct Stamp {
    /// The writer, unique within a phase.
    pub writer: u32,
    /// The writer's own count of events, from 1.
    pub n: u64,
    /// Index of the partition key.
    pub key: u32,
    /// When the append carrying it was scheduled, per [`Clock::stamp`].
    pub scheduled: u64,
}

impl Stamp {
    /// The stamp in `event`'s payload; `None` when it has none.
    pub fn read(event: &Event) -> Option<Self> {
        let data = event.data.as_ref()?;
        Some(Self {
            writer: u32::try_from(data.get("w")?.as_u64()?).ok()?,
            n: data.get("n")?.as_u64()?,
            key: u32::try_from(data.get("k")?.as_u64()?).ok()?,
            scheduled: data.get("t")?.as_u64()?,
        })
    }
}

/// The partition keys `k0..k{keys}` grouped by the partition they hash to, so
/// a batch can be built for one partition. Partitions no key hashes to are
/// left out.
pub struct KeyPools {
    pools: Vec<Vec<u32>>,
}

impl KeyPools {
    /// Groups the keys `k0..k{keys}`.
    pub fn new(keys: u32) -> Self {
        let mut pools = vec![Vec::new(); PARTITIONS as usize];
        for key in 0..keys {
            let at = partition_for(&format!("k{key}"), PARTITIONS);
            pools[at.value() as usize].push(key);
        }
        pools.retain(|pool| !pool.is_empty());
        Self { pools }
    }

    /// How many partitions have at least one key.
    pub fn len(&self) -> usize {
        self.pools.len()
    }
}

/// Builds the events one writer appends.
pub struct EventFactory {
    padding: Arc<str>,
    pools: Arc<KeyPools>,
    type_id: GtsTypeId,
    subject_type: GtsTypeId,
    tenant_id: Uuid,
    rng: Rng,
}

impl EventFactory {
    /// A factory with its own random stream.
    pub fn new(padding: Arc<str>, pools: Arc<KeyPools>) -> Self {
        Self {
            padding,
            pools,
            type_id: GtsTypeId::new(toolkit_gts::gts_id!(
                "cf.core.events.event.v1~cf.bench.x.happened.v1~"
            )),
            subject_type: GtsTypeId::new(toolkit_gts::gts_id!("cf.bench.x.thing.v1~")),
            tenant_id: Uuid::from_u128(0x0b0e_4c40_0001),
            rng: Rng::seeded(),
        }
    }

    /// One event of `writer`, its `n`-th, under a random key of the
    /// `pool`-th partition that has keys.
    pub fn build(&mut self, writer: u32, n: u64, scheduled: u64, pool: usize) -> KeyedEvent {
        let keys = &self.pools.pools[pool % self.pools.len()];
        let key = keys[self
            .rng
            .below(u32::try_from(keys.len()).unwrap_or(u32::MAX)) as usize];
        let id = Uuid::from_u64_pair(self.rng.next_u64(), self.rng.next_u64());
        KeyedEvent {
            key: format!("k{key}"),
            event: Event {
                id,
                type_id: self.type_id.clone(),
                tenant_id: self.tenant_id,
                source: "bench".to_owned(),
                subject: format!("k{key}"),
                subject_type: self.subject_type.clone(),
                occurred_at: Utc::now(),
                trace_parent: None,
                data: Some(serde_json::json!({
                    "w": writer,
                    "n": n,
                    "k": key,
                    "t": scheduled,
                    "pad": &*self.padding,
                })),
                partition: None,
                sequence: None,
                sequence_time: None,
                meta: None,
            },
        }
    }
}

/// Partition `partition` of `topic`.
pub fn partition(topic: &GtsInstanceId, partition: u32) -> TopicPartition {
    TopicPartition::new(topic.clone(), Partition::new(partition))
}

/// Provisions a topic no other cell uses, with [`PARTITIONS`] partitions,
/// waiting out `Pending`.
///
/// # Errors
/// What `provision` fails with, or `Unavailable` when it stays `Pending`.
pub async fn provision_topic(
    backend: &dyn Backend,
    label: &str,
) -> Result<TopicSpec, BackendError> {
    let unique = Uuid::new_v4().simple().to_string();
    let topic = GtsInstanceId::try_new(&format!(
        "{}cf.bench.{label}.t{}.v1",
        toolkit_gts::gts_id!("cf.core.events.topic.v1~"),
        &unique[..12]
    ))
    .map_err(|err| BackendError::Internal(format!("bench topic id: {err}")))?;
    let spec = TopicSpec {
        topic,
        partitions: PARTITIONS,
        retention: KEEP,
    };
    let claim = LogClaim {
        replacing: Replacing::Nothing,
        new: LogId::new_v4(),
    };
    let deadline = Instant::now() + PROVISION_DEADLINE;
    loop {
        match backend.provision(&spec, &claim).await? {
            Provisioning::Ready { partitions } if partitions == PARTITIONS => return Ok(spec),
            Provisioning::Ready { partitions } => {
                return Err(BackendError::Internal(format!(
                    "provisioned {partitions} partitions, the bench needs {PARTITIONS}"
                )));
            }
            Provisioning::Pending if Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            Provisioning::Pending => {
                return Err(BackendError::unavailable("provisioning stayed Pending"));
            }
        }
    }
}

/// Waits until `backend` answers something other than `Unavailable`: a
/// backend connects lazily, and its startup is not part of any cell.
///
/// # Errors
/// `Unavailable` when it does not within two minutes.
pub async fn wait_ready(backend: &dyn Backend) -> Result<(), BackendError> {
    let probe = GtsInstanceId::try_new(toolkit_gts::gts_id!(
        "cf.core.events.topic.v1~cf.bench.x.probe.v1"
    ))
    .map_err(|err| BackendError::Internal(format!("probe topic id: {err}")))?;
    let at = partition(&probe, 0);
    let deadline = Instant::now() + READY_DEADLINE;
    loop {
        match backend.resolve(&at, Position::Latest).await {
            Err(err @ BackendError::Unavailable { .. }) => {
                if Instant::now() >= deadline {
                    return Err(err);
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            _ => return Ok(()),
        }
    }
}
