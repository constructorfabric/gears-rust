use std::sync::Arc;
use std::time::Duration;

use uuid::Uuid;

use crate::domain::streaming::assignment::AssignmentDelta;
use toolkit_gts::GtsInstanceId;

use tokio::time::Instant;

use super::{ConsumerGroupCoordinator, MemberState, Removal, TopicInterest, range_split};

fn topic(suffix: &str, partitions: i32) -> TopicInterest {
    TopicInterest {
        id: GtsInstanceId::try_new(&format!("gts.cf.core.events.topic.v1~x.eb.t1.{suffix}.v1"))
            .unwrap(),
        partitions,
    }
}

fn group_id(suffix: &str) -> GtsInstanceId {
    GtsInstanceId::try_new(&format!(
        "gts.cf.core.events.consumer_group.v1~x.eb.cg.{suffix}.v1"
    ))
    .unwrap()
}

// --- range_split ---

#[test]
fn range_split_single_member_gets_all() {
    let members = vec![Uuid::new_v4()];
    let splits = range_split(4, &members);
    assert_eq!(splits, vec![vec![0, 1, 2, 3]]);
}

#[test]
fn range_split_two_members_two_plus_two() {
    let members = vec![Uuid::new_v4(), Uuid::new_v4()];
    let splits = range_split(4, &members);
    assert_eq!(splits[0].len() + splits[1].len(), 4);
    assert_eq!(splits[0], vec![0, 1]);
    assert_eq!(splits[1], vec![2, 3]);
}

#[test]
fn range_split_three_members_four_partitions() {
    let members = vec![Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4()];
    let splits = range_split(4, &members);
    let total: usize = splits.iter().map(Vec::len).sum();
    assert_eq!(total, 4);
    // floor(0*4/3)=0..floor(1*4/3)=1 → [0]
    // floor(1*4/3)=1..floor(2*4/3)=2 → [1]
    // floor(2*4/3)=2..floor(3*4/3)=4 → [2,3]
    assert_eq!(splits[0], vec![0]);
    assert_eq!(splits[1], vec![1]);
    assert_eq!(splits[2], vec![2, 3]);
}

#[test]
fn range_split_five_partitions_two_members() {
    let members = vec![Uuid::new_v4(), Uuid::new_v4()];
    let splits = range_split(5, &members);
    // floor(0*5/2)=0..floor(1*5/2)=2 → [0,1]
    // floor(1*5/2)=2..floor(2*5/2)=5 → [2,3,4]
    assert_eq!(splits[0], vec![0, 1]);
    assert_eq!(splits[1], vec![2, 3, 4]);
}

#[test]
fn range_split_empty_members() {
    assert!(range_split(4, &[]).is_empty());
}

#[test]
fn range_split_zero_partitions() {
    let members = vec![Uuid::new_v4(), Uuid::new_v4()];
    let splits = range_split(0, &members);
    assert_eq!(splits.len(), 2);
    assert!(splits.iter().all(Vec::is_empty));
}

// --- ConsumerGroupCoordinator::join ---

const JOIN_TIMEOUT: Duration = Duration::from_mins(1);

fn coordinator() -> Arc<ConsumerGroupCoordinator> {
    Arc::new(ConsumerGroupCoordinator::new(JOIN_TIMEOUT))
}

#[test]
fn first_join_version_one_all_partitions() {
    let coordinator = coordinator();
    let group = group_id("g1");
    let sub_id = Uuid::new_v4();

    let (assigned, version, _) = coordinator.join_member(
        &group,
        sub_id,
        &[topic("topic", 4)],
        Duration::from_secs(30),
    );

    assert_eq!(version, 1);
    let mut parts: Vec<i32> = assigned.iter().map(|a| a.partition).collect();
    parts.sort_unstable();
    assert_eq!(parts, vec![0, 1, 2, 3]);
    assert_eq!(coordinator.state_of(sub_id), Some(MemberState::Joined));
}

#[test]
fn second_join_splits_and_increments_version() {
    let coordinator = coordinator();
    let group = group_id("g2");

    let (_, v1, _) = coordinator.join_member(
        &group,
        Uuid::new_v4(),
        &[topic("topic", 4)],
        Duration::from_secs(30),
    );
    assert_eq!(v1, 1);

    let (assigned_b, v2, _) = coordinator.join_member(
        &group,
        Uuid::new_v4(),
        &[topic("topic", 4)],
        Duration::from_secs(30),
    );
    assert_eq!(v2, 2);
    assert_eq!(assigned_b.len(), 2); // 4 partitions / 2 members = 2 each
}

