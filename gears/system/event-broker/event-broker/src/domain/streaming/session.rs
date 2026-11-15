//! The frame source.
//!
//! Every frame on a consumption stream is constructed here and nowhere else,
//! which is what makes the mapping from an assignment change to a frame a single
//! decision rather than one re-made wherever a generation arrives.
//!
//! Pull-based on purpose. There is no spawned task and no channel between the
//! reader and the transport: the caller asks for the next frame, and a client
//! disconnect drops the session, which structurally stops reading and releases
//! the lease. Nothing has to remember to clean up.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;
use toolkit::domain_model;

use tokio::sync::{Notify, watch};
use tokio::time::Instant;
use tokio_util::sync::DropGuard;

use crate::domain::consumer_group_coordinator::MembershipHandle;
use crate::domain::consumer_group_coordinator::member_scope::{CachedScope, ScopeGeneration};
use crate::domain::error::DomainError;
use crate::domain::streaming::assignment::{AssignmentDelta, Generation};
use crate::domain::streaming::filter::{EventFilter, Prepared};
use crate::domain::streaming::frames::{CloseReason, ControlCode, Frame, Position};
use crate::domain::streaming::heartbeat::HeartbeatSchedule;
use crate::domain::streaming::lease::StreamLease;
use crate::domain::streaming::progress::ProgressPolicy;
use crate::domain::streaming::read::{PartitionRead, ReadLimit};
use crate::domain::streaming::read_set::{BatchOutcome, ReadSet};
use crate::domain::streaming::time::NowFn;

/// Where a session is in its life.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    /// Nothing has been emitted yet. The first frame is the topology baseline,
    /// which is what lets a consumer attribute every later position to a
    /// topology it has seen.
    Opening,
    Streaming,
    /// A terminal frame is owed, then the stream ends. Separate from `Closed`
    /// because the frame carrying the frontier has to get out first - that is
    /// the whole point of a graceful close.
    Closing(CloseReason),
    Closed,
}

/// One consumption stream.
#[domain_model]
pub struct StreamSession {
    state: SessionState,
    read_set: ReadSet,
    filter: Arc<dyn EventFilter>,
    heartbeat: HeartbeatSchedule,
    progress: ProgressPolicy,
    limit: ReadLimit,
    topology_version: i64,
    /// Frames produced but not yet handed over. One event per frame, so a batch
    /// of matches becomes a queue rather than a single fat frame.
    pending: VecDeque<Frame>,
    /// Woken when any of this session's partitions has something. One waker for
    /// every partition, so an idle wait costs one await rather than one per
    /// partition.
    ready: Arc<Notify>,
    now: NowFn,
    /// Whether any read in the current round actually returned something.
    ///
    /// Load-bearing against a spin. `has_data` is optimistic by design - the
    /// accounted frontier being ahead of a reader does not mean the gap in front
    /// of *that* reader has been filled - so a reader sitting in a gap is
    /// perpetually ready while every read reports its position unanswerable.
    /// Without this flag the session reopens the round, sees the same reader
    /// ready, reads nothing again, and never reaches an await: a tight loop with
    /// no yield point, which does not merely burn CPU but starves the runtime
    /// thread, so even a timeout cannot fire.
    progressed_in_round: bool,
    /// Whether any read in the current round reported its position
    /// unanswerable.
    saw_unanswerable: bool,
    /// When a position first became unanswerable with nothing else progressing.
    ///
    /// Only `Unknown` counts. A quiet tail reports `NothingNew`, which is a
    /// healthy idle stream and must never trip this - conflating the two would
    /// tear down every stream that simply had nothing to deliver.
    unanswerable_since: Option<Instant>,
    /// How long a position may stay unanswerable before the stream gives up.
    unanswerable_tolerance: Duration,
    generations: watch::Receiver<Generation>,
    /// The generation this session is currently reading against, so a change
    /// can be classified against what it held rather than against the previous
    /// published value.
    current: Generation,
    /// The member's consume scope, when the filter decides from one; see
    /// [`SessionOpening::scope`].
    scope: Option<ScopeFeed>,
    /// The scope generation last handed to the filter; `None` until the
    /// first pass hands it the member's current scope.
    scope_seen: Option<ScopeGeneration>,
    /// When the scope last read stops serving: its earliest group expiry.
    /// The session ends the stream there on its own, so a scope whose
    /// refresh worker died or fell behind never serves past its window.
    /// `None` with no feed, or a scope with no group.
    scope_expires_at: Option<Instant>,
    _membership: MembershipHandle,
    /// Released on drop, which is what makes at-most-one-stream a property of
    /// ownership rather than of a guard somebody has to run.
    _lease: StreamLease,
}

