//! The session's frame sequence, against stubbed partitions.
//!
//! No cache, no storage, no HTTP. What is asserted is the order and content of
//! the frames a consumer would receive, because that is the contract.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::time::Duration;

use authz_resolver_sdk::EvaluationRequest;
use chrono::Utc;
use serde_json::json;
use tokio::sync::{Notify, watch};
use tokio::time::Instant;
use toolkit_gts::GtsInstanceId;
use uuid::Uuid;

use crate::domain::consumer_group_coordinator::member_scope::{
    AuthorizedGroup, CachedScope, Expected, ScopeContent, ScopeGeneration, ScopePrincipal,
    ScopeWrite, ScopeWriteOutcome,
};
use crate::domain::consumer_group_coordinator::{
    ConsumerGroupCoordinator, MembershipHandle, TopicInterest,
};
use crate::domain::error::DomainError;
use crate::domain::model::{Assignment, BarrierMode, Event, OwnershipFilter, Sequence};
use crate::domain::streaming::assignment::{AssignmentDelta, Generation};
use crate::domain::streaming::filter::{EventFilter, InterestFilter, Prepared};
use crate::domain::streaming::frames::{CloseReason, ControlCode, Frame, Position};
use crate::domain::streaming::lease::{InProcessStreamLeases, StreamLeases};
use crate::domain::streaming::progress::ProgressConfig;
use crate::domain::streaming::read::{EventBatch, MaxBytes, MaxEvents, PartitionRead, ReadLimit};
use crate::domain::streaming::read_set::{PartitionSlot, ReadSet};
use crate::domain::streaming::reader::PartitionReader;
use crate::domain::streaming::scoped_filter::scoped_filter_tests::{
    self as scoped, Answer, KnownTypes, TypePdp,
};
use crate::domain::streaming::scoped_filter::{
    SINGLE_TYPE_CONCURRENCY, ScopedFilter, SingleTypeAuthorizer,
};
use crate::domain::streaming::session::{ScopeFeed, SessionOpening, SessionState, StreamSession};
use crate::domain::streaming::source::PartitionKey;
use crate::domain::streaming::time::NowFn;
use crate::test_support::{SpecQuery, SpecQueryHook};

const TOPIC: &str = "gts.cf.core.events.topic.v1~x.eb.orders.acme.v1";
const CREATED: &str = "gts.cf.core.events.event.v1~x.eb.orders.created.v1~";

fn gts(id: &str) -> GtsInstanceId {
    GtsInstanceId::try_new(id).expect("static gts id is valid")
}

fn key(partition: i32) -> PartitionKey {
    PartitionKey::new(gts(TOPIC), partition)
}

fn event(tenant: Uuid, sequence: i64) -> Event {
    let sequence = Sequence::assigned(sequence);
    Event {
        id: Uuid::nil(),
        r#type: crate::test_support::event_type_id(CREATED),
        topic: gts(TOPIC),
        subject_tenant: tenant,
        source: "session-test".to_owned(),
        subject: "order".to_owned(),
        subject_type: gts::GtsTypeId::new("order"),
        subject_owner: None,
        occurred_at: Utc::now(),
        trace_parent: None,
        data: json!({}),
        meta: None,
        ingest: None,
        partition: Some(0),
        sequence: Some(sequence),
        sequence_time: None,
    }
}

/// A partition whose contents a test sets, handing them over once.
struct StubPartition {
    events: Mutex<Vec<Event>>,
    accounted_through: AtomicI64,
    /// How many times the session has asked whether this partition is ready.
    ///
    /// The only observable that separates a parked session from a spinning one:
    /// both emit nothing, so "did a frame arrive" cannot tell them apart.
    probes: AtomicI64,
}

impl StubPartition {
    fn holding(events: Vec<Event>) -> Arc<Self> {
        let through = events
            .iter()
            .filter_map(|event| event.sequence)
            .max()
            .unwrap_or(Sequence::assigned(0));
        Arc::new(Self {
            events: Mutex::new(events),
            accounted_through: AtomicI64::new(through.as_i64()),
            probes: AtomicI64::new(0),
        })
    }

    fn empty() -> Arc<Self> {
        Arc::new(Self {
            events: Mutex::new(Vec::new()),
            accounted_through: AtomicI64::new(0),
            probes: AtomicI64::new(0),
        })
    }
}

/// One stub per partition, because there is one seam. Splitting the read from
/// the readiness probe would give the test two handles onto one partition,
/// a shape production does not have.
impl PartitionReader for StubPartition {
    fn has_data(&self) -> bool {
        self.probes.fetch_add(1, Ordering::Relaxed);
        !self.events.lock().expect("test lock").is_empty()
    }

    fn seek(&self, _offset: Sequence) {}

    fn report_scanning(&self, _scanning: bool) {}

    fn read(&self, _limit: ReadLimit) -> PartitionRead {
        let drained: Vec<Event> = std::mem::take(&mut *self.events.lock().expect("test lock"));
        if drained.is_empty() {
            return PartitionRead::NothingNew;
        }
        // A real cache hands back borrowed runs; a stub can only build an owned
        // batch, which is why the session takes an `EventBatch` and never a
        // concrete storage type.
        let accounted_through = Sequence::assigned(self.accounted_through.load(Ordering::Relaxed));
        let held = drained.len();
        PartitionRead::Hit {
            events: crate::domain::streaming::read::EventBatch::from_runs(vec![
                crate::domain::streaming::read::EventSlice::builder(drained.into())
                    .range(0..held)
                    .frontier(accounted_through)
                    .build(),
            ]),
            accounted_through,
        }
    }
}

struct Harness {
    session: StreamSession,
    _leases: Arc<InProcessStreamLeases>,
}

/// A real coordinator with one member, so a test session gets a real generation
/// watch and a real membership handle.
///
/// The same choice the lease already makes here: `InProcessStreamLeases` is the
/// production type, not a stub, because it is in-memory and has no I/O.
/// `ConsumerGroupCoordinator` is a `Mutex<HashMap>` for the same reason.
///
/// The handle owns an `Arc` to the coordinator, so nothing else needs to keep it
/// alive.
fn membership(
    partitions: i32,
) -> (
    tokio::sync::watch::Receiver<crate::domain::streaming::assignment::Generation>,
    crate::domain::consumer_group_coordinator::MembershipHandle,
) {
    use crate::domain::consumer_group_coordinator::{ConsumerGroupCoordinator, TopicInterest};

    let groups = Arc::new(ConsumerGroupCoordinator::new(
        std::time::Duration::from_mins(1),
    ));
    let group = gts("gts.cf.core.events.consumer_group.v1~02943530-10da-4624-a3ae-b998c425847f");
    let sub_id = Uuid::new_v4();
    groups.join_member(
        &group,
        sub_id,
        &[TopicInterest {
            id: gts(TOPIC),
            partitions,
        }],
        Duration::from_secs(30),
    );
    ConsumerGroupCoordinator::subscribe(&groups, &group, sub_id).expect("the member just joined")
}

/// A session over `partitions` deciding by the selector alone: one interest
/// in `CREATED` on `TOPIC` in `tenant`.
fn open(partitions: Vec<(i32, Arc<StubPartition>)>, tenant: Uuid) -> Harness {
    let interests = [crate::domain::model::Interest {
        topic: gts(TOPIC),
        subject_tenant: tenant,
        ownership: OwnershipFilter::EntireTenant,
        barrier_mode: BarrierMode::Respect,
        types: vec![gts::GtsIdPattern::try_new(CREATED).unwrap()],
        subject_types: Vec::new(),
        filter: None,
    }];
    open_with(
        partitions
            .into_iter()
            .map(|(partition, held)| (key(partition), held))
            .collect(),
        Arc::new(InterestFilter::compile(&interests)),
        ScopeInput::Alone,
    )
}

/// The member's consume scope a test session is fed.
enum ScopeInput {
    /// No scope: the filter decides alone.
    Alone,
    /// The scope this watch holds, with no refresh worker behind it.
    Fed(watch::Receiver<Option<Arc<CachedScope>>>),
}

impl From<ScopeInput> for Option<ScopeFeed> {
    fn from(input: ScopeInput) -> Self {
        match input {
            ScopeInput::Alone => None,
            ScopeInput::Fed(receiver) => Some(ScopeFeed::fixed(receiver)),
        }
    }
}

/// A session over `partitions` deciding through `filter`, fed the member's
/// consume scope as `scope` gives it.
fn open_with<R: PartitionReader + 'static>(
    partitions: Vec<(PartitionKey, Arc<R>)>,
    filter: Arc<dyn EventFilter>,
    scope: ScopeInput,
) -> Harness {
    open_slots(
        partitions
            .into_iter()
            .map(|(key, held)| PartitionSlot::new(key, held))
            .collect(),
        filter,
        scope,
    )
}

/// [`open_with`] over slots a test built, seeded where it chose.
fn open_slots(
    slots: Vec<PartitionSlot>,
    filter: Arc<dyn EventFilter>,
    scope: ScopeInput,
) -> Harness {
    open_member(slots, filter, scope, membership(8))
}

/// [`open_slots`] for a member a test joined itself, streaming on its
/// generation watch and holding its membership handle; the open read the
/// partitions that watch holds.
fn open_member(
    slots: Vec<PartitionSlot>,
    filter: Arc<dyn EventFilter>,
    scope: ScopeInput,
    streaming: (watch::Receiver<Generation>, MembershipHandle),
) -> Harness {
    let baseline = Generation::new(7, streaming.0.borrow().assigned.clone());
    open_from(baseline, slots, filter, scope, streaming)
}

/// [`open_member`] for a stream open that read `baseline`, whatever the
/// member's watch holds by the time the session opens.
fn open_from(
    baseline: Generation,
    slots: Vec<PartitionSlot>,
    filter: Arc<dyn EventFilter>,
    scope: ScopeInput,
    (generations, membership): (watch::Receiver<Generation>, MembershipHandle),
) -> Harness {
    let leases = Arc::new(InProcessStreamLeases::new());
    let lease = leases.acquire(Uuid::new_v4()).expect("lease is free");

    let now: NowFn = Arc::new(Utc::now);
    let session = StreamSession::open(SessionOpening {
        read_set: ReadSet::seed(slots),
        filter,
        progress: ProgressConfig::default(),
        heartbeat_interval: Duration::from_secs(5),
        limit: ReadLimit::new(MaxEvents(256), MaxBytes(1024 * 1024)),
        baseline,
        ready: Arc::new(Notify::new()),
        started_at: Instant::now(),
        now,
        unanswerable_tolerance: Duration::from_secs(30),
        lease,
        generations,
        membership,
        scope: scope.into(),
    });

    Harness {
        session,
        _leases: leases,
    }
}

/// Collects frames until the stream ends or `limit` frames have arrived,
/// whichever is first. Bounded so a stuck session fails rather than hangs.
async fn drain(session: &mut StreamSession, limit: usize) -> Vec<Frame> {
    drain_within(session, limit, Duration::from_millis(200)).await
}

/// [`drain`] with `bound` as the longest wait for each frame.
async fn drain_within(session: &mut StreamSession, limit: usize, bound: Duration) -> Vec<Frame> {
    let mut frames = Vec::new();
    for _ in 0..limit {
        match tokio::time::timeout(bound, session.next_frame()).await {
            Ok(Some(frame)) => frames.push(frame),
            // Either the stream ended or it went quiet. Both mean "no more
            // frames for this assertion", and the bound is what makes a stuck
            // session fail rather than hang.
            Ok(None) | Err(_) => break,
        }
    }
    frames
}