#[test]
fn third_join_four_partitions_all_members_covered() {
    let coordinator = coordinator();
    let group = group_id("g3");

    for _ in 0..2 {
        coordinator.join_member(
            &group,
            Uuid::new_v4(),
            &[topic("topic", 4)],
            Duration::from_secs(30),
        );
    }
    let (assigned_c, v3, _) = coordinator.join_member(
        &group,
        Uuid::new_v4(),
        &[topic("topic", 4)],
        Duration::from_secs(30),
    );

    assert_eq!(v3, 3);
    assert!(!assigned_c.is_empty());
    assert!(assigned_c.len() <= 2);
}

/// A rebalance rewrites every member's stored subscription, so a read of a
/// sibling is its current assignment and topology version rather than what it
/// was given at its own JOIN.
#[test]
fn a_join_rewrites_every_siblings_stored_subscription() {
    let coordinator = coordinator();
    let group = group_id("gsib");
    let sub_a = Uuid::new_v4();
    coordinator.join_member(&group, sub_a, &[topic("topic", 4)], Duration::from_secs(30));
    coordinator.join_member(
        &group,
        Uuid::new_v4(),
        &[topic("topic", 4)],
        Duration::from_secs(30),
    );

    let a = coordinator.get(sub_a).expect("sub_a is a member");
    assert_eq!(a.topology_version, 2);
    assert_eq!(a.assigned.len(), 2);
}

// --- lookups ---

#[test]
fn get_list_and_has_members_reflect_membership() {
    let coordinator = coordinator();
    let group = group_id("glookup");
    let sub_a = Uuid::new_v4();
    let sub_b = Uuid::new_v4();
    assert!(!coordinator.has_members(&group));
    assert!(coordinator.get(sub_a).is_none());

    coordinator.join_member(&group, sub_a, &[topic("topic", 2)], Duration::from_secs(30));
    coordinator.join_member(&group, sub_b, &[topic("topic", 2)], Duration::from_secs(30));

    assert!(coordinator.has_members(&group));
    assert_eq!(coordinator.get(sub_a).map(|s| s.id), Some(sub_a));
    let mut listed: Vec<Uuid> = coordinator.list().into_iter().map(|s| s.id).collect();
    listed.sort_unstable();
    let mut expected = vec![sub_a, sub_b];
    expected.sort_unstable();
    assert_eq!(listed, expected);
}

// --- ConsumerGroupCoordinator::leave ---

#[tokio::test]
async fn leave_sends_terminal_to_survivor() {
    let coordinator = coordinator();
    let group = group_id("g4");

    let sub_a = Uuid::new_v4();
    let sub_b = Uuid::new_v4();
    coordinator.join_member(&group, sub_a, &[topic("topic", 4)], Duration::from_secs(30));
    coordinator.join_member(&group, sub_b, &[topic("topic", 4)], Duration::from_secs(30));

    let (mut generations, _membership) =
        ConsumerGroupCoordinator::subscribe(&coordinator, &group, sub_a)
            .expect("sub_a is a member");
    let before = generations.borrow_and_update().clone();

    let removal = coordinator.leave(&group, sub_b);
    assert_eq!(
        removal,
        Some(Removal {
            subscription_id: sub_b,
            group,
            group_emptied: false,
        })
    );

    assert!(
        generations
            .has_changed()
            .expect("sender outlives the receiver"),
        "the survivor's assignment must be published when a sibling leaves"
    );
    let after = generations.borrow_and_update().clone();

    // The coordinator states the assignment; classifying it is the session's
    // job, so this asserts the input to that rather than a frame kind. A
    // survivor taking over a departed member's partitions is a *gain*, which
    // `apply` turns into a terminal close because it holds no cursor for them.
    assert_eq!(
        AssignmentDelta::classify(&before, &after),
        AssignmentDelta::Gain,
        "before {before:?}, after {after:?}"
    );
}

/// A LEAVE of a member whose stream is open must end that stream. The member's
/// watch is left holding an empty assignment - lose-all, which the session
/// turns into a terminal close - rather than a sender that simply vanished.
#[tokio::test]
async fn leave_of_a_streaming_member_publishes_lose_all_to_its_own_stream() {
    let coordinator = coordinator();
    let group = group_id("gleave");
    let sub_a = Uuid::new_v4();
    coordinator.join_member(&group, sub_a, &[topic("topic", 4)], Duration::from_secs(30));
    let (mut generations, _membership) =
        ConsumerGroupCoordinator::subscribe(&coordinator, &group, sub_a)
            .expect("sub_a is a member");
    let before = generations.borrow_and_update().clone();

    coordinator.leave(&group, sub_a);

    let after = generations.borrow_and_update().clone();
    assert!(after.assigned.is_empty(), "got {after:?}");
    assert_eq!(
        AssignmentDelta::classify(&before, &after),
        AssignmentDelta::LoseAll
    );
}

