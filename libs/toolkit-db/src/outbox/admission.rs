//! What the enqueue path consults, so it can decide admission without reading
//! the database.
//!
//! Two states per partition, and the enqueue's answer is their sum:
//!
//! 1. a **snapshot** - the four channel counters as last read from the counter
//!    table. This is the shared picture, and the only thing that can see what
//!    other instances have done.
//! 2. a **journal** - what *this* instance has done to the partition since that
//!    snapshot was taken, not yet written to the table.
//!
//! Nothing on a hot path writes the database. The enqueue, the sequencer and
//! the ack report into the journal, which costs a mutex and four additions; a
//! background worker publishes the journal, re-reads, and installs a fresh
//! snapshot. So the bound costs the caller's transaction nothing at all - not a
//! read, not a write, not a row lock.
//!
//! The journal is monotonic and remembers what it has already published, so a
//! flush that fails advances nothing and the next attempt recomputes the same
//! delta. Nothing is lost and nothing is counted twice.
//!
//! The bound can therefore be exceeded transiently, by what other instances
//! have done since the last snapshot plus whatever this instance has not yet
//! published. Both are bounded by the flush interval, and the reading converges
//! on the truth once the queue is quiet.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::limits::{QueueLimits, Volume};

/// One partition's four channel counters.
///
/// Each of the two channels has an in and an out, so each channel's depth is
/// its own difference. That is what lets a channel be proved empty on its own
/// and repaired without reference to the other.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Channels {
    /// The enqueue put a row into `incoming`.
    pub incoming_in: Volume,
    /// The sequencer took it out.
    pub incoming_out: Volume,
    /// The sequencer put it into `outgoing`.
    pub outgoing_in: Volume,
    /// The ack took it out. Not the vacuum: a row past the cursor has left the
    /// queue whether or not its row has been deleted yet.
    pub outgoing_out: Volume,
}

impl Channels {
    pub const ZERO: Self = Self {
        incoming_in: Volume::ZERO,
        incoming_out: Volume::ZERO,
        outgoing_in: Volume::ZERO,
        outgoing_out: Volume::ZERO,
    };

    /// What the partition is holding: both channels' depths.
    #[must_use]
    pub fn pending(&self) -> Volume {
        self.incoming_depth() + self.outgoing_depth()
    }

    /// Rows enqueued and not yet sequenced.
    #[must_use]
    pub fn incoming_depth(&self) -> Volume {
        self.incoming_in - self.incoming_out
    }

    /// Rows sequenced and not yet acked.
    #[must_use]
    pub fn outgoing_depth(&self) -> Volume {
        self.outgoing_in - self.outgoing_out
    }

    #[must_use]
    pub fn is_zero(&self) -> bool {
        *self == Self::ZERO
    }
}

impl std::ops::Add for Channels {
    type Output = Self;

    fn add(self, other: Self) -> Self {
        Self {
            incoming_in: self.incoming_in + other.incoming_in,
            incoming_out: self.incoming_out + other.incoming_out,
            outgoing_in: self.outgoing_in + other.outgoing_in,
            outgoing_out: self.outgoing_out + other.outgoing_out,
        }
    }
}

impl std::ops::Sub for Channels {
    type Output = Self;

    fn sub(self, other: Self) -> Self {
        Self {
            incoming_in: self.incoming_in - other.incoming_in,
            incoming_out: self.incoming_out - other.incoming_out,
            outgoing_in: self.outgoing_in - other.outgoing_in,
            outgoing_out: self.outgoing_out - other.outgoing_out,
        }
    }
}

/// The shared numbers as last read from the counter table.
#[derive(Debug, Clone, Copy)]
struct Snapshot {
    channels: Channels,
    at: Instant,
}

/// What this instance has done to one partition.
///
/// `totals` only grows; `published` records what a successful flush has already
/// written. The difference is both what the enqueue must add to the snapshot
/// and what the flusher must send, so there are never two numbers to keep in
/// agreement.
#[derive(Debug, Clone, Copy, Default)]
struct Journal {
    totals: Channels,
    published: Channels,
}

