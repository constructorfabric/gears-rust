use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use modkit_security::SecurityContext;

use crate::error::OffsetManagerError;
use crate::ids::ConsumerGroupId;

/// Where the consumer wants the broker to begin emitting for an assigned
/// `(topic, partition)`. The integer in [`ResolvedPosition::Exact`] is the
/// **last offset the consumer has already processed** — symmetric with the
/// argument to `save_on_eb` / `save_in_tx`. The broker computes "emit from
/// `offset + 1`" server-side.
///
/// The two sentinel variants are resolved by the broker at admission time:
/// `Earliest` → cursor set to `retention_floor - 1` (emit from `retention_floor`);
/// `Latest` → cursor set to the current high-water mark (emit only future events).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResolvedPosition {
    /// Last offset the consumer has processed; broker emits from offset + 1.
    Exact(i64),
    /// Broker-resolved: emit from the partition's retention floor onwards.
    Earliest,
    /// Broker-resolved: emit only events admitted after this SEEK.
    Latest,
}

/// Policy applied when no committed cursor exists for an assigned partition.
/// Required argument to the constructors of all built-in [`OffsetManager`]s.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fallback {
    Earliest,
    Latest,
}

impl From<Fallback> for ResolvedPosition {
    fn from(f: Fallback) -> Self {
        match f {
            Fallback::Earliest => ResolvedPosition::Earliest,
            Fallback::Latest => ResolvedPosition::Latest,
        }
    }
}

/// Pluggable offset persistence + start-position strategy.
/// Keyed by `(group, topic, partition)` per `evbk_group_offsets`.
///
/// `position(...)` is the single source of truth for "where should this
/// partition start?". It returns a definitive [`ResolvedPosition`]: either an
/// exact last-processed offset (verbatim from the backing store or from a
/// configured per-partition override), or a sentinel for the broker to resolve.
#[async_trait]
pub trait OffsetManager: Send + Sync {
    async fn save_on_eb(
        &self,
        ctx: &SecurityContext,
        group: &ConsumerGroupId,
        topic: &str,
        partition: u32,
        offset: i64,
    ) -> Result<(), OffsetManagerError>;

    async fn position(
        &self,
        ctx: &SecurityContext,
        group: &ConsumerGroupId,
        topic: &str,
        partition: u32,
    ) -> Result<ResolvedPosition, OffsetManagerError>;
}

/// Supertrait: adds atomic in-tx persistence to [`OffsetManager`].
/// Only implementations that can participate in a caller-supplied transaction
/// (e.g., `LocalDbOffsetManager`) implement this trait.
/// Requires the `outbox` feature (depends on `modkit-db`).
#[cfg(feature = "outbox")]
#[async_trait]
pub trait TxOffsetManager: OffsetManager {
    async fn save_in_tx<TX>(
        &self,
        txn: &TX,
        group: &ConsumerGroupId,
        topic: &str,
        partition: u32,
        offset: i64,
    ) -> Result<(), OffsetManagerError>
    where
        TX: modkit_db::secure::DBRunner + Sync + ?Sized;
}

// ---- Built-in implementations ----

/// Broker-only offset manager. Posts group-cursor acks to the Event Broker.
/// Does NOT implement [`TxOffsetManager`].
pub struct BrokerOffsetManager {
    fallback: Fallback,
    overrides: HashMap<(String, u32), i64>,
}

impl BrokerOffsetManager {
    /// Construct with the policy applied when no committed cursor exists.
    pub fn new(fallback: Fallback) -> Self {
        Self {
            fallback,
            overrides: HashMap::new(),
        }
    }

    /// Per-partition seed offsets, consulted only when the backing store
    /// reports no committed cursor. Override values are last-processed offsets
    /// (the same semantic as the integer carried by `ResolvedPosition::Exact`).
    ///
    /// **Operational note:** overrides are easy to forget about. A stale
    /// override left in source after a one-off replay will silently re-apply
    /// to any future fresh-partition assignment (rebalance, new partition
    /// added to topology, fresh group). Prefer constructing the override map
    /// at runtime from configuration / CLI flags / an admin tool rather than
    /// hardcoding offsets in source code.
    pub fn with_overrides(
        mut self,
        overrides: impl IntoIterator<Item = ((String, u32), i64)>,
    ) -> Self {
        self.overrides.extend(overrides);
        self
    }
}

#[async_trait]
impl OffsetManager for BrokerOffsetManager {
    async fn save_on_eb(
        &self,
        _ctx: &SecurityContext,
        _group: &ConsumerGroupId,
        _topic: &str,
        _partition: u32,
        _offset: i64,
    ) -> Result<(), OffsetManagerError> {
        // Full implementation in a follow-up task: POST ack to broker.
        Ok(())
    }