fn kinds(frames: &[Frame]) -> Vec<&'static str> {
    frames
        .iter()
        .map(|frame| match frame {
            Frame::Event(_) => "event",
            Frame::Heartbeat { .. } => "heartbeat",
            Frame::Topology { .. } => "topology",
            Frame::Control {
                code: ControlCode::Progress,
                ..
            } => "progress",
            Frame::Control {
                code: ControlCode::Terminal,
                ..
            } => "terminal",
        })
        .collect()
}

#[tokio::test]
async fn the_first_frame_is_the_open_time_topology_baseline() {
    let tenant = Uuid::new_v4();
    let mut harness = open(vec![(0, StubPartition::empty())], tenant);

    let frames = drain(&mut harness.session, 1).await;

    // The baseline is what lets a consumer attribute every later position to a
    // topology it has actually seen.
    assert_eq!(kinds(&frames), vec!["topology"]);
    assert_eq!(harness.session.state(), SessionState::Streaming);
}

#[tokio::test]
async fn matching_events_are_delivered_one_per_frame_in_order() {
    let tenant = Uuid::new_v4();
    let held = StubPartition::holding(vec![event(tenant, 1), event(tenant, 2), event(tenant, 3)]);
    let mut harness = open(vec![(0, held)], tenant);

    let frames = drain(&mut harness.session, 4).await;

    assert_eq!(kinds(&frames), vec!["topology", "event", "event", "event"]);
    let delivered: Vec<i64> = frames
        .iter()
        .filter_map(|frame| match frame {
            Frame::Event(event) => event.sequence,
            _ => None,
        })
        .map(Sequence::as_i64)
        .collect();
    assert_eq!(delivered, vec![1, 2, 3]);
}

#[tokio::test]
async fn an_event_of_another_tenant_is_not_delivered_but_still_advances_the_frontier() {
    let tenant = Uuid::new_v4();
    let held = StubPartition::holding(vec![event(Uuid::new_v4(), 1), event(tenant, 2)]);
    let mut harness = open(vec![(0, held)], tenant);

    let frames = drain(&mut harness.session, 3).await;

    assert_eq!(kinds(&frames), vec!["topology", "event"]);
    let position = harness
        .session
        .positions()
        .into_iter()
        .next()
        .expect("one partition");
    assert_eq!(
        position.offset,
        Sequence::assigned(2),
        "the cursor is the last delivered"
    );
    assert_eq!(
        position.last_examined,
        Sequence::assigned(2),
        "and the frontier covers the rejected one"
    );
}

#[tokio::test]
async fn an_idle_stream_emits_a_heartbeat_rather_than_nothing() {
    tokio::time::pause();
    let tenant = Uuid::new_v4();
    let mut harness = open(vec![(0, StubPartition::empty())], tenant);

    let baseline = harness.session.next_frame().await;
    assert!(matches!(baseline, Some(Frame::Topology { .. })));

    tokio::time::advance(Duration::from_secs(6)).await;
    let next = harness.session.next_frame().await;

    assert!(matches!(next, Some(Frame::Heartbeat { .. })));
}

#[tokio::test]
async fn a_loss_narrows_the_read_set_and_the_stream_continues() {
    let tenant = Uuid::new_v4();
    let mut harness = open(
        vec![
            (0, StubPartition::empty()),
            (1, StubPartition::empty()),
            (2, StubPartition::empty()),
        ],
        tenant,
    );
    let _ = harness.session.next_frame().await;

    let before = Generation::new(7, vec![assignment(0), assignment(1), assignment(2)]);
    let after = Generation::new(8, vec![assignment(0), assignment(1)]);
    harness
        .session
        .apply(&AssignmentDelta::classify(&before, &after));

    let frame = harness.session.next_frame().await.expect("a frame");
    match frame {
        Frame::Topology {
            topology_version,
            positions,
        } => {
            assert_eq!(topology_version, 8);
            // The frame reports what the session will read next, not what it
            // held a moment ago.
            assert_eq!(positions.len(), 2);
        }
        other => panic!("expected a topology frame, got {other:?}"),
    }
    assert_eq!(harness.session.state(), SessionState::Streaming);
}

#[tokio::test]
async fn a_gain_ends_the_stream_with_a_terminal_frame_carrying_the_frontier() {
    let tenant = Uuid::new_v4();
    let held = StubPartition::holding(vec![event(tenant, 41)]);
    let mut harness = open(vec![(0, held)], tenant);
    let _ = drain(&mut harness.session, 2).await;

    harness.session.apply(&AssignmentDelta::Gain);
    let frames = drain(&mut harness.session, 3).await;

    assert_eq!(kinds(&frames), vec!["terminal"]);
    match frames.first() {
        Some(Frame::Control {
            positions, reason, ..
        }) => {
            assert_eq!(*reason, Some(CloseReason::Rebalanced));
            // The frontier goes out with the close, so the consumer can commit
            // before re-joining.
            assert_eq!(
                positions.first().map(|p| p.offset),
                Some(Sequence::assigned(41))
            );
        }
        other => panic!("expected a terminal control frame, got {other:?}"),
    }
    assert_eq!(harness.session.state(), SessionState::Closed);
    assert!(harness.session.next_frame().await.is_none());
}

#[tokio::test]
async fn losing_every_partition_ends_the_stream() {
    let tenant = Uuid::new_v4();
    let mut harness = open(vec![(0, StubPartition::empty())], tenant);
    let _ = harness.session.next_frame().await;

    harness.session.apply(&AssignmentDelta::LoseAll);
    let frame = harness.session.next_frame().await.expect("a frame");

    match frame {
        Frame::Control { reason, .. } => assert_eq!(reason, Some(CloseReason::LoseAll)),
        other => panic!("expected a terminal control frame, got {other:?}"),
    }
}

#[tokio::test]
async fn a_teardown_closes_with_its_own_reason() {
    let tenant = Uuid::new_v4();
    let mut harness = open(vec![(0, StubPartition::empty())], tenant);
    let _ = harness.session.next_frame().await;

    harness.session.tear_down();
    let frame = harness.session.next_frame().await.expect("a frame");

    match frame {
        Frame::Control { reason, .. } => assert_eq!(reason, Some(CloseReason::Teardown)),
        other => panic!("expected a terminal control frame, got {other:?}"),
    }
}

#[tokio::test]
async fn an_unchanged_delta_emits_nothing() {
    let tenant = Uuid::new_v4();
    tokio::time::pause();
    let mut harness = open(vec![(0, StubPartition::empty())], tenant);
    let _ = harness.session.next_frame().await;

    harness.session.apply(&AssignmentDelta::Unchanged);

    // Nothing queued, so the next frame is whatever the cadence produces - not a
    // topology frame for a topology that did not move.
    let next = tokio::time::timeout(Duration::from_millis(50), harness.session.next_frame()).await;
    assert!(next.is_err(), "no frame is owed yet");
}

#[tokio::test]
async fn reads_rotate_over_partitions_that_have_something() {
    let tenant = Uuid::new_v4();
    let busy = StubPartition::holding(vec![event(tenant, 1)]);
    let other = StubPartition::holding(vec![event(tenant, 2)]);
    let mut harness = open(
        vec![(0, StubPartition::empty()), (1, busy), (2, other)],
        tenant,
    );

    let frames = drain(&mut harness.session, 4).await;

    // The empty partition is skipped rather than taking a turn, so both busy
    // ones are served.
    assert_eq!(kinds(&frames), vec!["topology", "event", "event"]);
}

fn assignment(partition: i32) -> Assignment {
    Assignment {
        topic: gts(TOPIC),
        partition,
        offset: Sequence::assigned(0),
        last_examined: Sequence::assigned(0),
    }
}

/// A partition whose reader always claims readiness but whose reads never yield.
///
/// Not contrived: `has_data` is optimistic by design - the accounted frontier
/// being ahead of a reader does not mean the gap in front of *that* reader has
/// been filled - so a reader in a gap is perpetually "ready" while every read
/// reports the position unanswerable.
struct LyingPartition {
    reads: AtomicI64,
}

impl PartitionReader for LyingPartition {
    fn has_data(&self) -> bool {
        true
    }

    fn read(&self, _limit: ReadLimit) -> PartitionRead {
        self.reads.fetch_add(1, Ordering::Relaxed);
        PartitionRead::Unknown
    }

    fn seek(&self, _offset: Sequence) {}

    fn report_scanning(&self, _scanning: bool) {}
}

#[tokio::test]
async fn a_perpetually_ready_partition_that_never_yields_does_not_spin() {
    let tenant = Uuid::new_v4();
    let lying = Arc::new(LyingPartition {
        reads: AtomicI64::new(0),
    });
    let leases = Arc::new(InProcessStreamLeases::new());
    let lease = leases.acquire(Uuid::new_v4()).expect("lease is free");
    let filter: Arc<dyn EventFilter> =
        Arc::new(InterestFilter::compile(&[crate::domain::model::Interest {
            topic: gts(TOPIC),
            subject_tenant: tenant,
            ownership: OwnershipFilter::EntireTenant,
            barrier_mode: BarrierMode::Respect,
            types: vec![gts::GtsIdPattern::try_new(CREATED).unwrap()],
            subject_types: Vec::new(),
            filter: None,
        }]));
    let (generations, membership) = membership(8);
    let mut session = StreamSession::open(SessionOpening {
        read_set: ReadSet::seed(vec![PartitionSlot::new(
            key(0),
            Arc::clone(&lying) as Arc<dyn PartitionReader>,
        )]),
        filter,
        progress: ProgressConfig::default(),
        heartbeat_interval: Duration::from_secs(5),
        limit: ReadLimit::new(MaxEvents(256), MaxBytes(1024 * 1024)),
        baseline: Generation::new(1, (0..8).map(assignment).collect()),
        ready: Arc::new(Notify::new()),
        started_at: Instant::now(),
        now: Arc::new(Utc::now),
        unanswerable_tolerance: Duration::from_secs(30),
        lease,
        generations,
        membership,
        scope: None,
    });

    let baseline = session.next_frame().await;
    assert!(matches!(baseline, Some(Frame::Topology { .. })));

    // Real time, and the heartbeat is five seconds away, so a session that
    // parks makes at most a handful of reads before this expires. A spinning one
    // makes thousands.
    // Expected to expire: the point is that the session parked, not that it
    // produced anything.
    let parked = tokio::time::timeout(Duration::from_millis(50), session.next_frame()).await;
    assert!(
        parked.is_err(),
        "nothing is owed yet, so it should still be waiting"
    );

    let reads = lying.reads.load(Ordering::Relaxed);
    assert!(
        reads < 10,
        "the session issued {reads} reads in 50ms - it is spinning on an \
         optimistically-ready reader instead of parking"
    );
}

