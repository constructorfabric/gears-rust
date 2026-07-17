use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{Mutex, Notify};
use uuid::Uuid;

use crate::api::{ProducerMode, SubscriptionInterest};
use crate::ids::{ConsumerGroupId, ProducerId, SubscriptionId};
use crate::models::ConsumerGroupKind;
use crate::models::Event;

// --- Topic & event log --------------------------------------------------------

#[derive(Debug, Clone)]
pub(super) struct EventTypeReg {
    pub data_schema: serde_json::Value,
    pub allowed_subject_types: Vec<String>,
}

/// Append-only event stored in the mock log.
#[derive(Debug, Clone)]
pub struct StoredEvent {
    pub event: Event,
}

#[derive(Debug, Clone)]
pub(super) struct TopicState {
    pub partitions: u32,
    pub event_types: HashMap<String, EventTypeReg>, // type_id → reg
    pub log: HashMap<u32, Vec<StoredEvent>>,        // partition → events (offset == index)
    pub next_offset: HashMap<u32, i64>,
}

impl TopicState {
    pub(super) fn new(partitions: u32) -> Self {
        Self {
            partitions,
            event_types: HashMap::new(),
            log: HashMap::new(),
            next_offset: HashMap::new(),
        }
    }

    pub(super) fn next_offset_for(&mut self, partition: u32) -> i64 {
        // Offsets are 1-based (A6/A7): sequence floor is 1, never 0.
        *self.next_offset.entry(partition).or_insert(1)
    }

    pub(super) fn append(&mut self, partition: u32, event: Event) -> i64 {
        // Offsets are 1-based (A6/A7): the first event on a partition is offset 1.
        let offset = self.next_offset.entry(partition).or_insert(1);
        let assigned = *offset;
        self.log
            .entry(partition)
            .or_default()
            .push(StoredEvent { event });
        *offset += 1;
        assigned
    }

    pub(super) fn read(
        &self,
        partition: u32,
        start_offset: i64,
        max_count: usize,
    ) -> Vec<&StoredEvent> {
        self.log
            .get(&partition)
            .map(|log| {
                log.iter()
                    .skip(start_offset.max(0) as usize)
                    .take(max_count)
                    .collect()
            })
            .unwrap_or_default()
    }
}

// --- Producer state -----------------------------------------------------------

#[derive(Debug, Clone)]
pub(super) struct ProducerReg {
    pub mode: ProducerMode,
}

// --- Consumer group -----------------------------------------------------------

#[derive(Debug, Clone)]
pub(super) struct GroupReg {
    pub kind: ConsumerGroupKind,
    pub owner_tenant: Uuid,
    pub owner_principal: String,
}

/// Group-scoped cursor position (per-`(topic, partition)`).
#[derive(Debug, Clone, Default)]
pub struct CursorEntry {
    /// Session cursor set by SEEK. Broker emits from offset+1.
    pub offset: i64,
    /// Highest offset the broker has scanned for this group/partition (offset-adviser).
    pub last_examined: i64,
}

/// Runtime state for a group with ≥1 active subscription.
#[derive(Debug)]
pub(super) struct GroupState {
    /// Sorted by `(created_at, id)` for deterministic v1 rebalance.
    pub members: Vec<SubscriptionId>,
    /// Per-group topology version - bumped on every JOIN/LEAVE/expiry.
    pub topology_version: i64,
    /// Inverted map: `(topic, partition)` → owning subscription.
    pub assignments: HashMap<(String, u32), SubscriptionId>,
    /// Group-scoped cursors (sticky across subscription churn).
    pub cursor: HashMap<(String, u32), CursorEntry>,
}

impl GroupState {
    pub(super) fn new() -> Self {
        Self {
            members: Vec::new(),
            topology_version: 0,
            assignments: HashMap::new(),
            cursor: HashMap::new(),
        }
    }
}

// --- Subscription state -------------------------------------------------------

