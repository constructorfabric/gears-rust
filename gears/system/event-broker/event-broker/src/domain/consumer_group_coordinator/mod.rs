//! In-process consumer group state and partition-assignment coordinator.
//! (`eb-group-rebalance-coordinator` design.md D1–D7)
//!
//! A consumer group is owned by exactly one delivery instance.  All open
//! streams for that group live in the same OS process and are reachable via
//! in-process `mpsc` channels — no cluster watch or distributed lock is
//! needed for steady-state operation.
//!
//! It is also the only home a subscription has. A subscription is session
//! state of the one instance its group lives on, so it is held here, beside
//! the membership it describes, and nowhere else - the cluster cache carries
//! only the routing markers a dispatcher needs to find this instance
//! (`domain::repo::RoutingMarkers`). A second copy elsewhere was a second
//! source of truth: it expired on a clock of its own while the member was
//! still here, and the two drifted.
//!
//! Each member moves through [`MemberState`], and every state but
//! `Streaming` has a lifetime. [`ConsumerGroupCoordinator::sweep`] is what
//! enforces it; nothing else removes a member except an explicit LEAVE and
//! the eviction a JOIN performs.

use crate::domain::model::{Sequence, Subscription};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use tokio::sync::watch;
use tokio::time::Instant;
use toolkit_gts::GtsInstanceId;
use uuid::Uuid;

use crate::domain::model::Assignment;
use crate::domain::streaming::assignment::Generation;

/// Topic identity and partition count, passed to `join`.
pub struct TopicInterest {
    pub id: GtsInstanceId,
    pub partitions: i32,
}

pub struct ConsumerGroupCoordinator {
    pub(crate) state: Mutex<CoordinatorState>,
    /// How long a member may stay `Joined` - joined, never streamed - before
    /// it is reaped.
    join_timeout: Duration,
}

#[derive(Default)]
pub(crate) struct CoordinatorState {
    pub(crate) groups: HashMap<GtsInstanceId, GroupState>,
    /// Which group each subscription belongs to, so a lookup by subscription
    /// id - what every per-subscription request carries - is not a scan.
    pub(crate) index: HashMap<Uuid, GtsInstanceId>,
}

pub(crate) struct GroupState {
    pub(crate) topology_version: i64,
    pub(crate) members: HashMap<Uuid, MemberEntry>,
}

pub(crate) struct MemberEntry {
    /// The subscription itself. Its `assigned` and `topology_version` are
    /// rewritten by every rebalance, so reading it is reading current state.
    pub(crate) subscription: Subscription,
    /// `(topic_id, partition_count)` for each topic this member is interested in.
    pub(crate) interests: Vec<(GtsInstanceId, i32)>,
    pub(crate) state: MemberState,
    /// When `state` was entered - the age every lifetime is measured from.
    pub(crate) state_since: Instant,
    /// This member's assignment, published rather than pushed.
    ///
    /// A `watch` and not a channel: it keeps only the latest value, so a
    /// topology or terminal frame cannot be dropped by delivery-path
    /// backpressure when a consumer stops draining.
    ///
    /// Present from join, with no open stream: a `watch::Sender` is valid with
    /// zero receivers, which is what makes `receiver_count()` mean "no stream
    /// open". The coordinator publishes; the session classifies.
    pub(crate) generations: watch::Sender<Generation>,
}

/// Where a member is in its life, and so how long it may stay there.
///
/// | state          | lifetime                   |
/// |----------------|----------------------------|
/// | `Joined`       | the coordinator's `join_timeout` |
/// | `Streaming`    | none - an open stream is the member |
/// | `Disconnected` | the member's own `session_timeout` |
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub(crate) enum MemberState {
    /// Joined, no stream opened yet. Holds its assignment.
    Joined,
    /// A stream is open.
    Streaming,
    /// Its stream closed; partitions held pending `session_timeout`, so a
    /// reconnect resumes without a rebalance.
    Disconnected,
}

/// A member that left the group, for the caller to clear its markers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Removal {
    pub subscription_id: Uuid,
    pub group: GtsInstanceId,
    /// The group has no members left, and is gone.
    pub group_emptied: bool,
}

/// What a JOIN produced.
pub struct JoinOutcome {
    /// The stored subscription, carrying the assignment and topology version
    /// the join computed.
    pub subscription: Subscription,
    /// `Disconnected` members the join evicted.
    pub evicted: Vec<Removal>,
}

impl ConsumerGroupCoordinator {
    #[must_use]
    pub fn new(join_timeout: Duration) -> Self {
        Self {
            state: Mutex::new(CoordinatorState::default()),
            join_timeout,
        }
    }