#[tokio::test]
async fn a_session_idle_across_many_topics_parks_on_one_waker() {
    let tenant = Uuid::new_v4();
    let leases = Arc::new(InProcessStreamLeases::new());
    let lease = leases.acquire(Uuid::new_v4()).expect("lease is free");

    // Four topics of eight partitions, all silent - the shape a consumer group
    // subscribing broadly actually has.
    let mut slots = Vec::new();
    let mut interests = Vec::new();
    let mut counters = Vec::new();
    for topic_index in 0..4 {
        let topic = gts(&format!(
            "gts.cf.core.events.topic.v1~x.eb.t{topic_index}.acme.v1"
        ));
        interests.push(crate::domain::model::Interest {
            topic: topic.clone(),
            subject_tenant: tenant,
            ownership: OwnershipFilter::EntireTenant,
            barrier_mode: BarrierMode::Respect,
            types: vec![gts::GtsIdPattern::try_new(CREATED).unwrap()],
            subject_types: Vec::new(),
            filter: None,
        });
        for partition in 0..8 {
            let quiet = Arc::new(LyingPartition {
                reads: AtomicI64::new(0),
            });
            counters.push(Arc::clone(&quiet));
            slots.push(PartitionSlot::new(
                PartitionKey::new(topic.clone(), partition),
                Arc::clone(&quiet) as Arc<dyn PartitionReader>,
            ));
        }
    }

    let filter: Arc<dyn EventFilter> = Arc::new(InterestFilter::compile(&interests));
    let (generations, membership) = membership(8);
    let mut session = StreamSession::open(SessionOpening {
        read_set: ReadSet::seed(slots),
        filter,
        progress: ProgressConfig::default(),
        heartbeat_interval: Duration::from_secs(5),
        limit: ReadLimit::new(MaxEvents(256), MaxBytes(1024 * 1024)),
        baseline: Generation::new(1, (0..8).map(assignment).collect()),
        ready: Arc::new(Notify::new()),
        started_at: Instant::now(),
        now: Arc::new(Utc::now),
        unanswerable_tolerance: Duration::from_secs(30),
        lease,
        generations,
        membership,
        scope: None,
    });

    let baseline = session.next_frame().await;
    assert!(matches!(baseline, Some(Frame::Topology { .. })));

    let parked = tokio::time::timeout(Duration::from_millis(50), session.next_frame()).await;
    assert!(
        parked.is_err(),
        "an idle session owes nothing for five seconds"
    );

    // Every reader claims readiness, so each partition is asked exactly once
    // before the round is found fruitless and the session parks on its single
    // shared waker. Thirty-two partitions therefore cost thirty-two reads, not
    // thirty-two futures and not an unbounded loop.
    let total: i64 = counters
        .iter()
        .map(|quiet| quiet.reads.load(Ordering::Relaxed))
        .sum();
    assert!(
        total <= 32,
        "thirty-two idle partitions issued {total} reads in 50ms - the session \
         is not parking after a fruitless round"
    );
}

#[tokio::test]
async fn a_quiet_tail_never_tears_the_stream_down() {
    tokio::time::pause();
    let tenant = Uuid::new_v4();
    let mut harness = open(vec![(0, StubPartition::empty())], tenant);
    let _ = harness.session.next_frame().await;

    // An empty partition reports `NothingNew`, which is a healthy idle stream.
    // Hours of it must produce heartbeats, never a teardown - conflating a quiet
    // tail with an unanswerable position would kill every idle consumer.
    for _ in 0..20 {
        tokio::time::advance(Duration::from_secs(6)).await;
        let frame = harness.session.next_frame().await;
        assert!(
            matches!(frame, Some(Frame::Heartbeat { .. })),
            "expected a heartbeat, got {frame:?}"
        );
    }
    assert_eq!(harness.session.state(), SessionState::Streaming);
}

#[tokio::test]
async fn a_sustained_unanswerable_position_tears_the_stream_down() {
    tokio::time::pause();
    let tenant = Uuid::new_v4();
    let lying = Arc::new(LyingPartition {
        reads: AtomicI64::new(0),
    });
    let leases = Arc::new(InProcessStreamLeases::new());
    let lease = leases.acquire(Uuid::new_v4()).expect("lease is free");
    let filter: Arc<dyn EventFilter> =
        Arc::new(InterestFilter::compile(&[crate::domain::model::Interest {
            topic: gts(TOPIC),
            subject_tenant: tenant,
            ownership: OwnershipFilter::EntireTenant,
            barrier_mode: BarrierMode::Respect,
            types: vec![gts::GtsIdPattern::try_new(CREATED).unwrap()],
            subject_types: Vec::new(),
            filter: None,
        }]));
    let (generations, membership) = membership(8);
    let mut session = StreamSession::open(SessionOpening {
        read_set: ReadSet::seed(vec![PartitionSlot::new(
            key(0),
            Arc::clone(&lying) as Arc<dyn PartitionReader>,
        )]),
        filter,
        progress: ProgressConfig::default(),
        heartbeat_interval: Duration::from_secs(5),
        limit: ReadLimit::new(MaxEvents(256), MaxBytes(1024 * 1024)),
        baseline: Generation::new(1, (0..8).map(assignment).collect()),
        ready: Arc::new(Notify::new()),
        started_at: Instant::now(),
        now: Arc::new(Utc::now),
        unanswerable_tolerance: Duration::from_secs(30),
        lease,
        generations,
        membership,
        scope: None,
    });
    let _ = session.next_frame().await;

    // The position never becomes answerable. Heartbeats carry on while the
    // tolerance runs, and then the stream ends with a reason the consumer can
    // act on rather than an apparently-alive stream that delivers nothing.
    let mut frames = Vec::new();
    for _ in 0..12 {
        tokio::time::advance(Duration::from_secs(6)).await;
        if let Some(frame) = session.next_frame().await {
            let terminal = matches!(frame, Frame::Control { .. });
            frames.push(frame);
            if terminal {
                break;
            }
        }
    }

    match frames.last() {
        Some(Frame::Control { reason, .. }) => {
            assert_eq!(*reason, Some(CloseReason::Teardown));
        }
        other => panic!("expected a teardown terminal frame, got {other:?}"),
    }
    assert!(
        frames.len() > 1,
        "it should have heartbeated while the tolerance ran, not given up at once"
    );
    assert_eq!(session.state(), SessionState::Closed);
}

#[tokio::test]
async fn a_version_only_delta_reports_the_version_and_continues() {
    let tenant = Uuid::new_v4();
    let mut harness = open(vec![(0, StubPartition::empty())], tenant);
    let _ = harness.session.next_frame().await;

    let before = Generation::new(7, vec![assignment(0)]);
    let after = Generation::new(9, vec![assignment(0)]);
    harness
        .session
        .apply(&AssignmentDelta::classify(&before, &after));

    match harness.session.next_frame().await {
        Some(Frame::Topology {
            topology_version, ..
        }) => assert_eq!(topology_version, 9),
        other => panic!("expected a topology frame, got {other:?}"),
    }
    assert_eq!(harness.session.state(), SessionState::Streaming);
}

#[tokio::test]
async fn a_loss_and_a_gain_together_terminate() {
    let tenant = Uuid::new_v4();
    let mut harness = open(
        vec![(0, StubPartition::empty()), (1, StubPartition::empty())],
        tenant,
    );
    let _ = harness.session.next_frame().await;

    // Partition 0 goes, 2 arrives. The loss alone would continue; the gain
    // cannot, because a gained partition has no cursor here.
    let before = Generation::new(7, vec![assignment(0), assignment(1)]);
    let after = Generation::new(8, vec![assignment(1), assignment(2)]);
    harness
        .session
        .apply(&AssignmentDelta::classify(&before, &after));

    match harness.session.next_frame().await {
        Some(Frame::Control { reason, .. }) => {
            assert_eq!(reason, Some(CloseReason::Rebalanced));
        }
        other => panic!("expected a terminal frame, got {other:?}"),
    }
}

// The member a stream opens for in the tests below, and peers that join its
// group after it. The coordinator splits a topic's partitions in ranges over
// the members sorted by id: of two partitions the member keeps 0 and `PEER`
// takes 1; of one, `LOWER_PEER` takes none and the member keeps it.
const MEMBER: Uuid = Uuid::from_u128(0x5a);
const PEER: Uuid = Uuid::from_u128(0x5b);
const LOWER_PEER: Uuid = Uuid::from_u128(0x59);

/// A coordinator that split `partitions` partitions of `TOPIC` over
/// `members`, joined in this order.
fn group_of(partitions: i32, members: &[Uuid]) -> (Arc<ConsumerGroupCoordinator>, GtsInstanceId) {
    let groups = Arc::new(ConsumerGroupCoordinator::new(Duration::from_mins(1)));
    let group = gts("gts.cf.core.events.consumer_group.v1~02943530-10da-4624-a3ae-b998c425847f");
    for member in members {
        groups.join_member(
            &group,
            *member,
            &[TopicInterest {
                id: gts(TOPIC),
                partitions,
            }],
            Duration::from_secs(30),
        );
    }
    (groups, group)
}

/// `TOPIC`'s `partition`, at the start of an empty partition.
fn at_start(partition: i32) -> Position {
    Position::builder(gts(TOPIC), partition).build()
}

/// The stream open read the member's subscription at v1 with partitions 0
/// and 1 and seeded its read set from it; the peer joined before the session
/// opened, so the watch already holds v2 with partition 0. The opening frame
/// reports what the read set holds, and the loss follows at once, without
/// waiting for the watch to change again.
#[tokio::test(start_paused = true)]
async fn an_opening_whose_watch_lost_partitions_reports_the_loss_first() {
    let (groups, group) = group_of(2, &[MEMBER, PEER]);
    let streaming =
        ConsumerGroupCoordinator::subscribe(&groups, &group, MEMBER).expect("the member joined");
    assert_eq!(
        *streaming.0.borrow(),
        Generation::new(2, vec![assignment(0)])
    );
    let mut harness = open_from(
        Generation::new(1, vec![assignment(0), assignment(1)]),
        vec![
            PartitionSlot::new(key(0), StubPartition::empty()),
            PartitionSlot::new(key(1), StubPartition::empty()),
        ],
        Arc::new(InterestFilter::compile(&[])),
        ScopeInput::Alone,
        streaming,
    );
    let start = Instant::now();

    let (_, frames) = frames_until(&mut harness.session, start, start + BOUND).await;

    assert_eq!(kinds(&frames), vec!["topology", "topology"]);
    let [
        Frame::Topology {
            topology_version: 1,
            positions: opening,
        },
        Frame::Topology {
            topology_version: 2,
            positions: loss,
        },
    ] = frames.as_slice()
    else {
        panic!("expected the opening at v1, then the loss at v2, got {frames:?}");
    };
    assert_eq!(opening, &[at_start(0), at_start(1)]);
    assert_eq!(loss, &[at_start(0)]);
    assert_eq!(harness.session.positions(), vec![at_start(0)]);
    assert_eq!(harness.session.state(), SessionState::Streaming);
}

/// The stream open read the member's subscription at v2 with partition 0;
/// the peer left before the session opened, so the watch already holds v3
/// with partitions 0 and 1. The stream ends with a rebalance at once: the
/// gained partition has no cursor here.
#[tokio::test(start_paused = true)]
async fn an_opening_whose_watch_gained_partitions_closes_with_a_rebalance() {
    let (groups, group) = group_of(2, &[MEMBER, PEER]);
    assert!(groups.leave(&group, PEER).is_some(), "the peer leaves");
    let streaming =
        ConsumerGroupCoordinator::subscribe(&groups, &group, MEMBER).expect("the member joined");
    assert_eq!(
        *streaming.0.borrow(),
        Generation::new(3, vec![assignment(0), assignment(1)])
    );
    let mut harness = open_from(
        Generation::new(2, vec![assignment(0)]),
        vec![PartitionSlot::new(key(0), StubPartition::empty())],
        Arc::new(InterestFilter::compile(&[])),
        ScopeInput::Alone,
        streaming,
    );
    let start = Instant::now();

    let (_, frames) = frames_until(&mut harness.session, start, start + BOUND).await;

    assert_eq!(kinds(&frames), vec!["topology", "terminal"]);
    let [
        Frame::Topology {
            topology_version: 2,
            positions: opening,
        },
        Frame::Control {
            code: ControlCode::Terminal,
            positions: frontier,
            reason: Some(CloseReason::Rebalanced),
        },
    ] = frames.as_slice()
    else {
        panic!("expected the opening at v2, then a rebalanced close, got {frames:?}");
    };
    assert_eq!(opening, &[at_start(0)]);
    assert_eq!(frontier, &[at_start(0)]);
    assert_eq!(harness.session.state(), SessionState::Closed);
}