    async fn position(
        &self,
        _ctx: &SecurityContext,
        _group: &ConsumerGroupId,
        topic: &str,
        partition: u32,
    ) -> Result<ResolvedPosition, OffsetManagerError> {
        // Full implementation in a follow-up task: GET group cursor from broker.
        // For now the broker returns no cursor; fall through to overrides → fallback.
        if let Some(&off) = self.overrides.get(&(topic.to_owned(), partition)) {
            return Ok(ResolvedPosition::Exact(off));
        }
        Ok(self.fallback.into())
    }
}

/// DB-backed offset manager. Implements both [`OffsetManager`] and [`TxOffsetManager`].
/// Cursor table: `(group TEXT, topic TEXT, partition INTEGER, offset BIGINT, PK(group,topic,partition))`.
/// Requires the `outbox` feature.
#[cfg(feature = "outbox")]
pub struct LocalDbOffsetManager {
    _db: modkit_db::Db,
    _table: String,
    fallback: Fallback,
    overrides: HashMap<(String, u32), i64>,
}

#[cfg(feature = "outbox")]
impl LocalDbOffsetManager {
    pub fn new(db: modkit_db::Db, table: impl Into<String>, fallback: Fallback) -> Self {
        Self {
            _db: db,
            _table: table.into(),
            fallback,
            overrides: HashMap::new(),
        }
    }

    /// Per-partition seed offsets, consulted only when the DB has no row.
    pub fn with_overrides(
        mut self,
        overrides: impl IntoIterator<Item = ((String, u32), i64)>,
    ) -> Self {
        self.overrides.extend(overrides);
        self
    }
}

#[cfg(feature = "outbox")]
#[async_trait]
impl OffsetManager for LocalDbOffsetManager {
    async fn save_on_eb(
        &self,
        _ctx: &SecurityContext,
        _group: &ConsumerGroupId,
        _topic: &str,
        _partition: u32,
        _offset: i64,
    ) -> Result<(), OffsetManagerError> {
        Ok(())
    }

    async fn position(
        &self,
        _ctx: &SecurityContext,
        _group: &ConsumerGroupId,
        topic: &str,
        partition: u32,
    ) -> Result<ResolvedPosition, OffsetManagerError> {
        // Full implementation in a follow-up task: SELECT offset FROM <table>
        // WHERE group=? AND topic=? AND partition=?.
        // For now: no row → overrides → fallback.
        if let Some(&off) = self.overrides.get(&(topic.to_owned(), partition)) {
            return Ok(ResolvedPosition::Exact(off));
        }
        Ok(self.fallback.into())
    }
}

#[cfg(feature = "outbox")]
#[async_trait]
impl TxOffsetManager for LocalDbOffsetManager {
    async fn save_in_tx<TX>(
        &self,
        _txn: &TX,
        _group: &ConsumerGroupId,
        _topic: &str,
        _partition: u32,
        _offset: i64,
    ) -> Result<(), OffsetManagerError>
    where
        TX: modkit_db::secure::DBRunner + Sync + ?Sized,
    {
        Ok(())
    }
}

/// In-memory offset manager for tests. `save_on_eb` persists to a map (so the
/// test can assert round-trip); the real broker is not contacted.
pub struct InMemoryOffsetManager {
    inner: Mutex<HashMap<(String, String, u32), i64>>, // (group, topic, partition) → last-processed offset
    fallback: Fallback,
    overrides: HashMap<(String, u32), i64>,
}

impl InMemoryOffsetManager {
    pub fn new(fallback: Fallback) -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
            fallback,
            overrides: HashMap::new(),
        }
    }

    pub fn with_overrides(
        mut self,
        overrides: impl IntoIterator<Item = ((String, u32), i64)>,
    ) -> Self {
        self.overrides.extend(overrides);
        self
    }
}

#[async_trait]
impl OffsetManager for InMemoryOffsetManager {
    async fn save_on_eb(
        &self,
        _ctx: &SecurityContext,
        group: &ConsumerGroupId,
        topic: &str,
        partition: u32,
        offset: i64,
    ) -> Result<(), OffsetManagerError> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|_| OffsetManagerError::Internal("mutex poisoned".into()))?;
        let entry = guard
            .entry((group.0.clone(), topic.to_owned(), partition))
            .or_insert(0);
        *entry = (*entry).max(offset);
        Ok(())
    }

    async fn position(
        &self,
        _ctx: &SecurityContext,
        group: &ConsumerGroupId,
        topic: &str,
        partition: u32,
    ) -> Result<ResolvedPosition, OffsetManagerError> {
        let guard = self
            .inner
            .lock()
            .map_err(|_| OffsetManagerError::Internal("mutex poisoned".into()))?;
        if let Some(&stored) = guard.get(&(group.0.clone(), topic.to_owned(), partition)) {
            return Ok(ResolvedPosition::Exact(stored));
        }
        drop(guard);
        if let Some(&off) = self.overrides.get(&(topic.to_owned(), partition)) {
            return Ok(ResolvedPosition::Exact(off));
        }
        Ok(self.fallback.into())
    }
}