    /// Records the new member, computes a range-based partition split across
    /// all members, increments `topology_version`, and publishes every
    /// member's new assignment.
    ///
    /// Evicts the group's `Disconnected` members first - a new JOIN fills the
    /// gap rather than rebalancing against a member that is not coming back.
    pub fn join(&self, subscription: Subscription, interests: &[TopicInterest]) -> JoinOutcome {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let CoordinatorState { groups, index } = &mut *state;
        let group_id = subscription.consumer_group.clone();
        let sub_id = subscription.id;
        let group = groups
            .entry(group_id.clone())
            .or_insert_with(|| GroupState {
                topology_version: 0,
                members: HashMap::new(),
            });

        let version = group.topology_version;
        let evicted = group
            .members
            .extract_if(|_, m| m.state == MemberState::Disconnected)
            .map(|(id, member)| {
                retire(&member, version);
                index.remove(&id);
                Removal {
                    subscription_id: id,
                    group: group_id.clone(),
                    // The joiner is about to be inserted.
                    group_emptied: false,
                }
            })
            .collect();

        let mut stored = subscription.clone();
        group.members.insert(
            sub_id,
            MemberEntry {
                subscription,
                interests: interests
                    .iter()
                    .map(|t| (t.id.clone(), t.partitions))
                    .collect(),
                state: MemberState::Joined,
                state_since: Instant::now(),
                // Seeded empty at the version the join is about to produce;
                // `rebalance` below sends the real assignment. A sender with
                // no receivers is exactly the "no stream open" state.
                generations: watch::Sender::new(Generation::new(version, Vec::new())),
            },
        );
        index.insert(sub_id, group_id);

        let assignments = rebalance(group);
        stored.assigned = assignments.get(&sub_id).cloned().unwrap_or_default();
        stored.topology_version = group.topology_version;
        JoinOutcome {
            subscription: stored,
            evicted,
        }
    }

    /// Removes the member and redistributes its partitions.
    ///
    /// `None` when the member is already gone - a LEAVE racing the sweep or
    /// another LEAVE, which leaves nothing to do.
    pub fn leave(&self, group_id: &GtsInstanceId, sub_id: Uuid) -> Option<Removal> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let CoordinatorState { groups, index } = &mut *state;
        let group = groups.get_mut(group_id)?;
        let member = group.members.remove(&sub_id)?;
        retire(&member, group.topology_version);
        index.remove(&sub_id);
        let group_emptied = group.members.is_empty();
        if group_emptied {
            groups.remove(group_id);
        } else {
            let _ = rebalance(group);
        }
        Some(Removal {
            subscription_id: sub_id,
            group: group_id.clone(),
            group_emptied,
        })
    }

    /// The subscription, as of now.
    #[must_use]
    pub fn get(&self, sub_id: Uuid) -> Option<Subscription> {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let group_id = state.index.get(&sub_id)?;
        state
            .groups
            .get(group_id)?
            .members
            .get(&sub_id)
            .map(|m| m.subscription.clone())
    }

    /// Every subscription this instance holds.
    #[must_use]
    pub fn list(&self) -> Vec<Subscription> {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state
            .groups
            .values()
            .flat_map(|g| g.members.values().map(|m| m.subscription.clone()))
            .collect()
    }

    /// Whether the group has any member, in any state.
    #[must_use]
    pub fn has_members(&self, group_id: &GtsInstanceId) -> bool {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state
            .groups
            .get(group_id)
            .is_some_and(|g| !g.members.is_empty())
    }

    /// Subscribes to this member's assignment, moves it to `Streaming`, and
    /// takes the handle whose drop moves it to `Disconnected`.
    ///
    /// Returns `None` when the member is unknown - a caller racing a LEAVE or
    /// the sweep.
    pub fn subscribe(
        this: &Arc<Self>,
        group_id: &GtsInstanceId,
        sub_id: Uuid,
    ) -> Option<(watch::Receiver<Generation>, MembershipHandle)> {
        let receiver = {
            let mut state = this.state.lock().unwrap_or_else(PoisonError::into_inner);
            let member = state.groups.get_mut(group_id)?.members.get_mut(&sub_id)?;
            member.state = MemberState::Streaming;
            member.state_since = Instant::now();
            member.generations.subscribe()
        };
        Some((
            receiver,
            MembershipHandle {
                coordinator: Arc::clone(this),
                group_id: group_id.clone(),
                sub_id,
            },
        ))
    }

    /// Moves a streaming member to `Disconnected`, starting its
    /// `session_timeout`. Its partitions stay held, so a reconnect in time
    /// resumes without a rebalance.
    ///
    /// Only from `Streaming`. A second stream cannot have moved the member on
    /// in between: the session drops this handle before it releases its stream
    /// lease (field order in `StreamSession`), so no other stream can open until
    /// this has run.
    pub(crate) fn on_stream_closed(&self, group_id: &GtsInstanceId, sub_id: Uuid) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(member) = state
            .groups
            .get_mut(group_id)
            .and_then(|g| g.members.get_mut(&sub_id))
        else {
            return;
        };
        if member.state == MemberState::Streaming {
            member.state = MemberState::Disconnected;
            member.state_since = Instant::now();
        }
    }

    /// Reaps every member that has outlived its state: `Joined` past
    /// `join_timeout`, `Disconnected` past its `session_timeout`. Each group
    /// that lost a member is rebalanced once; a group left empty is dropped.
    ///
    /// Takes `now` so a test can drive it without a clock.
    pub fn sweep(&self, now: Instant) -> Vec<Removal> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let CoordinatorState { groups, index } = &mut *state;
        let mut removals = Vec::new();
        let mut emptied = Vec::new();
        for (group_id, group) in groups.iter_mut() {
            let version = group.topology_version;
            let expired: Vec<Uuid> = group
                .members
                .extract_if(|_, m| {
                    let lifetime = match m.state {
                        MemberState::Joined => self.join_timeout,
                        MemberState::Disconnected => m.subscription.session_timeout,
                        MemberState::Streaming => return false,
                    };
                    now.saturating_duration_since(m.state_since) >= lifetime
                })
                .map(|(id, member)| {
                    retire(&member, version);
                    index.remove(&id);
                    id
                })
                .collect();
            if expired.is_empty() {
                continue;
            }
            let group_emptied = group.members.is_empty();
            if group_emptied {
                emptied.push(group_id.clone());
            } else {
                let _ = rebalance(group);
            }
            removals.extend(expired.into_iter().map(|id| Removal {
                subscription_id: id,
                group: group_id.clone(),
                group_emptied,
            }));
        }
        for group_id in emptied {
            groups.remove(&group_id);
        }
        removals
    }
}