/// A watch can hold the subscription's partitions under an older version:
/// the coordinator republishes a member only when its partitions move, while
/// every rebalance advances the version the subscription carries. That is no
/// change for the session: after the opening frame it owes nothing, not even
/// the version-only `topology` frame a real version bump gets at once. In the
/// second row the baseline lists its partitions in another order than the
/// watch, which is the same set.
#[tokio::test(start_paused = true)]
async fn an_opening_whose_watch_differs_only_in_version_sends_no_version_frame() {
    for (members, partitions, baseline) in [
        (&[MEMBER, LOWER_PEER][..], 1, vec![assignment(0)]),
        (&[MEMBER][..], 2, vec![assignment(1), assignment(0)]),
    ] {
        let (groups, group) = group_of(partitions, members);
        let streaming = ConsumerGroupCoordinator::subscribe(&groups, &group, MEMBER)
            .expect("the member joined");
        assert_eq!(
            *streaming.0.borrow(),
            Generation::new(1, (0..partitions).map(assignment).collect())
        );
        let mut harness = open_from(
            Generation::new(2, baseline),
            (0..partitions)
                .map(|partition| PartitionSlot::new(key(partition), StubPartition::empty()))
                .collect(),
            Arc::new(InterestFilter::compile(&[])),
            ScopeInput::Alone,
            streaming,
        );
        let start = Instant::now();

        let (_, frames) = frames_until(&mut harness.session, start, start + BOUND).await;

        let [
            Frame::Topology {
                topology_version: 2,
                positions,
            },
        ] = frames.as_slice()
        else {
            panic!("expected the opening at v2 alone, got {frames:?}");
        };
        assert_eq!(
            positions,
            &(0..partitions).map(at_start).collect::<Vec<_>>()
        );
        assert_eq!(harness.session.state(), SessionState::Streaming);
    }
}

#[tokio::test]
async fn dropping_an_unpolled_session_releases_its_lease() {
    let leases = Arc::new(InProcessStreamLeases::new());
    let subscription = Uuid::new_v4();
    let lease = leases.acquire(subscription).expect("lease is free");
    let filter: Arc<dyn EventFilter> = Arc::new(InterestFilter::compile(&[]));

    let (generations, membership) = membership(8);
    let session = StreamSession::open(SessionOpening {
        read_set: ReadSet::seed(Vec::new()),
        filter,
        progress: ProgressConfig::default(),
        heartbeat_interval: Duration::from_secs(5),
        limit: ReadLimit::new(MaxEvents(1), MaxBytes(1)),
        baseline: Generation::new(1, (0..8).map(assignment).collect()),
        ready: Arc::new(Notify::new()),
        started_at: Instant::now(),
        now: Arc::new(Utc::now),
        unanswerable_tolerance: Duration::from_secs(30),
        lease,
        generations,
        membership,
        scope: None,
    });
    assert!(leases.is_held(subscription));

    // Never polled. A client that opens a stream and vanishes must not leave the
    // subscription unable to stream again, and release is ownership rather than
    // a guard somebody has to run.
    drop(session);

    assert!(!leases.is_held(subscription));
}

#[tokio::test]
async fn the_frame_stream_yields_the_same_sequence_as_the_session() {
    use tokio_stream::StreamExt;

    let tenant = Uuid::new_v4();
    let held = StubPartition::holding(vec![event(tenant, 1), event(tenant, 2)]);
    let harness = open(vec![(0, held)], tenant);
    let mut stream = crate::domain::streaming::session::FrameStream::new(harness.session);

    let mut kinds_seen = Vec::new();
    for _ in 0..3 {
        match tokio::time::timeout(Duration::from_millis(200), stream.next()).await {
            Ok(Some(frame)) => kinds_seen.push(kinds(&[frame]).remove(0)),
            Ok(None) | Err(_) => break,
        }
    }

    assert_eq!(kinds_seen, vec!["topology", "event", "event"]);
}

#[tokio::test]
async fn the_frame_stream_ends_when_the_session_closes() {
    use tokio_stream::StreamExt;

    let tenant = Uuid::new_v4();
    let mut harness = open(vec![(0, StubPartition::empty())], tenant);
    let _ = harness.session.next_frame().await;
    harness.session.tear_down();

    let mut stream = crate::domain::streaming::session::FrameStream::new(harness.session);

    let terminal = stream.next().await;
    assert!(matches!(terminal, Some(Frame::Control { .. })));
    assert!(stream.next().await.is_none(), "and stays ended");
}

/// A quiet session must park between frames, not spin.
///
/// The rhythm no other test here uses: **progress overdue, heartbeat not yet
/// due**. Every other idle test advances past the heartbeat, so the heartbeat
/// branch returns first and the park below it is never reached - which is
/// exactly why the defect this guards survived.
///
/// Falsify by reverting the unconditional `record_emitted` in `advance`: the
/// progress timer then stays permanently overdue, the deadline handed to the
/// park is already expired, and the probe count runs to millions.
#[tokio::test]
async fn a_quiet_session_parks_between_frames_rather_than_spinning() {
    let tenant = Uuid::new_v4();
    let held = StubPartition::empty();
    let leases = Arc::new(InProcessStreamLeases::new());
    let lease = leases.acquire(Uuid::new_v4()).expect("lease is free");
    let (generations, membership) = membership(8);

    let filter: Arc<dyn EventFilter> =
        Arc::new(InterestFilter::compile(&[crate::domain::model::Interest {
            topic: gts(TOPIC),
            subject_tenant: tenant,
            ownership: OwnershipFilter::EntireTenant,
            barrier_mode: BarrierMode::Respect,
            types: vec![gts::GtsIdPattern::try_new(CREATED).unwrap()],
            subject_types: Vec::new(),
            filter: None,
        }]));

    let now: NowFn = Arc::new(Utc::now);
    let mut session = StreamSession::open(SessionOpening {
        read_set: ReadSet::seed(vec![PartitionSlot::new(
            key(0),
            Arc::clone(&held) as Arc<dyn PartitionReader>,
        )]),
        filter,
        // Progress falls due almost immediately and has nothing to report,
        // because an empty partition never drifts. The heartbeat is the only
        // frame that can legitimately arrive, and it is what bounds the test.
        progress: ProgressConfig {
            drift_threshold: 1000,
            min_interval: Duration::from_millis(10),
        },
        heartbeat_interval: Duration::from_millis(300),
        limit: ReadLimit::new(MaxEvents(256), MaxBytes(1024 * 1024)),
        baseline: Generation::new(7, (0..8).map(assignment).collect()),
        ready: Arc::new(Notify::new()),
        started_at: Instant::now(),
        now,
        unanswerable_tolerance: Duration::from_mins(5),
        lease,
        generations,
        membership,
        scope: None,
    });

    // Real time, not paused: a spinning task never yields to a paused clock, so
    // the failure would be a hang rather than an assertion.
    let first = session.next_frame().await;
    assert!(
        matches!(first, Some(Frame::Topology { .. })),
        "expected the open-time baseline, got {first:?}"
    );
    let after_baseline = held.probes.load(Ordering::Relaxed);

    let second = tokio::time::timeout(Duration::from_secs(5), session.next_frame())
        .await
        .expect("a heartbeat must arrive within 5s");
    assert!(
        matches!(second, Some(Frame::Heartbeat { .. })),
        "expected a heartbeat, got {second:?}"
    );

    // Across ~300ms of idling the session should probe a handful of times - once
    // per wake. A spin does it as fast as the CPU allows.
    let probes = held.probes.load(Ordering::Relaxed) - after_baseline;
    assert!(
        probes < 1000,
        "a quiet session probed readiness {probes} times between two frames; \
         it is spinning rather than parking"
    );
}

// -- Deciding through the member's consume scope --

/// Bounds a wait for a frame the session owes at once. Well below the
/// heartbeat interval (5 s), so a frame that only a heartbeat pass would
/// produce fails the wait.
const BOUND: Duration = Duration::from_secs(1);

/// A session over `partitions` of the scoped fixture's `TOPIC`, deciding
/// through a [`ScopedFilter`] over its wildcard interest, which starts from
/// the scope `feed` holds and is fed every later one.
fn open_scoped<R: PartitionReader + 'static>(
    partitions: Vec<(i32, Arc<R>)>,
    authorizer: SingleTypeAuthorizer,
    feed: watch::Receiver<Option<Arc<CachedScope>>>,
) -> Harness {
    let held = feed.borrow().clone().expect("the feed starts with a scope");
    let filter = ScopedFilter::new(&[scoped::wildcard_interest()], held, authorizer)
        .expect("the scope holds the interest's group");
    open_with(
        partitions
            .into_iter()
            .map(|(partition, held)| (PartitionKey::new(gts(scoped::TOPIC), partition), held))
            .collect(),
        Arc::new(filter),
        ScopeInput::Fed(feed),
    )
}

/// A partition holding one event of each of `types`, in order from
/// sequence `first`.
fn scoped_partition(types: &[&str], first: i64) -> Arc<StubPartition> {
    StubPartition::holding(
        types
            .iter()
            .zip(first..)
            .map(|(event_type, sequence)| scoped::event_of(scoped::TOPIC, event_type, sequence))
            .collect(),
    )
}

fn delivered(frames: &[Frame]) -> Vec<i64> {
    frames
        .iter()
        .filter_map(|frame| match frame {
            Frame::Event(event) => event.sequence.map(Sequence::as_i64),
            _ => None,
        })
        .collect()
}

fn position(partition: i32, offset: i64, last_examined: i64) -> Position {
    Position::builder(gts(scoped::TOPIC), partition)
        .offset(Sequence::assigned(offset))
        .last_examined(Sequence::assigned(last_examined))
        .build()
}

/// The frame a teardown ends with, carrying `positions`.
fn assert_teardown(frame: Option<&Frame>, expected: &[Position]) {
    let Some(Frame::Control {
        code: ControlCode::Terminal,
        positions,
        reason: Some(CloseReason::Teardown),
    }) = frame
    else {
        panic!("expected a teardown terminal frame, got {frame:?}");
    };
    assert_eq!(positions, expected);
}

/// Lines of the warning a refused single-type evaluation logs, for `reason`.
fn refusal_warnings(lines: &[&str], reason: &str) -> usize {
    lines
        .iter()
        .filter(|line| line.contains("refused it") && line.contains(&format!("reason={reason}")))
        .count()
}

/// A parent selector can match a derived event that the caller cannot consume.
#[tokio::test]
async fn parent_subscription_does_not_deliver_a_denied_derived_type() {
    const AUDIT: &str = "gts.cf.core.events.event.v1~x.eb.scoped.created.v1~x.eb.scoped.audit.v1~";
    let pdp = TypePdp::answering(|request: &EvaluationRequest| {
        if request.resource.properties["event_types"] == json!([AUDIT]) {
            Answer::Deny
        } else {
            Answer::Allow
        }
    });
    let created = scoped::interest(scoped::TOPIC, &[scoped::CREATED], BarrierMode::Respect);
    let scope = scoped::held_scope(
        1,
        vec![scoped::group_of(
            &created,
            scoped::tenant_scope(Vec::new()),
            &[scoped::CREATED],
        )],
    );
    let filter = ScopedFilter::new(
        &[created],
        scope,
        scoped::authorizer(&pdp, KnownTypes::on_topic(&[]), scoped::DEADLINE),
    )
    .unwrap();
    let held = scoped_partition(&[scoped::CREATED, AUDIT, scoped::CREATED], 1);
    let mut harness = open_with(
        vec![(PartitionKey::new(gts(scoped::TOPIC), 0), held)],
        Arc::new(filter),
        ScopeInput::Alone,
    );

    let frames = drain(&mut harness.session, 4).await;

    assert_eq!(delivered(&frames), vec![1, 3]);
    assert_eq!(harness.session.positions(), vec![position(0, 3, 3)]);
    assert_eq!(pdp.asked_types(), vec![AUDIT]);
}

