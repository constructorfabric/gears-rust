//! Doubles for the activation tests: an in-process key lock and a recording
//! lock-metrics sink.
//!
//! The production lock is `toolkit-db`'s advisory lock, which on `SQLite` is a
//! file marker under the user's cache directory shared by every test process:
//! tests that run in parallel over different in-memory databases would contend
//! on it through equal keys. The unit tests therefore take the lock through
//! this double, and the real one is exercised on `PostgreSQL`.

#![allow(
    clippy::must_use_candidate,
    clippy::missing_panics_doc,
    reason = "test doubles: a poisoned lock is a test failure"
)]

use std::collections::HashSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;

use super::{HeldKey, KeyLocks, LockMetrics};
use crate::domain::error::DomainError;

/// A lock table in this process: one holder per key, a budget, and a count of
/// what was asked.
pub struct InProcessLocks {
    held: Arc<Mutex<HashSet<String>>>,
    budget: Duration,
    /// How many acquisitions were attempted, by key.
    pub asked: Arc<Mutex<Vec<String>>>,
    /// How many releases ran.
    pub released: Arc<AtomicUsize>,
}

impl InProcessLocks {
    /// A table whose waits give up after `budget`.
    #[must_use]
    pub fn with_budget(budget: Duration) -> Arc<Self> {
        Arc::new(Self {
            held: Arc::default(),
            budget,
            asked: Arc::default(),
            released: Arc::default(),
        })
    }

    /// A table with a budget no test outlasts.
    #[must_use]
    pub fn generous() -> Arc<Self> {
        Self::with_budget(Duration::from_secs(30))
    }

    /// Hold `key` as another mutation would, until the guard is released.
    ///
    /// # Panics
    /// If the table's own mutex is poisoned.
    #[must_use]
    pub fn hold(&self, key: &str) -> Held {
        self.held.lock().expect("lock table").insert(key.to_owned());
        Held {
            table: Arc::clone(&self.held),
            key: key.to_owned(),
            released: Arc::clone(&self.released),
        }
    }

    /// Whether `key` is held now.
    ///
    /// # Panics
    /// If the table's own mutex is poisoned.
    #[must_use]
    pub fn is_held(&self, key: &str) -> bool {
        self.held.lock().expect("lock table").contains(key)
    }

    /// How many times `key` was asked for.
    ///
    /// # Panics
    /// If the table's own mutex is poisoned.
    #[must_use]
    pub fn asks_for(&self, key: &str) -> usize {
        self.asked
            .lock()
            .expect("asked")
            .iter()
            .filter(|k| *k == key)
            .count()
    }
}

/// A held key of [`InProcessLocks`].
pub struct Held {
    table: Arc<Mutex<HashSet<String>>>,
    key: String,
    released: Arc<AtomicUsize>,
}

#[async_trait]
impl HeldKey for Held {
    async fn release(self: Box<Self>) {
        self.table.lock().expect("lock table").remove(&self.key);
        self.released.fetch_add(1, Ordering::SeqCst);
    }
}