#[cfg(test)]
impl ConsumerGroupCoordinator {
    /// JOINs a bare member - for tests that need a membership, not a
    /// subscription's full shape.
    pub(crate) fn join_member(
        &self,
        group_id: &GtsInstanceId,
        sub_id: Uuid,
        interests: &[TopicInterest],
        session_timeout: Duration,
    ) -> (Vec<Assignment>, i64, Vec<Removal>) {
        let outcome = self.join(
            Subscription {
                id: sub_id,
                tenant_id: Uuid::nil(),
                consumer_group: group_id.clone(),
                client_agent: "test".to_owned(),
                interests: Vec::new(),
                topics: interests.iter().map(|t| t.id.clone()).collect(),
                assigned: Vec::new(),
                topology_version: 0,
                session_timeout,
                created_at: chrono::Utc::now(),
            },
            interests,
        );
        (
            outcome.subscription.assigned,
            outcome.subscription.topology_version,
            outcome.evicted,
        )
    }

    pub(crate) fn state_of(&self, sub_id: Uuid) -> Option<MemberState> {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let group_id = state.index.get(&sub_id)?;
        state
            .groups
            .get(group_id)?
            .members
            .get(&sub_id)
            .map(|m| m.state)
    }
}

/// Tells a member that has just been taken out of its group that it holds
/// nothing.
///
/// For a member with an open stream - a LEAVE of a streaming subscription -
/// an empty assignment reads as lose-all, which the session turns into a
/// terminal close; without it the session would be left holding a watch whose
/// sender is gone, waking on every pass for nothing.
fn retire(member: &MemberEntry, version: i64) {
    member
        .generations
        .send_replace(Generation::new(version, Vec::new()));
}

/// Recomputes the split over the group's members under a new topology
/// version, publishes it, and writes it back onto every member's
/// subscription.
///
/// Every member's `topology_version` advances, including one whose partitions
/// did not move: the version names the group's topology, and a SEEK is fenced
/// on it. Returns the new split.
fn rebalance(group: &mut GroupState) -> HashMap<Uuid, Vec<Assignment>> {
    group.topology_version += 1;
    let version = group.topology_version;
    let new_assignments = compute_assignments(&group.members);
    publish_generations(group, &new_assignments, version);
    for (id, member) in &mut group.members {
        if let Some(assigned) = new_assignments.get(id) {
            member.subscription.assigned.clone_from(assigned);
        }
        member.subscription.topology_version = version;
    }
    new_assignments
}