/// A session over `held` on partition 0 of the scoped fixture's `TOPIC`,
/// deciding through one interest in `types`, whose group holds `candidates`
/// and a scope over the fixture's tenant that `filters` narrow.
fn open_selecting(
    pdp: &Arc<TypePdp>,
    types: &[&str],
    filters: Vec<toolkit_security::ScopeFilter>,
    candidates: &[&str],
    held: Arc<StubPartition>,
) -> Harness {
    let selector = scoped::interest(scoped::TOPIC, types, BarrierMode::Respect);
    let scope = scoped::held_scope(
        1,
        vec![scoped::group_of(
            &selector,
            scoped::tenant_scope(filters),
            candidates,
        )],
    );
    let filter = ScopedFilter::new(
        &[selector],
        scope,
        scoped::authorizer(pdp, KnownTypes::on_topic(&[]), scoped::DEADLINE),
    )
    .unwrap();
    open_with(
        vec![(PartitionKey::new(gts(scoped::TOPIC), 0), held)],
        Arc::new(filter),
        ScopeInput::Alone,
    )
}

/// `CREATED` and `EXPRESS` both match the one `EXPRESS` event; it becomes one
/// frame.
#[tokio::test]
async fn an_event_two_selectors_of_one_interest_match_is_delivered_once() {
    let pdp = TypePdp::answering(|_: &EvaluationRequest| Answer::Allow);
    let mut harness = open_selecting(
        &pdp,
        &[scoped::CREATED, scoped::EXPRESS],
        Vec::new(),
        &[scoped::CREATED, scoped::EXPRESS],
        scoped_partition(&[scoped::EXPRESS], 1),
    );

    let frames = drain(&mut harness.session, 4).await;

    assert_eq!(kinds(&frames), vec!["topology", "event"]);
    assert_eq!(delivered(&frames), vec![1]);
    assert_eq!(harness.session.positions(), vec![position(0, 1, 1)]);
    assert_eq!(pdp.recorded(), Vec::<serde_json::Value>::new());
}

/// The selector `CREATED` matches all three candidates; the scope names
/// `CREATED` and `RUSH` only. The skipped `EXPRESS` is the batch's last event,
/// so the position examined past it shows the skip moved the frontier.
#[tokio::test]
async fn a_derived_candidate_the_scope_excludes_is_skipped_and_progress_advances() {
    let pdp = TypePdp::answering(|_: &EvaluationRequest| Answer::Allow);
    let mut harness = open_selecting(
        &pdp,
        &[scoped::CREATED],
        vec![toolkit_security::ScopeFilter::r#in(
            crate::domain::streaming::scope::EVENT_TYPE,
            vec![
                toolkit_security::ScopeValue::String(scoped::CREATED.to_owned()),
                toolkit_security::ScopeValue::String(scoped::RUSH.to_owned()),
            ],
        )],
        &[scoped::CREATED, scoped::EXPRESS, scoped::RUSH],
        scoped_partition(&[scoped::CREATED, scoped::RUSH, scoped::EXPRESS], 1),
    );

    let frames = drain(&mut harness.session, 4).await;

    assert_eq!(kinds(&frames), vec!["topology", "event", "event"]);
    assert_eq!(delivered(&frames), vec![1, 2]);
    assert_eq!(harness.session.positions(), vec![position(0, 2, 3)]);
    assert_eq!(pdp.recorded(), Vec::<serde_json::Value>::new());
}

/// The denied `SHIPPED` ends partition 0's batch; partition 1's batch holds
/// another, which needs no call.
#[tokio::test]
#[tracing_test::traced_test]
async fn a_denied_single_type_is_skipped_and_progress_advances() {
    let pdp = TypePdp::answering(|_: &EvaluationRequest| Answer::Deny);
    let (_feed, scope) = watch::channel(Some(scoped::wildcard_scope(1)));
    let mut harness = open_scoped(
        vec![
            (0, scoped_partition(&[scoped::CREATED, scoped::SHIPPED], 1)),
            (1, scoped_partition(&[scoped::SHIPPED], 3)),
        ],
        scoped::authorizer(&pdp, KnownTypes::on_topic(&[]), scoped::DEADLINE),
        scope,
    );

    let frames = drain(&mut harness.session, 4).await;

    assert_eq!(kinds(&frames), vec!["topology", "event"]);
    assert_eq!(delivered(&frames), vec![1]);
    assert_eq!(harness.session.state(), SessionState::Streaming);
    assert_eq!(
        harness.session.positions(),
        vec![position(0, 1, 2), position(1, 0, 3)]
    );
    assert_eq!(pdp.asked_types(), vec![scoped::SHIPPED]);
    logs_assert(|lines: &[&str]| match refusal_warnings(lines, "Denied") {
        1 => Ok(()),
        n => Err(format!("expected one refusal warning, got {n}")),
    });
}

#[tokio::test]
#[tracing_test::traced_test]
async fn a_compile_failure_or_an_unenforceable_scope_is_a_deny() {
    for (refused, reason) in [
        (Answer::Uncompilable, "CompileFailed"),
        (Answer::Unenforceable, "NotEnforceable"),
    ] {
        let pdp = TypePdp::answering(move |_: &EvaluationRequest| refused);
        let (_feed, scope) = watch::channel(Some(scoped::wildcard_scope(1)));
        let mut harness = open_scoped(
            vec![
                (0, scoped_partition(&[scoped::CREATED, scoped::SHIPPED], 1)),
                (1, scoped_partition(&[scoped::SHIPPED], 3)),
            ],
            scoped::authorizer(&pdp, KnownTypes::on_topic(&[]), scoped::DEADLINE),
            scope,
        );

        let frames = drain(&mut harness.session, 4).await;

        assert_eq!(kinds(&frames), vec!["topology", "event"], "{reason}");
        assert_eq!(delivered(&frames), vec![1], "{reason}");
        assert_eq!(
            harness.session.positions(),
            vec![position(0, 1, 2), position(1, 0, 3)],
            "{reason}"
        );
        assert_eq!(pdp.asked_types(), vec![scoped::SHIPPED], "{reason}");
    }
    logs_assert(|lines: &[&str]| {
        match (
            refusal_warnings(lines, "CompileFailed"),
            refusal_warnings(lines, "NotEnforceable"),
        ) {
            (1, 1) => Ok(()),
            counts => Err(format!(
                "expected one refusal warning per row, got {counts:?}"
            )),
        }
    });
}

/// A PDP outage must not publish a partial batch or report a cursor beyond
/// events whose authorization could not be established.
#[tokio::test]
async fn an_unavailable_single_type_evaluation_tears_down_without_progress() {
    let pdp = TypePdp::answering(|_: &EvaluationRequest| Answer::Fail);
    let (_feed, scope) = watch::channel(Some(scoped::wildcard_scope(1)));
    let mut harness = open_scoped(
        vec![(0, scoped_partition(&[scoped::CREATED, scoped::SHIPPED], 1))],
        scoped::authorizer(&pdp, KnownTypes::on_topic(&[]), scoped::DEADLINE),
        scope,
    );

    let frames = drain(&mut harness.session, 4).await;

    assert_eq!(kinds(&frames), vec!["topology", "terminal"]);
    assert_teardown(frames.last(), &[position(0, 0, 0)]);
    assert_eq!(harness.session.positions(), vec![position(0, 0, 0)]);
    assert_eq!(pdp.asked_types(), vec![scoped::SHIPPED]);
}

/// The PDP never answers and the evaluation deadline is 100 ms. The teardown
/// is owed at about 100 ms; `SHORT + BOUND` leaves 1 s of margin for a slow
/// runner. Without the deadline the enforcer's own 30 s timeout would apply,
/// far past the bound.
#[tokio::test]
async fn a_single_type_deadline_tears_down_without_progress() {
    const SHORT: Duration = Duration::from_millis(100);
    let pdp = TypePdp::answering(|_: &EvaluationRequest| Answer::Never);
    let (_feed, scope) = watch::channel(Some(scoped::wildcard_scope(1)));
    let mut harness = open_scoped(
        vec![(0, scoped_partition(&[scoped::CREATED, scoped::SHIPPED], 1))],
        scoped::authorizer(&pdp, KnownTypes::on_topic(&[]), SHORT),
        scope,
    );
    let topology = harness.session.next_frame().await;
    assert!(
        matches!(topology, Some(Frame::Topology { .. })),
        "{topology:?}"
    );

    let started = Instant::now();
    let terminal = tokio::time::timeout(SHORT + BOUND, harness.session.next_frame())
        .await
        .expect("the deadline ends the batch");

    assert!(started.elapsed() >= SHORT, "torn down before the deadline");
    assert_teardown(terminal.as_ref(), &[position(0, 0, 0)]);
    assert_eq!(harness.session.positions(), vec![position(0, 0, 0)]);
}

/// In production a publish reads the event type too, so a test there arms
/// the failure after publishing; here the events are placed directly.
#[tokio::test]
async fn a_failed_point_read_tears_down_without_progress() {
    let pdp = TypePdp::answering(|_: &EvaluationRequest| Answer::Allow);
    let hook = SpecQueryHook::new();
    hook.arm(|query| match query {
        SpecQuery::EventType => Err(DomainError::StorageUnavailable {
            reason: "the event type cannot be read".to_owned(),
            source: None,
        }),
        _ => Ok(()),
    });
    let (_feed, scope) = watch::channel(Some(scoped::wildcard_scope(1)));
    let mut harness = open_scoped(
        vec![(0, scoped_partition(&[scoped::CREATED, scoped::SHIPPED], 1))],
        scoped::authorizer(&pdp, hook.wrap(KnownTypes::on_topic(&[])), scoped::DEADLINE),
        scope,
    );

    let frames = drain(&mut harness.session, 4).await;

    assert_eq!(kinds(&frames), vec!["topology", "terminal"]);
    assert_teardown(frames.last(), &[position(0, 0, 0)]);
    assert_eq!(hook.calls(SpecQuery::EventType), 1);
    assert_eq!(pdp.recorded(), Vec::<serde_json::Value>::new());
}

/// Parks the idle session, then runs `act` beside it: `join!` polls the
/// session first, which parks on its first poll (nothing to read, no frame
/// due); `act` then waits 16 yields, a bound on the polls a slower park
/// would need, before it runs.
async fn next_frame_after(
    session: &mut StreamSession,
    act: impl std::future::Future<Output = ()>,
) -> Option<Frame> {
    let (frame, ()) = tokio::join!(tokio::time::timeout(BOUND, session.next_frame()), async {
        for _ in 0..16 {
            tokio::task::yield_now().await;
        }
        act.await;
    });
    frame.expect("the session wakes on its scope feed, not on a heartbeat")
}

#[tokio::test]
async fn a_cleared_scope_tears_an_idle_stream_down_at_once() {
    let pdp = TypePdp::answering(|_: &EvaluationRequest| Answer::Allow);
    let (feed, scope) = watch::channel(Some(scoped::wildcard_scope(1)));
    let mut harness = open_scoped(
        vec![(0, StubPartition::empty())],
        scoped::authorizer(&pdp, KnownTypes::on_topic(&[]), scoped::DEADLINE),
        scope,
    );
    let topology = harness.session.next_frame().await;
    assert!(
        matches!(topology, Some(Frame::Topology { .. })),
        "{topology:?}"
    );

    let frame = next_frame_after(&mut harness.session, async {
        feed.send_replace(None);
    })
    .await;

    assert_teardown(frame.as_ref(), &[position(0, 0, 0)]);
    assert!(harness.session.next_frame().await.is_none());
}