impl StreamSession {
    /// Opens a session and picks the assignment it classifies every later
    /// one against.
    ///
    /// `opening.baseline` is the subscription as the stream open read it: the
    /// read set was seeded from it, and the opening `topology` frame carries
    /// its version. The member's watch is taken after that read, so a
    /// rebalance in between may already sit in it. When the watch holds other
    /// partitions than the baseline, the session classifies against the
    /// baseline, so its first pass reports the move: a loss narrows the read
    /// set with a `topology` frame, and a gain or the loss of every partition
    /// ends the stream.
    /// When it holds the same partitions, the session classifies against the
    /// watch: the coordinator republishes a member only when its partitions
    /// move, while every rebalance advances the version the subscription
    /// carries, so the watch's version can trail the baseline's, and
    /// classifying against the baseline would send a `topology` frame with
    /// that older version.
    #[must_use]
    pub fn open(opening: SessionOpening) -> Self {
        let watched = opening.generations.borrow().clone();
        let moved = !matches!(
            AssignmentDelta::classify(&opening.baseline, &watched),
            AssignmentDelta::Unchanged | AssignmentDelta::VersionOnly { .. }
        );
        let topology_version = opening.baseline.topology_version;
        let current = if moved { opening.baseline } else { watched };
        Self {
            state: SessionState::Opening,
            read_set: opening.read_set,
            filter: opening.filter,
            heartbeat: HeartbeatSchedule::new(opening.heartbeat_interval, opening.started_at),
            progress: ProgressPolicy::new(&opening.progress, opening.started_at),
            limit: opening.limit,
            topology_version,
            pending: VecDeque::new(),
            ready: opening.ready,
            now: opening.now,
            progressed_in_round: false,
            saw_unanswerable: false,
            unanswerable_since: None,
            unanswerable_tolerance: opening.unanswerable_tolerance,
            generations: opening.generations,
            current,
            scope: opening.scope,
            scope_seen: None,
            scope_expires_at: None,
            _membership: opening.membership,
            _lease: opening.lease,
        }
    }

    #[must_use]
    pub fn state(&self) -> SessionState {
        self.state
    }

    #[must_use]
    pub fn positions(&self) -> Vec<Position> {
        self.read_set.list_positions()
    }

    /// Reads the currently published assignment and says how it differs from
    /// what this session is reading against.
    ///
    /// `AssignmentDelta::Unchanged` in the common case, which `apply` ignores.
    fn observe_assignment(&mut self) -> AssignmentDelta {
        let next = self.generations.borrow_and_update().clone();
        let delta = AssignmentDelta::classify(&self.current, &next);
        self.current = next;
        delta
    }

    /// Reads the member's latest consume scope, the one the open stored
    /// included, and notes its earliest group expiry; ends the stream when
    /// that expiry has come by `now`. A member that holds no scope, or is
    /// gone, is left to the caller to close.
    fn check_scope(&mut self, now: Instant) -> ScopeCheck {
        let Some(ScopeFeed { receiver: feed, .. }) = self.scope.as_mut() else {
            return ScopeCheck::Unscoped;
        };
        // A closed feed is the member's removal; its last value is stale.
        let latest = match feed.has_changed() {
            Ok(_) => feed.borrow_and_update().clone(),
            Err(_closed) => None,
        };
        let Some(scope) = latest else {
            return ScopeCheck::Cleared;
        };
        self.scope_expires_at = scope.groups.iter().map(|group| group.expires_at).min();
        if self
            .scope_expires_at
            .is_some_and(|expires_at| now >= expires_at)
        {
            tracing::warn!("consume scope expired; closing stream");
            self.state = SessionState::Closing(CloseReason::Teardown);
            return ScopeCheck::Expired;
        }
        ScopeCheck::Serving(scope)
    }