/// Computes range-based partition assignments for all active members across
/// every topic any of them is interested in.
fn compute_assignments(members: &HashMap<Uuid, MemberEntry>) -> HashMap<Uuid, Vec<Assignment>> {
    let mut result: HashMap<Uuid, Vec<Assignment>> =
        members.keys().map(|&id| (id, Vec::new())).collect();

    let mut all_topics: HashMap<GtsInstanceId, i32> = HashMap::new();
    for m in members.values() {
        for (topic_id, n_parts) in &m.interests {
            all_topics.entry(topic_id.clone()).or_insert(*n_parts);
        }
    }

    for (topic_id, n_partitions) in &all_topics {
        let mut interested: Vec<Uuid> = members
            .iter()
            .filter(|(_, m)| m.interests.iter().any(|(t, _)| t == topic_id))
            .map(|(&id, _)| id)
            .collect();
        interested.sort_unstable(); // deterministic ordering by UUID

        let splits = range_split(n_partitions.unsigned_abs(), &interested);
        for (i, &member_id) in interested.iter().enumerate() {
            let entries = result.entry(member_id).or_default();
            for &p in &splits[i] {
                entries.push(Assignment {
                    topic: topic_id.clone(),
                    partition: i32::try_from(p).unwrap_or(i32::MAX),
                    offset: Sequence::assigned(0),
                    last_examined: Sequence::assigned(0),
                });
            }
        }
    }

    result
}

/// Pushes `Frame::Topology` (loss only) or `Frame::Control { Terminal }`
/// (gain or lose-all) to members whose assignments changed.
/// `skip_id` is excluded — used to skip the newly-joined member who has no
/// open stream yet.
/// One session's membership in its group, for as long as its stream lives.
///
/// Dropping it reports the stream closed, which moves the member to
/// `Disconnected` and so starts its `session_timeout`. That replaces a task per stream awaiting a channel
/// close: the signal is the same moment, and the mechanism is the one this
/// change already uses twice - `StreamLease` releases exclusion on drop, and the
/// module owns the loader's lifetime the same way.
pub struct MembershipHandle {
    coordinator: Arc<ConsumerGroupCoordinator>,
    group_id: GtsInstanceId,
    sub_id: Uuid,
}

impl MembershipHandle {
    #[must_use]
    pub fn subscription_id(&self) -> Uuid {
        self.sub_id
    }
}

impl Drop for MembershipHandle {
    fn drop(&mut self) {
        self.coordinator
            .on_stream_closed(&self.group_id, self.sub_id);
    }
}

impl std::fmt::Debug for MembershipHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MembershipHandle")
            .field("group_id", &self.group_id)
            .field("sub_id", &self.sub_id)
            .finish_non_exhaustive()
    }
}

/// Publishes each changed member's new assignment on its own `watch`.
///
/// No classification and no frames. Whether a change is a gain, a loss, or only
/// a version bump is `AssignmentDelta::classify`'s answer, and it is the
/// session's to ask - it is the only party that knows where its readers
/// actually are. The coordinator computes assignments; it does not decide
/// frames.
fn publish_generations(
    group: &GroupState,
    new_assignments: &HashMap<Uuid, Vec<Assignment>>,
    version: i64,
) {
    // No member is skipped, including the one whose join or leave triggered
    // this. The frame-push version skipped the caller, because it received its
    // topology baseline in the response body and a pushed frame would have
    // duplicated it. A `watch` is state rather than an event: every member's
    // watch has to hold that member's *current* assignment, or a session opening
    // later reads a stale baseline and classifies its first real change against
    // it. Skipping the joiner left its watch on the empty seed, so the next
    // sibling change looked like a gain and terminated a stream that had only
    // lost partitions.
    for (&member_id, new_assigned) in new_assignments {
        let Some(member) = group.members.get(&member_id) else {
            continue;
        };

        let old_set: HashSet<(GtsInstanceId, i32)> = member
            .subscription
            .assigned
            .iter()
            .map(|a| (a.topic.clone(), a.partition))
            .collect();
        let new_set: HashSet<(GtsInstanceId, i32)> = new_assigned
            .iter()
            .map(|a| (a.topic.clone(), a.partition))
            .collect();
        if new_set == old_set {
            continue;
        }

        // `send_replace`, not `send`. `send` *fails and discards the value* when
        // there are no receivers, and at join time there is none yet - the
        // stream opens afterwards. That silently left a member's watch holding
        // its empty seed, so its session's first real change classified against
        // nothing and read as a gain, terminating a stream that had only lost
        // partitions. A watch used as state has to be written whether or not
        // anyone is currently listening.
        member
            .generations
            .send_replace(Generation::new(version, new_assigned.clone()));
    }
}

/// Partition split: member `i` of `k` receives partitions `[i*n/k .. (i+1)*n/k)`.
/// Produces contiguous ranges; minimises partition movement on incremental JOIN/LEAVE.
#[must_use]
pub fn range_split(n_partitions: u32, members: &[Uuid]) -> Vec<Vec<u32>> {
    let k = u32::try_from(members.len()).unwrap_or(u32::MAX);
    if k == 0 {
        return Vec::new();
    }
    (0..k)
        .map(|i| {
            let start = (i * n_partitions).div_euclid(k);
            let end = ((i + 1) * n_partitions).div_euclid(k);
            (start..end).collect()
        })
        .collect()
}

pub mod sweeper;

#[cfg(test)]
mod tests;
