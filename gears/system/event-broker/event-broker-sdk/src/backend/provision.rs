//! What `provision` takes and answers (feature 0006 §2.3, feature 0007 §2.6).

use std::time::Duration;

use super::BackendError;
use super::ids::LogId;
use crate::GtsInstanceId;

/// What a backend is told about a topic when it provisions its log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopicSpec {
    pub topic: GtsInstanceId,
    /// The count to create the log with; the backend partitions keys into
    /// this many. For a log the backend already holds, it is the count the
    /// backend reported when it provisioned that log.
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
    /// The log is already held under `new`: apply the settings only, never
    /// create. A backend holding nothing for the topic answers
    /// [`BackendError::NotProvisioned`] and creates nothing.
    Held,
}

impl LogClaim {
    /// Whether a backend holding nothing for the topic may create its log
    /// under this claim; `false` for [`Replacing::Held`]. A backend that
    /// creates before it reads what it holds skips the create when this is
    /// `false`.
    #[must_use]
    pub fn allows_creation(&self) -> bool {
        self.replacing != Replacing::Held
    }

    /// The compare-and-set every backend applies to what it holds for the
    /// topic, `stored`, before acting on the claim for `operation`: accepted
    /// when the stored id is already `new` (the same attempt, continued), when
    /// it is the one being replaced, or when nothing is stored and the claim
    /// allows creation.
    ///
    /// # Errors
    ///
    /// [`BackendError::NotProvisioned`] for a [`Replacing::Held`] claim over
    /// nothing; [`BackendError::LogConflict`] for any other stored id.
    pub fn check(&self, operation: &str, stored: Stored) -> Result<(), BackendError> {
        match (self.replacing, stored) {
            (Replacing::Held, Stored::Nothing) => Err(BackendError::not_provisioned(operation)),
            (_, Stored::Nothing) => Ok(()),
            (_, Stored::Log(id)) if id == self.new => Ok(()),
            (Replacing::Log(replaced), Stored::Log(id)) if id == replaced => Ok(()),
            (_, Stored::Log(_)) => Err(BackendError::log_conflict(operation)),
        }
    }
}

/// What a backend holds for a topic when it checks a [`LogClaim`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stored {
    /// The backend holds no log for the topic.
    Nothing,
    /// The backend holds the topic's log under this id.
    Log(LogId),
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
