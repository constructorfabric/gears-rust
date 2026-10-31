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

use event_broker_sdk::Event;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::{
    ActivationCounts, ActivationNotifier, ActivationStatus, BuiltNotification, DueRow,
    NotificationGateway, NotificationStore, NotifiedChange, NotifyMetrics, ScanLock,
    SubscriberTenant,
};
use crate::domain::broker_sink::{BroadcastSink, PublishError};
use crate::infra::broadcast::build_notifications;

/// A notify-metrics sink that remembers what it was told.
#[derive(Default)]
pub struct RecordingNotifyMetrics {
    /// `(event, elapsed)` of each publish-to-deliver observation.
    pub delivered: Mutex<Vec<(&'static str, Duration)>>,
    /// Each response latency observed.
    pub latencies: Mutex<Vec<Duration>>,
    /// How many redeliveries were counted.
    pub redeliveries: AtomicUsize,
    /// Each value the awaiting gauge was set to.
    pub awaiting: Mutex<Vec<u64>>,
    /// Each value the subscriptions gauge was set to.
    pub subscriptions: Mutex<Vec<u64>>,
    /// Each reaction outcome counted.
    pub reactions: Mutex<Vec<&'static str>>,
    /// Each report result counted.
    pub reports: Mutex<Vec<&'static str>>,
}

impl NotifyMetrics for RecordingNotifyMetrics {
    fn publish_to_deliver(&self, event: &'static str, elapsed: Duration) {
        self.delivered
            .lock()
            .expect("metrics")
            .push((event, elapsed));
    }

    fn response_latency(&self, elapsed: Duration) {
        self.latencies.lock().expect("metrics").push(elapsed);
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

    fn reaction(&self, outcome: &'static str) {
        self.reactions.lock().expect("metrics").push(outcome);
    }

    fn report(&self, result: &'static str) {
        self.reports.lock().expect("metrics").push(result);
    }
}

/// One activation state of the in-memory repository.
#[derive(Debug, Clone)]
pub struct MemoryRow {
    /// The pair it belongs to.
    pub pair: SubscriberTenant,
    /// The setting key.
    pub key: String,
    /// The change it expects.
    pub change_ref: Uuid,
    /// Where it stands.
    pub status: ActivationStatus,
    /// The fake time of its last publish.
    pub last_notified: Option<Duration>,
}

/// A [`NotificationStore`] over memory, on a fake clock.
#[derive(Default)]
pub struct InMemoryNotificationRepo {
    /// The states.
    pub rows: Mutex<Vec<MemoryRow>>,
    /// The registered `(subscriber, key, tenant)` triples.
    pub subscribed: Mutex<std::collections::HashSet<(String, String, String)>>,
    /// The fake clock.
    pub clock: Mutex<Duration>,
    /// While set, every read fails.
    pub failing: std::sync::atomic::AtomicBool,
    /// How many orphan-sweep statements ran.
    pub sweeps: AtomicUsize,
    /// How many due scans ran.
    pub scans: AtomicUsize,
}

impl InMemoryNotificationRepo {
    /// An empty repository.
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::default()
    }

    /// An `awaiting` state with its subscription, never published.
    pub fn add(&self, subscriber: &str, tenant: &str, key: &str, change_ref: Uuid) {
        self.subscribed.lock().expect("repo").insert((
            subscriber.to_owned(),
            key.to_owned(),
            tenant.to_owned(),
        ));
        self.rows.lock().expect("repo").push(MemoryRow {
            pair: SubscriberTenant {
                subscriber: subscriber.to_owned(),
                tenant: tenant.to_owned(),
            },
            key: key.to_owned(),
            change_ref,
            status: ActivationStatus::Awaiting,
            last_notified: None,
        });
    }

    /// Move the fake clock.
    pub fn advance(&self, by: Duration) {
        *self.clock.lock().expect("repo") += by;
    }

    /// The state of `key` for `subscriber` at `tenant`.
    #[must_use]
    pub fn row(&self, subscriber: &str, tenant: &str, key: &str) -> Option<MemoryRow> {
        self.rows
            .lock()
            .expect("repo")
            .iter()
            .find(|r| r.pair.subscriber == subscriber && r.pair.tenant == tenant && r.key == key)
            .cloned()
    }