#[test]
fn leave_last_member_removes_group() {
    let coordinator = coordinator();
    let group = group_id("g5");
    let sub_a = Uuid::new_v4();
    coordinator.join_member(&group, sub_a, &[topic("topic", 2)], Duration::from_secs(30));

    assert_eq!(
        coordinator.leave(&group, sub_a),
        Some(Removal {
            subscription_id: sub_a,
            group: group.clone(),
            group_emptied: true,
        })
    );
    assert!(
        !coordinator
            .state
            .lock()
            .unwrap()
            .groups
            .contains_key(&group)
    );
    assert!(coordinator.get(sub_a).is_none());
}

#[test]
fn leave_of_an_unknown_member_is_none() {
    let coordinator = coordinator();
    let group = group_id("gunknown");
    coordinator.join_member(
        &group,
        Uuid::new_v4(),
        &[topic("topic", 2)],
        Duration::from_secs(30),
    );
    assert_eq!(coordinator.leave(&group, Uuid::new_v4()), None);
}

// --- Disconnected eviction on JOIN ---

#[test]
fn disconnected_member_evicted_on_next_join() {
    let coordinator = coordinator();
    let group = group_id("g6");

    let sub_a = Uuid::new_v4();
    coordinator.join_member(&group, sub_a, &[topic("topic", 4)], Duration::from_secs(30));
    let (_generations, membership) =
        ConsumerGroupCoordinator::subscribe(&coordinator, &group, sub_a)
            .expect("sub_a is a member");
    drop(membership);
    assert_eq!(coordinator.state_of(sub_a), Some(MemberState::Disconnected));

    let sub_b = Uuid::new_v4();
    let (assigned_b, _, evicted) =
        coordinator.join_member(&group, sub_b, &[topic("topic", 4)], Duration::from_secs(30));
    assert_eq!(assigned_b.len(), 4);
    assert_eq!(
        evicted,
        vec![Removal {
            subscription_id: sub_a,
            group,
            group_emptied: false,
        }]
    );
    assert!(coordinator.get(sub_a).is_none());
}

/// Only `Disconnected` members are evicted by a JOIN: a member that joined and
/// has not opened its stream yet is mid-handshake, not gone.
#[test]
fn a_joined_member_is_not_evicted_by_a_later_join() {
    let coordinator = coordinator();
    let group = group_id("gjoined");
    let sub_a = Uuid::new_v4();
    coordinator.join_member(&group, sub_a, &[topic("topic", 4)], Duration::from_secs(30));

    let (_, _, evicted) = coordinator.join_member(
        &group,
        Uuid::new_v4(),
        &[topic("topic", 4)],
        Duration::from_secs(30),
    );
    assert!(evicted.is_empty());
    assert_eq!(coordinator.state_of(sub_a), Some(MemberState::Joined));
}

// --- state transitions ---

#[test]
fn opening_a_stream_moves_to_streaming_and_closing_it_to_disconnected() {
    let coordinator = coordinator();
    let group = group_id("gtrans");
    let sub_a = Uuid::new_v4();
    coordinator.join_member(&group, sub_a, &[topic("topic", 2)], Duration::from_secs(30));
    assert_eq!(coordinator.state_of(sub_a), Some(MemberState::Joined));

    let (_generations, membership) =
        ConsumerGroupCoordinator::subscribe(&coordinator, &group, sub_a)
            .expect("sub_a is a member");
    assert_eq!(coordinator.state_of(sub_a), Some(MemberState::Streaming));

    drop(membership);
    assert_eq!(coordinator.state_of(sub_a), Some(MemberState::Disconnected));

    // Reconnect.
    let (_generations, _membership) =
        ConsumerGroupCoordinator::subscribe(&coordinator, &group, sub_a)
            .expect("a disconnected member can reopen its stream");
    assert_eq!(coordinator.state_of(sub_a), Some(MemberState::Streaming));
}

#[test]
fn subscribing_an_unknown_member_is_none() {
    let coordinator = coordinator();
    let group = group_id("gnone");
    assert!(ConsumerGroupCoordinator::subscribe(&coordinator, &group, Uuid::new_v4()).is_none());
}

// --- sweep ---

