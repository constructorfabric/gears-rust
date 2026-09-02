//! Election subscriptions — the *only* server-side state on the coordination
//! plane (DESIGN.md).
//!
//! An `election_id` addresses a **subscription**, not a lease. That distinction is
//! the whole reason this table is allowed to exist alongside §5.8.1's "no
//! server-side lease state":
//!
//! - a **lease** is a record in the backing store, so any replica serves any
//!   operation against it and no process's death ends it (invariant I7);
//! - a **subscription** is an open channel to one client through one replica, so
//!   it is replica-local by nature and dies with the replica. That is why
//!   `await_change` is the one operation that can report its replica going away
//!   while the lease it observes is untouched (§6.9).
//!
//! **Nothing in the lease path reads this table.** Dropping an entry revokes no
//! leadership and fails no renewal — item `S2`'s exit criterion asserts exactly
//! that, and it holds by construction here because
//! [`LeaderElectionService`](super::LeaderElectionService)'s `renew` and `resign`
//! never touch it.
//!
//! # What this is, and what `S2` added
//!
//! This is the registry `S1` needs to serve `await_change` at all: mint on `join`,
//! look up on `await_change`, remove when the stream ends. `S2` added
//! [`sweep`](ElectionSubscriptions::sweep) — the removal of *abandoned*
//! subscriptions, per §5.4.1's keying, plus the per-profile population the gauge
//! reads. The cadence, the grace window and the metrics live one module over, in
//! [`sweep`](super::sweep), because this type holds the state and that one holds
//! the schedule.
//!
//! It replaced a `retain(|id| ..)` seam that had been left here for it. The seam
//! could not carry the policy: the predicate is over an entry's *state* — how long
//! since its client was last present, and whether a reader is still holding it —
//! and a closure handed only the id can see neither. Widening it would have made
//! [`Subscription`] public, sender and all, so the sweep took the map's own
//! `retain` from the inside instead. Nothing outside called the seam.

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use cluster_sdk::leader::LeaderWatchEvent;
use tokio::sync::mpsc;
use tokio::time::Instant;
use uuid::Uuid;

/// How many events a subscription buffers before the server starts dropping and
/// reporting `Lagged`.
///
/// Much smaller than the cache watch's buffer, and deliberately so: an election
/// emits a transition, not a stream of mutations, so a subscriber that is 32
/// events behind is not slow — it is gone.
const SUBSCRIPTION_BUFFER: usize = 32;

/// Extra channel slots reserved as permits for the terminal shutdown sequence
/// (`Status(Lost)` then `Closed(Shutdown)`), so ADR-003's two-step remains
/// deliverable even when the consumer-visible buffer is full.
///
/// The same constant, for the same reason, as Profile 1's
/// `cluster_sdk::leader::watch::TERMINAL_HEADROOM`: back-pressure is worst
/// exactly during a drain, which is exactly when the two-step is sent, so a
/// fan-out that only ever `try_send`s would drop both terminal events precisely
/// when they matter. The reservation is made once per attached reader and
/// consumed by [`broadcast_terminal`](ElectionSubscriptions::broadcast_terminal).
const TERMINAL_HEADROOM: usize = 2;

/// The identity of one election subscription.
///
/// A v4 UUID rather than the election name: two participants in one election hold
/// two subscriptions, and the id has to tell them apart. It is also
/// unguessable, which matters because it is the whole of the authority
/// `await_change` presents — there is no token on that call.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SubscriptionId(String);

impl SubscriptionId {
    /// Mints a fresh subscription id.
    #[must_use]
    pub fn mint() -> Self {
        Self(Uuid::new_v4().to_string())
    }

