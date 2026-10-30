//! Identities the backend contract names things by.

use std::fmt;

use uuid::Uuid;

use crate::GtsInstanceId;

/// One partition of a topic, in `0..partitions`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Partition(u32);

impl Partition {
    #[must_use]
    pub const fn new(partition: u32) -> Self {
        Self(partition)
    }

    #[must_use]
    pub const fn value(self) -> u32 {
        self.0
    }
}

impl fmt::Display for Partition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// One partition of one topic: the unit sequences, heads and reads are scoped
/// to.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TopicPartition {
    pub topic: GtsInstanceId,
    pub partition: Partition,
}

impl TopicPartition {
    #[must_use]
    pub fn new(topic: GtsInstanceId, partition: Partition) -> Self {
        Self { topic, partition }
    }
}

/// One topic's log inside one backend (feature 0006 §2.1): minted by ingest
/// at every provisioning attempt and stored by the backend with the log.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LogId(Uuid);

impl LogId {
    /// A fresh id for a new provisioning attempt.
    #[must_use]
    pub fn new_v4() -> Self {
        Self(Uuid::new_v4())
    }

    #[must_use]
    pub const fn from_uuid(id: Uuid) -> Self {
        Self(id)
    }

    #[must_use]
    pub const fn uuid(self) -> Uuid {
        self.0
    }
}

impl fmt::Display for LogId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// The key one fence is kept under: appends carrying equal keys share one
/// [`FenceSeq`] space. Opaque - the caller builds it, a backend stores and
/// compares it but never looks inside.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FenceKey(u128);

impl FenceKey {
    #[must_use]
    pub const fn new(key: u128) -> Self {
        Self(key)
    }

    #[must_use]
    pub const fn value(self) -> u128 {
        self.0
    }

    /// The same 16 bytes as a UUID, for a native 16-byte column.
    #[must_use]
    pub const fn uuid(self) -> Uuid {
        Uuid::from_u128(self.0)
    }

    #[must_use]
    pub const fn from_uuid(id: Uuid) -> Self {
        Self(id.as_u128())
    }
}

/// Thirty-two lowercase hex digits: the form names and logs use.
impl fmt::Display for FenceKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:032x}", self.0)
    }
}

/// A fence's position: strictly increasing per [`FenceKey`], gaps allowed. A
/// separate space from the backend's [`Sequence`](crate::Sequence).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FenceSeq(i64);

impl FenceSeq {
    #[must_use]
    pub const fn new(seq: i64) -> Self {
        Self(seq)
    }

    #[must_use]
    pub const fn value(self) -> i64 {
        self.0
    }
}

impl fmt::Display for FenceSeq {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}