impl Journal {
    fn unpublished(&self) -> Channels {
        self.totals - self.published
    }
}

/// One partition's occupancy: the shared picture plus this instance's own
/// unpublished work.
#[derive(Debug)]
struct Level {
    /// Both states under one lock. Separate locks would let a flush landing
    /// between two acquisitions pair a fresh snapshot with a stale journal,
    /// which understates the backlog - the direction that lets a bound be
    /// crossed.
    state: Mutex<LevelState>,
}

#[derive(Debug, Default)]
struct LevelState {
    snapshot: Option<Snapshot>,
    journal: Journal,
}

impl Level {
    fn new() -> Self {
        Self {
            state: Mutex::new(LevelState::default()),
        }
    }

    /// What this partition is holding, as this instance understands it.
    fn pending(&self) -> Volume {
        self.state.lock().map_or(Volume::ZERO, |state| {
            let snapshot = state
                .snapshot
                .map_or(Channels::ZERO, |snapshot| snapshot.channels);
            (snapshot + state.journal.unpublished()).pending()
        })
    }

    fn report(&self, delta: Channels) {
        if let Ok(mut state) = self.state.lock() {
            state.journal.totals = state.journal.totals + delta;
        }
    }

    /// Take back a report for work that turned out never to have happened.
    ///
    /// The totals stop being monotonic here, deliberately: if the report was
    /// already published, `unpublished` goes negative and the next flush sends
    /// that negative delta, which is exactly the correction the table needs.
    fn unreport(&self, delta: Channels) {
        if let Ok(mut state) = self.state.lock() {
            state.journal.totals = state.journal.totals - delta;
        }
    }

    fn unpublished(&self) -> Channels {
        self.state
            .lock()
            .map_or(Channels::ZERO, |state| state.journal.unpublished())
    }

    /// Record that `applied` reached the table. Additive rather than a
    /// wholesale overwrite, so work reported while the flush was in flight
    /// stays unpublished instead of being silently marked done.
    fn published(&self, applied: Channels) {
        if let Ok(mut state) = self.state.lock() {
            state.journal.published = state.journal.published + applied;
        }
    }

    fn observe(&self, channels: Channels) {
        if let Ok(mut state) = self.state.lock() {
            state.snapshot = Some(Snapshot {
                channels,
                at: Instant::now(),
            });
        }
    }

    fn is_stale(&self, interval: Duration) -> bool {
        self.state.lock().map_or(true, |state| {
            state
                .snapshot
                .is_none_or(|snapshot| snapshot.at.elapsed() >= interval)
        })
    }
}

/// Decides whether a submission may enter a bounded queue.
///
/// One per bounded queue. A queue with no configured bound has none of this,
/// so it pays one map miss on enqueue and nothing else.
#[derive(Debug)]
pub struct Admission {
    limits: QueueLimits,
    levels: Vec<Level>,
}

impl Admission {
    #[must_use]
    pub fn new(limits: QueueLimits, partitions: usize) -> Self {
        Self {
            limits,
            levels: (0..partitions.max(1)).map(|_| Level::new()).collect(),
        }
    }

    #[must_use]
    pub const fn limits(&self) -> &QueueLimits {
        &self.limits
    }

    /// The whole queue's backlog: the sum of its partitions.
    #[must_use]
    pub fn pending(&self) -> Volume {
        self.levels
            .iter()
            .fold(Volume::ZERO, |acc, level| acc + level.pending())
    }

    /// One partition's backlog.
    #[must_use]
    pub fn partition_pending(&self, partition: usize) -> Volume {
        self.levels
            .get(partition)
            .map_or(Volume::ZERO, Level::pending)
    }

