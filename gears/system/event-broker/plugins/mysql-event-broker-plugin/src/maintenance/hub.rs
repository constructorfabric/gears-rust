//! What the operations and the maintenance of one instance share: what this
//! instance knows of each partition's segments, so `append` tells with two
//! compares whether a head calls for work, and the queue of partitions that
//! do.
//!
//! The registry may lag the database, never lead it: the maintenance writes
//! the entry of the partition it just changed from what it read under the
//! partition's lock, and the rescan replaces every entry it read. A stale
//! entry only asks for work that may already be done; the run finds that.

use std::collections::HashMap;
use std::sync::{PoisonError, RwLock};

use toolkit_taskward::Signal;

use super::catalog::{Segment, SegmentState};
use crate::heads::Key;
use crate::segment;

/// One partition's segments, as this instance last saw them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    /// The head at which room gets short.
    pub room_alarm: i64,
    /// The head at which the oldest open segment is full, and due a seal.
    pub seal_alarm: i64,
}

impl Layout {
    /// The layout of a partition with `segments`, oldest first.
    pub fn new(segments: &[Segment]) -> Self {
        let ranges: Vec<_> = segments
            .iter()
            .filter(|s| s.is_live())
            .map(|s| (s.first, s.end))
            .collect();
        let seal_alarm = segments
            .iter()
            .find(|s| s.state == SegmentState::Open)
            .map_or(i64::MAX, |open| open.end - 1);
        Self {
            room_alarm: segment::room_alarm(&ranges, 0),
            seal_alarm,
        }
    }

    /// Whether a partition at `head` calls for work.
    pub fn is_due(&self, head: i64) -> bool {
        head >= self.room_alarm || head >= self.seal_alarm
    }
}

/// The maintenance state of one instance.
#[derive(Default)]
pub struct Hub {
    layouts: RwLock<HashMap<Key, Layout>>,
    /// Partitions the maintenance worker is to look at.
    pub queue: Signal<Key>,
}

impl Hub {
    /// After an append to `key` committed up to `head`.
    pub fn appended(&self, key: Key, head: i64) {
        let due = self
            .layouts
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&key)
            // Not known here yet: whatever the head calls for, the run finds
            // out from the database.
            .is_none_or(|layout| layout.is_due(head));
        if due {
            self.queue.signal(key);
        }
    }

    /// After an append to `key` found no segment for a sequence.
    pub fn no_room(&self, key: Key) {
        self.queue.signal(key);
    }

    /// Sets the layout of `key`.
    pub fn replace(&self, key: Key, layout: Layout) {
        self.layouts
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(key, layout);
    }

    /// Sets every layout in `layouts` at once; a partition it does not name
    /// is kept.
    pub fn replace_all(&self, layouts: HashMap<Key, Layout>) {
        self.layouts
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .extend(layouts);
    }

    /// Forgets `key`, whose partition is gone.
    pub fn forget(&self, key: Key) {
        self.layouts
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&key);
    }
}