/// Per-subscription ephemeral state (DESIGN §3.1 Subscription schema, wire-aligned).
#[derive(Debug)]
pub(super) struct SubState {
    pub group: ConsumerGroupId,
    /// Per-member interests (topic-anchored typed-filter selections; C8/C8a rolling deploy).
    pub interests: Vec<SubscriptionInterest>,
    /// Derived from interests; used by rebalance eligibility check.
    pub topics: HashSet<String>,
    /// Partitions owned by this subscription (updated on every rebalance).
    pub assigned: Vec<(String, u32)>,
    /// Topology version *at last poll* - consumer detects change by comparing.
    pub topology_version: i64,
    /// Sort key for deterministic v1 round-robin rebalance.
    pub created_at: Instant,
    pub session_timeout: Duration,
    /// Refreshed on stream/seek; `expires_at = now + session_timeout`.
    pub expires_at: Instant,
    /// Explicit per-partition seek override (set by SEEK; last-processed offset).
    pub seek: HashMap<(String, u32), i64>,
    /// Highest offset delivered per `(topic, partition)` (last-processed; SEEK is the
    /// only cursor-advance mechanism - there is no ack). Reset on partition migration
    /// for at-least-once redelivery.
    pub sent: HashMap<(String, u32), i64>,
    /// Highest offset SCANNED per `(topic, partition)` regardless of filter match
    /// (the offset-adviser frontier). Drives `Control{Progress}` emission and the
    /// read skip-cursor so server-side-filtered events are not re-scanned.
    pub scanned: HashMap<(String, u32), i64>,
    /// Set when the subscription is terminated by a gain / lose-all rebalance
    /// (after emitting the terminal control frame). Any reuse of this
    /// `subscription_id` thereafter returns `410 SubscriptionTerminated`.
    pub terminated: bool,
}

// --- Fault injection ----------------------------------------------------------

#[derive(Debug, Default)]
pub struct FaultConfig {
    /// Immediately terminates the stream with a 410-equivalent error.
    pub force_gone: HashSet<SubscriptionId>,
    /// Immediately terminates the stream with a 404-equivalent error.
    pub force_not_found: HashSet<SubscriptionId>,
    /// Immediately fires session_timeout for this sub → triggers rebalance (C6/C9).
    pub expire_sub: HashSet<SubscriptionId>,
    /// If set, `persist` and `publish` return an error matching the rule (M3 chain-gap surface).
    pub reject_persist: Option<String>,
    /// Producer rate-limit allowance. When `Some(n)`, the next `n` publishes
    /// (single or per-event in a batch) succeed; once the allowance is exhausted,
    /// further publishes return `EventBrokerError::RateLimited` (429-equivalent).
    /// `Some(0)` refuses the very next publish. `None` disables the limit.
    pub publish_rate_limit: Option<u32>,
    /// Heartbeat cadence for the stream. Tests set it tiny/zero to trigger heartbeats quickly.
    pub heartbeat_interval: Duration,
}

impl FaultConfig {
    pub fn new() -> Self {
        Self {
            heartbeat_interval: Duration::from_secs(5),
            ..Default::default()
        }
    }
}

// --- Core aggregate -----------------------------------------------------------

#[derive(Debug, Default)]
pub struct Core {
    pub(super) topics: HashMap<String, TopicState>,
    pub(super) producers: HashMap<ProducerId, ProducerReg>,
    /// Chain dedup state: last_sequence per `(producer_id, topic, partition)`.
    pub(super) producer_state: HashMap<(ProducerId, String, u32), i64>,
    /// ALL registered groups (exists before any JOIN; needed for NotFound vs HasActiveMembers).
    pub(super) groups_registry: HashMap<ConsumerGroupId, GroupReg>,
    /// Groups with ≥1 active subscription.
    pub(super) groups: HashMap<ConsumerGroupId, GroupState>,
    pub(super) subscriptions: HashMap<SubscriptionId, SubState>,
}

// --- MockBroker ---------------------------------------------------------------

/// In-memory mock Event Broker. Implements `EventBrokerTransport`, `EventBroker`,
/// and `EventBrokerStorageBackend`. All state is shared behind `Arc<Mutex<Core>>`.
///
/// Obtain via `MockBroker::new()` and pass to producers:
/// ```ignore
/// let mock = MockBroker::new();
/// let producer = Producer::builder().broker(Arc::new(mock.clone()));
/// ```
#[derive(Clone, Debug)]
pub struct MockBroker {
    pub(super) core: Arc<Mutex<Core>>,
    /// Fires on every `publish`/`persist` to wake waiting stream readers.
    pub(super) notify: Arc<Notify>,
    pub(super) faults: Arc<Mutex<FaultConfig>>,
    /// Subscriptions with a currently-open stream. A second `stream()` or any
    /// `seek()` while present is rejected with `StreamingInProgress` (A2). A
    /// `std::sync::Mutex` so the stream's `Drop` guard can clear it synchronously.
    pub(super) streaming: Arc<std::sync::Mutex<HashSet<crate::ids::SubscriptionId>>>,
}

impl MockBroker {
    pub fn new() -> Self {
        Self {
            core: Arc::new(Mutex::new(Core::default())),
            notify: Arc::new(Notify::new()),
            faults: Arc::new(Mutex::new(FaultConfig::new())),
            streaming: Arc::new(std::sync::Mutex::new(HashSet::new())),
        }
    }
}

impl Default for MockBroker {
    fn default() -> Self {
        Self::new()
    }
}