    /// The id as it travels on the wire.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SubscriptionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for SubscriptionId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

/// One live subscription's server-side half.
struct Subscription {
    /// The caller that opened it. `await_change` is answered only to the caller
    /// that joined, so one workload cannot observe another's election feed by
    /// guessing an id — the id is unguessable, and this makes it moot anyway.
    caller: String,
    /// The election this subscription follows. Diagnostics only; an **election
    /// name is unbounded-cardinality** and therefore never a metric label
    /// (invariant I15) — it reaches the reap log and nothing else.
    election: String,
    /// The profile this subscription was opened against, interned.
    ///
    /// The gauge label, and `&'static str` for the reason
    /// [`cluster_sdk::intern`] exists: a profile name is drawn from the
    /// operator's configured set, which is exactly the bounded population that
    /// module sanctions interning. Nothing unbound can reach it — a request
    /// against an unbound profile fails in `authorize` before `open` is called.
    profile: &'static str,
    /// The last moment this subscription's client was demonstrably present for
    /// it (§5.4.1) — set at [`open`](ElectionSubscriptions::open), refreshed at
    /// [`attach`](ElectionSubscriptions::attach), and refreshed again by every
    /// [`sweep`](ElectionSubscriptions::sweep) pass that finds a live reader.
    ///
    /// That third writer is what makes one timestamp enough. Without it a stream
    /// held open for an hour would read as an hour stale the instant its reader
    /// went away, and would be reaped on the next pass rather than after the
    /// grace window it is owed.
    ///
    /// A [`tokio::time::Instant`], not a [`std::time::Instant`], so the sweep's
    /// arithmetic is testable under a paused clock.
    last_seen: Instant,
    /// Where the server pushes events. Bounded, and never blocked on.
    ///
    /// `None` between [`open`](ElectionSubscriptions::open) and the first
    /// [`attach`](ElectionSubscriptions::attach) — registered, but with no stream
    /// reading it yet. That window is real: `join` mints the id and
    /// `await_change` opens the stream, and they are two calls. Buffering into a
    /// channel nobody will ever read is exactly the abandoned-subscription leak
    /// `A6` and `S2` exist to bound, so the state is represented rather than
    /// approximated with a live sender.
    events: Option<mpsc::Sender<LeaderWatchEvent>>,
    /// Reserved slots for the terminal shutdown sequence, taken by
    /// [`broadcast_terminal`](ElectionSubscriptions::broadcast_terminal).
    ///
    /// Reserved at [`attach`](ElectionSubscriptions::attach) against a freshly
    /// created channel, so both reservations succeed; they are dropped with the
    /// subscription, releasing the slots, so a reaped or closed entry still lets
    /// its reader observe channel closure.
    terminal_headroom: Vec<mpsc::OwnedPermit<LeaderWatchEvent>>,
}

impl Subscription {
    /// Whether a stream is still reading this subscription.
    ///
    /// Both halves of the answer matter to the sweep. `None` is the window
    /// between `join` and `await_change`; a **closed** sender is a client that
    /// attached and then went away, which the stream task reports promptly by
    /// selecting on its own outbound channel closing (see
    /// [`subscription_stream`](super::leader)) rather than staying parked on
    /// `recv()` holding a receiver nobody will ever read.
    fn has_live_reader(&self) -> bool {
        self.events
            .as_ref()
            .is_some_and(|events| !events.is_closed())
    }
}

/// What one [`sweep`](ElectionSubscriptions::sweep) pass did and left behind,
/// counted per profile — the two signals §5.4.1 contracts.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SweepReport {
    /// Subscriptions reaped by this pass, by profile. Backs
    /// `cluster_subscriptions_reaped`.
    pub reaped: BTreeMap<&'static str, u64>,
    /// Subscriptions still live after it, by profile. Backs
    /// `cluster_subscriptions_active`, and is read from the *same* critical
    /// section as `reaped` so the two cannot describe different tables.
    pub live: BTreeMap<&'static str, u64>,
}

impl SweepReport {
    /// How many subscriptions this pass reaped across every profile.
    #[must_use]
    pub fn reaped_total(&self) -> u64 {
        self.reaped.values().sum()
    }
}

/// The live subscriptions this replica is serving.
#[derive(Default)]
pub struct ElectionSubscriptions {
    inner: Mutex<HashMap<SubscriptionId, Subscription>>,
}

impl fmt::Debug for ElectionSubscriptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let live = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        f.debug_struct("ElectionSubscriptions")
            .field("live", &live.len())
            .finish()
    }
}

impl ElectionSubscriptions {
    /// An empty table.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a subscription for `caller` on `election` under `profile`,
    /// returning its id.
    ///
    /// No stream is reading it yet; [`attach`](Self::attach) is what opens one.
    /// The entry starts its grace window here, so one that is never attached is
    /// reaped by the sweep rather than kept forever (§5.4.1).
    ///
    /// `profile` must be a name the registry resolved — it is interned as a
    /// metric label, and the intern table is only bounded because nothing
    /// caller-supplied reaches it.
    pub fn open(&self, caller: &str, election: &str, profile: &str) -> SubscriptionId {
        let id = SubscriptionId::mint();
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(
                id.clone(),
                Subscription {
                    caller: caller.to_owned(),
                    election: election.to_owned(),
                    profile: cluster_sdk::intern::intern(profile),
                    last_seen: Instant::now(),
                    events: None,
                    terminal_headroom: Vec::new(),
                },
            );
        id
    }