    /// Hands the filter the member's latest scope before a read, or ends
    /// the stream when that scope is cleared, gone or expired. Returns
    /// whether the pass goes on to read.
    fn observe_scope(&mut self, now: Instant) -> bool {
        match self.check_scope(now) {
            ScopeCheck::Unscoped => true,
            ScopeCheck::Serving(scope) => {
                self.hand_scope_to_filter(scope);
                true
            }
            ScopeCheck::Cleared => {
                tracing::warn!("consume scope cleared; closing stream");
                self.state = SessionState::Closing(CloseReason::Teardown);
                false
            }
            ScopeCheck::Expired => false,
        }
    }

    /// Hands the filter `scope` when it is a generation the filter has not
    /// had. Detected by comparing generations, as `observe_assignment`
    /// compares, so a scope that the park's `changed()`, or the check after
    /// a batch's decisions, already marked seen is still handed over.
    fn hand_scope_to_filter(&mut self, scope: Arc<CachedScope>) {
        if self.scope_seen != Some(scope.generation) {
            self.scope_seen = Some(scope.generation);
            self.filter.swap_scope(scope);
        }
    }

    /// Applies an assignment change.
    ///
    /// The one place a delta becomes a frame. A loss narrows the read set and
    /// continues; a gain or a total loss ends the stream, because a gained
    /// partition has no cursor here and continuing would mean replaying from
    /// zero or guessing.
    pub fn apply(&mut self, delta: &AssignmentDelta) {
        match delta {
            AssignmentDelta::Unchanged => {}
            AssignmentDelta::VersionOnly { topology_version } => {
                self.topology_version = *topology_version;
                self.enqueue_topology();
            }
            AssignmentDelta::Loss {
                topology_version,
                retained,
            } => {
                self.topology_version = *topology_version;
                // Narrowed *before* the frame is built, so the frame reports
                // what the session will actually read next rather than what it
                // held a moment ago.
                let keys = retained
                    .iter()
                    .map(|held| {
                        crate::domain::streaming::source::PartitionKey::new(
                            held.topic.clone(),
                            held.partition,
                        )
                    })
                    .collect::<Vec<_>>();
                self.read_set.retain(&keys);
                self.enqueue_topology();
            }
            AssignmentDelta::Gain => self.state = SessionState::Closing(CloseReason::Rebalanced),
            AssignmentDelta::LoseAll => self.state = SessionState::Closing(CloseReason::LoseAll),
        }
    }

    /// Ends the stream for a reason of the broker's own, such as a sustained
    /// read failure.
    pub fn tear_down(&mut self) {
        if self.state != SessionState::Closed {
            self.state = SessionState::Closing(CloseReason::Teardown);
        }
    }

    /// The next frame, or `None` once the stream has ended.
    pub async fn next_frame(&mut self) -> Option<Frame> {
        loop {
            if let Some(frame) = self.pending.pop_front() {
                self.heartbeat.record_frame(Instant::now());
                return Some(frame);
            }

            match self.state {
                SessionState::Closed => return None,
                SessionState::Opening => {
                    self.state = SessionState::Streaming;
                    self.enqueue_topology();
                }
                SessionState::Closing(reason) => {
                    self.state = SessionState::Closed;
                    // The frontier goes out with the close, so a consumer can
                    // commit the ground it covered before re-joining.
                    return Some(Frame::Control {
                        code: ControlCode::Terminal,
                        positions: self.read_set.list_positions(),
                        reason: Some(reason),
                    });
                }
                SessionState::Streaming => {
                    if !self.advance().await {
                        return None;
                    }
                }
            }
        }
    }

