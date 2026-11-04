//! The queue of one job (`docs/DESIGN.md` §4.9): which partition it works on
//! next, and when.
//!
//! A key is pending, leased to one worker, or absent. Offers raise a key's
//! priority and never lose a poke; the worker settles its lease by what its
//! run found, which says whether, and when, the key comes back.
//!
//! ## No lost pokes
//!
//! Every offer bumps the key's version, and a lease remembers the version it
//! took. `offer`, `take` and `settle` run under one mutex, so an offer is
//! either before a `take` or after it:
//!
//! - before: it is part of the version leased, and the job reads the database
//!   after the lease, so it sees whatever the offer was about;
//! - after, even after the job's commit: the version moved past the leased
//!   one, so `settle` keeps the key pending whatever the outcome.
//!
//! A priority raised while the key is leased is kept for its next lease.
//!
//! ## Why `lease.done()`, not `queue.done(lease)`
//!
//! The lease holds its queue anyway, to give the key back when dropped. A
//! queue-side method would also type-check against another queue of the
//! same priority type; each lease method consumes the lease, so settling
//! twice does not compile; and a lease that owns an `Arc` is `'static +
//! Send`, so parallel workers can carry it across awaits.

use std::cmp::Reverse;
use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use event_broker_sdk::backend::Partition;
use tokio::sync::Notify;
use tokio::time::Instant;
use uuid::Uuid;

/// One partition: `(topic_uuid, partition)`.
pub type Key = (Uuid, Partition);

/// How long postponed keys wait, and when they are reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    /// The wait after [`Lease::contended`], every time.
    pub contended_delay: Duration,
    /// The wait after the first [`Lease::failed`]; it doubles with each
    /// further one.
    pub failed_first: Duration,
    /// The cap on the doubling, and the wait of a stuck key.
    pub failed_max: Duration,
    /// Postponements in a row after which the key is reported once.
    pub warn_after: u32,
    /// Postponements in a row after which the key is stuck.
    pub stuck_after: u32,
    /// How long a key may keep being postponed before it is stuck.
    pub stuck_age: Duration,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            contended_delay: Duration::from_millis(250),
            failed_first: Duration::from_millis(100),
            failed_max: Duration::from_secs(30),
            warn_after: 5,
            stuck_after: 20,
            stuck_age: Duration::from_mins(5),
        }
    }
}

/// How a run finished with one key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Done,
    Again,
    Busy,
    Gone,
    Postponed(Postpone),
    Deferred,
    Cancelled,
}

/// Why a key was postponed, which sets how long it waits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Postpone {
    Contended,
    Failed,
}

/// When [`Prioritizer::take`] would next answer with a lease.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Due {
    /// Now.
    Now,
    /// Once this instant has passed, unless an offer comes first.
    At(Instant),
    /// Not until an offer.
    Never,
}

/// What [`Prioritizer::take`] answers.
pub enum Taken<P: Ord + Copy> {
    /// The key to work on.
    Lease(Lease<P>),
    /// Nothing is due before this instant.
    Until(Instant),
    /// Nothing is pending.
    Empty,
}

/// One job's queue of partitions, highest priority first, ties by the order
/// they became pending.
pub struct Prioritizer<P: Ord + Copy> {
    inner: Arc<Inner<P>>,
}

impl<P: Ord + Copy> Clone for Prioritizer<P> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<P: Ord + Copy> Prioritizer<P> {
    /// An empty queue for the job `name`, which names it in logs.
    #[must_use]
    pub fn new(name: &'static str, policy: Policy) -> Self {
        Self {
            inner: Arc::new(Inner {
                name,
                policy,
                state: Mutex::new(State {
                    entries: HashMap::new(),
                    ready: BTreeSet::new(),
                    delayed: BTreeSet::new(),
                    next_seq: 0,
                    stuck: 0,
                }),
                wakeup: Arc::new(Notify::new()),
            }),
        }
    }

    /// Adds `key`, or raises its priority. Always bumps the key's version.
    pub fn offer(&self, key: Key, priority: P) {
        let became_pending = self.inner.lock().offer(key, priority, Instant::now());
        if became_pending {
            self.inner.wakeup.notify_one();
        }
    }

    /// The highest-priority pending key whose delay has passed, leased.
    pub fn take(&self) -> Taken<P> {
        let now = Instant::now();
        let (taken, more) = {
            let mut state = self.inner.lock();
            state.promote(now);
            let taken = state.take();
            (taken, !state.ready.is_empty())
        };
        // One offer wakes one worker; this passes the wakeup on, so a burst
        // of offers reaches every idle worker.
        if more {
            self.inner.wakeup.notify_one();
        }
        match taken {
            Some((key, version, attempt)) => Taken::Lease(Lease {
                inner: Arc::clone(&self.inner),
                key,
                version,
                attempt,
                settled: false,
            }),
            None => match self.peek() {
                Due::At(at) => Taken::Until(at),
                Due::Now | Due::Never => Taken::Empty,
            },
        }
    }

