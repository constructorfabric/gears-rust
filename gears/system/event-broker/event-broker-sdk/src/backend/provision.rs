//! What `provision` takes and answers (feature 0006 §2.3, feature 0007 §2.6).

use std::time::Duration;

use super::ids::LogId;
use crate::GtsInstanceId;

/// What a backend is told about a topic when it provisions its log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopicSpec {
    pub topic: GtsInstanceId,
    /// From the topic's resolved settings; the backend partitions keys into
    /// this many.
    pub partitions: u32,
    /// From the topic's resolved settings; the backend enforces it on its own
    /// schedule.
    pub retention: Retention,
}

/// Per-partition retention bounds; an unset bound does not limit the
/// partition.
///
/// The backend removes an event only once it is past a bound, and always the
/// oldest first, so what remains is a suffix of what was stored. When and in
/// what units it removes - single events or whole segments, how long an event
/// may outlive its bound - is the backend's own and documented with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Retention {
    /// An event stored longer ago than this is past the bound. Age is
    /// measured from the event's `sequence_time`, when the backend stored it -
    /// never from its `occurred_at`, which is the producer's claim and may be
    /// any time.
    pub max_age: Option<Duration>,
    /// An event outside the newest this-many bytes of its partition is past
    /// the bound.
    pub max_bytes: Option<u64>,
}

/// The log id a provisioning attempt asks the backend to hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LogClaim {
    pub replacing: Replacing,
    pub new: LogId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Replacing {
    /// No earlier attempt is being replaced.
    Nothing,
    /// The attempt being replaced.
    Log(LogId),
}

impl LogClaim {
    /// Whether a backend holding `stored` accepts this claim: when nothing is
    /// stored, when the stored id is the one being replaced, or when it is
    /// already `new` (the same attempt, continued). Anything else is a
    /// [`BackendError::LogConflict`](super::BackendError::LogConflict).
    #[must_use]
    pub fn accepts(&self, stored: Option<LogId>) -> bool {
        match stored {
            None => true,
            Some(id) if id == self.new => true,
            Some(id) => self.replacing == Replacing::Log(id),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provisioning {
    /// The log exists and accepts writes; `partitions` is the count the
    /// backend actually holds, which a pre-existing log may make differ from
    /// the spec's.
    Ready { partitions: u32 },
    /// Provisioning was started and has not finished; call `provision` again
    /// with the same `new`.
    Pending,
}