#[tokio::test]
async fn a_removed_member_tears_the_stream_down() {
    let pdp = TypePdp::answering(|_: &EvaluationRequest| Answer::Allow);
    let (feed, scope) = watch::channel(Some(scoped::wildcard_scope(1)));
    let mut harness = open_scoped(
        vec![(0, StubPartition::empty())],
        scoped::authorizer(&pdp, KnownTypes::on_topic(&[]), scoped::DEADLINE),
        scope,
    );
    let topology = harness.session.next_frame().await;
    assert!(
        matches!(topology, Some(Frame::Topology { .. })),
        "{topology:?}"
    );

    let frame = next_frame_after(&mut harness.session, async move {
        drop(feed);
    })
    .await;

    assert_teardown(frame.as_ref(), &[position(0, 0, 0)]);
}

/// The usual member scope under generation 1, its one group serving until
/// `expires_at`.
fn wildcard_scope_until(expires_at: Instant) -> Arc<CachedScope> {
    let [group] = <[AuthorizedGroup; 1]>::try_from(scoped::wildcard_scope(1).groups.to_vec())
        .expect("the usual scope has one group");
    scoped::held_scope(
        1,
        vec![AuthorizedGroup {
            expires_at,
            ..group
        }],
    )
}

/// Every frame `session` hands over until it ends or `until` passes, each
/// with the time from `start` it came at. Bounded as a whole: heartbeats
/// keep coming while the stream lives.
async fn frames_until(
    session: &mut StreamSession,
    start: Instant,
    until: Instant,
) -> (Vec<Duration>, Vec<Frame>) {
    let mut frames = (Vec::new(), Vec::new());
    while let Ok(Some(frame)) = tokio::time::timeout_at(until, session.next_frame()).await {
        frames.0.push(Instant::now().duration_since(start));
        frames.1.push(frame);
    }
    frames
}

/// No worker refreshes a fixed feed. Its scope's one group expires 7 s after
/// the open, between the heartbeats at 5 s and 10 s: the idle stream tears
/// down at the expiry, not at its next heartbeat. The paused clock makes the
/// instants exact.
#[tokio::test(start_paused = true)]
async fn an_idle_stream_tears_down_when_its_scope_expires() {
    let pdp = TypePdp::answering(|_: &EvaluationRequest| Answer::Allow);
    let start = Instant::now();
    let expires_in = Duration::from_secs(7);
    let (_feed, scope) = watch::channel(Some(wildcard_scope_until(start + expires_in)));
    let mut harness = open_scoped(
        vec![(0, StubPartition::empty())],
        scoped::authorizer(&pdp, KnownTypes::on_topic(&[]), scoped::DEADLINE),
        scope,
    );

    let (at, frames) = frames_until(&mut harness.session, start, start + expires_in + BOUND).await;

    assert_eq!(at, [0, 5, 7].map(Duration::from_secs));
    assert_eq!(kinds(&frames), vec!["topology", "heartbeat", "terminal"]);
    assert_teardown(frames.last(), &[position(0, 0, 0)]);
    assert!(harness.session.next_frame().await.is_none());
}

/// The scope expires while nobody reads the stream, and an event arrives
/// after that. The next read finds the scope expired before it reads: the
/// stream tears down and the event stays unread.
#[tokio::test(start_paused = true)]
async fn a_stream_reads_nothing_after_its_scope_expires() {
    let pdp = TypePdp::answering(|_: &EvaluationRequest| Answer::Allow);
    let start = Instant::now();
    let expires_in = Duration::from_secs(7);
    let (_feed, scope) = watch::channel(Some(wildcard_scope_until(start + expires_in)));
    let held = StubPartition::empty();
    let mut harness = open_scoped(
        vec![(0, Arc::clone(&held))],
        scoped::authorizer(&pdp, KnownTypes::on_topic(&[]), scoped::DEADLINE),
        scope,
    );
    let topology = harness.session.next_frame().await;
    assert!(
        matches!(topology, Some(Frame::Topology { .. })),
        "{topology:?}"
    );

    tokio::time::advance(expires_in + Duration::from_secs(1)).await;
    held.events
        .lock()
        .expect("test lock")
        .push(scoped::event_of(scoped::TOPIC, scoped::CREATED, 1));
    let read_at = Instant::now();
    let (_, frames) = frames_until(&mut harness.session, read_at, read_at + BOUND).await;

    assert_eq!(kinds(&frames), vec!["terminal"]);
    assert_teardown(frames.last(), &[position(0, 0, 0)]);
    let unread: Vec<Uuid> = held
        .events
        .lock()
        .expect("test lock")
        .iter()
        .map(|event| event.id)
        .collect();
    assert_eq!(unread, vec![Uuid::from_u128(1)]);
}

/// Decides every event once `self.0` has passed, and admits each.
struct SlowPrepare(Duration);

#[async_trait::async_trait]
impl EventFilter for SlowPrepare {
    fn matches(&self, _event: &Event) -> bool {
        true
    }

    async fn prepare(&self, _events: &EventBatch) -> Result<Prepared, DomainError> {
        tokio::time::sleep(self.0).await;
        Ok(Prepared::All)
    }

    fn admits(&self, _event: &Event) -> Result<bool, DomainError> {
        Ok(true)
    }
}

/// A batch is read while the scope serves, and its decisions come 1 s after
/// the scope expires, 7 s after the open. None of its events is handed over:
/// the stream tears down when they are decided, with no progress for the
/// batch.
#[tokio::test(start_paused = true)]
async fn a_batch_decided_after_the_expiry_is_not_delivered() {
    let start = Instant::now();
    let expires_in = Duration::from_secs(7);
    let decided_in = expires_in + Duration::from_secs(1);
    let (_feed, scope) = watch::channel(Some(wildcard_scope_until(start + expires_in)));
    let mut harness = open_with(
        vec![(
            PartitionKey::new(gts(scoped::TOPIC), 0),
            scoped_partition(&[scoped::CREATED], 1),
        )],
        Arc::new(SlowPrepare(decided_in)),
        ScopeInput::Fed(scope),
    );

    let (at, frames) = frames_until(&mut harness.session, start, start + decided_in + BOUND).await;

    assert_eq!(at, [Duration::ZERO, decided_in]);
    assert_eq!(kinds(&frames), vec!["topology", "terminal"]);
    assert_teardown(frames.last(), &[position(0, 0, 0)]);
    assert_eq!(harness.session.positions(), vec![position(0, 0, 0)]);
}

/// Clears the member's scope while it decides a batch, then decides every
/// event, and admits each.
struct ClearingPrepare(watch::Sender<Option<Arc<CachedScope>>>);

#[async_trait::async_trait]
impl EventFilter for ClearingPrepare {
    fn matches(&self, _event: &Event) -> bool {
        true
    }

    async fn prepare(&self, _events: &EventBatch) -> Result<Prepared, DomainError> {
        self.0.send_replace(None);
        tokio::task::yield_now().await;
        Ok(Prepared::All)
    }

    fn admits(&self, _event: &Event) -> Result<bool, DomainError> {
        Ok(true)
    }
}

/// The member's scope is cleared while a batch's decisions are awaited. None
/// of its events is handed over: the stream tears down when they are
/// decided, with no progress for the batch.
#[tokio::test]
async fn a_batch_decided_after_a_clear_is_not_delivered() {
    let (feed, scope) = watch::channel(Some(scoped::wildcard_scope(1)));
    let mut harness = open_with(
        vec![(
            PartitionKey::new(gts(scoped::TOPIC), 0),
            scoped_partition(&[scoped::CREATED], 1),
        )],
        Arc::new(ClearingPrepare(feed)),
        ScopeInput::Fed(scope),
    );

    let frames = drain(&mut harness.session, 4).await;

    assert_eq!(kinds(&frames), vec!["topology", "terminal"]);
    assert_teardown(frames.last(), &[position(0, 0, 0)]);
    assert_eq!(harness.session.positions(), vec![position(0, 0, 0)]);
}

/// Removes the member from its group while a batch is decided, as a LEAVE
/// does, then decides every event, and admits each.
struct LeavingPrepare {
    groups: Arc<ConsumerGroupCoordinator>,
    group: GtsInstanceId,
    member: Uuid,
}

#[async_trait::async_trait]
impl EventFilter for LeavingPrepare {
    fn matches(&self, _event: &Event) -> bool {
        true
    }

    async fn prepare(&self, _events: &EventBatch) -> Result<Prepared, DomainError> {
        assert!(
            self.groups.leave(&self.group, self.member).is_some(),
            "the member leaves"
        );
        tokio::task::yield_now().await;
        Ok(Prepared::All)
    }

    fn admits(&self, _event: &Event) -> Result<bool, DomainError> {
        Ok(true)
    }
}

/// The member LEAVEs while a batch's decisions are awaited: the LEAVE sends
/// the empty assignment, then drops the member, which closes its scope feed.
/// None of the batch's events is handed over and it records no progress,
/// and the stream closes with `lose_all`, as a LEAVE between batches does:
/// the next pass reads the assignment before the scope.
#[tokio::test]
async fn a_leave_while_a_batch_is_decided_closes_with_lose_all() {
    let groups = Arc::new(ConsumerGroupCoordinator::new(Duration::from_mins(1)));
    let group = gts("gts.cf.core.events.consumer_group.v1~02943530-10da-4624-a3ae-b998c425847f");
    let member = Uuid::from_u128(0x5ab);
    groups.join_member(
        &group,
        member,
        &[TopicInterest {
            id: gts(scoped::TOPIC),
            partitions: 1,
        }],
        Duration::from_secs(30),
    );
    assert_eq!(
        groups.write_scope(
            member,
            ScopeWrite::Store {
                expected: Expected::Absent,
                content: ScopeContent {
                    principal: ScopePrincipal::from(&scoped::caller()),
                    groups: Arc::clone(&scoped::wildcard_scope(1).groups),
                },
            },
        ),
        ScopeWriteOutcome::Applied
    );
    let streaming =
        ConsumerGroupCoordinator::subscribe(&groups, &group, member).expect("the member joined");
    let feed = groups.watch_scope(member).expect("the member");
    let mut harness = open_member(
        vec![PartitionSlot::new(
            PartitionKey::new(gts(scoped::TOPIC), 0),
            scoped_partition(&[scoped::CREATED], 1),
        )],
        Arc::new(LeavingPrepare {
            groups: Arc::clone(&groups),
            group,
            member,
        }),
        ScopeInput::Fed(feed),
        streaming,
    );
    let start = Instant::now();

    let (_, frames) = frames_until(&mut harness.session, start, start + BOUND).await;

    assert_eq!(kinds(&frames), vec!["topology", "terminal"]);
    let Some(Frame::Control {
        code: ControlCode::Terminal,
        positions,
        reason: Some(CloseReason::LoseAll),
    }) = frames.last()
    else {
        panic!(
            "expected a lose_all terminal frame, got {:?}",
            frames.last()
        );
    };
    assert_eq!(positions, &[position(0, 0, 0)]);
    assert_eq!(harness.session.positions(), vec![position(0, 0, 0)]);
    assert_eq!(harness.session.state(), SessionState::Closed);
}