    /// The bound one partition gets: an equal share of the queue's, so a single
    /// skewed partition key cannot consume the whole allowance.
    #[must_use]
    pub fn partition_budget(&self) -> Volume {
        let partitions = i64::try_from(self.levels.len()).unwrap_or(1).max(1);
        // Rounded up, and never below what one admissible submission needs.
        // Flooring would hand a queue whose bound is smaller than its partition
        // count a budget of zero, and a budget of zero refuses every submission
        // to an empty queue with no way back.
        Volume {
            entities: share(self.limits.max_entities(), partitions).max(1),
            bytes: share(self.limits.max_bytes(), partitions).max(MAX_PAYLOAD_BYTES),
        }
    }

    /// The enqueue put work into the incoming channel.
    pub fn arrived(&self, partition: usize, volume: Volume) {
        self.report(
            partition,
            Channels {
                incoming_in: volume,
                ..Channels::ZERO
            },
        );
    }

    /// The sequencer moved work from the incoming channel to the outgoing one.
    /// One report, because it is one transaction: the two counters can only
    /// diverge if something moved rows outside the pipeline, which is exactly
    /// what makes each channel's check able to say which side drifted.
    pub fn sequenced(&self, partition: usize, volume: Volume) {
        self.report(
            partition,
            Channels {
                incoming_out: volume,
                outgoing_in: volume,
                ..Channels::ZERO
            },
        );
    }

    /// The ack took work out of the outgoing channel.
    pub fn acked(&self, partition: usize, volume: Volume) {
        self.report(
            partition,
            Channels {
                outgoing_out: volume,
                ..Channels::ZERO
            },
        );
    }

    fn report(&self, partition: usize, delta: Channels) {
        if let Some(level) = self.levels.get(partition) {
            level.report(delta);
        }
    }

    /// Every partition with work the table has not been told about yet.
    #[must_use]
    pub fn unpublished(&self) -> Vec<(usize, Channels)> {
        self.levels
            .iter()
            .enumerate()
            .map(|(partition, level)| (partition, level.unpublished()))
            .filter(|(_, delta)| !delta.is_zero())
            .collect()
    }

    /// Take back an arrival whose transaction rolled back.
    pub fn arrival_rolled_back(&self, partition: usize, volume: Volume) {
        if let Some(level) = self.levels.get(partition) {
            level.unreport(Channels {
                incoming_in: volume,
                ..Channels::ZERO
            });
        }
    }

    /// One partition's unpublished work.
    #[must_use]
    pub fn unpublished_at(&self, partition: usize) -> Channels {
        self.levels
            .get(partition)
            .map_or(Channels::ZERO, Level::unpublished)
    }

    /// Record that a partition's delta reached the table.
    pub fn published(&self, partition: usize, applied: Channels) {
        if let Some(level) = self.levels.get(partition) {
            level.published(applied);
        }
    }

    /// Install a partition's fresh reading of the counter table.
    pub fn observe(&self, partition: usize, channels: Channels) {
        if let Some(level) = self.levels.get(partition) {
            level.observe(channels);
        }
    }

    /// Whether any partition's snapshot is older than the configured interval.
    #[must_use]
    pub fn is_stale(&self) -> bool {
        let interval = self.limits.refresh_interval();
        self.levels.iter().any(|level| level.is_stale(interval))
    }
}

/// What one enqueue reported to bounded queues' journals, carried on its
/// [`Wake`](super::Wake) so the rollback path can hand it back exactly.
///
/// Per enqueue and not per queue, because wakes from several enqueues - to
/// different queues - combine into one for the unit of work.
#[derive(Default)]
pub struct Arrivals(Vec<(Arc<Admission>, usize, Volume)>);

impl Arrivals {
    pub fn push(&mut self, admission: Arc<Admission>, partition: usize, volume: Volume) {
        self.0.push((admission, partition, volume));
    }

    pub fn append(&mut self, other: &mut Self) {
        self.0.append(&mut other.0);
    }

    /// The transaction rolled back, so nothing it wrote arrived.
    pub fn roll_back(self) {
        for (admission, partition, volume) in self.0 {
            admission.arrival_rolled_back(partition, volume);
        }
    }
}

/// A partition's byte budget can never be smaller than one admissible payload,
/// or a partition would refuse a message the queue's own bound allows and
/// nothing would ever make room for it.
#[allow(clippy::cast_possible_wrap)]
const MAX_PAYLOAD_BYTES: i64 = super::validation::MAX_PAYLOAD_SIZE as i64;

