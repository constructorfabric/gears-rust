use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{Mutex, Notify};
use uuid::Uuid;

use crate::consumer::backend::SubscriptionInterest;
use crate::ids::{ConsumerGroupId, ProducerId, SubscriptionId};
use crate::internal::envelope::Event as WireEnvelope;
use crate::models::ConsumerGroupKind;

// ─── Topic & event log ────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub(super) struct EventTypeReg {
    pub data_schema: serde_json::Value,
    pub allowed_subject_types: Vec<String>,
}

/// Append-only event stored in the mock log.
#[derive(Debug, Clone)]
pub struct StoredEvent {
    pub event: WireEnvelope,
}

#[derive(Debug)]
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
        *self.next_offset.entry(partition).or_insert(0)
    }

    pub(super) fn append(&mut self, partition: u32, event: WireEnvelope) -> i64 {
        let offset = self.next_offset.entry(partition).or_insert(0);
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

// ─── Producer state ───────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub(super) struct ProducerReg {
    pub mode_str: String,
    pub owner_principal: String,
    pub owner_tenant: Uuid,
}

// ─── Consumer group ───────────────────────────────────────────────────────────

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
    /// Per-group topology version — bumped on every JOIN/LEAVE/expiry.
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

    /// Ensure a `CursorEntry` exists for `(topic, partition)`, creating one at `offset=0` if absent.
    pub(super) fn ensure_cursor(&mut self, topic: &str, partition: u32) {
        self.cursor
            .entry((topic.to_owned(), partition))
            .or_default();
    }
}

// ─── Subscription state ───────────────────────────────────────────────────────

/// Per-subscription ephemeral state (DESIGN §3.1 Subscription schema, wire-aligned).
#[derive(Debug)]
pub(super) struct SubState {
    pub group: ConsumerGroupId,
    pub client_agent: String,
    /// Per-member interests (topic-anchored typed-filter selections; C8/C8a rolling deploy).
    pub interests: Vec<SubscriptionInterest>,
    /// Derived from interests; used by rebalance eligibility check.
    pub topics: HashSet<String>,
    /// Partitions owned by this subscription (updated on every rebalance).
    pub assigned: Vec<(String, u32)>,
    /// Topology version *at last poll* — consumer detects change by comparing.
    pub topology_version: i64,
    /// Sort key for deterministic v1 round-robin rebalance.
    pub created_at: Instant,
    pub session_timeout: Duration,
    /// Refreshed on every poll/ack; `expires_at = last_seen_at + session_timeout`.
    pub last_seen_at: Instant,
    pub expires_at: Instant,
    /// Explicit seek override from `POST /positions`.
    pub seek: HashMap<(String, u32), i64>,
    /// Highest offset delivered-but-not-yet-acked per `(topic, partition)`.
    /// Reset to `cursor.offset` when a partition migrates → at-least-once redelivery.
    pub sent: HashMap<(String, u32), i64>,
}

// ─── Fault injection ──────────────────────────────────────────────────────────

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

// ─── Core aggregate ───────────────────────────────────────────────────────────

#[derive(Debug, Default)]
pub struct Core {
    pub topics: HashMap<String, TopicState>,
    pub producers: HashMap<ProducerId, ProducerReg>,
    /// Chain dedup state: last_sequence per `(producer_id, topic, partition)`.
    pub producer_state: HashMap<(ProducerId, String, u32), i64>,
    /// ALL registered groups (exists before any JOIN; needed for NotFound vs HasActiveMembers).
    pub groups_registry: HashMap<ConsumerGroupId, GroupReg>,
    /// Groups with ≥1 active subscription.
    pub groups: HashMap<ConsumerGroupId, GroupState>,
    pub subscriptions: HashMap<SubscriptionId, SubState>,
}

// ─── MockBroker ───────────────────────────────────────────────────────────────

/// In-memory mock Event Broker. Implements `EventBrokerTransport`, `EventBroker`,
/// and `EventBrokerStorageBackend`. All state is shared behind `Arc<Mutex<Core>>`.
///
/// Obtain via `MockBroker::new()` and pass to builders:
/// ```ignore
/// let mock = MockBroker::new();
/// let producer = ProducerBuilder::new(Arc::new(mock.clone())).build_sync(ctx).await?;
/// ```
#[derive(Clone, Debug)]
pub struct MockBroker {
    pub(super) core: Arc<Mutex<Core>>,
    /// Fires on every `publish`/`persist` to wake waiting stream readers.
    pub(super) notify: Arc<Notify>,
    pub(super) faults: Arc<Mutex<FaultConfig>>,
}

impl MockBroker {
    pub fn new() -> Self {
        Self {
            core: Arc::new(Mutex::new(Core::default())),
            notify: Arc::new(Notify::new()),
            faults: Arc::new(Mutex::new(FaultConfig::new())),
        }
    }
}

impl Default for MockBroker {
    fn default() -> Self {
        Self::new()
    }
}