    /// Attaches a reader to the existing subscription `id`, replacing whatever
    /// sender it had, and returns the receiver the stream reads.
    ///
    /// `None` when `id` is unknown **or** belongs to another caller — the same
    /// answer for both, for the same reason a foreign lease token and an absent
    /// one give the same answer: a distinguishable "exists but not yours" would
    /// make the table enumerable.
    ///
    /// The id is kept, so a client's `election_id` stays valid across a
    /// reconnect and a broken stream needs no fresh `join`. Replacing the sender
    /// is also how §6.6's *at most one in-flight `await_change` per
    /// `election_id`* is enforced on a streaming projection: the older reader's
    /// channel closes, so the newer reader wins rather than the two being
    /// serialised — which would hand one of them a stale event.
    ///
    /// The check and the swap are one critical section on purpose: split in two,
    /// a concurrent [`close`](Self::close) between them would resurrect a
    /// subscription that had just been swept.
    pub fn attach(
        &self,
        id: &SubscriptionId,
        caller: &str,
    ) -> Option<mpsc::Receiver<LeaderWatchEvent>> {
        let mut live = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let subscription = live.get_mut(id)?;
        if subscription.caller != caller {
            return None;
        }
        // The consumer-visible buffer stays `SUBSCRIPTION_BUFFER`: the extra
        // slots are reserved below and never available to ordinary events.
        let (events, rx) = mpsc::channel(SUBSCRIPTION_BUFFER + TERMINAL_HEADROOM);
        // The channel was created a line ago and `rx` is still held here, so both
        // reservations succeed. Written as a loop rather than an `unreachable!`
        // because this runs inside a request handler: a capacity-accounting bug
        // must cost the two-step its guarantee, not the process its liveness.
        let mut terminal_headroom = Vec::with_capacity(TERMINAL_HEADROOM);
        for _ in 0..TERMINAL_HEADROOM {
            let Ok(permit) = events.clone().try_reserve_owned() else {
                break;
            };
            terminal_headroom.push(permit);
        }
        subscription.events = Some(events);
        subscription.terminal_headroom = terminal_headroom;
        // The client is here, which is the whole of what `last_seen` records.
        subscription.last_seen = Instant::now();
        Some(rx)
    }

    /// Closes a subscription, dropping its sender.
    ///
    /// **This revokes nothing.** The leader whose feed it was keeps its claim,
    /// which the lease in the store holds and only the client's own renewal
    /// sustains (invariant I8).
    pub fn close(&self, id: &SubscriptionId) {
        let removed = self
            .inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(id);
        if let Some(subscription) = removed {
            tracing::debug!(
                subscription = %id,
                election = subscription.election,
                caller = subscription.caller,
                "cluster: closed an election subscription"
            );
        }
    }

    /// Pushes one event of the terminal shutdown sequence through the headroom
    /// reserved at [`attach`](Self::attach), so it lands even against a full
    /// buffer.
    ///
    /// This is the fan-out item `S5` uses to deliver `Status(Lost)` and then
    /// `Closed(Shutdown)` to remote leaders, in that order, before `stop()`
    /// returns (§4.8) — the Profile 3 counterpart of Profile 1's
    /// `LeaderWatchSender::revoke_for_shutdown`, which reserves the same two
    /// slots for the same two events (ADR-003). It is here rather than in `S5`
    /// because the table it walks is here; `S5` supplies the ordering, the
    /// shutdown trigger, and the decision of whether a `Status(Lost)` is owed at
    /// all — the leader/follower question this table deliberately does not
    /// answer, exactly as `revoke_for_shutdown(was_leader)` leaves it to its
    /// caller.
    ///
    /// Two calls per subscription are guaranteed; a third falls back to a
    /// best-effort `try_send` that drops on a full buffer, because there is no
    /// third terminal event and a caller that sends one is not owed a
    /// reservation for it.
    pub fn broadcast_terminal(&self, event: &LeaderWatchEvent) {
        let mut live = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        for (id, subscription) in live.iter_mut() {
            if subscription.events.is_none() {
                continue;
            }
            let Some(permit) = subscription.terminal_headroom.pop() else {
                tracing::debug!(
                    subscription = %id,
                    "cluster: terminal headroom exhausted; falling back to a best-effort send"
                );
                if let Some(events) = subscription.events.as_ref() {
                    let _dropped = events.try_send(event.clone());
                }
                continue;
            };
            // Consuming the permit returns the sender clone it held; dropping it
            // releases nothing the subscription still needs, since its own
            // sender lives in `events`.
            let _sender = permit.send(event.clone());
        }
    }