#[async_trait]
impl KeyLocks for InProcessLocks {
    async fn acquire(
        &self,
        key: &str,
        cap: Option<Duration>,
    ) -> Result<Box<dyn HeldKey>, DomainError> {
        self.asked.lock().expect("asked").push(key.to_owned());
        let budget = cap.map_or(self.budget, |cap| cap.min(self.budget));
        let started = std::time::Instant::now();
        loop {
            if self.held.lock().expect("lock table").insert(key.to_owned()) {
                return Ok(Box::new(Held {
                    table: Arc::clone(&self.held),
                    key: key.to_owned(),
                    released: Arc::clone(&self.released),
                }));
            }
            if started.elapsed() >= budget {
                return Err(DomainError::Unavailable {
                    detail: "the key's mutation lock is busy".to_owned(),
                });
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    }
}

/// A metrics sink that remembers what it was told.
#[derive(Default)]
pub struct RecordingLockMetrics {
    /// The waits of the acquisitions that succeeded.
    pub acquired: Mutex<Vec<Duration>>,
    /// The waits of those that ran out their budget.
    pub exhausted: Mutex<Vec<Duration>>,
    /// How many errors were counted.
    pub errors: AtomicUsize,
}

impl LockMetrics for RecordingLockMetrics {
    fn acquired(&self, wait: Duration) {
        self.acquired.lock().expect("metrics").push(wait);
    }

    fn exhausted(&self, wait: Duration) {
        self.exhausted.lock().expect("metrics").push(wait);
    }

    fn error(&self) {
        self.errors.fetch_add(1, Ordering::SeqCst);
    }
}

// ---------------------------------------------------------------------------
// The notification half
// ---------------------------------------------------------------------------

use event_broker_sdk::EventV1;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::{
    ActivationCounts, ActivationNotifier, ActivationStatus, BuiltNotification, DueRow, InstanceKey,
    NotificationGateway, NotificationStore, NotifiedChange, NotifyMetrics, ScanLock, Silenced,
    SubscriberTenant,
};
use crate::domain::broker_sink::{BroadcastSink, PublishError};
use crate::infra::broadcast::build_notifications;

/// A notify-metrics sink that remembers what it was told.
#[derive(Default)]
pub struct RecordingNotifyMetrics {
    /// `(event, elapsed)` of each publish-to-deliver observation.
    pub delivered: Mutex<Vec<(&'static str, Duration)>>,
    /// Each response latency observed, with its `late` label.
    pub latencies: Mutex<Vec<(Duration, bool)>>,
    /// How many redeliveries were counted.
    pub redeliveries: AtomicUsize,
    /// Each value the awaiting gauge was set to.
    pub awaiting: Mutex<Vec<u64>>,
    /// Each value the subscriptions gauge was set to.
    pub subscriptions: Mutex<Vec<u64>>,
    /// Each value the silent-states gauge was set to.
    pub silent: Mutex<Vec<u64>>,
    /// Each value the silent-subscriptions gauge was set to.
    pub silent_subscriptions: Mutex<Vec<u64>>,
    /// How many states were counted called silent.
    pub silenced: AtomicUsize,
    /// How many transitions into `incomplete` were counted.
    pub incomplete: AtomicUsize,
    /// How many subscriptions were counted forgotten.
    pub forgotten: AtomicUsize,
    /// Each reaction outcome counted.
    pub reactions: Mutex<Vec<&'static str>>,
    /// Each report result counted.
    pub reports: Mutex<Vec<&'static str>>,
    /// Each registration-cap decision counted.
    pub evictions: Mutex<Vec<&'static str>>,
}

impl NotifyMetrics for RecordingNotifyMetrics {
    fn publish_to_deliver(&self, event: &'static str, elapsed: Duration) {
        self.delivered
            .lock()
            .expect("metrics")
            .push((event, elapsed));
    }

    fn response_latency(&self, elapsed: Duration, late: bool) {
        self.latencies
            .lock()
            .expect("metrics")
            .push((elapsed, late));
    }

    fn redelivery(&self) {
        self.redeliveries.fetch_add(1, Ordering::SeqCst);
    }

    fn awaiting(&self, count: u64) {
        self.awaiting.lock().expect("metrics").push(count);
    }

    fn subscriptions(&self, count: u64) {
        self.subscriptions.lock().expect("metrics").push(count);
    }

    fn silent(&self, count: u64) {
        self.silent.lock().expect("metrics").push(count);
    }

    fn silent_subscriptions(&self, count: u64) {
        self.silent_subscriptions
            .lock()
            .expect("metrics")
            .push(count);
    }

    fn silenced(&self) {
        self.silenced.fetch_add(1, Ordering::SeqCst);
    }

    fn incomplete(&self) {
        self.incomplete.fetch_add(1, Ordering::SeqCst);
    }

    fn forgotten(&self) {
        self.forgotten.fetch_add(1, Ordering::SeqCst);
    }

    fn reaction(&self, outcome: &'static str) {
        self.reactions.lock().expect("metrics").push(outcome);
    }

    fn report(&self, result: &'static str) {
        self.reports.lock().expect("metrics").push(result);
    }

    fn eviction(&self, result: &'static str) {
        self.evictions.lock().expect("metrics").push(result);
    }
}

/// One activation state of the in-memory repository.
#[derive(Debug, Clone)]
pub struct MemoryRow {
    /// The pair it belongs to.
    pub pair: SubscriberTenant,
    /// The consumer instance whose state it is.
    pub instance_id: Uuid,
    /// The setting key.
    pub key: String,
    /// The change it expects.
    pub change_ref: Uuid,
    /// Where it stands.
    pub status: ActivationStatus,
    /// The fake time of its last publish.
    pub last_notified: Option<Duration>,
    /// The accepted publications of its `change_ref`.
    pub deliveries: u32,
    /// The fake time it was called silent.
    pub silent_since: Option<Duration>,
}

impl MemoryRow {
    /// The state's identity.
    #[must_use]
    pub fn instance_key(&self) -> InstanceKey {
        InstanceKey {
            subscriber: self.pair.subscriber.clone(),
            instance_id: self.instance_id,
            key: self.key.clone(),
            tenant: self.pair.tenant.clone(),
        }
    }
}

/// A [`NotificationStore`] over memory, on a fake clock, keyed per consumer
/// instance like the tables.
#[derive(Default)]
pub struct InMemoryNotificationRepo {
    /// The states.
    pub rows: Mutex<Vec<MemoryRow>>,
    /// The registered instances, each with the fake time its subscription was
    /// first called silent.
    pub subscribed: Mutex<std::collections::HashMap<InstanceKey, Option<Duration>>>,
    /// The fake clock.
    pub clock: Mutex<Duration>,
    /// While set, every read fails.
    pub failing: std::sync::atomic::AtomicBool,
    /// How many orphan-sweep statements ran.
    pub sweeps: AtomicUsize,
    /// How many due scans ran.
    pub scans: AtomicUsize,
    /// The instances' labels, as their subscriptions hold them.
    pub labels: Mutex<std::collections::HashMap<InstanceKey, String>>,
    /// The `activation` records the calls of silence appended.
    pub silent_records: Mutex<Vec<SilentRecord>>,
    /// Instances whose silence a report or a registration ends right after a
    /// read of the subscriptions to forget, as one landing between that read
    /// and the forgetting would.
    pub reenter_after_read: Mutex<Vec<InstanceKey>>,
}

/// What a call of silence's `activation` record carries, as the in-memory
/// repository keeps it: outcome `silent`, no detail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SilentRecord {
    /// The silent instance, its key and tenant; the subscriber is the actor.
    pub instance: InstanceKey,
    /// Its label at the time of the call.
    pub instance_label: Option<String>,
    /// The change the record is under.
    pub change_ref: Uuid,
    /// The fake time of the call.
    pub at: Duration,
}

/// The instance [`InMemoryNotificationRepo::add`] registers: the one instance
/// of a subscriber that runs one process.
pub const ONE_INSTANCE: Uuid = Uuid::nil();

impl InMemoryNotificationRepo {
    /// An empty repository.
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::default()
    }

    /// An `awaiting` state of [`ONE_INSTANCE`] with its subscription, never
    /// published.
    pub fn add(&self, subscriber: &str, tenant: &str, key: &str, change_ref: Uuid) {
        self.add_instance(subscriber, ONE_INSTANCE, tenant, key, change_ref);
    }

    /// An `awaiting` state of `instance_id` with its subscription, never
    /// published.
    pub fn add_instance(
        &self,
        subscriber: &str,
        instance_id: Uuid,
        tenant: &str,
        key: &str,
        change_ref: Uuid,
    ) {
        self.subscribed.lock().expect("repo").insert(
            InstanceKey {
                subscriber: subscriber.to_owned(),
                instance_id,
                key: key.to_owned(),
                tenant: tenant.to_owned(),
            },
            None,
        );
        self.rows.lock().expect("repo").push(MemoryRow {
            pair: SubscriberTenant {
                subscriber: subscriber.to_owned(),
                tenant: tenant.to_owned(),
            },
            instance_id,
            key: key.to_owned(),
            change_ref,
            status: ActivationStatus::Awaiting,
            last_notified: None,
            deliveries: 0,
            silent_since: None,
        });
    }

    /// Delete the subscriptions of every instance of `subscriber` for `key` at
    /// `tenant` behind the states' backs, leaving the states orphaned.
    pub fn drop_subscriptions(&self, subscriber: &str, tenant: &str, key: &str) {
        self.subscribed
            .lock()
            .expect("repo")
            .retain(|k, _| !(k.subscriber == subscriber && k.tenant == tenant && k.key == key));
    }

    /// Move the fake clock.
    pub fn advance(&self, by: Duration) {
        *self.clock.lock().expect("repo") += by;
    }

    /// The state of `key` for `subscriber` at `tenant` — of its first
    /// instance, for a test that runs one.
    #[must_use]
    pub fn row(&self, subscriber: &str, tenant: &str, key: &str) -> Option<MemoryRow> {
        self.rows
            .lock()
            .expect("repo")
            .iter()
            .find(|r| r.pair.subscriber == subscriber && r.pair.tenant == tenant && r.key == key)
            .cloned()
    }

    /// The state of exactly this instance.
    #[must_use]
    pub fn row_of(&self, instance: &InstanceKey) -> Option<MemoryRow> {
        self.rows
            .lock()
            .expect("repo")
            .iter()
            .find(|r| r.instance_key() == *instance)
            .cloned()
    }

    /// Whether the instance's subscription exists.
    #[must_use]
    pub fn is_subscribed(&self, instance: &InstanceKey) -> bool {
        self.subscribed.lock().expect("repo").contains_key(instance)
    }

    /// The fake time the instance's subscription was first called silent, if
    /// it exists and was.
    #[must_use]
    pub fn silent_since_of(&self, instance: &InstanceKey) -> Option<Duration> {
        self.subscribed
            .lock()
            .expect("repo")
            .get(instance)
            .copied()
            .flatten()
    }

    /// End the instance's silence, as a report or a registration of it does:
    /// its subscription's `silent_since` cleared.
    pub fn reenter(&self, instance: &InstanceKey) {
        if let Some(since) = self.subscribed.lock().expect("repo").get_mut(instance) {
            *since = None;
        }
    }

    /// Set the status of exactly this instance's state, as a report would.
    pub fn set_status(&self, instance: &InstanceKey, status: ActivationStatus) {
        for row in self
            .rows
            .lock()
            .expect("repo")
            .iter_mut()
            .filter(|r| r.instance_key() == *instance)
        {
            row.status = status;
        }
    }

    /// The fake now.
    #[must_use]
    pub fn now(&self) -> Duration {
        *self.clock.lock().expect("repo")
    }

    /// Apply a change that overwrites the states of every instance of the
    /// pair at `key`: a new `change_ref`, `awaiting`, the stamp, the delivery
    /// count and the silence cleared.
    pub fn overwrite(&self, subscriber: &str, tenant: &str, key: &str, change_ref: Uuid) {
        let mut rows = self.rows.lock().expect("repo");
        for row in rows
            .iter_mut()
            .filter(|r| r.pair.subscriber == subscriber && r.pair.tenant == tenant && r.key == key)
        {
            row.change_ref = change_ref;
            row.status = ActivationStatus::Awaiting;
            row.last_notified = None;
            row.deliveries = 0;
            row.silent_since = None;
        }
    }

    /// As `ActivationRepo::stamp_due`:
    /// stamp the fake now and count one delivery on exactly the rows of `due`
    /// still naming their `change_ref`, still `awaiting` and still at the
    /// `deliveries` they were read with. Returns how many rows were stamped.
    ///
    /// # Errors
    /// [`DomainError::Unavailable`] while the repository is failing.
    pub fn stamp_due(&self, due: &[DueRow]) -> Result<u64, DomainError> {
        self.check()?;
        let now = *self.clock.lock().expect("repo");
        let mut stamped = 0;
        for row in self.rows.lock().expect("repo").iter_mut() {
            let named = due.iter().any(|d| {
                d.instance_key() == row.instance_key()
                    && d.change.change_ref == row.change_ref
                    && d.deliveries == row.deliveries
            });
            if named && row.status == ActivationStatus::Awaiting {
                row.last_notified = Some(now);
                row.deliveries += 1;
                stamped += 1;
            }
        }
        Ok(stamped)
    }

    /// As `ActivationRepo::call_silent`: the instance's state of `change_ref`
    /// made `silent` at the fake now, guarded on it still naming `change_ref`
    /// and still being `awaiting`. `true` when the guard matched.
    ///
    /// # Errors
    /// [`DomainError::Unavailable`] while the repository is failing.
    pub fn call_silent(
        &self,
        instance: &InstanceKey,
        change_ref: Uuid,
    ) -> Result<bool, DomainError> {
        self.check()?;
        let now = *self.clock.lock().expect("repo");
        let mut rows = self.rows.lock().expect("repo");
        let Some(row) = rows.iter_mut().find(|r| {
            r.instance_key() == *instance
                && r.change_ref == change_ref
                && r.status == ActivationStatus::Awaiting
        }) else {
            return Ok(false);
        };
        row.status = ActivationStatus::Silent;
        row.silent_since = Some(now);
        Ok(true)
    }

    /// As `ActivationRepo::mark_silent`: the instance's subscription marked
    /// silent at the fake now unless it already is. `true` when marked.
    ///
    /// # Errors
    /// [`DomainError::Unavailable`] while the repository is failing.
    pub fn mark_silent(&self, instance: &InstanceKey) -> Result<bool, DomainError> {
        self.check()?;
        let now = *self.clock.lock().expect("repo");
        match self.subscribed.lock().expect("repo").get_mut(instance) {
            Some(since @ None) => {
                *since = Some(now);
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    /// As `ActivationRepo::forgettable`: the subscriptions silent since before
    /// the fake instant `cutoff`, oldest silence first.
    ///
    /// # Errors
    /// [`DomainError::Unavailable`] while the repository is failing.
    pub fn forgettable(&self, cutoff: Duration) -> Result<Vec<InstanceKey>, DomainError> {
        self.check()?;
        let mut found: Vec<(Duration, InstanceKey)> = self
            .subscribed
            .lock()
            .expect("repo")
            .iter()
            .filter_map(|(k, since)| since.filter(|s| *s < cutoff).map(|s| (s, k.clone())))
            .collect();
        found.sort_by(|a, b| {
            (a.0, &a.1.subscriber, a.1.instance_id, &a.1.key, &a.1.tenant).cmp(&(
                b.0,
                &b.1.subscriber,
                b.1.instance_id,
                &b.1.key,
                &b.1.tenant,
            ))
        });
        Ok(found.into_iter().map(|(_, k)| k).collect())
    }

    /// As `ActivationRepo::forget`: the instance's state, then its
    /// subscription, deleted while the subscription is still silent since
    /// before the fake instant `cutoff`. `true` when the subscription went.
    ///
    /// # Errors
    /// [`DomainError::Unavailable`] while the repository is failing.
    pub fn forget(&self, instance: &InstanceKey, cutoff: Duration) -> Result<bool, DomainError> {
        self.check()?;
        let mut subscribed = self.subscribed.lock().expect("repo");
        let still_silent = subscribed
            .get(instance)
            .is_some_and(|since| since.is_some_and(|s| s < cutoff));
        if !still_silent {
            return Ok(false);
        }
        self.rows
            .lock()
            .expect("repo")
            .retain(|r| r.instance_key() != *instance);
        subscribed.remove(instance);
        Ok(true)
    }

    fn check(&self) -> Result<(), DomainError> {
        if self.failing.load(Ordering::SeqCst) {
            Err(DomainError::Unavailable {
                detail: "the database is down".to_owned(),
            })
        } else {
            Ok(())
        }
    }
}

impl InMemoryNotificationRepo {
    /// As `ActivationRepo::due`: the `awaiting` rows never stamped or stamped
    /// at least `interval` before the fake now.
    fn due_rows(&self, interval: Duration, pair: Option<&SubscriberTenant>) -> Vec<DueRow> {
        let now = *self.clock.lock().expect("repo");
        let mut due: Vec<DueRow> = self
            .rows
            .lock()
            .expect("repo")
            .iter()
            .filter(|r| pair.is_none_or(|p| r.pair == *p))
            .filter(|r| r.status == ActivationStatus::Awaiting)
            .filter(|r| {
                r.last_notified
                    .is_none_or(|at| now.saturating_sub(at) >= interval)
            })
            .map(|r| DueRow {
                pair: r.pair.clone(),
                instance_id: r.instance_id,
                change: NotifiedChange {
                    key: r.key.clone(),
                    change_ref: r.change_ref,
                },
                deliveries: r.deliveries,
            })
            .collect();
        due.sort_by(|a, b| {
            (
                &a.pair.subscriber,
                &a.pair.tenant,
                &a.change.key,
                a.instance_id,
            )
                .cmp(&(
                    &b.pair.subscriber,
                    &b.pair.tenant,
                    &b.change.key,
                    b.instance_id,
                ))
        });
        due
    }
}

#[async_trait]
impl NotificationStore for InMemoryNotificationRepo {
    async fn due_of(
        &self,
        pair: &SubscriberTenant,
        interval: Duration,
    ) -> Result<Vec<DueRow>, DomainError> {
        self.check()?;
        Ok(self.due_rows(interval, Some(pair)))
    }

    async fn stamp_due(&self, rows: &[DueRow]) -> Result<u64, DomainError> {
        Self::stamp_due(self, rows)
    }

    async fn due(&self, interval: Duration) -> Result<Vec<DueRow>, DomainError> {
        self.check()?;
        self.scans.fetch_add(1, Ordering::SeqCst);
        Ok(self.due_rows(interval, None))
    }

    async fn silence(&self, row: &DueRow) -> Result<Option<Silenced>, DomainError> {
        let instance = row.instance_key();
        if !self.call_silent(&instance, row.change.change_ref)? {
            return Ok(None);
        }
        self.mark_silent(&instance)?;
        let instance_label = self.labels.lock().expect("repo").get(&instance).cloned();
        self.silent_records
            .lock()
            .expect("repo")
            .push(SilentRecord {
                instance,
                instance_label,
                change_ref: row.change.change_ref,
                at: self.now(),
            });
        let incomplete = !self.rows.lock().expect("repo").iter().any(|r| {
            r.pair == row.pair
                && r.key == row.change.key
                && r.instance_id != row.instance_id
                && matches!(
                    r.status,
                    ActivationStatus::Silent | ActivationStatus::Failed
                )
        });
        Ok(Some(Silenced { incomplete }))
    }

    async fn silent_past(&self, age: Duration) -> Result<Vec<InstanceKey>, DomainError> {
        let cutoff = self.now().saturating_sub(age);
        let found = self.forgettable(cutoff)?;
        for instance in self.reenter_after_read.lock().expect("repo").drain(..) {
            self.reenter(&instance);
        }
        Ok(found)
    }

    async fn forget_silent(
        &self,
        instance: &InstanceKey,
        age: Duration,
    ) -> Result<bool, DomainError> {
        let cutoff = self.now().saturating_sub(age);
        self.forget(instance, cutoff)
    }

    async fn sweep_orphans(&self) -> Result<u64, DomainError> {
        self.check()?;
        self.sweeps.fetch_add(1, Ordering::SeqCst);
        let subscribed = self.subscribed.lock().expect("repo").clone();
        let mut rows = self.rows.lock().expect("repo");
        let before = rows.len();
        rows.retain(|r| subscribed.contains_key(&r.instance_key()));
        Ok((before - rows.len()) as u64)
    }

    async fn counts(&self) -> Result<ActivationCounts, DomainError> {
        self.check()?;
        let rows = self.rows.lock().expect("repo");
        let of = |status| rows.iter().filter(|r| r.status == status).count() as u64;
        let subscribed = self.subscribed.lock().expect("repo");
        Ok(ActivationCounts {
            subscriptions: subscribed.len() as u64,
            awaiting: of(ActivationStatus::Awaiting),
            silent: of(ActivationStatus::Silent),
            silent_subscriptions: subscribed.values().filter(|s| s.is_some()).count() as u64,
        })
    }
}

/// What a publish through a [`FakeNotifications`] session does.
#[derive(Debug, Clone)]
pub enum PublishScript {
    /// The broker accepts it.
    Accept,
    /// The broker refuses it.
    Refuse,
    /// The broker does not accept it within the publish timeout.
    TimeOut,
    /// The broker accepts it, but only after `Gate::open` — and the test
    /// observes that the publish is in flight first.
    Hold,
}

/// A test gate: a publish parked on it signals that it arrived, and goes on
/// when the test opens the gate.
#[derive(Default)]
pub struct Gate {
    /// Signalled when a publish reached the gate.
    pub arrived: tokio::sync::Notify,
    /// Opened by the test.
    pub open: tokio::sync::Notify,
}

/// A [`NotificationGateway`] double: a broker that may be absent, a root that
/// may be unknown, registration that may be refused, and a publishing session
/// that records what it is handed and answers as scripted.
pub struct FakeNotifications {
    /// Whether the hub holds a broker.
    pub broker: Arc<std::sync::atomic::AtomicBool>,
    /// The root tenant's id, once known.
    pub root: Mutex<Option<Uuid>>,
    /// Whether the activation types are registered.
    pub registered: std::sync::atomic::AtomicBool,
    /// Whether the next registration succeeds.
    pub registration_succeeds: std::sync::atomic::AtomicBool,
    /// How many registrations were attempted.
    pub registrations: AtomicUsize,
    /// The events the session was handed, in order.
    pub published: Arc<Mutex<Vec<EventV1>>>,
    /// What the next publishes do; once empty, they are accepted.
    pub script: Arc<Mutex<Vec<PublishScript>>>,
    /// The gate a [`PublishScript::Hold`] publish waits at.
    pub gate: Arc<Gate>,
    /// The broker leaves the hub when a publish is refused.
    pub broker_leaves_on_refusal: Arc<std::sync::atomic::AtomicBool>,
}

impl FakeNotifications {
    /// A ready gateway: a broker, a known root, registered types.
    #[must_use]
    pub fn ready() -> Arc<Self> {
        Arc::new(Self {
            broker: Arc::new(true.into()),
            root: Mutex::new(Some(Uuid::from_u128(0xa0))),
            registered: true.into(),
            registration_succeeds: true.into(),
            registrations: AtomicUsize::new(0),
            published: Arc::default(),
            script: Arc::default(),
            gate: Arc::default(),
            broker_leaves_on_refusal: Arc::new(false.into()),
        })
    }

    /// The events published so far.
    #[must_use]
    pub fn events(&self) -> Vec<EventV1> {
        self.published.lock().expect("published").clone()
    }
}

struct FakeSession {
    published: Arc<Mutex<Vec<EventV1>>>,
    script: Arc<Mutex<Vec<PublishScript>>>,
    gate: Arc<Gate>,
    broker: Arc<std::sync::atomic::AtomicBool>,
    leaves_on_refusal: Arc<std::sync::atomic::AtomicBool>,
}

#[async_trait]
impl BroadcastSink for FakeSession {
    async fn publish(&self, event: EventV1) -> Result<(), PublishError> {
        let next = {
            let mut script = self.script.lock().expect("script");
            if script.is_empty() {
                PublishScript::Accept
            } else {
                script.remove(0)
            }
        };
        match next {
            PublishScript::Accept => {
                self.published.lock().expect("published").push(event);
                Ok(())
            }
            PublishScript::Refuse => {
                if self.leaves_on_refusal.load(Ordering::SeqCst) {
                    self.broker.store(false, Ordering::SeqCst);
                }
                Err(PublishError::Refused("no produce grant".to_owned()))
            }
            PublishScript::TimeOut => Err(PublishError::TimedOut),
            PublishScript::Hold => {
                self.gate.arrived.notify_one();
                self.gate.open.notified().await;
                self.published.lock().expect("published").push(event);
                Ok(())
            }
        }
    }
}

#[async_trait]
impl NotificationGateway for FakeNotifications {
    fn broker_present(&self) -> bool {
        self.broker.load(Ordering::SeqCst)
    }

    fn root_tenant(&self) -> Option<Uuid> {
        *self.root.lock().expect("root")
    }

    fn registered(&self) -> bool {
        self.registered.load(Ordering::SeqCst)
    }

    async fn register(&self, _cancel: &CancellationToken) -> bool {
        self.registrations.fetch_add(1, Ordering::SeqCst);
        let ok = self.registration_succeeds.load(Ordering::SeqCst);
        if ok {
            self.registered.store(true, Ordering::SeqCst);
        }
        ok
    }

    fn session(&self) -> Option<Box<dyn BroadcastSink>> {
        self.broker_present().then(|| {
            Box::new(FakeSession {
                published: Arc::clone(&self.published),
                script: Arc::clone(&self.script),
                gate: Arc::clone(&self.gate),
                broker: Arc::clone(&self.broker),
                leaves_on_refusal: Arc::clone(&self.broker_leaves_on_refusal),
            }) as Box<dyn BroadcastSink>
        })
    }

    fn notifications(
        &self,
        pair: &SubscriberTenant,
        changes: &[NotifiedChange],
        trace_parent: Option<String>,
    ) -> Vec<BuiltNotification> {
        build_notifications(pair, changes, trace_parent.as_deref())
    }
}

/// A scan lock another replica may hold.
#[derive(Default)]
pub struct FakeScanLock {
    /// Whether another replica holds it.
    pub held_elsewhere: std::sync::atomic::AtomicBool,
    /// Whether the backend cannot be reached.
    pub broken: std::sync::atomic::AtomicBool,
    /// Attempts made.
    pub attempts: AtomicUsize,
    /// Locks currently held by the loop.
    pub held: Arc<AtomicUsize>,
    /// Releases awaited.
    pub released: Arc<AtomicUsize>,
}

struct HeldScan {
    held: Arc<AtomicUsize>,
    released: Arc<AtomicUsize>,
}

#[async_trait]
impl HeldKey for HeldScan {
    async fn release(self: Box<Self>) {
        self.held.fetch_sub(1, Ordering::SeqCst);
        self.released.fetch_add(1, Ordering::SeqCst);
    }
}

#[async_trait]
impl ScanLock for FakeScanLock {
    async fn try_acquire(&self) -> Result<Option<Box<dyn HeldKey>>, DomainError> {
        self.attempts.fetch_add(1, Ordering::SeqCst);
        if self.broken.load(Ordering::SeqCst) {
            return Err(DomainError::Unavailable {
                detail: "the lock service is down".to_owned(),
            });
        }
        if self.held_elsewhere.load(Ordering::SeqCst) {
            return Ok(None);
        }
        self.held.fetch_add(1, Ordering::SeqCst);
        Ok(Some(Box::new(HeldScan {
            held: Arc::clone(&self.held),
            released: Arc::clone(&self.released),
        })))
    }
}

// ---------------------------------------------------------------------------
// The notifier rig, shared by the notifier's tests and the hook's
// ---------------------------------------------------------------------------

pub const INTERVAL: Duration = Duration::from_secs(30);
pub const T1: &str = "00000000-0000-0000-0000-0000000000c1";
pub const T2: &str = "00000000-0000-0000-0000-0000000000c2";
pub const KEY_A: &str = "gts.cf.core.settings.setting_type.v1~acme.net.alpha.v1~";
pub const KEY_B: &str = "gts.cf.core.settings.setting_type.v1~acme.net.beta.v1~";

/// The notifier over the in-memory store and the fake gateway.
pub struct Rig {
    pub repo: Arc<InMemoryNotificationRepo>,
    pub gateway: Arc<FakeNotifications>,
    pub scan: Arc<FakeScanLock>,
    pub metrics: Arc<RecordingNotifyMetrics>,
    pub notifier: Arc<ActivationNotifier>,
}

/// The notifier at the default report timeout: `K` = 20 at [`INTERVAL`].
pub fn rig() -> Rig {
    rig_with_timeout(Duration::from_secs(super::DEFAULT_REPORT_TIMEOUT_SECONDS))
}

/// The notifier with this report timeout, `K` = `ceil(timeout / INTERVAL)`.
pub fn rig_with_timeout(report_timeout: Duration) -> Rig {
    let repo = InMemoryNotificationRepo::new();
    let gateway = FakeNotifications::ready();
    let scan = Arc::new(FakeScanLock::default());
    let metrics = Arc::new(RecordingNotifyMetrics::default());
    let notifier = Arc::new(
        ActivationNotifier::new(
            Arc::clone(&repo) as Arc<dyn NotificationStore>,
            Arc::clone(&gateway) as Arc<dyn NotificationGateway>,
            Arc::clone(&scan) as Arc<dyn ScanLock>,
            Arc::clone(&metrics) as Arc<dyn NotifyMetrics>,
            INTERVAL,
        )
        .with_report_timeout(report_timeout),
    );
    Rig {
        repo,
        gateway,
        scan,
        metrics,
        notifier,
    }
}

pub fn change_ref(n: u128) -> Uuid {
    Uuid::from_u128(n)
}

pub fn pair(subscriber: &str, tenant: &str) -> SubscriberTenant {
    SubscriberTenant {
        subscriber: subscriber.to_owned(),
        tenant: tenant.to_owned(),
    }
}

/// The keys and `change_ref`s one event's body names.
pub fn body_changes(event: &event_broker_sdk::EventV1) -> Vec<(String, String)> {
    event.data().expect("a body")["changes"]
        .as_array()
        .expect("changes")
        .iter()
        .map(|c| {
            (
                c["key"].as_str().expect("key").to_owned(),
                c["change_ref"].as_str().expect("ref").to_owned(),
            )
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Log capture
//
// The capture is installed per thread and assertions over it assume one
// process per test (nextest, as the gear's `make` targets run); under plain
// `cargo test` another test's lines on a shared thread may flake them.
// ---------------------------------------------------------------------------

#[derive(Clone, Default)]
pub struct LogBuf(Arc<Mutex<Vec<u8>>>);

impl LogBuf {
    /// Everything captured so far.
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
    /// How often `needle` occurs in what was captured.
    pub fn count(&self, needle: &str) -> usize {
        self.text().matches(needle).count()
    }
}

impl std::io::Write for LogBuf {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogBuf {
    type Writer = Self;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// Captures this thread's log lines until the guard drops.
///
/// Assertions over the captured text assume **one process per test** (nextest,
/// as the gear's `make` targets run): a global subscriber another test in the
/// same process installed, or a thread a runtime reuses, may make them flake
/// under plain `cargo test`.
pub fn capture() -> (LogBuf, tracing::subscriber::DefaultGuard) {
    let buf = LogBuf::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(buf.clone())
        .with_ansi(false)
        .finish();
    (buf, tracing::subscriber::set_default(subscriber))
}

pub fn logged(buf: &LogBuf) -> String {
    String::from_utf8(buf.0.lock().unwrap().clone()).unwrap()
}

// ---------------------------------------------------------------------------
// Span capture
// ---------------------------------------------------------------------------

/// Exports every span this thread closes to memory until it drops, through
/// the `OTel` layer, with the W3C propagator installed so a carried
/// `traceparent` is continued. A current-thread runtime keeps a test's tasks on
/// the thread.
pub struct SpanCapture {
    exporter: opentelemetry_sdk::trace::InMemorySpanExporter,
    _provider: opentelemetry_sdk::trace::SdkTracerProvider,
    _guard: tracing::subscriber::DefaultGuard,
}

impl SpanCapture {
    pub fn start() -> Self {
        use opentelemetry::trace::TracerProvider as _;
        use tracing_subscriber::layer::SubscriberExt as _;

        opentelemetry::global::set_text_map_propagator(
            opentelemetry_sdk::propagation::TraceContextPropagator::new(),
        );
        let exporter = opentelemetry_sdk::trace::InMemorySpanExporter::default();
        let provider = opentelemetry_sdk::trace::SdkTracerProvider::builder()
            .with_simple_exporter(exporter.clone())
            .build();
        let subscriber = tracing_subscriber::registry()
            .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("span-capture")));
        Self {
            exporter,
            _provider: provider,
            _guard: tracing::subscriber::set_default(subscriber),
        }
    }

    /// The closed spans named `name`, in the order they closed.
    pub fn named(&self, name: &str) -> Vec<opentelemetry_sdk::trace::SpanData> {
        self.exporter
            .get_finished_spans()
            .expect("finished spans")
            .into_iter()
            .filter(|s| s.name == name)
            .collect()
    }

    /// The one closed span named `name`.
    pub fn one(&self, name: &str) -> opentelemetry_sdk::trace::SpanData {
        let mut spans = self.named(name);
        assert_eq!(spans.len(), 1, "one `{name}` span");
        spans.remove(0)
    }
}

/// A span as [`SpanLog`] saw it opened and recorded into.
#[derive(Debug, Clone)]
pub struct LoggedSpan {
    pub id: u64,
    pub name: &'static str,
    pub parent: Option<u64>,
    pub fields: Vec<(String, String)>,
}

/// Logs every span this thread opens — its parent and every field recorded
/// into it — whether or not it ever closes: a database driver may keep the
/// span of its last statement, and with it the span's ancestors, past the
/// test's end.
#[derive(Clone, Default)]
pub struct SpanLog(Arc<Mutex<Vec<LoggedSpan>>>);

impl SpanLog {
    pub fn start() -> (Self, tracing::subscriber::DefaultGuard) {
        use tracing_subscriber::layer::SubscriberExt as _;

        let log = Self::default();
        let subscriber = tracing_subscriber::registry().with(log.clone());
        (log, tracing::subscriber::set_default(subscriber))
    }

    /// The spans named `name`, in the order they opened.
    pub fn named(&self, name: &str) -> Vec<LoggedSpan> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .filter(|s| s.name == name)
            .cloned()
            .collect()
    }
}

struct Fields<'a>(&'a mut Vec<(String, String)>);

impl tracing::field::Visit for Fields<'_> {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.0.push((field.name().to_owned(), value.to_owned()));
    }
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.0.push((field.name().to_owned(), format!("{value:?}")));
    }
}

impl<S> tracing_subscriber::Layer<S> for SpanLog
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    fn on_new_span(
        &self,
        attrs: &tracing::span::Attributes<'_>,
        id: &tracing::span::Id,
        ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let parent = ctx
            .span(id)
            .and_then(|span| span.parent())
            .map(|parent| parent.id().into_u64());
        let mut fields = Vec::new();
        attrs.record(&mut Fields(&mut fields));
        self.0.lock().unwrap().push(LoggedSpan {
            id: id.into_u64(),
            name: attrs.metadata().name(),
            parent,
            fields,
        });
    }

    fn on_record(
        &self,
        id: &tracing::span::Id,
        values: &tracing::span::Record<'_>,
        _: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let mut spans = self.0.lock().unwrap();
        if let Some(span) = spans.iter_mut().rev().find(|s| s.id == id.into_u64()) {
            values.record(&mut Fields(&mut span.fields));
        }
    }
}

/// The attributes the gear itself put on `span` — the `OTel` layer's own
/// (source location, thread, timings) left out — by name, with their values.
pub fn gear_attributes(span: &opentelemetry_sdk::trace::SpanData) -> Vec<(String, String)> {
    const LAYER_PREFIXES: [&str; 2] = ["code.", "thread."];
    const LAYER_OWN: [&str; 4] = ["target", "level", "busy_ns", "idle_ns"];
    span.attributes
        .iter()
        .map(|kv| (kv.key.as_str().to_owned(), kv.value.as_str().into_owned()))
        .filter(|(name, _)| {
            !LAYER_OWN.contains(&name.as_str())
                && !LAYER_PREFIXES.iter().any(|own| name.starts_with(own))
        })
        .collect()
}

/// The identity of the one instance [`ONE_INSTANCE`] of `subscriber` for `key`
/// at `tenant`: what a test that runs one process per subscriber names.
#[must_use]
pub fn one(subscriber: &str, key: &str, tenant: &str) -> InstanceKey {
    InstanceKey {
        subscriber: subscriber.to_owned(),
        instance_id: ONE_INSTANCE,
        key: key.to_owned(),
        tenant: tenant.to_owned(),
    }
}