/// The Windows CI failure: a member with a one-second `session_timeout` whose
/// JOIN -> SEEK -> open took longer than a second lost its subscription before
/// the stream opened. `session_timeout` governs only a *disconnected* member;
/// a joined one has the join timeout, however short its own timeout is.
#[test]
fn a_joined_member_with_a_short_session_timeout_survives_until_the_join_timeout() {
    let coordinator = coordinator();
    let group = group_id("gshort");
    let sub_a = Uuid::new_v4();
    let joined_at = Instant::now();
    coordinator.join_member(&group, sub_a, &[topic("topic", 2)], Duration::from_secs(1));

    assert!(
        coordinator
            .sweep(joined_at + Duration::from_secs(59))
            .is_empty(),
        "a joined member must outlive its own session_timeout"
    );
    assert!(
        ConsumerGroupCoordinator::subscribe(&coordinator, &group, sub_a).is_some(),
        "its stream must still open 59s after JOIN"
    );
}

#[test]
fn a_joined_member_that_never_streams_is_reaped_at_the_join_timeout() {
    let coordinator = coordinator();
    let group = group_id("gjt");
    let sub_a = Uuid::new_v4();
    let joined_at = Instant::now();
    coordinator.join_member(&group, sub_a, &[topic("topic", 2)], Duration::from_secs(30));

    assert_eq!(
        coordinator.sweep(joined_at + JOIN_TIMEOUT + Duration::from_millis(1)),
        vec![Removal {
            subscription_id: sub_a,
            group: group.clone(),
            group_emptied: true,
        }]
    );
    assert!(coordinator.get(sub_a).is_none());
    assert!(!coordinator.has_members(&group));
}

#[test]
fn a_streaming_member_is_never_swept() {
    let coordinator = coordinator();
    let group = group_id("gstream");
    let sub_a = Uuid::new_v4();
    let joined_at = Instant::now();
    coordinator.join_member(&group, sub_a, &[topic("topic", 2)], Duration::from_secs(1));
    let (_generations, _membership) =
        ConsumerGroupCoordinator::subscribe(&coordinator, &group, sub_a)
            .expect("sub_a is a member");

    assert!(
        coordinator
            .sweep(joined_at + Duration::from_hours(24))
            .is_empty()
    );
    assert_eq!(coordinator.state_of(sub_a), Some(MemberState::Streaming));
}

/// A disconnected member keeps its partitions until its `session_timeout`
/// runs out; then it is reaped and the survivor gains them.
#[test]
fn a_disconnected_member_is_reaped_after_its_session_timeout_and_the_survivor_gains() {
    let coordinator = coordinator();
    let group = group_id("g8");
    let session_timeout = Duration::from_millis(100);

    let sub_a = Uuid::new_v4();
    let sub_b = Uuid::new_v4();
    coordinator.join_member(&group, sub_a, &[topic("topic", 4)], session_timeout);
    coordinator.join_member(&group, sub_b, &[topic("topic", 4)], session_timeout);

    let (mut generations_b, _membership_b) =
        ConsumerGroupCoordinator::subscribe(&coordinator, &group, sub_b)
            .expect("sub_b is a member");
    generations_b.borrow_and_update();

    let (_generations_a, membership_a) =
        ConsumerGroupCoordinator::subscribe(&coordinator, &group, sub_a)
            .expect("sub_a is a member");
    drop(membership_a);
    let disconnected_at = Instant::now();

    assert!(
        coordinator
            .sweep(disconnected_at + session_timeout / 2)
            .is_empty(),
        "partitions stay held within session_timeout"
    );
    assert!(
        !generations_b
            .has_changed()
            .expect("sender outlives the receiver"),
        "no rebalance while the disconnected member is within its timeout"
    );

    assert_eq!(
        coordinator.sweep(disconnected_at + session_timeout + Duration::from_millis(1)),
        vec![Removal {
            subscription_id: sub_a,
            group,
            group_emptied: false,
        }]
    );
    assert!(
        generations_b
            .has_changed()
            .expect("sender outlives the receiver"),
        "the surviving member's new assignment must be published once the sweep reaps"
    );
    let after_b = generations_b.borrow_and_update().clone();
    assert_eq!(
        after_b.assigned.len(),
        4,
        "the survivor must hold every partition after eviction, got {after_b:?}"
    );
    assert!(coordinator.get(sub_a).is_none());
    assert_eq!(coordinator.state_of(sub_b), Some(MemberState::Streaming));
}