    /// One step of the streaming state. Returns false when the stream ended.
    async fn advance(&mut self) -> bool {
        let now = Instant::now();

        // Before anything else: a session that has lost a partition must stop
        // reading it before announcing the loss, and one that has gained must
        // terminate rather than read a partition it holds no cursor for.
        //
        // Detected by comparison, not by `has_changed()`. The park below waits
        // on `changed()`, which marks the newest value seen when it resolves -
        // so a `has_changed()` check afterwards would report false and swallow
        // the very change that woke the session. Comparing costs one clone of a
        // short `Vec` per pass and cannot miss.
        let delta = self.observe_assignment();
        if !matches!(delta, AssignmentDelta::Unchanged) {
            self.apply(&delta);
            // Ends the pass. `apply` has either enqueued a frame or moved the
            // session to `Closing`, and both need `next_frame` to run again
            // immediately - falling through would park on the deadline below
            // with the frame still sitting in `pending`, delaying a rebalance
            // by a whole heartbeat interval.
            return true;
        }

        // Before any read: a cleared or expired scope ends the stream here,
        // and a newer one decides every event read from now on. After the
        // assignment, so a member removed by a LEAVE still closes with
        // `lose_all`.
        if !self.observe_scope(now) {
            return true;
        }

        if self.progress.due(now) {
            // Rearmed whether or not anything is reported. Declining to report
            // is a decision taken at *this* tick, and recording it only on the
            // emit path left `next_due()` permanently in the past for any
            // session with nothing drifted: the deadline below then computed an
            // already-expired instant, `timeout_at` returned immediately, and
            // `next_frame` looped with no yield point at roughly a full core.
            self.progress.record_emitted(now);

            let drifted = self.read_set.list_drifted(self.progress.drift_threshold());
            if !drifted.is_empty() {
                self.pending.push_back(Frame::Control {
                    code: ControlCode::Progress,
                    positions: drifted,
                    reason: None,
                });
                return true;
            }
        }

        if let Some(index) = self.read_set.next_to_read() {
            self.progressed_in_round |= self.read_one(index).await;
            self.read_set.mark_throttled(index);
            // A read may complete without yielding (for example, filtered events), so
            // without this a session with a large resident backlog would hold a
            // runtime worker for the whole drain - starving the loader that
            // feeds it and every other session on the same thread.
            tokio::task::yield_now().await;
            return true;
        }

        // Every ready partition has had its turn. If any of them returned
        // something, another round is worth taking.
        if std::mem::take(&mut self.progressed_in_round) {
            // Something moved, so whatever was unanswerable a moment ago is no
            // longer the whole story.
            self.saw_unanswerable = false;
            self.unanswerable_since = None;
            self.read_set.open_round();
            return true;
        }

        // A fruitless round in which something was unanswerable. Giving up
        // eventually beats heartbeating indefinitely over data the session
        // cannot read: a consumer can commit its frontier and recover, where an
        // apparently-alive stream delivering nothing gives it nothing to act on.
        if std::mem::take(&mut self.saw_unanswerable) {
            let since = *self.unanswerable_since.get_or_insert(now);
            if now.duration_since(since) >= self.unanswerable_tolerance {
                self.state = SessionState::Closing(CloseReason::Teardown);
                return true;
            }
        } else {
            self.unanswerable_since = None;
        }

        // A whole round yielded nothing, so asking again changes nothing until
        // something else does. Park - and reopen the round only afterwards, so a
        // perpetually-ready reader costs one read per wake rather than an
        // unbounded loop with no yield point.
        if self.heartbeat.is_due(now) {
            self.pending
                .push_back(Frame::Heartbeat { at: (self.now)() });
            self.read_set.open_round();
            return true;
        }

        // Bounded by the next frame deadline, so an idle stream still emits its
        // cadence rather than waiting for data that may never come, and by the
        // scope's expiry, so an idle stream ends when its scope stops serving.
        let cadence = self.heartbeat.next_due().min(self.progress.next_due());
        let deadline = self
            .scope_expires_at
            .map_or(cadence, |expires_at| cadence.min(expires_at));
        // The timeout expiring is the normal case - it means the cadence came
        // due before any partition did - so the result is deliberately ignored
        // rather than distinguished.
        // Four ways out, and the assignment and scope arms are why this is a
        // `select!` rather than a `timeout_at`: a membership change lands on
        // the generation watch and a new or cleared scope on the scope watch,
        // neither of which touches the readiness waker, so a parked session
        // would otherwise sleep through a rebalance or a revocation until its
        // next heartbeat.
        let ready = Arc::clone(&self.ready);
        let generations = &mut self.generations;
        let scope = &mut self.scope;
        let scope_changed = async {
            match scope {
                // A close resolves at once too; the next pass ends the stream.
                Some(ScopeFeed { receiver: feed, .. }) => {
                    let _changed_or_closed = feed.changed().await;
                }
                // No feed: nothing to wait for on this arm.
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            () = ready.notified() => {}
            // `Err` means the coordinator dropped this member's sender - the
            // group is gone. Waking is right; the next pass classifies it.
            _ = generations.changed() => {}
            () = scope_changed => {}
            () = tokio::time::sleep_until(deadline) => {}
        }
        self.read_set.open_round();
        true
    }

    /// Reads one partition, filters what came back, and records what it cost.
    ///
    /// Returns whether the read actually yielded a span. A round has to be able
    /// to tell an idle partition from a productive one, or the session never
    /// parks.
    async fn read_one(&mut self, index: usize) -> bool {
        let Some(slot) = self.read_set.slot(index) else {
            return false;
        };
        let offset = slot.offset();
        // Where the reader stands before the read: a slot is seeded together
        // with its reader, and each recorded batch moves both to one frontier.
        let examined_before = slot.last_examined();
        let reader = Arc::clone(slot.reader());

        // No offset passed: the reader is the authority on where it is, and it
        // advances itself over what it accounted for.
        let outcome = match reader.read(self.limit) {
            PartitionRead::Hit {
                events,
                accounted_through,
            } => {
                let mut matched = 0;
                let mut examined = 0;
                let mut delivered_through = offset;

                // The read moved the reader past the batch. Holding it back
                // where it stood keeps the batch's span from being reclaimed
                // while its decisions are awaited, so a cut batch rereads its
                // rest from memory; a pass between the read and this seek can
                // still take the span, which is then fetched again. Until the
                // release below, up to the evaluation deadline, the cache sees
                // the reader up to one batch behind, which shifts the inputs
                // that read reader positions (retained windows, memory
                // pressure, loader sizing) by that batch. On a failure the
                // stream tears down and where the reader stands no longer
                // matters.
                reader.seek(examined_before);
                // The only await on the delivery path. A failure tears the
                // stream down before anything of the batch is queued.
                let prepared = self.filter.prepare(&events).await;
                // The scope may have been cleared, or expired, while the
                // batch was decided: then none of it is handed over or
                // counted, and the reader stays where it stood. An expiry
                // ends the stream here. A clear is closed by the next pass,
                // which reads the assignment first, so a member removed by a
                // LEAVE meanwhile still closes with `lose_all`. A newer
                // generation is not handed to the filter here: the batch
                // keeps the decisions it was prepared with, under the scope
                // that served when it was read, and the next read uses the
                // newer one.
                match self.check_scope(Instant::now()) {
                    ScopeCheck::Unscoped | ScopeCheck::Serving(_) => {}
                    ScopeCheck::Cleared | ScopeCheck::Expired => return false,
                }
                let (decidable, examined_through) = match prepared {
                    Ok(Prepared::All) => (events.len(), accounted_through),
                    // A cut batch counts as examined through its last decided
                    // event, so the next read starts at the first undecided
                    // one.
                    Ok(Prepared::UpTo(decidable)) => {
                        let Some(last) = decidable
                            .checked_sub(1)
                            .and_then(|last| events.iter().nth(last))
                            .and_then(|event| event.sequence)
                        else {
                            self.fail_batch(&DomainError::Internal(
                                "a cut batch has no last decided event with a sequence".to_owned(),
                            ));
                            return false;
                        };
                        (decidable, last)
                    }
                    Err(error) => {
                        self.fail_batch(&error);
                        return false;
                    }
                };
                // Released to the batch's examined frontier, the frontier
                // `record_batch` gives the slot below.
                reader.seek(examined_through);
                for event in events.iter().take(decidable) {
                    examined += 1;
                    let allowed = match self.filter.admits(event) {
                        Ok(allowed) => allowed,
                        Err(error) => {
                            self.fail_batch(&error);
                            return false;
                        }
                    };
                    if allowed {
                        matched += 1;
                        delivered_through = event.sequence.unwrap_or(delivered_through);
                        self.pending
                            .push_back(Frame::Event(Box::new(event.clone())));
                    }
                }

                BatchOutcome::builder(examined_through)
                    .delivered_through(delivered_through)
                    .counts(matched, examined)
                    .build()
            }
            // Neither the tail nor an unanswerable position moves anything. A
            // reader must not advance past a span nobody has accounted for.
            // A quiet tail is healthy; an unanswerable position is not, and the
            // two have to be distinguished or a sustained read failure looks
            // exactly like an idle stream.
            PartitionRead::NothingNew => return false,
            PartitionRead::Unknown => {
                self.saw_unanswerable = true;
                return false;
            }
        };

        self.read_set.record_batch(index, outcome);
        true
    }

    /// Ends the stream over a batch delivery could not decide. The reader may
    /// stand past the reported frontier, but the batch is not committed to
    /// it. Its queued frames are dropped too, so a retry can replay it without
    /// losing events during a PDP outage.
    fn fail_batch(&mut self, error: &DomainError) {
        tracing::warn!(%error, "event authorization failed; closing stream");
        self.pending.clear();
        self.state = SessionState::Closing(CloseReason::Teardown);
    }

    fn enqueue_topology(&mut self) {
        self.pending.push_back(Frame::Topology {
            topology_version: self.topology_version,
            positions: self.read_set.list_positions(),
        });
    }
}

/// Whether the member's consume scope still serves a session.
enum ScopeCheck {
    /// No feed: the filter decides alone.
    Unscoped,
    /// The member's latest scope, serving.
    Serving(Arc<CachedScope>),
    /// Cleared, or the member is gone: nothing is closed yet, so a caller
    /// can read the assignment first and close with the reason it gives.
    Cleared,
    /// Past its earliest group expiry: the stream is closing with
    /// `teardown`.
    Expired,
}

/// What a session needs to open.
///
/// A struct rather than a long argument list: several fields are same-typed,
/// and a positional call would let them be transposed silently.
#[domain_model]
pub struct SessionOpening {
    pub read_set: ReadSet,
    pub filter: Arc<dyn EventFilter>,
    pub progress: crate::domain::streaming::progress::ProgressConfig,
    pub heartbeat_interval: std::time::Duration,
    pub limit: ReadLimit,
    /// The assignment the read set was seeded from; the opening `topology`
    /// frame carries its version. See [`StreamSession::open`] for how a watch
    /// that moved since then is classified.
    pub baseline: Generation,
    pub ready: Arc<Notify>,
    pub started_at: Instant,
    pub now: NowFn,
    /// How long a position may stay unanswerable before the session tears the
    /// stream down rather than heartbeating forever over data it cannot read.
    pub unanswerable_tolerance: Duration,
    pub lease: StreamLease,
    /// This session's view of its own assignment. The coordinator publishes;
    /// this side classifies, because only it knows where its readers are.
    ///
    /// A `watch` keeps only the latest value, so a burst of membership changes
    /// collapses to the state that matters and none of them can be dropped by
    /// backpressure.
    pub generations: watch::Receiver<Generation>,
    /// Held for its `Drop`: releasing it is what reports the stream closed and
    /// starts the group's grace period. Never read.
    pub membership: MembershipHandle,
    /// The member's consume scope as the coordinator holds it, when the
    /// filter decides from it; `None` for a filter that decides alone.
    ///
    /// Every pass hands the filter a newer generation before it reads, and
    /// a cleared scope, a closed feed (the member is gone) or a scope past
    /// its earliest group expiry ends the stream with `teardown`, unless
    /// the pass that closes it finds the member's assignment changed first:
    /// a LEAVE closes with `lose_all`, a gain with `rebalanced`. That is
    /// checked before each read and again once a batch is decided, so no
    /// event of a batch decided after it is handed over and the batch
    /// records no progress; a parked session wakes at that expiry.
    /// The session holds the feed until it is dropped, and with it the
    /// scope's refresh worker.
    pub scope: Option<ScopeFeed>,
}

/// The member's consume scope as a session reads it: the coordinator's watch
/// of it and, for a stream open, the guard of the worker that refreshes it.
/// Dropping the feed, which the session does when the stream is dropped,
/// cancels that worker.
#[derive(Debug)]
pub struct ScopeFeed {
    receiver: watch::Receiver<Option<Arc<CachedScope>>>,
    _refresh: Option<DropGuard>,
}

impl ScopeFeed {
    /// `receiver`, with the guard that cancels the scope's refresh worker
    /// when the feed is dropped.
    #[must_use]
    pub fn refreshed(
        receiver: watch::Receiver<Option<Arc<CachedScope>>>,
        refresh: DropGuard,
    ) -> Self {
        Self {
            receiver,
            _refresh: Some(refresh),
        }
    }