    /// Empties the table, dropping every subscription's sender — the last step of
    /// the drain, after [`broadcast_terminal`](Self::broadcast_terminal) has put
    /// the two-step in the channels.
    ///
    /// **What this is and is not responsible for.** The thing that unblocks
    /// tonic's graceful shutdown is the `Closed(Shutdown)` itself:
    /// [`subscription_stream`](super::leader) returns on a terminal event, which
    /// drops its response stream, which is what lets an in-flight `await_change`
    /// complete. Measured, not assumed — with the two-step sent and this call
    /// stubbed out, the graceful-shutdown regression test still passes. What this
    /// call adds is the two cases the terminal event does not cover: a
    /// subscription registered by `join` that no `await_change` ever attached to
    /// (no stream to terminate, and a sender that would otherwise outlive the
    /// gear), and a reader whose headroom was exhausted so its close was only
    /// best-effort. It also leaves the table empty, so nothing survives `stop`
    /// holding a channel.
    ///
    /// Order matters and is the caller's to keep: events already queued in a
    /// channel survive their sender being dropped (the receiver drains them, then
    /// observes the close), so the two-step must be *sent* first. Closing first
    /// would deliver neither event.
    ///
    /// **It revokes nothing**, on exactly the terms [`close`](Self::close)
    /// states: a claim is a record in the store, and it lapses at its own
    /// deadline whatever this replica does (invariants I7, I8). Returns how many
    /// subscriptions were closed, which is what the caller logs.
    pub fn close_all(&self) -> usize {
        let closed =
            std::mem::take(&mut *self.inner.lock().unwrap_or_else(PoisonError::into_inner));
        // Dropped outside the lock: `Subscription` owns senders and reserved
        // permits, and dropping a permit wakes the receiver's task. Nothing here
        // takes this lock, but holding it across a wake is a habit worth not
        // forming.
        let count = closed.len();
        drop(closed);
        count
    }

    /// How many subscriptions are live. Diagnostics, and `S2`'s gauge source.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    /// Whether the table is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Reaps every abandoned subscription — §5.4.1's predicate, in one pass.
    ///
    /// A subscription is abandoned when it has **no live reader** and has been
    /// that way for at least `grace`. A pass that finds a live reader refreshes
    /// that entry's `last_seen` instead, which makes the single timestamp
    /// mean "last seen present" rather than "last touched by a call".
    ///
    /// **This is not a lease operation and reaps no lease.** Removing an entry
    /// revokes no leadership and fails no renewal: the claim is a row in the
    /// backing store and only the client's own renewal sustains it (invariants
    /// I7, I8). The one thing a reaped client loses is its event feed, and it
    /// learns that the same way it learns about any broken stream.
    ///
    /// Whole-table, under one lock, and deliberately so: the table holds one
    /// entry per live election participant on this replica, so a walk is cheap
    /// and a consistent `(reaped, live)` pair is worth more than a shorter
    /// critical section on a path nothing latency-sensitive takes.
    pub fn sweep(&self, grace: Duration) -> SweepReport {
        let now = Instant::now();
        let mut report = SweepReport::default();
        let mut live = self.inner.lock().unwrap_or_else(PoisonError::into_inner);

        live.retain(|id, subscription| {
            if subscription.has_live_reader() {
                subscription.last_seen = now;
                return true;
            }
            if now.saturating_duration_since(subscription.last_seen) < grace {
                return true;
            }
            tracing::debug!(
                subscription = %id,
                profile = subscription.profile,
                // Unbounded-cardinality, so it lives here and never on the
                // metric (invariant I15).
                election = subscription.election,
                caller = subscription.caller,
                attached = subscription.events.is_some(),
                "cluster: reaping an abandoned election subscription"
            );
            *report.reaped.entry(subscription.profile).or_default() += 1;
            false
        });

        for subscription in live.values() {
            *report.live.entry(subscription.profile).or_default() += 1;
        }
        report
    }

    /// Test-only: floods a subscription's consumer-visible buffer with `count`
    /// filler events, so a back-pressure test can arrange a full buffer.
    ///
    /// Production has no ordinary fan-out on this path — a leadership transition
    /// is derived client-side (see [`super::leader`]), so nothing but the
    /// terminal two-step is ever sent, and the two-step travels through the
    /// reserved [`terminal_headroom`](Subscription::terminal_headroom). This
    /// exists solely to reconstruct the "buffer already full" precondition that
    /// [`broadcast_terminal`](Self::broadcast_terminal)'s headroom reservation is
    /// there to survive.
    #[cfg(test)]
    pub(crate) fn fill_buffer_for_test(&self, id: &SubscriptionId, count: usize) {
        let live = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(events) = live.get(id).and_then(|s| s.events.as_ref()) {
            for _ in 0..count {
                if events.try_send(LeaderWatchEvent::Reset).is_err() {
                    break;
                }
            }
        }
    }
}

/// Shared handle, as the leader service and (later) `S5` both hold it.
pub type SharedSubscriptions = Arc<ElectionSubscriptions>;

#[cfg(test)]
#[path = "subscriptions_tests.rs"]
mod subscriptions_tests;
