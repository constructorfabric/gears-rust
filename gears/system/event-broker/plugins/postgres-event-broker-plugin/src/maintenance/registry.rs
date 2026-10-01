//! What this instance knows of each partition's segments, so `append` tells
//! with two compares whether a head calls for maintenance.
//!
//! The registry may lag the database, never lead it: each job writes the
//! entry of the partition it just changed from what it read in its own
//! transaction, and the rescan replaces them all. A stale entry only asks for
//! work that may already be done; the job finds that and answers `Done`.

use std::cmp::Reverse;
use std::collections::HashMap;
use std::sync::{PoisonError, RwLock};

use super::catalog::{Segment, SegmentState};
use super::prioritizer::Key;
use crate::segment;

/// Sequences counted between two counter runs on the open segment.
pub const COUNT_STEP: i64 = 100_000;

/// How far ahead of the head room is made.
const ROOM_MARGIN: i64 = 0;

/// How badly a partition needs segments: the extender's priority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Urgency {
    /// This many sequences of room are left; fewer is more urgent.
    Room(Reverse<i64>),
    /// No segment, an unknown layout, or an append found no segment.
    Urgent,
}

impl Urgency {
    /// `left` sequences of room.
    pub fn room(left: i64) -> Self {
        Self::Room(Reverse(left))
    }
}

/// One partition's segments, as this instance last saw them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    /// The open and sealed segments' `[first, end)`, oldest first.
    pub ranges: Vec<(i64, i64)>,
    /// The head at which room gets short.
    pub room_alarm: i64,
    /// The head at which the open segment is due a count.
    pub count_alarm: i64,
    /// Whether the topic has a retention bound.
    pub bounded: bool,
}

impl Layout {
    /// The layout of a partition with `segments`, oldest first.
    pub fn new(segments: &[Segment], bounded: bool) -> Self {
        let ranges: Vec<_> = segments
            .iter()
            .filter(|s| s.is_live())
            .map(|s| (s.first, s.end))
            .collect();
        let count_alarm = segments
            .iter()
            .find(|s| s.state == SegmentState::Open)
            .map_or(i64::MAX, |open| {
                open.counted_to.saturating_add(COUNT_STEP).min(open.end - 1)
            });
        let room_alarm = segment::room_alarm(&ranges, ROOM_MARGIN);
        Self {
            ranges,
            room_alarm,
            count_alarm,
            bounded,
        }
    }

    /// How urgently a partition at `head` needs room.
    pub fn urgency(&self, head: i64) -> Urgency {
        self.ranges
            .last()
            .map_or(Urgency::Urgent, |(_, end)| Urgency::room(end - 1 - head))
    }
}

/// What a head calls for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Check {
    /// Nothing.
    Fine,
    /// More segments.
    Room(Urgency),
    /// A count of the open segment.
    Count,
    /// Both.
    RoomAndCount(Urgency),
    /// The partition is not known here yet.
    Unknown,
}

/// The layouts of every partition this instance knows.
#[derive(Default)]
pub struct Registry {
    layouts: RwLock<HashMap<Key, Layout>>,
}

impl Registry {
    /// What `head` of `key` calls for: a lookup and two compares.
    pub fn check(&self, key: Key, head: i64) -> Check {
        let layouts = self.layouts.read().unwrap_or_else(PoisonError::into_inner);
        let Some(layout) = layouts.get(&key) else {
            return Check::Unknown;
        };
        match (head >= layout.room_alarm, head >= layout.count_alarm) {
            (false, false) => Check::Fine,
            (true, false) => Check::Room(layout.urgency(head)),
            (false, true) => Check::Count,
            (true, true) => Check::RoomAndCount(layout.urgency(head)),
        }
    }

    /// Sets the layout of `key`.
    pub fn replace(&self, key: Key, layout: Layout) {
        self.layouts
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(key, layout);
    }

    /// Sets every layout at once; partitions not in `layouts` are forgotten.
    pub fn replace_all(&self, layouts: HashMap<Key, Layout>) {
        *self.layouts.write().unwrap_or_else(PoisonError::into_inner) = layouts;
    }

    /// Forgets `key`, whose partition is gone.
    pub fn forget(&self, key: Key) {
        self.layouts
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&key);
    }
}