/// Decides every event at `decided_at`, and admits each; at `renewed_at`,
/// while it decides, it stores `renewed` on the member's scope feed, as the
/// refresh worker does.
struct RenewingPrepare {
    feed: watch::Sender<Option<Arc<CachedScope>>>,
    renewed: Arc<CachedScope>,
    renewed_at: Instant,
    decided_at: Instant,
}

#[async_trait::async_trait]
impl EventFilter for RenewingPrepare {
    fn matches(&self, _event: &Event) -> bool {
        true
    }

    async fn prepare(&self, _events: &EventBatch) -> Result<Prepared, DomainError> {
        tokio::time::sleep_until(self.renewed_at).await;
        self.feed.send_replace(Some(Arc::clone(&self.renewed)));
        tokio::time::sleep_until(self.decided_at).await;
        Ok(Prepared::All)
    }

    fn admits(&self, _event: &Event) -> Result<bool, DomainError> {
        Ok(true)
    }
}

/// The scope's one group expires 7 s after the open, and a batch is read at
/// once and decided 1 s after that expiry. A renewal stored at 6 s, while
/// the batch is decided, serves to 67 s: the batch is handed over at 8 s
/// under it, its progress is recorded, and the stream goes on.
#[tokio::test(start_paused = true)]
async fn a_renewal_stored_while_a_batch_is_decided_keeps_the_stream() {
    let start = Instant::now();
    let expires_in = Duration::from_secs(7);
    let decided_in = expires_in + Duration::from_secs(1);
    let (feed, scope) = watch::channel(Some(wildcard_scope_until(start + expires_in)));
    let renewed = scoped::held_scope(
        2,
        wildcard_scope_until(start + expires_in + Duration::from_mins(1))
            .groups
            .to_vec(),
    );
    let event = scoped::event_of(scoped::TOPIC, scoped::CREATED, 1);
    let mut harness = open_with(
        vec![(
            PartitionKey::new(gts(scoped::TOPIC), 0),
            StubPartition::holding(vec![event.clone()]),
        )],
        Arc::new(RenewingPrepare {
            feed,
            renewed,
            renewed_at: start + Duration::from_secs(6),
            decided_at: start + decided_in,
        }),
        ScopeInput::Fed(scope),
    );

    let (at, frames) = frames_until(&mut harness.session, start, start + decided_in + BOUND).await;

    assert_eq!(at, [Duration::ZERO, decided_in]);
    let [Frame::Topology { .. }, Frame::Event(delivered)] = frames.as_slice() else {
        panic!("expected a topology frame, then the event, got {frames:?}");
    };
    assert_eq!(
        serde_json::to_value(delivered.as_ref()).unwrap(),
        serde_json::to_value(&event).unwrap()
    );
    assert_eq!(harness.session.positions(), vec![position(0, 1, 1)]);
    assert_eq!(harness.session.state(), SessionState::Streaming);
}

/// A new generation that revokes `SHIPPED` arrives while the stream idles;
/// the next batch asks again under it and skips the event.
#[tokio::test]
async fn a_new_scope_generation_decides_the_next_batch() {
    let revoked = Arc::new(AtomicBool::new(false));
    let pdp = TypePdp::answering({
        let revoked = Arc::clone(&revoked);
        move |_: &EvaluationRequest| {
            if revoked.load(Ordering::SeqCst) {
                Answer::Deny
            } else {
                Answer::Allow
            }
        }
    });
    let (feed, scope) = watch::channel(Some(scoped::wildcard_scope(1)));
    let held = scoped_partition(&[scoped::SHIPPED], 1);
    let mut harness = open_scoped(
        vec![(0, Arc::clone(&held))],
        scoped::authorizer(&pdp, KnownTypes::on_topic(&[]), scoped::DEADLINE),
        scope,
    );
    let frames = drain(&mut harness.session, 2).await;
    assert_eq!(delivered(&frames), vec![1]);

    revoked.store(true, Ordering::SeqCst);
    feed.send_replace(Some(scoped::wildcard_scope(2)));
    held.events
        .lock()
        .expect("test lock")
        .push(scoped::event_of(scoped::TOPIC, scoped::SHIPPED, 2));
    held.accounted_through.store(2, Ordering::Relaxed);
    let frames = drain(&mut harness.session, 2).await;

    assert_eq!(delivered(&frames), Vec::<i64>::new());
    assert_eq!(harness.session.positions(), vec![position(0, 1, 2)]);
    assert_eq!(pdp.asked_types(), vec![scoped::SHIPPED, scoped::SHIPPED]);
}

/// Admits event 1, then cannot decide event 2.
struct UndecidableSecond;

impl EventFilter for UndecidableSecond {
    fn matches(&self, _event: &Event) -> bool {
        true
    }

    fn admits(&self, event: &Event) -> Result<bool, DomainError> {
        if event.sequence == Some(Sequence::assigned(2)) {
            Err(DomainError::Internal(
                "no consume decision was prepared".to_owned(),
            ))
        } else {
            Ok(true)
        }
    }
}

/// The event admitted before the failure is not delivered either: the batch
/// is replayed whole by the next stream.
#[tokio::test]
async fn an_undecidable_event_tears_down_without_delivering_its_batch() {
    let tenant = Uuid::new_v4();
    let mut harness = open_with(
        vec![(
            key(0),
            StubPartition::holding(vec![event(tenant, 1), event(tenant, 2)]),
        )],
        Arc::new(UndecidableSecond),
        ScopeInput::Alone,
    );

    let frames = drain(&mut harness.session, 4).await;

    assert_eq!(kinds(&frames), vec!["topology", "terminal"]);
    let Some(Frame::Control {
        reason: Some(CloseReason::Teardown),
        positions,
        ..
    }) = frames.last()
    else {
        panic!("expected a teardown, got {frames:?}");
    };
    let [held] = positions.as_slice() else {
        panic!("one partition: {positions:?}");
    };
    assert_eq!(
        (held.offset, held.last_examined),
        (Sequence::assigned(0), Sequence::assigned(0))
    );
}

/// Fails every `prepare`, and would admit every event.
struct UnpreparedBatch;

#[async_trait::async_trait]
impl EventFilter for UnpreparedBatch {
    fn matches(&self, _event: &Event) -> bool {
        true
    }

    async fn prepare(&self, _events: &EventBatch) -> Result<Prepared, DomainError> {
        Err(DomainError::Internal(
            "the consume evaluations did not finish".to_owned(),
        ))
    }

    fn admits(&self, _event: &Event) -> Result<bool, DomainError> {
        Ok(true)
    }
}

/// No event of a batch whose `prepare` failed is decided, even one the filter
/// would admit.
#[tokio::test]
async fn a_failed_prepare_tears_down_before_any_event_is_decided() {
    let tenant = Uuid::new_v4();
    let mut harness = open_with(
        vec![(key(0), StubPartition::holding(vec![event(tenant, 1)]))],
        Arc::new(UnpreparedBatch),
        ScopeInput::Alone,
    );

    let frames = drain(&mut harness.session, 4).await;

    assert_eq!(kinds(&frames), vec!["topology", "terminal"]);
    let Some(Frame::Control {
        reason: Some(CloseReason::Teardown),
        positions,
        ..
    }) = frames.last()
    else {
        panic!("expected a teardown, got {frames:?}");
    };
    let [held] = positions.as_slice() else {
        panic!("one partition: {positions:?}");
    };
    assert_eq!(
        (held.offset, held.last_examined),
        (Sequence::assigned(0), Sequence::assigned(0))
    );
}

/// A partition that serves its events from its own position, as the cache
/// does: a read returns what lies past the position and moves the position to
/// the last of it, and a seek moves it anywhere and is recorded.
struct SeekablePartition {
    events: Vec<Event>,
    position: AtomicI64,
    seeks: Mutex<Vec<Sequence>>,
}

impl SeekablePartition {
    fn holding(events: Vec<Event>) -> Arc<Self> {
        Arc::new(Self {
            events,
            position: AtomicI64::new(0),
            seeks: Mutex::new(Vec::new()),
        })
    }

    fn seeks(&self) -> Vec<Sequence> {
        self.seeks.lock().expect("test lock").clone()
    }

    fn position(&self) -> Sequence {
        Sequence::assigned(self.position.load(Ordering::Relaxed))
    }

    fn unread(&self) -> impl Iterator<Item = &Event> {
        let position = Sequence::assigned(self.position.load(Ordering::Relaxed));
        self.events
            .iter()
            .filter(move |event| event.sequence.is_some_and(|sequence| sequence > position))
    }
}

impl PartitionReader for SeekablePartition {
    fn has_data(&self) -> bool {
        self.unread().next().is_some()
    }

    fn seek(&self, offset: Sequence) {
        self.position.store(offset.as_i64(), Ordering::Relaxed);
        self.seeks.lock().expect("test lock").push(offset);
    }

    fn report_scanning(&self, _scanning: bool) {}

    fn read(&self, limit: ReadLimit) -> PartitionRead {
        let unread: Vec<Event> = self.unread().take(limit.max_events()).cloned().collect();
        let Some(through) = unread.last().and_then(|event| event.sequence) else {
            return PartitionRead::NothingNew;
        };
        self.position.store(through.as_i64(), Ordering::Relaxed);
        let held = unread.len();
        PartitionRead::Hit {
            events: EventBatch::from_runs(vec![
                crate::domain::streaming::read::EventSlice::builder(unread.into())
                    .range(0..held)
                    .frontier(through)
                    .build(),
            ]),
            accounted_through: through,
        }
    }
}

/// A partition holding one event of each of `types`, in order from sequence
/// 1, read from its own position.
fn seekable_partition(types: &[&str]) -> Arc<SeekablePartition> {
    SeekablePartition::holding(
        types
            .iter()
            .zip(1..)
            .map(|(event_type, sequence)| scoped::event_of(scoped::TOPIC, event_type, sequence))
            .collect(),
    )
}

/// A candidate, then `SINGLE_TYPE_CONCURRENCY + 2` new types. Each pass holds
/// the reader at its pre-read position while the wave is asked, then moves it
/// to the examined frontier. The first pass asks one wave (`t0`..`t4`),
/// delivers events 1 to 6 and records 6 as examined, so the reader moves back
/// to 6; the next pass reads 7 and 8, asks `t5` and `t6`, delivers them, and
/// moves the reader to 8.
#[tokio::test]
async fn a_cut_batch_resumes_at_the_first_undecided_event() {
    assert_eq!(
        SINGLE_TYPE_CONCURRENCY, 5,
        "the partition below is laid out for a wave of five"
    );
    let pdp = TypePdp::answering(|_: &EvaluationRequest| Answer::Allow);
    let types = scoped::many_types(SINGLE_TYPE_CONCURRENCY + 2);
    let mut held_types = vec![scoped::CREATED];
    held_types.extend(types.iter().map(String::as_str));
    let held = seekable_partition(&held_types);
    let (_feed, scope) = watch::channel(Some(scoped::wildcard_scope(1)));
    let mut harness = open_scoped(
        vec![(0, Arc::clone(&held))],
        scoped::authorizer(&pdp, KnownTypes::on_topic(&[]), scoped::DEADLINE),
        scope,
    );

    let first = drain(&mut harness.session, 7).await;

    assert_eq!(
        kinds(&first),
        vec![
            "topology", "event", "event", "event", "event", "event", "event"
        ]
    );
    assert_eq!(delivered(&first), vec![1, 2, 3, 4, 5, 6]);
    assert_eq!(harness.session.positions(), vec![position(0, 6, 6)]);
    assert_eq!(
        held.seeks(),
        vec![Sequence::assigned(0), Sequence::assigned(6)]
    );
    assert_eq!(pdp.asked_types(), types[..5].to_vec());

    let rest = drain(&mut harness.session, 2).await;

    assert_eq!(kinds(&rest), vec!["event", "event"]);
    assert_eq!(delivered(&rest), vec![7, 8]);
    assert_eq!(harness.session.positions(), vec![position(0, 8, 8)]);
    assert_eq!(
        held.seeks(),
        vec![
            Sequence::assigned(0),
            Sequence::assigned(6),
            Sequence::assigned(6),
            Sequence::assigned(8),
        ]
    );
    assert_eq!(pdp.asked_types(), types);
    assert_eq!(harness.session.state(), SessionState::Streaming);
}