    /// What `take` would answer, without leasing: for the worker's directive.
    #[must_use]
    pub fn peek(&self) -> Due {
        let state = self.inner.lock();
        if !state.ready.is_empty() {
            return Due::Now;
        }
        match state.delayed.first() {
            Some(&(at, _, _)) if at <= Instant::now() => Due::Now,
            Some(&(at, _, _)) => Due::At(at),
            None => Due::Never,
        }
    }

    /// Fires when a key becomes pending: the workers' notifier.
    #[must_use]
    pub fn wakeup(&self) -> Arc<Notify> {
        Arc::clone(&self.inner.wakeup)
    }

    /// Keys currently stuck.
    #[must_use]
    #[allow(
        dead_code,
        reason = "for a stuck-partitions gauge; the plugin exports no metrics yet"
    )]
    pub fn stuck(&self) -> usize {
        self.inner.lock().stuck
    }
}

/// One key leased to one worker until it is settled.
#[must_use = "settle the lease; dropping it gives the key back at once, uncounted"]
pub struct Lease<P: Ord + Copy> {
    inner: Arc<Inner<P>>,
    key: Key,
    version: u64,
    attempt: u32,
    settled: bool,
}

impl<P: Ord + Copy> Lease<P> {
    /// The partition leased.
    #[must_use]
    pub fn key(&self) -> Key {
        self.key
    }

    /// Consecutive postponements of this key before this lease (0 on a fresh
    /// key).
    #[must_use]
    pub fn attempt(&self) -> u32 {
        self.attempt
    }

    /// Finished: the key is dropped unless offered during the lease;
    /// postponements reset.
    pub fn done(mut self) {
        self.finish(Outcome::Done);
    }

    /// More to do on this key: back at once at the same priority;
    /// postponements reset.
    pub fn again(mut self) {
        self.finish(Outcome::Again);
    }

    /// Another instance holds the partition and finishes it: dropped unless
    /// offered during the lease.
    pub fn busy(mut self) {
        self.finish(Outcome::Busy);
    }

    /// The partition no longer exists: dropped unless offered during the
    /// lease.
    pub fn gone(mut self) {
        self.finish(Outcome::Gone);
    }

    /// Another transaction held what the run needed: back after
    /// [`Policy::contended_delay`], counted towards a warning and stuck.
    pub fn contended(mut self, error: &str) {
        self.note(error);
        self.finish(Outcome::Postponed(Postpone::Contended));
    }

    /// The run failed: back after a doubling wait, counted towards a warning
    /// and stuck.
    pub fn failed(mut self, error: &str) {
        self.note(error);
        self.finish(Outcome::Postponed(Postpone::Failed));
    }

    /// The database is down for every key: back at once and not counted,
    /// while the worker backs off.
    pub fn deferred(mut self) {
        self.finish(Outcome::Deferred);
    }

    /// Keeps `error` as the key's last one, for the report after
    /// [`Policy::warn_after`] postponements.
    fn note(&self, error: &str) {
        if let Some(entry) = self.inner.lock().entries.get_mut(&self.key) {
            entry.postponed.last_error = Some(error.to_owned());
        }
    }

    fn finish(&mut self, outcome: Outcome) {
        if self.settled {
            return;
        }
        self.settled = true;
        let pending = self
            .inner
            .lock()
            .settle(&self.inner, self.key, self.version, outcome);
        if pending {
            self.inner.wakeup.notify_one();
        }
    }
}

impl<P: Ord + Copy> Drop for Lease<P> {
    fn drop(&mut self) {
        self.finish(Outcome::Cancelled);
    }
}

struct Inner<P> {
    name: &'static str,
    policy: Policy,
    state: Mutex<State<P>>,
    wakeup: Arc<Notify>,
}

impl<P> Inner<P> {
    fn lock(&self) -> MutexGuard<'_, State<P>> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

struct State<P> {
    entries: HashMap<Key, Entry<P>>,
    /// Pending keys whose delay has passed, in the order `take` serves them.
    ready: BTreeSet<(Reverse<P>, u64, Key)>,
    /// Pending keys still waiting, soonest first.
    delayed: BTreeSet<(Instant, u64, Key)>,
    /// Orders keys that became pending, for ties.
    next_seq: u64,
    stuck: usize,
}

struct Entry<P> {
    version: u64,
    priority: P,
    slot: Slot,
    postponed: Postponed,
}

#[derive(Clone, Copy)]
enum Slot {
    Pending { seq: u64 },
    Leased,
}

#[derive(Default)]
struct Postponed {
    count: u32,
    since: Option<Instant>,
    stuck: bool,
    last_error: Option<String>,
}

impl<P: Ord + Copy> State<P> {
    /// Whether `key` became pending.
    fn offer(&mut self, key: Key, priority: P, now: Instant) -> bool {
        if let Some(entry) = self.entries.get_mut(&key) {
            entry.version += 1;
            if priority > entry.priority {
                if let Slot::Pending { seq } = entry.slot
                    && self.ready.remove(&(Reverse(entry.priority), seq, key))
                {
                    self.ready.insert((Reverse(priority), seq, key));
                }
                entry.priority = priority;
            }
            return false;
        }
        self.entries.insert(
            key,
            Entry {
                version: 1,
                priority,
                slot: Slot::Leased,
                postponed: Postponed::default(),
            },
        );
        self.pend(key, now, now);
        true
    }

