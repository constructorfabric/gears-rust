//! What `append` takes and answers (feature 0007 §2.2).

use super::ids::{FenceKey, FenceSeq, TopicPartition};
use crate::GtsInstanceId;
use crate::models::Event;
use crate::sequence::Sequence;

/// Fencing token of one append: `seq` is strictly increasing per `key`, gaps
/// allowed.
///
/// A backend applies a fence at most once: an append whose `seq` is at or
/// below the last one applied under its `key` stores nothing and answers
/// [`Appended::Duplicate`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Fence {
    pub key: FenceKey,
    pub seq: FenceSeq,
}

#[derive(Debug, Clone, Copy)]
pub struct AppendRequest<'a> {
    pub fence: Fence,
    pub topic: &'a GtsInstanceId,
    /// At most [`Backend::max_events_per_append`](super::Backend::max_events_per_append).
    pub events: &'a [KeyedEvent],
    pub flush: Flush,
}

/// One event and the key the backend partitions it by.
#[derive(Debug, Clone)]
pub struct KeyedEvent {
    /// The value at the event type's partition-key pointer (ADR-0002),
    /// resolved by ingest.
    pub key: String,
    /// Carries no partition and no sequence: both are the backend's to assign.
    pub event: Event,
}

/// When `append` returns. Either way it returns only after every event is
/// durably stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flush {
    /// Return only once positions are assigned and readable; the answer is
    /// [`Appended::Heads`].
    Sync,
    /// Do not wait for assignment or visibility. A plugin may treat this as
    /// `Sync`.
    Async,
}

/// Which one comes back is the implementation's choice under
/// [`Flush::Async`], and may differ per call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Appended {
    /// The highest sequence per partition this call touched.
    Heads(Vec<Head>),
    /// Stored; positions become known through `watch`.
    Accepted,
    /// Already stored under this or a later fence of the same key; nothing
    /// written.
    Duplicate,
}

/// How a backend keeps the exactly-once-per-[`Fence`] guarantee; declared once
/// and logged at startup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dedup {
    /// The events and the last applied seq are written atomically; a fence at
    /// or below it is `Duplicate`.
    Fenced,
    /// Each target stream drops fences at or below the highest it has seen
    /// under this key; a partially stored message is completed by resending it
    /// whole.
    ProducerSequence,
    /// One create-once object per fence.
    CreateOnce,
    /// No deduplication: a retried message may be stored twice.
    AtLeastOnce,
}

/// The highest sequence assigned in one partition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Head {
    pub at: TopicPartition,
    pub sequence: Sequence,
}