/// Decides each batch by its next answer, every event once they run out, and
/// admits every event it decides. Each `prepare` records, while the session
/// awaits it, where the partition's reader stands and every seek it has had.
struct WatchingPrepare {
    partition: Arc<SeekablePartition>,
    answers: Mutex<VecDeque<Prepared>>,
    seen: Mutex<Vec<(Sequence, Vec<Sequence>)>>,
}

impl WatchingPrepare {
    fn over(partition: &Arc<SeekablePartition>, answers: Vec<Prepared>) -> Arc<Self> {
        Arc::new(Self {
            partition: Arc::clone(partition),
            answers: Mutex::new(answers.into()),
            seen: Mutex::new(Vec::new()),
        })
    }

    fn seen(&self) -> Vec<(Sequence, Vec<Sequence>)> {
        self.seen.lock().expect("test lock").clone()
    }
}

#[async_trait::async_trait]
impl EventFilter for WatchingPrepare {
    fn matches(&self, _event: &Event) -> bool {
        true
    }

    async fn prepare(&self, _events: &EventBatch) -> Result<Prepared, DomainError> {
        self.seen
            .lock()
            .expect("test lock")
            .push((self.partition.position(), self.partition.seeks()));
        tokio::task::yield_now().await;
        Ok(self
            .answers
            .lock()
            .expect("test lock")
            .pop_front()
            .unwrap_or(Prepared::All))
    }

    fn admits(&self, _event: &Event) -> Result<bool, DomainError> {
        Ok(true)
    }
}

/// While a batch's decisions are awaited, the reader stands where it stood
/// before the read, which is the slot's examined frontier, so a pass after the
/// hold keeps the batch's span. After them it stands at the batch's examined
/// frontier: the read's `accounted_through` on `All`, the last decided event
/// on a cut. Each drain allows more frames than the events, so a batch read
/// twice would show.
#[tokio::test]
async fn the_reader_holds_the_batch_while_its_decisions_are_awaited() {
    let tenant = Uuid::new_v4();

    // An `All` batch, from the open position.
    let held = SeekablePartition::holding((1..=4).map(|n| event(tenant, n)).collect());
    let filter = WatchingPrepare::over(&held, vec![Prepared::All]);
    let mut harness = open_slots(
        vec![PartitionSlot::new(
            key(0),
            Arc::clone(&held) as Arc<dyn PartitionReader>,
        )],
        Arc::clone(&filter) as Arc<dyn EventFilter>,
        ScopeInput::Alone,
    );

    let frames = drain(&mut harness.session, 8).await;

    assert_eq!(delivered(&frames), vec![1, 2, 3, 4]);
    assert_eq!(
        kinds(&frames),
        vec!["topology", "event", "event", "event", "event"]
    );
    assert_eq!(
        filter.seen(),
        vec![(Sequence::assigned(0), vec![Sequence::assigned(0)])]
    );
    assert_eq!(
        held.seeks(),
        vec![Sequence::assigned(0), Sequence::assigned(4)]
    );
    assert_eq!(
        harness.session.positions(),
        vec![
            Position::builder(gts(TOPIC), 0)
                .offset(Sequence::assigned(4))
                .last_examined(Sequence::assigned(4))
                .build()
        ]
    );

    // A cut batch, then the rest, from a seeded position.
    let held = SeekablePartition::holding((1..=10).map(|n| event(tenant, n)).collect());
    let filter = WatchingPrepare::over(&held, vec![Prepared::UpTo(3), Prepared::All]);
    let mut harness = open_slots(
        vec![
            PartitionSlot::new(key(0), Arc::clone(&held) as Arc<dyn PartitionReader>)
                .starting_at(Sequence::assigned(2)),
        ],
        Arc::clone(&filter) as Arc<dyn EventFilter>,
        ScopeInput::Alone,
    );

    let frames = drain(&mut harness.session, 12).await;

    assert_eq!(delivered(&frames), vec![3, 4, 5, 6, 7, 8, 9, 10]);
    assert_eq!(
        kinds(&frames),
        vec![
            "topology", "event", "event", "event", "event", "event", "event", "event", "event"
        ]
    );
    assert_eq!(
        filter.seen(),
        vec![
            (
                Sequence::assigned(2),
                vec![Sequence::assigned(2), Sequence::assigned(2)]
            ),
            (
                Sequence::assigned(5),
                vec![
                    Sequence::assigned(2),
                    Sequence::assigned(2),
                    Sequence::assigned(5),
                    Sequence::assigned(5),
                ]
            ),
        ]
    );
    assert_eq!(
        held.seeks(),
        vec![
            Sequence::assigned(2),
            Sequence::assigned(2),
            Sequence::assigned(5),
            Sequence::assigned(5),
            Sequence::assigned(10),
        ]
    );
    assert_eq!(
        harness.session.positions(),
        vec![
            Position::builder(gts(TOPIC), 0)
                .offset(Sequence::assigned(10))
                .last_examined(Sequence::assigned(10))
                .build()
        ]
    );
}

/// Three waves of new types, each E answered after 0.6 x `DEADLINE` on the
/// paused clock. Each pass asks one wave, which ends within the deadline, so
/// every pass delivers five events and advances the position; the stream
/// never tears down. Each frame wait is bounded by `DEADLINE + BOUND`.
#[tokio::test(start_paused = true)]
async fn a_slow_but_healthy_pdp_never_stalls_a_stream() {
    assert_eq!(
        SINGLE_TYPE_CONCURRENCY, 5,
        "the partition below is laid out for waves of five"
    );
    let slow = scoped::DEADLINE * 3 / 5;
    let pdp = TypePdp::answering(move |_: &EvaluationRequest| Answer::AllowAfter(slow));
    let types = scoped::many_types(3 * SINGLE_TYPE_CONCURRENCY);
    let held_types: Vec<&str> = types.iter().map(String::as_str).collect();
    let (_feed, scope) = watch::channel(Some(scoped::wildcard_scope(1)));
    let mut harness = open_scoped(
        vec![(0, seekable_partition(&held_types))],
        scoped::authorizer(&pdp, KnownTypes::on_topic(&[]), scoped::DEADLINE),
        scope,
    );
    let topology = drain_within(&mut harness.session, 1, scoped::DEADLINE + BOUND).await;
    assert_eq!(kinds(&topology), vec!["topology"]);

    let mut passes = Vec::new();
    for _ in 0..3 {
        let frames = drain_within(&mut harness.session, 5, scoped::DEADLINE + BOUND).await;
        passes.push((
            delivered(&frames),
            harness.session.positions(),
            pdp.recorded().len(),
        ));
    }

    assert_eq!(
        passes,
        vec![
            (vec![1, 2, 3, 4, 5], vec![position(0, 5, 5)], 5),
            (vec![6, 7, 8, 9, 10], vec![position(0, 10, 10)], 10),
            (vec![11, 12, 13, 14, 15], vec![position(0, 15, 15)], 15),
        ]
    );
    assert_eq!(harness.session.state(), SessionState::Streaming);
}

/// A new generation stored for another caller arrives before the next read.
/// That batch, of a candidate type that needs no call, fails, and the stream
/// tears down without delivering it.
#[tokio::test]
#[tracing_test::traced_test]
async fn a_scope_for_another_principal_fails_the_next_batch() {
    let pdp = TypePdp::answering(|_: &EvaluationRequest| Answer::Allow);
    let (feed, scope) = watch::channel(Some(scoped::wildcard_scope(1)));
    let held = StubPartition::empty();
    let mut harness = open_scoped(
        vec![(0, Arc::clone(&held))],
        scoped::authorizer(&pdp, KnownTypes::on_topic(&[]), scoped::DEADLINE),
        scope,
    );
    let topology = drain(&mut harness.session, 1).await;
    assert_eq!(kinds(&topology), vec!["topology"]);

    let someone_else = toolkit_security::SecurityContext::builder()
        .subject_id(Uuid::from_u128(0x789))
        .subject_tenant_id(Uuid::from_u128(0x123))
        .build()
        .expect("valid security context");
    feed.send_replace(Some(Arc::new(CachedScope {
        generation: ScopeGeneration::nth(2),
        principal: ScopePrincipal::from(&someone_else),
        groups: scoped::wildcard_scope(1).groups.clone(),
    })));
    held.events
        .lock()
        .expect("test lock")
        .push(scoped::event_of(scoped::TOPIC, scoped::CREATED, 1));
    held.accounted_through.store(1, Ordering::Relaxed);
    let frames = drain(&mut harness.session, 2).await;

    assert_eq!(kinds(&frames), vec!["terminal"]);
    assert_teardown(frames.last(), &[position(0, 0, 0)]);
    assert_eq!(pdp.recorded(), Vec::<serde_json::Value>::new());
    logs_assert(|lines: &[&str]| {
        match lines
            .iter()
            .filter(|line| line.contains("the consume scope belongs to another caller"))
            .count()
        {
            1 => Ok(()),
            n => Err(format!(
                "expected one failed batch for the principal, got {n}"
            )),
        }
    });
}

/// Cuts every batch before event `self.0` and admits every event it decides.
struct CutBefore(usize);

#[async_trait::async_trait]
impl EventFilter for CutBefore {
    fn matches(&self, _event: &Event) -> bool {
        true
    }

    async fn prepare(&self, _events: &EventBatch) -> Result<Prepared, DomainError> {
        Ok(Prepared::UpTo(self.0))
    }

    fn admits(&self, _event: &Event) -> Result<bool, DomainError> {
        Ok(true)
    }
}

/// A cut that leaves no event to resume after, or whose last decided event
/// has no sequence, is a broken invariant: the stream tears down with no
/// event delivered and no progress, whatever the cut.
#[tokio::test]
async fn a_cut_that_names_no_decided_event_tears_down_without_progress() {
    let tenant = Uuid::new_v4();
    let mut unsequenced = event(tenant, 1);
    unsequenced.sequence = None;
    for (row, cut, held) in [
        (
            "cut before the first event",
            0,
            vec![event(tenant, 1), event(tenant, 2)],
        ),
        (
            "cut past the batch",
            3,
            vec![event(tenant, 1), event(tenant, 2)],
        ),
        (
            "last decided event unsequenced",
            1,
            vec![unsequenced, event(tenant, 2)],
        ),
    ] {
        let mut harness = open_with(
            vec![(key(0), StubPartition::holding(held))],
            Arc::new(CutBefore(cut)),
            ScopeInput::Alone,
        );

        let frames = drain(&mut harness.session, 4).await;

        assert_eq!(kinds(&frames), vec!["topology", "terminal"], "{row}");
        let Some(Frame::Control {
            reason: Some(CloseReason::Teardown),
            positions,
            ..
        }) = frames.last()
        else {
            panic!("{row}: expected a teardown, got {frames:?}");
        };
        let [held] = positions.as_slice() else {
            panic!("{row}: one partition: {positions:?}");
        };
        assert_eq!(
            (held.offset, held.last_examined),
            (Sequence::assigned(0), Sequence::assigned(0)),
            "{row}"
        );
    }
}