    fn pend(&mut self, key: Key, not_before: Instant, now: Instant) {
        let seq = self.next_seq;
        self.next_seq += 1;
        let Some(entry) = self.entries.get_mut(&key) else {
            return;
        };
        entry.slot = Slot::Pending { seq };
        if not_before <= now {
            self.ready.insert((Reverse(entry.priority), seq, key));
        } else {
            self.delayed.insert((not_before, seq, key));
        }
    }

    /// Moves the keys whose delay has passed to `ready`.
    fn promote(&mut self, now: Instant) {
        while let Some(&(at, seq, key)) = self.delayed.first() {
            if at > now {
                break;
            }
            self.delayed.pop_first();
            if let Some(entry) = self.entries.get(&key) {
                self.ready.insert((Reverse(entry.priority), seq, key));
            }
        }
    }

    /// `(key, version, attempt)` of the key leased.
    fn take(&mut self) -> Option<(Key, u64, u32)> {
        let (_, _, key) = self.ready.pop_first()?;
        let entry = self.entries.get_mut(&key)?;
        entry.slot = Slot::Leased;
        Some((key, entry.version, entry.postponed.count))
    }

    fn remove(&mut self, key: Key) {
        if let Some(entry) = self.entries.remove(&key)
            && entry.postponed.stuck
        {
            self.stuck -= 1;
        }
    }

    /// Whether `key` is pending afterwards.
    fn settle(&mut self, inner: &Inner<P>, key: Key, version: u64, outcome: Outcome) -> bool {
        let now = Instant::now();
        let Some(entry) = self.entries.get_mut(&key) else {
            return false;
        };
        let dirty = entry.version != version;
        match outcome {
            Outcome::Done | Outcome::Busy | Outcome::Gone => {
                if outcome == Outcome::Done {
                    self.clear_postponed(inner, key);
                }
                if !dirty {
                    self.remove(key);
                    return false;
                }
                self.pend(key, now, now);
            }
            Outcome::Again => {
                self.clear_postponed(inner, key);
                self.pend(key, now, now);
            }
            Outcome::Postponed(why) => {
                let delay = self.postpone(inner, key, why, now);
                self.pend(key, now + delay, now);
            }
            Outcome::Deferred | Outcome::Cancelled => self.pend(key, now, now),
        }
        true
    }

    fn clear_postponed(&mut self, inner: &Inner<P>, key: Key) {
        let Some(entry) = self.entries.get_mut(&key) else {
            return;
        };
        let postponed = std::mem::take(&mut entry.postponed);
        if postponed.stuck {
            self.stuck -= 1;
            tracing::info!(
                job = inner.name,
                topic = %key.0,
                partition = %key.1,
                "event-broker maintenance: partition no longer stuck"
            );
        }
    }

    /// Counts one more postponement of `key` and answers how long it waits.
    fn postpone(&mut self, inner: &Inner<P>, key: Key, why: Postpone, now: Instant) -> Duration {
        let policy = &inner.policy;
        let Some(entry) = self.entries.get_mut(&key) else {
            return Duration::ZERO;
        };
        let postponed = &mut entry.postponed;
        postponed.count = postponed.count.saturating_add(1);
        let since = *postponed.since.get_or_insert(now);
        let postponed_for = now.saturating_duration_since(since);
        let last_error = postponed.last_error.as_deref().unwrap_or_default();
        if postponed.count == policy.warn_after {
            tracing::warn!(
                job = inner.name,
                topic = %key.0,
                partition = %key.1,
                postponements = postponed.count,
                ?postponed_for,
                last_error,
                "event-broker maintenance: partition keeps being postponed"
            );
        }
        if !postponed.stuck
            && (postponed.count >= policy.stuck_after || postponed_for >= policy.stuck_age)
        {
            postponed.stuck = true;
            self.stuck += 1;
            tracing::error!(
                job = inner.name,
                topic = %key.0,
                partition = %key.1,
                postponements = postponed.count,
                ?postponed_for,
                last_error,
                "event-broker maintenance: partition stuck"
            );
        }
        if postponed.stuck {
            return policy.failed_max;
        }
        match why {
            Postpone::Contended => policy.contended_delay,
            Postpone::Failed => {
                let factor = 1_u32.checked_shl(postponed.count - 1).unwrap_or(u32::MAX);
                policy
                    .failed_first
                    .saturating_mul(factor)
                    .min(policy.failed_max)
            }
        }
    }
}