    /// `receiver` alone: nothing refreshes the scope it watches.
    #[must_use]
    pub fn fixed(receiver: watch::Receiver<Option<Arc<CachedScope>>>) -> Self {
        Self {
            receiver,
            _refresh: None,
        }
    }
}

/// A [`StreamSession`] as a `Stream`, which is what a transport consumes.
///
/// The session's own interface is an `async fn`, because its state machine is
/// naturally written as one - the read, the filter, and the park read in order.
/// A hand-rolled `poll_next` over the same logic would have to store every
/// intermediate state by hand, and the compiler already does that better inside
/// an `async` block.
///
/// The cost is one boxed future per frame. It buys no task and no channel: a
/// dropped stream drops the session, which stops reading and releases the lease,
/// so there is nothing to notice a disconnect and nothing to clean up. If that
/// allocation ever shows up in a profile, the fix is to rewrite the state
/// machine as a poll loop, not to put a channel back.
#[domain_model]
pub struct FrameStream {
    /// The session, when it is not currently borrowed by a pending future.
    idle: Option<StreamSession>,
    #[expect(
        clippy::type_complexity,
        reason = "the future owns the session and hands it back; naming it costs more than it explains"
    )]
    polling: Option<
        std::pin::Pin<Box<dyn std::future::Future<Output = (StreamSession, Option<Frame>)> + Send>>,
    >,
}

impl FrameStream {
    #[must_use]
    pub fn new(session: StreamSession) -> Self {
        Self {
            idle: Some(session),
            polling: None,
        }
    }
}

impl tokio_stream::Stream for FrameStream {
    type Item = Frame;

    fn poll_next(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Frame>> {
        use std::task::Poll;

        let this = self.get_mut();
        loop {
            if let Some(pending) = this.polling.as_mut() {
                let (session, frame) = match pending.as_mut().poll(cx) {
                    Poll::Ready(ready) => ready,
                    Poll::Pending => return Poll::Pending,
                };
                this.polling = None;
                // Only kept if the stream continues. A `None` frame means the
                // session closed, and dropping it here is what releases the
                // lease without anyone having to remember to.
                if frame.is_some() {
                    this.idle = Some(session);
                }
                return Poll::Ready(frame);
            }

            let Some(mut session) = this.idle.take() else {
                return Poll::Ready(None);
            };
            this.polling = Some(Box::pin(async move {
                let frame = session.next_frame().await;
                (session, frame)
            }));
        }
    }
}