/// One partition's share of a queue-wide bound, rounded up.
///
/// Written out rather than `div_ceil`, which is unstable for signed integers,
/// and rather than `(bound + partitions - 1) / partitions`, which overflows on
/// the `i64::MAX` that stands for an unset bound.
fn share(bound: i64, partitions: i64) -> i64 {
    let Some(whole) = bound.checked_div(partitions) else {
        return bound;
    };
    let remainder = bound.checked_rem(partitions).unwrap_or(0);
    whole + i64::from(remainder != 0)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    fn volume(entities: i64, bytes: i64) -> Volume {
        Volume { entities, bytes }
    }

    fn bounded() -> Admission {
        Admission::new(QueueLimits::builder().max_entities(100).build(), 1)
    }

    #[test]
    fn a_partitions_depth_is_each_channel_on_its_own() {
        let channels = Channels {
            incoming_in: volume(10, 100),
            incoming_out: volume(4, 40),
            outgoing_in: volume(4, 40),
            outgoing_out: volume(1, 10),
        };
        assert_eq!(
            channels.incoming_depth(),
            volume(6, 60),
            "enqueued, unsequenced"
        );
        assert_eq!(
            channels.outgoing_depth(),
            volume(3, 30),
            "sequenced, unacked"
        );
        assert_eq!(
            channels.pending(),
            volume(9, 90),
            "and the queue holds both"
        );
    }

    #[test]
    fn work_is_visible_before_it_is_published() {
        let admission = bounded();
        admission.arrived(0, volume(3, 30));
        assert_eq!(
            admission.pending(),
            volume(3, 30),
            "the enqueue writes no statement, so the journal is the only witness"
        );
    }

    #[test]
    fn publishing_neither_doubles_nor_loses() {
        let admission = bounded();
        admission.arrived(0, volume(3, 30));

        // The flusher takes the delta and writes it.
        let delta = admission.unpublished_at(0);
        assert_eq!(delta.incoming_in, volume(3, 30));

        // Work arriving while that write is in flight must not be marked
        // published by it.
        admission.arrived(0, volume(1, 10));
        admission.published(0, delta);
        assert_eq!(
            admission.unpublished_at(0).incoming_in,
            volume(1, 10),
            "only what was actually sent is marked published"
        );

        // The snapshot now contains the published part, and the estimate must
        // not count it twice.
        admission.observe(0, delta);
        assert_eq!(
            admission.pending(),
            volume(4, 40),
            "snapshot plus what is still unpublished, counted once each"
        );
    }

    #[test]
    fn a_failed_publish_advances_nothing() {
        let admission = bounded();
        admission.arrived(0, volume(2, 20));
        let delta = admission.unpublished_at(0);
        // The write failed, so `published` is never called.
        assert_eq!(
            admission.unpublished_at(0),
            delta,
            "the next pass recomputes the identical delta"
        );
        assert_eq!(admission.pending(), volume(2, 20));
    }

    #[test]
    fn a_drained_partition_reads_exactly_zero() {
        let admission = bounded();
        admission.arrived(0, volume(3, 30));
        admission.sequenced(0, volume(3, 30));
        admission.acked(0, volume(3, 30));
        assert_eq!(
            admission.pending(),
            Volume::ZERO,
            "both channels net out, with no residue"
        );

        let delta = admission.unpublished_at(0);
        assert_eq!(
            delta.incoming_out, delta.outgoing_in,
            "one move, two counters"
        );
        assert_eq!(delta.pending(), Volume::ZERO);
    }

    #[test]
    fn only_partitions_with_work_are_published() {
        let admission = Admission::new(QueueLimits::builder().max_entities(100).build(), 4);
        admission.arrived(2, volume(1, 10));
        let unpublished = admission.unpublished();
        assert_eq!(unpublished.len(), 1, "a quiet partition costs no statement");
        assert_eq!(unpublished[0].0, 2);
    }
}
