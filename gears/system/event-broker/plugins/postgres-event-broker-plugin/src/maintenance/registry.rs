//! What this instance knows of each partition's segments, so `append` tells
//! with two compares whether a head calls for maintenance.
//!
//! The registry may lag the database, never lead it: each job writes the
//! entry of the partition it just changed from what it read in its own
//! transaction, and the rescan replaces every entry its snapshot read. A
//! stale entry only asks for work that may already be done; the job finds
//! that and answers `Done`.

use std::cmp::Reverse;
use std::collections::HashMap;
use std::sync::{PoisonError, RwLock};

use super::catalog::{Segment, SegmentState};
use super::prioritizer::Key;
use crate::segment;

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
        let room_alarm = segment::room_alarm(&ranges, ROOM_MARGIN);
        Self {
            ranges,
            room_alarm,
            seal_alarm,
        }
    }

    /// How urgently a partition at `head` needs room.
    pub fn urgency(&self, head: i64) -> Urgency {
        self.ranges
            .last()
            .map_or(Urgency::Urgent, |(_, end)| Urgency::room(end - 1 - head))
    }

    /// What a partition at `head` calls for.
    pub fn due(&self, head: i64) -> Due {
        Due {
            room: (head >= self.room_alarm).then(|| self.urgency(head)),
            seal: head >= self.seal_alarm,
        }
    }
}

/// What a head calls for, on a partition the registry knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Due {
    /// More segments, this urgently.
    pub room: Option<Urgency>,
    /// Sealing the full open segments.
    pub seal: bool,
}

/// What a head calls for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Check {
    /// What the layout says.
    Known(Due),
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
        layouts
            .get(&key)
            .map_or(Check::Unknown, |layout| Check::Known(layout.due(head)))
    }

    /// Sets the layout of `key`.
    pub fn replace(&self, key: Key, layout: Layout) {
        self.layouts
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(key, layout);
    }

    /// Sets every layout in `layouts` at once. A partition it does not name
    /// is kept: the snapshot `layouts` come from may predate it, and a
    /// partition that is gone is forgotten by the job that finds it so.
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