/// A reconnect inside `session_timeout` makes the member `Streaming` again,
/// so a later sweep - even one past the original timeout - leaves it alone.
#[test]
fn a_reconnect_within_session_timeout_keeps_the_member() {
    let coordinator = coordinator();
    let group = group_id("g7");
    let session_timeout = Duration::from_millis(50);
    let sub_a = Uuid::new_v4();
    coordinator.join_member(&group, sub_a, &[topic("topic", 2)], session_timeout);

    let (_generations, membership) =
        ConsumerGroupCoordinator::subscribe(&coordinator, &group, sub_a)
            .expect("sub_a is a member");
    drop(membership);
    let disconnected_at = Instant::now();
    let (_generations, _membership) =
        ConsumerGroupCoordinator::subscribe(&coordinator, &group, sub_a).expect("sub_a reconnects");

    assert!(
        coordinator
            .sweep(disconnected_at + session_timeout * 10)
            .is_empty()
    );
    assert_eq!(coordinator.state_of(sub_a), Some(MemberState::Streaming));
}

/// A member's lifetime runs from when it entered its current state, not from
/// its JOIN: a disconnect long after joining still gets its full timeout.
#[test]
fn a_disconnected_members_timeout_runs_from_the_disconnect_not_the_join() {
    let coordinator = coordinator();
    let group = group_id("gsince");
    let session_timeout = Duration::from_secs(5);
    let sub_a = Uuid::new_v4();
    coordinator.join_member(&group, sub_a, &[topic("topic", 2)], session_timeout);
    let (_generations, membership) =
        ConsumerGroupCoordinator::subscribe(&coordinator, &group, sub_a)
            .expect("sub_a is a member");
    drop(membership);
    let disconnected_at = Instant::now();

    assert!(
        coordinator
            .sweep(disconnected_at + session_timeout - Duration::from_millis(1))
            .is_empty()
    );
    assert_eq!(
        coordinator.sweep(disconnected_at + session_timeout).len(),
        1
    );
}

/// A membership change is never lost, whether or not a stream is open.
///
/// The frame-push version used `try_send` into a 16-slot channel, so a topology
/// or terminal frame was silently discarded whenever a consumer's buffer was
/// full - which `event-broker-consumption-frames` forbids. A `watch` cannot drop
/// a value; it can only coalesce to the newest, which is what a session needs
/// since only the latest assignment is actionable.
#[test]
fn an_assignment_published_with_no_stream_open_is_not_lost() {
    let coordinator = coordinator();
    let group = group_id("gnostream");
    let sub_a = Uuid::new_v4();
    coordinator.join_member(&group, sub_a, &[topic("topic", 4)], Duration::from_secs(30));

    // Subscribing *after* the join, as a real stream does - the assignment was
    // published when nobody was listening. `watch::Sender::send` fails and
    // discards the value with zero receivers, which left the member's watch on
    // its empty seed; `send_replace` is what makes this hold.
    let (generations, _membership) =
        ConsumerGroupCoordinator::subscribe(&coordinator, &group, sub_a)
            .expect("sub_a is a member");

    let seen = generations.borrow().clone();
    assert_eq!(
        seen.assigned.len(),
        4,
        "a stream opening after the join must see the assignment the join produced, got {seen:?}"
    );
    assert_eq!(seen.topology_version, 1);
}

/// Several changes in a row collapse to the newest rather than dropping any.
#[test]
fn rapid_membership_changes_coalesce_to_the_latest() {
    let coordinator = coordinator();
    let group = group_id("gcoalesce");
    let sub_a = Uuid::new_v4();
    coordinator.join_member(&group, sub_a, &[topic("topic", 8)], Duration::from_secs(30));

    let (mut generations, _membership) =
        ConsumerGroupCoordinator::subscribe(&coordinator, &group, sub_a)
            .expect("sub_a is a member");
    generations.borrow_and_update();

    // Three siblings join back to back, with nothing reading in between.
    for _ in 0..3 {
        coordinator.join_member(
            &group,
            Uuid::new_v4(),
            &[topic("topic", 8)],
            Duration::from_secs(30),
        );
    }

    assert!(
        generations
            .has_changed()
            .expect("sender outlives the receiver"),
        "a change must be observable"
    );
    let latest = generations.borrow_and_update().clone();
    assert_eq!(
        latest.assigned.len(),
        2,
        "four members over eight partitions is two each - the newest split, not an \
         intermediate one, got {latest:?}"
    );

    // Deliberately no assertion on `topology_version`. A member whose partition
    // set is unchanged by a rebalance is skipped, so its watch keeps the version
    // at which its assignment last actually moved. With eight partitions the
    // range split gives member 0 the range [0, 2) at both three and four
    // members, so whether this member's version reaches the newest one depends
    // on where it lands in the member ordering. What coalescing has to
    // guarantee is that no change is dropped - that the value read is the
    // newest split rather than an intermediate one - and the partition count
    // above is what states that.
}