    /// Apply a change that overwrites the state: a new `change_ref`, `awaiting`,
    /// the stamp cleared.
    pub fn overwrite(&self, subscriber: &str, tenant: &str, key: &str, change_ref: Uuid) {
        let mut rows = self.rows.lock().expect("repo");
        if let Some(row) = rows
            .iter_mut()
            .find(|r| r.pair.subscriber == subscriber && r.pair.tenant == tenant && r.key == key)
        {
            row.change_ref = change_ref;
            row.status = ActivationStatus::Awaiting;
            row.last_notified = None;
        }
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

#[async_trait]
impl NotificationStore for InMemoryNotificationRepo {
    async fn awaiting_of(
        &self,
        pair: &SubscriberTenant,
    ) -> Result<Vec<NotifiedChange>, DomainError> {
        self.check()?;
        let mut found: Vec<NotifiedChange> = self
            .rows
            .lock()
            .expect("repo")
            .iter()
            .filter(|r| r.pair == *pair && r.status == ActivationStatus::Awaiting)
            .map(|r| NotifiedChange {
                key: r.key.clone(),
                change_ref: r.change_ref,
            })
            .collect();
        found.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(found)
    }

    async fn stamp(
        &self,
        pair: &SubscriberTenant,
        changes: &[NotifiedChange],
    ) -> Result<u64, DomainError> {
        self.check()?;
        let now = *self.clock.lock().expect("repo");
        let mut stamped = 0;
        for row in self.rows.lock().expect("repo").iter_mut() {
            let named = changes
                .iter()
                .any(|c| c.key == row.key && c.change_ref == row.change_ref);
            if row.pair == *pair && named && row.status == ActivationStatus::Awaiting {
                row.last_notified = Some(now);
                stamped += 1;
            }
        }
        Ok(stamped)
    }

    async fn due(&self, interval: Duration) -> Result<Vec<DueRow>, DomainError> {
        self.check()?;
        self.scans.fetch_add(1, Ordering::SeqCst);
        let now = *self.clock.lock().expect("repo");
        let mut due: Vec<DueRow> = self
            .rows
            .lock()
            .expect("repo")
            .iter()
            .filter(|r| r.status == ActivationStatus::Awaiting)
            .filter(|r| {
                r.last_notified
                    .is_none_or(|at| now.saturating_sub(at) > interval)
            })
            .map(|r| DueRow {
                pair: r.pair.clone(),
                change: NotifiedChange {
                    key: r.key.clone(),
                    change_ref: r.change_ref,
                },
            })
            .collect();
        due.sort_by(|a, b| {
            (&a.pair.subscriber, &a.pair.tenant, &a.change.key).cmp(&(
                &b.pair.subscriber,
                &b.pair.tenant,
                &b.change.key,
            ))
        });
        Ok(due)
    }

    async fn sweep_orphans(&self) -> Result<u64, DomainError> {
        self.check()?;
        self.sweeps.fetch_add(1, Ordering::SeqCst);
        let subscribed = self.subscribed.lock().expect("repo").clone();
        let mut rows = self.rows.lock().expect("repo");
        let before = rows.len();
        rows.retain(|r| {
            subscribed.contains(&(
                r.pair.subscriber.clone(),
                r.key.clone(),
                r.pair.tenant.clone(),
            ))
        });
        Ok((before - rows.len()) as u64)
    }

    async fn counts(&self) -> Result<ActivationCounts, DomainError> {
        self.check()?;
        let awaiting = self
            .rows
            .lock()
            .expect("repo")
            .iter()
            .filter(|r| r.status == ActivationStatus::Awaiting)
            .count() as u64;
        Ok(ActivationCounts {
            subscriptions: self.subscribed.lock().expect("repo").len() as u64,
            awaiting,
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
    pub published: Arc<Mutex<Vec<Event>>>,
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
    pub fn events(&self) -> Vec<Event> {
        self.published.lock().expect("published").clone()
    }
}

struct FakeSession {
    published: Arc<Mutex<Vec<Event>>>,
    script: Arc<Mutex<Vec<PublishScript>>>,
    gate: Arc<Gate>,
    broker: Arc<std::sync::atomic::AtomicBool>,
    leaves_on_refusal: Arc<std::sync::atomic::AtomicBool>,
}

#[async_trait]
impl BroadcastSink for FakeSession {
    async fn publish(&self, event: Event) -> Result<(), PublishError> {
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

pub fn rig() -> Rig {
    let repo = InMemoryNotificationRepo::new();
    let gateway = FakeNotifications::ready();
    let scan = Arc::new(FakeScanLock::default());
    let metrics = Arc::new(RecordingNotifyMetrics::default());
    let notifier = Arc::new(ActivationNotifier::new(
        Arc::clone(&repo) as Arc<dyn NotificationStore>,
        Arc::clone(&gateway) as Arc<dyn NotificationGateway>,
        Arc::clone(&scan) as Arc<dyn ScanLock>,
        Arc::clone(&metrics) as Arc<dyn NotifyMetrics>,
        INTERVAL,
    ));
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
pub fn body_changes(event: &event_broker_sdk::Event) -> Vec<(String, String)> {
    event.data.as_ref().expect("a body")["changes"]
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
