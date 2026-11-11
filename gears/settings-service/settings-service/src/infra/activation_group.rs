// @cpt-dod:cpt-cf-settings-service-dod-consumer-activation-sdk:p1
// @cpt-dod:cpt-cf-settings-service-dod-consumer-activation-observability-sdk:p1
//! The subscribing gear's one anonymous consumer group per process on the
//! activation topic, and what a delivery does.
//!
//! One group per subscriber (the gear), shared by every handle the gear holds:
//! created once under the subscriber's identity (the broker's `define` grant),
//! joined with one interest per subscription tenant — the `change_notification`
//! type, `tenant_id` exactly that tenant, since the broker matches interests by
//! exact tenant — offsets in memory from the latest position, kept across
//! refused joins, and left and deleted best-effort when the gear's last live
//! handle goes or on stop. The group runs the session the replica's cache
//! consumer runs too (`broker_group`), driven through the broker API rather
//! than the SDK's `ConsumerBuilder`: the builder never hands the group id back,
//! so the gear could neither delete it nor keep a refused join on the same
//! group.
//!
//! A delivery is dropped silently when it names another subscriber — every
//! process with an interest in a tenant receives that tenant's notifications
//! for its other subscribers — and otherwise handled: each notified key a live
//! handle holds is re-read fresh and the handle's bundle is queued to the
//! handle's own task, where the handler gets it and its outcomes are reported
//! (`activation_client`). The group never waits for a handler, so a slow one
//! delays neither another handle nor the rejoin for a new interest. Nor does
//! the stream wait for the re-reads: a notification is handed to its tenant's
//! lane, which takes that tenant's notifications one at a time, in the order
//! they came, and re-reads a notification's keys at most
//! `REREAD_CONCURRENCY` at once. The re-reads of one notification end within
//! the client's fresh-read timeout (`ClientTiming::fresh_read`): a key not read
//! by then is left out like a failed read, and the service's delivery loop
//! delivers it again. So a tenant whose reads are slow or hang holds up neither
//! another tenant nor the stream's end, a new interest or the stop while fewer
//! than `LANE_DEPTH` of its notifications wait; with that many waiting, the
//! stream waits for room on the lane, at most one fresh-read timeout per
//! notification. A repeat is handled like the first and never suppressed.
//!
//! The broker is a soft dependency: with none, keys stay `registered` and the
//! group is created and joined once one resolves; a missing `define` or
//! `consume` grant moves the handles' registered keys to `pending` with their
//! subscriptions kept, until an operator grants it.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use event_broker_sdk::{
    EventBrokerApi, EventV1, GtsIdPattern, GtsInstanceId, api::SubscriptionInterest,
};
use futures_util::StreamExt as _;
use serde::Deserialize as _;
use settings_service_sdk::gts::{ACTIVATION_TOPIC_ID, CHANGE_NOTIFICATION_EVENT_TYPE};
use settings_service_sdk::{BundledKey, ChangeBundle, ChangeNotification, SettingKey};
use tokio::sync::{mpsc, watch};
use tokio::task::{JoinHandle, JoinSet};
use tokio_util::sync::CancellationToken;
use toolkit_security::SecurityContext;
use tracing::{Instrument as _, info_span, warn};
use uuid::Uuid;

use super::activation_client::ConsumerDeps;
use super::broker_group::{self, GroupSpec, GroupTiming};
use super::event_broker::{Backoff, or_cancelled};
use crate::domain::activation::tenant_text;

/// How many of the stop's unsubscribes run at once.
const DEREGISTER_CONCURRENCY: usize = 8;

/// How many fresh re-reads one notification has in flight at once. Each
/// tenant's lane holds one notification at a time, so a group reads at most
/// this many keys per tenant interest at once.
pub(crate) const REREAD_CONCURRENCY: usize = 16;

/// How many notifications of one tenant wait for that tenant's lane before the
/// stream task waits for room: the cap on what a tenant whose reads are slow
/// or hang holds queued, in each of at most [`MAX_TENANT_INTERESTS`] lanes. Below
/// it such a tenant holds up no other tenant and not the stream; at it, the
/// stream waits until the lane takes its next notification, which is at most
/// one fresh-read timeout away, since the re-reads of the one in flight end by
/// then. Nothing is dropped or merged to make room: each notification owes its
/// reports. A queued notification is a tenant, a subscriber and keys with their
/// change refs; filling the cap takes a burst of as many write requests at one
/// tenant while its reads are stalled.
pub(crate) const LANE_DEPTH: usize = 256;

/// The broker's limit on the interests of one subscription: a gear holds at
/// most this many tenant interests per process, and the client refuses one past
/// it before registering anything.
pub const MAX_TENANT_INTERESTS: usize = 64;

/// Make `span` continue the trace a W3C `traceparent` names; a value that is
/// absent or not a header value leaves it a root.
pub(crate) fn continue_trace(span: &tracing::Span, traceparent: Option<&str>) {
    if let Some(traceparent) = traceparent
        && let Ok(value) = http::HeaderValue::from_str(traceparent)
    {
        let mut headers = http::HeaderMap::new();
        headers.insert(toolkit_trace_context::TRACEPARENT, value);
        toolkit_trace_context::set_parent_from_headers(span, &headers);
    }
}

/// A live handle as the group reaches it. Implemented by the handle's core; the
/// group holds it weakly, so a handle the caller forgot cannot keep a group
/// alive.
#[async_trait]
pub trait Reactor: Send + Sync {
    /// The handle's id.
    fn id(&self) -> Uuid;
    /// The tenant the handle's keys are subscribed at.
    fn tenant(&self) -> Uuid;
    /// The identity the handle was subscribed under, which its reads are made
    /// with.
    fn ctx(&self) -> SecurityContext;
    /// Whether the handle is live: not dropped and not unsubscribed.
    fn is_live(&self) -> bool;
    /// The keys the handle holds: every key that is not `refused`.
    fn holds(&self) -> Vec<SettingKey>;
    /// Queue a bundle for the handle's own task, which hands it to the handler
    /// and reports the outcomes; returns at once, the order kept per handle.
    /// The group calls it inside the notification's
    /// `change_notification.consume` span, whose trace the handler and the
    /// reports continue.
    fn enqueue(&self, bundle: ChangeBundle);
    /// The broker grants the group (`true`), or refuses it a grant (`false`).
    fn broker_grant(&self, granted: bool);
    /// A report of `key` was answered `not_subscribed`: a handle that holds the
    /// key registers it again.
    fn not_registered(&self, key: &SettingKey);
}

/// Why a handle could not be attached to a group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachRefused {
    /// The process already holds [`MAX_TENANT_INTERESTS`] tenant interests for
    /// this gear and the tenant is not one of them.
    InterestLimit,
    /// The client is stopping.
    Stopped,
}

impl fmt::Display for AttachRefused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InterestLimit => write!(
                f,
                "the gear already holds {MAX_TENANT_INTERESTS} tenant interests in this process"
            ),
            Self::Stopped => f.write_str("the client is stopping"),
        }
    }
}

/// What the process has registered for one subscriber: the identity the
/// service calls were made under, and every key and tenant it holds a
/// subscription for, dropped handles' included — what the gear's `stop`
/// deletes.
struct Registrations {
    ctx: SecurityContext,
    keys: BTreeSet<(SettingKey, Uuid)>,
}

struct GroupState {
    tenants: BTreeSet<Uuid>,
    reactors: Vec<Weak<dyn Reactor>>,
}

/// One subscriber's group in this process.
pub struct Group {
    subscriber: String,
    /// The identity the broker calls are made under: the newest live handle's,
    /// refreshed on every attach, so a token that outlived its handle is not
    /// mistaken for a missing grant.
    ctx: Mutex<SecurityContext>,
    state: Mutex<GroupState>,
    /// Bumped, under the state's lock, whenever the interest set grows.
    interests: watch::Sender<u64>,
    cancel: CancellationToken,
    granted: AtomicBool,
    /// Serialises the unsubscribes of the gear's handles, so the check for
    /// another live holder of a key and the deletion that follows it are one
    /// step across handles.
    unsubscribing: Arc<tokio::sync::Mutex<()>>,
}

impl Group {
    fn state(&self) -> MutexGuard<'_, GroupState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The live reactors, dead and dropped ones forgotten.
    fn reactors(&self) -> Vec<Arc<dyn Reactor>> {
        let mut state = self.state();
        state
            .reactors
            .retain(|r| r.upgrade().is_some_and(|r| r.is_live()));
        state.reactors.iter().filter_map(Weak::upgrade).collect()
    }

    fn ctx(&self) -> SecurityContext {
        self.ctx
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn set_ctx(&self, ctx: SecurityContext) {
        *self
            .ctx
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = ctx;
    }

    /// The tenant interests the group holds.
    #[must_use]
    pub fn tenants(&self) -> BTreeSet<Uuid> {
        self.state().tenants.clone()
    }

    fn set_granted(&self, granted: bool) {
        // Stored under the state lock `add` reads it under, so a grant that lands
        // between `add`'s read and its `broker_grant(false)` is never overwritten.
        {
            let _state = self.state();
            self.granted.store(granted, Ordering::SeqCst);
        }
        for reactor in self.reactors() {
            reactor.broker_grant(granted);
        }
    }

    /// Add `tenant`'s interest unless the group holds it, refusing a 65th.
    fn add(
        &self,
        tenant: Uuid,
        ctx: &SecurityContext,
        reactor: &Weak<dyn Reactor>,
    ) -> Result<(), AttachRefused> {
        {
            let mut state = self.state();
            if !state.tenants.contains(&tenant) {
                if state.tenants.len() >= MAX_TENANT_INTERESTS {
                    return Err(AttachRefused::InterestLimit);
                }
                state.tenants.insert(tenant);
                self.interests.send_modify(|v| *v += 1);
            }
            state.reactors.push(Weak::clone(reactor));
        }
        self.set_ctx(ctx.clone());
        if let Some(reactor) = reactor.upgrade() {
            // Read under the state lock (see `set_granted`); the lock is not held
            // across the call. A grant landing after the read finds this reactor
            // already registered and reaches it through `set_granted`.
            let granted = {
                let _state = self.state();
                self.granted.load(Ordering::SeqCst)
            };
            if !granted {
                reactor.broker_grant(false);
            }
        }
        Ok(())
    }
}

/// The groups of the process: one per subscriber.
pub struct GroupRegistry {
    deps: Arc<ConsumerDeps>,
    groups: Mutex<HashMap<String, Arc<Group>>>,
    /// The process's own subscriptions, per subscriber.
    registrations: Mutex<HashMap<String, Registrations>>,
    /// The tasks of groups released but not yet finished leaving the broker.
    finishing: Mutex<Vec<JoinHandle<()>>>,
    stopped: AtomicBool,
    stop: CancellationToken,
}

impl GroupRegistry {
    /// An empty registry over `deps`.
    #[must_use]
    pub fn new(deps: Arc<ConsumerDeps>) -> Self {
        Self {
            deps,
            groups: Mutex::new(HashMap::new()),
            registrations: Mutex::new(HashMap::new()),
            finishing: Mutex::new(Vec::new()),
            stopped: AtomicBool::new(false),
            stop: CancellationToken::new(),
        }
    }

    fn groups(&self) -> MutexGuard<'_, HashMap<String, Arc<Group>>> {
        self.groups
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn registrations(&self) -> MutexGuard<'_, HashMap<String, Registrations>> {
        self.registrations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The process holds a subscription of `subscriber` to `key` at `tenant`
    /// now, registered under `ctx`. `false` when the stop has begun: the stop
    /// takes the registrations it finds once, so one noted after that is not
    /// among them, and the caller unsubscribes it itself.
    ///
    /// The stop flag is read under the registrations' lock, which the stop's
    /// drain takes after setting it: a registration is either noted before the
    /// drain, and deleted by the stop, or refused here.
    #[must_use]
    pub(crate) fn note_registered(
        &self,
        subscriber: &str,
        ctx: &SecurityContext,
        key: &SettingKey,
        tenant: Uuid,
    ) -> bool {
        let mut registrations = self.registrations();
        if self.stopped.load(Ordering::SeqCst) {
            return false;
        }
        let entry = registrations
            .entry(subscriber.to_owned())
            .or_insert_with(|| Registrations {
                ctx: ctx.clone(),
                keys: BTreeSet::new(),
            });
        entry.ctx = ctx.clone();
        entry.keys.insert((key.clone(), tenant));
        true
    }

    /// The service no longer holds the subscription: deleted by an unsubscribe,
    /// or forgotten.
    pub(crate) fn note_unsubscribed(&self, subscriber: &str, key: &SettingKey, tenant: Uuid) {
        if let Some(entry) = self.registrations().get_mut(subscriber) {
            entry.keys.remove(&(key.clone(), tenant));
        }
    }

    /// A report of `key` at `tenant` was answered `not_subscribed`: the service
    /// holds no subscription of this process for it. Every live handle of the
    /// subscriber that holds the key registers it again; a key no live handle
    /// holds — released by `unsubscribe()` or by `stop` — is not registered.
    pub(crate) fn not_subscribed(&self, subscriber: &str, tenant: Uuid, key: &SettingKey) {
        self.note_unsubscribed(subscriber, key, tenant);
        let group = self.groups().get(subscriber).map(Arc::clone);
        let Some(group) = group else {
            return;
        };
        for reactor in group.reactors() {
            if reactor.tenant() == tenant {
                reactor.not_registered(key);
            }
        }
    }

    /// Unsubscribe, one call per key and tenant, what each of `gears` registered,
    /// best-effort within the cleanup deadline: a call that fails or does not
    /// finish leaves its key registered, to follow the silent path.
    async fn deregister(&self, gears: Vec<(String, Registrations)>) {
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-unsubscribe:p1:inst-ca-unsubscribe-19
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-unsubscribe:p1:inst-ca-unsubscribe-20
        let total: usize = gears.iter().map(|(_, r)| r.keys.len()).sum();
        if total == 0 {
            return;
        }
        let instance_id = self.deps.identity.instance_id();
        let done = std::sync::atomic::AtomicUsize::new(0);
        let mut calls: Vec<(&str, &Registrations, &SettingKey, Uuid)> = Vec::with_capacity(total);
        for (subscriber, regs) in &gears {
            for (key, tenant) in &regs.keys {
                calls.push((subscriber.as_str(), regs, key, *tenant));
            }
        }
        let work = futures_util::stream::iter(calls).for_each_concurrent(DEREGISTER_CONCURRENCY, |(subscriber, regs, key, tenant)| {
            let done = &done;
            async move {
                // Serialised with the gear's handles' own unsubscribes, as they
                // are with each other.
                let _gear = self.unsubscribe_guard(subscriber).await;
                match self
                    .deps
                    .backend
                    .unsubscribe(&regs.ctx, key, tenant, instance_id)
                    .await
                {
                    Ok(()) | Err(super::activation_client::UnsubscribeFailure::NotFound) => {
                        done.fetch_add(1, Ordering::SeqCst);
                    }
                    Err(super::activation_client::UnsubscribeFailure::Unavailable) => {
                        warn!(key = %key, "a subscription was not deregistered on stop; it stays registered");
                    }
                }
            }
        });
        let deadline = self.deps.timing.cleanup_deadline;
        if tokio::time::timeout(deadline, work).await.is_err() {
            warn!(
                unfinished = total.saturating_sub(done.load(Ordering::SeqCst)),
                deadline_secs = deadline.as_secs(),
                "subscriptions not deregistered within the cleanup deadline; they stay registered"
            );
        }
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-unsubscribe:p1:inst-ca-unsubscribe-20
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-unsubscribe:p1:inst-ca-unsubscribe-19
    }

    /// The token the client's stop cancels: the parent of every handle's own, so
    /// a stop ends the handles' tasks along with the groups.
    pub(crate) fn stop_token(&self) -> &CancellationToken {
        &self.stop
    }

    /// How many groups the process holds now.
    #[must_use]
    pub fn group_count(&self) -> usize {
        self.groups().len()
    }

    /// The tenant interests `subscriber`'s group holds, if it has one.
    #[must_use]
    pub fn tenants_of(&self, subscriber: &str) -> Option<BTreeSet<Uuid>> {
        self.groups().get(subscriber).map(|g| g.tenants())
    }

    /// Attach a handle of `subscriber` at `tenant`: ensure the subscriber's one
    /// group, add the tenant's interest and register the handle with it.
    ///
    /// # Errors
    /// [`AttachRefused::InterestLimit`] for a tenant interest past the 64th, and
    /// then nothing is changed.
    pub fn attach(
        &self,
        subscriber: &str,
        ctx: &SecurityContext,
        tenant: Uuid,
        reactor: &Weak<dyn Reactor>,
    ) -> Result<(), AttachRefused> {
        if self.stopped.load(Ordering::SeqCst) {
            return Err(AttachRefused::Stopped);
        }
        let mut groups = self.groups();
        if let Some(group) = groups.get(subscriber) {
            return group.add(tenant, ctx, reactor);
        }
        let group = Arc::new(Group {
            subscriber: subscriber.to_owned(),
            ctx: Mutex::new(ctx.clone()),
            state: Mutex::new(GroupState {
                tenants: BTreeSet::new(),
                reactors: Vec::new(),
            }),
            interests: watch::channel(0).0,
            cancel: self.stop.child_token(),
            granted: AtomicBool::new(true),
            unsubscribing: Arc::new(tokio::sync::Mutex::new(())),
        });
        group.add(tenant, ctx, reactor)?;
        let task = GroupTask {
            group: Arc::clone(&group),
            deps: Arc::clone(&self.deps),
            create_warned: Throttle::new(),
            join_warned: Throttle::new(),
            reaction: Arc::new(Reaction {
                group: Arc::clone(&group),
                deps: Arc::clone(&self.deps),
            }),
            lanes: Mutex::new(Lanes::default()),
        };
        let handle = tokio::spawn(async move { task.run().await });
        {
            let mut finishing = self
                .finishing
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            // A group that finished leaving is not waited for again.
            finishing.retain(|task| !task.is_finished());
            finishing.push(handle);
        }
        groups.insert(subscriber.to_owned(), group);
        Ok(())
    }

    /// The guard that serialises `subscriber`'s unsubscribes in this process,
    /// held for the whole of one handle's `unsubscribe()` and taken before the
    /// handle's own lock. `None` when the subscriber has no group, and then no
    /// other handle of it is live.
    pub async fn unsubscribe_guard(
        &self,
        subscriber: &str,
    ) -> Option<tokio::sync::OwnedMutexGuard<()>> {
        let lock = self
            .groups()
            .get(subscriber)
            .map(|g| Arc::clone(&g.unsubscribing))?;
        Some(lock.lock_owned().await)
    }

    /// Whether a live handle of `subscriber` other than `handle`, at `tenant`,
    /// still holds `key`: a refused key is not held, a closed handle is not live.
    #[must_use]
    pub fn held_by_other(
        &self,
        subscriber: &str,
        handle: Uuid,
        tenant: Uuid,
        key: &SettingKey,
    ) -> bool {
        let group = self.groups().get(subscriber).map(Arc::clone);
        group.is_some_and(|group| {
            group
                .reactors()
                .iter()
                .any(|r| r.id() != handle && r.tenant() == tenant && r.holds().contains(key))
        })
    }

    /// A handle of `subscriber` is no longer live. When it was the gear's last,
    /// the group is dropped from the registry and told to leave and delete
    /// itself best-effort.
    pub fn release(&self, subscriber: &str, handle: Uuid) {
        let mut groups = self.groups();
        let Some(group) = groups.get(subscriber) else {
            return;
        };
        group
            .state()
            .reactors
            .retain(|r| r.upgrade().is_some_and(|r| r.id() != handle && r.is_live()));
        let live = group.reactors();
        if live.is_empty() {
            if let Some(group) = groups.remove(subscriber) {
                group.cancel.cancel();
            }
        } else if let Some(newest) = live.last() {
            // The identity of the handle the group was last refreshed from may
            // be the one that just went: the newest that remains is current.
            group.set_ctx(newest.ctx());
        }
    }

    /// Refuse further handles from now on; [`Self::stop`] does the rest.
    pub(crate) fn refuse_new(&self) {
        self.stopped.store(true, Ordering::SeqCst);
    }

    /// Stop: refuse further handles, tell every group to leave and delete itself
    /// and wait for them, within the cleanup deadline — a group still at it after
    /// that is abandoned and stays in the broker, as a crashed process's does.
    pub async fn stop(&self) {
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-unsubscribe:p1:inst-ca-unsubscribe-12
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-unsubscribe:p1:inst-ca-unsubscribe-21
        // A key released by `stop` is never registered again: the handles' tasks
        // end with the token before the unsubscribes below delete the rows, so
        // no walker re-registers what they delete. A walker already inside a
        // registration is not cut short; its registration, if it commits
        // after the drain below, is refused by `note_registered` and the
        // walker unsubscribes it itself.
        self.stopped.store(true, Ordering::SeqCst);
        self.stop.cancel();
        self.groups().clear();
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-unsubscribe:p1:inst-ca-unsubscribe-21
        let gears: Vec<(String, Registrations)> = self.registrations().drain().collect();
        let tasks: Vec<JoinHandle<()>> = std::mem::take(
            &mut *self
                .finishing
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        let deadline = self.deps.timing.cleanup_deadline;
        let joined = futures_util::future::join_all(tasks);
        let leave = async {
            if tokio::time::timeout(deadline, joined).await.is_err() {
                warn!(
                    deadline_secs = deadline.as_secs(),
                    "consumer groups not left within the cleanup deadline; they stay in the broker"
                );
            }
        };
        // The subscriptions and the groups go in parallel, each within the deadline.
        tokio::join!(self.deregister(gears), leave);
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-unsubscribe:p1:inst-ca-unsubscribe-12
    }
}

/// The keys a notification names, each with the change to report on.
type Notified = BTreeMap<SettingKey, Uuid>;

/// The fresh values of the keys that could be re-read.
type Fresh = BTreeMap<SettingKey, settings_service_sdk::models::EffectiveValueResponse>;

/// A live handle and the notified keys it holds.
struct Holder {
    reactor: Arc<dyn Reactor>,
    keys: Vec<SettingKey>,
}

/// A warning allowed once each `every`: a refusal that repeats on the backoff
/// schedule is logged at the same cadence as a `pending` key, not on every try.
struct Throttle {
    last: Mutex<Option<Instant>>,
}

impl Throttle {
    const fn new() -> Self {
        Self {
            last: Mutex::new(None),
        }
    }

    /// Whether a warning is due now, and if so the clock restarts.
    fn due(&self, every: Duration) -> bool {
        let mut last = self
            .last
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let now = Instant::now();
        if last.is_some_and(|at| now.duration_since(at) < every) {
            return false;
        }
        *last = Some(now);
        true
    }
}

/// One notification of this gear's, handed from the stream to its tenant's
/// lane with the `change_notification.consume` span it was received in.
struct Job {
    notification: ChangeNotification,
    tenant: Uuid,
    span: tracing::Span,
}

/// The group's lanes: one task per tenant that reacts to that tenant's
/// notifications one after another, so the stream task waits for a re-read
/// only when that tenant's lane is full ([`LANE_DEPTH`]). A tenant's handles
/// hear only that tenant's notifications, so one lane per tenant keeps every
/// handle's bundles in the order the notifications came; lanes of different
/// tenants run side by side. At most one per tenant interest, so at most
/// [`MAX_TENANT_INTERESTS`].
#[derive(Default)]
struct Lanes {
    queues: HashMap<Uuid, mpsc::Sender<Job>>,
    tasks: JoinSet<()>,
}

/// What reacting to a notification needs, shared by the group's lanes.
struct Reaction {
    group: Arc<Group>,
    deps: Arc<ConsumerDeps>,
}

/// The background task of one group.
struct GroupTask {
    group: Arc<Group>,
    deps: Arc<ConsumerDeps>,
    /// The warning of a refused group creation.
    create_warned: Throttle,
    /// The warning of a refused join.
    join_warned: Throttle,
    reaction: Arc<Reaction>,
    /// Kept across the group's streams: a reaction outlives the loss of the
    /// stream it came on, and ends with the group.
    lanes: Mutex<Lanes>,
}

impl GroupTask {
    fn lanes(&self) -> MutexGuard<'_, Lanes> {
        self.lanes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Hand `job` to its tenant's lane, started on the tenant's first
    /// notification and again if it ended. A full lane is waited for — the
    /// bounded queue is the back-pressure — until the lane takes its next
    /// notification, at most one fresh-read timeout, or the stop.
    async fn queue(&self, job: Job) {
        let lane = {
            let mut lanes = self.lanes();
            match lanes.queues.get(&job.tenant) {
                Some(lane) if !lane.is_closed() => lane.clone(),
                _ => {
                    // A lane that ended before is reaped, not left to accumulate.
                    while lanes.tasks.try_join_next().is_some() {}
                    let (lane, jobs) = mpsc::channel(LANE_DEPTH);
                    lanes.tasks.spawn(Reaction::lane(
                        Arc::clone(&self.reaction),
                        jobs,
                        self.group.cancel.clone(),
                    ));
                    lanes.queues.insert(job.tenant, lane.clone());
                    lane
                }
            }
        };
        if lane.send(job).await.is_err() {
            // Only a lane that panicked ends while the group runs.
            warn!("a change notification was dropped: its tenant's lane ended");
        }
    }

    /// Run until the group is cancelled — its last handle gone, or the stop —
    /// then leave and delete the group best-effort.
    async fn run(self) {
        broker_group::run(&self, &self.group.cancel).await;
        // The lanes saw the same cancellation; a re-read still in flight is
        // dropped here, not waited for.
        let mut lanes = std::mem::take(&mut *self.lanes());
        lanes.queues.clear();
        lanes.tasks.shutdown().await;
    }

    /// One delivery, inside a `change_notification.consume` span that continues
    /// the publisher's trace when the event carries a `trace_parent`.
    async fn react(&self, event: &EventV1) {
        let span = info_span!(
            "change_notification.consume",
            event_id = %event.id(),
            tenant = tracing::field::Empty,
        );
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-react:p1:inst-ca-react-14
        continue_trace(&span, event.trace_parent());
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-react:p1:inst-ca-react-14
        self.dispatch(event, &span).instrument(span.clone()).await;
    }

    async fn dispatch(&self, event: &EventV1, span: &tracing::Span) {
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-react:p1:inst-ca-react-2
        // At receipt, before the malformed and foreign-subscriber drops below,
        // from the envelope's `occurred_at`, so a dropped delivery is observed too.
        self.deps.metrics.publish_to_deliver(
            "change_notification",
            (chrono::Utc::now() - event.occurred_at())
                .to_std()
                .unwrap_or(Duration::ZERO),
        );
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-react:p1:inst-ca-react-2
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-react:p1:inst-ca-react-3
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-react:p1:inst-ca-react-5
        // Attributes of `data` this version does not know, and the optional
        // subject pair, absent in v1, are ignored by the tolerant reader.
        let parsed = (*event.event_type() == CHANGE_NOTIFICATION_EVENT_TYPE)
            .then(|| event.data())
            .flatten()
            .and_then(|data| ChangeNotification::deserialize(data).ok())
            .and_then(|n| Uuid::parse_str(&n.tenant).ok().map(|tenant| (n, tenant)));
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-react:p1:inst-ca-react-5
        let Some((notification, tenant)) = parsed else {
            warn!(event_id = %event.id(), "a malformed change_notification was dropped");
            return;
        };
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-react:p1:inst-ca-react-3
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-react:p1:inst-ca-react-4
        // Every process with an interest in a tenant receives that tenant's
        // notifications for its other subscribers: dropped, without a log line.
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-react:p1:inst-ca-react-16
        // Nothing here asks which instance's state the notification was built
        // from: a process that was left out as silent re-reads and reports all
        // the same, and its report ends its silence.
        if notification.subscriber != self.group.subscriber {
            return;
        }
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-react:p1:inst-ca-react-16
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-react:p1:inst-ca-react-4
        // The broker matches interests by exact tenant: a tenant the group holds
        // no interest for has no handle to hear it, and gets no lane.
        if !self.group.state().tenants.contains(&tenant) {
            return;
        }
        span.record("tenant", tracing::field::display(tenant));
        // The re-reads and the hand-over run on the tenant's lane, so the stream
        // goes on with the next frame — another tenant's notification, the
        // stream's end, a new interest, the stop — while they are in flight,
        // unless the lane is full: then it waits for room, at most one
        // fresh-read timeout.
        self.queue(Job {
            notification,
            tenant,
            span: span.clone(),
        })
        .await;
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-react:p1:inst-ca-react-13
        // Completed: the group's offset advances in memory and nothing is
        // acknowledged to the broker.
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-react:p1:inst-ca-react-13
    }
}

impl GroupSpec for GroupTask {
    /// The interest set's version, as the join read it.
    type Mark = watch::Receiver<u64>;
    /// Each delivery is handed to its tenant's lane as it comes.
    type Taking = ();
    /// One frame at a time: a delivery's hand-over waits for room on its lane,
    /// which is the back-pressure.
    const DRAIN_LIMIT: usize = 1;
    const LOST: &'static str = "event broker lost; the activation consumer group is paused";
    /// With no broker the keys stay `registered`: the group waits for one
    /// quietly, and warns only of a broker lost while it held a group.
    const WARN_UNHELD_LOSS: bool = false;
    const LEAVE_FAILED: &'static str = "leaving the activation topic failed";

    fn broker(&self) -> Option<Arc<dyn EventBrokerApi>> {
        self.deps.hub.try_get::<dyn EventBrokerApi>()
    }

    fn identity(&self) -> Option<SecurityContext> {
        Some(self.group.ctx())
    }

    /// The newest live handle's identity, read for every call: it is refreshed
    /// on every attach, so a token that outlived its handle is not mistaken for
    /// a missing grant.
    fn call_ctx(&self, _cycle: &SecurityContext) -> SecurityContext {
        self.group.ctx()
    }

    fn alias(&self) -> String {
        format!("settings-consumer:{}", self.group.subscriber)
    }

    fn description(&self) -> String {
        format!(
            "settings activation consumer of `{}`",
            self.group.subscriber
        )
    }

    fn timing(&self) -> GroupTiming {
        let timing = &self.deps.timing;
        GroupTiming {
            cleanup_deadline: timing.cleanup_deadline,
            liveness: timing.liveness,
            healthy: timing.healthy,
        }
    }

    fn backoff(&self) -> Backoff {
        self.deps.timing.backoff.clone()
    }

    /// A refusal for a missing `define` grant moves the handles' keys to
    /// `pending`.
    fn create_refused(&self, reason: &str, grant_missing: bool) {
        if grant_missing {
            self.group.set_granted(false);
        }
        if self.create_warned.due(self.deps.timing.warn_every) {
            warn!(%reason, grant_missing, "consumer group refused by the broker; retrying");
        }
    }

    fn join_refused(&self, reason: &str, grant_missing: bool) {
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-subscribe:p1:inst-ca-subscribe-26
        // A missing `define` or `consume` grant: the handles' keys are `pending`
        // with their subscriptions kept, and the join is retried on the same
        // group until an operator grants.
        if grant_missing {
            self.group.set_granted(false);
        }
        if self.join_warned.due(self.deps.timing.warn_every) {
            warn!(%reason, grant_missing, "activation topic join refused; retrying on the same group");
        }
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-subscribe:p1:inst-ca-subscribe-26
    }

    /// The join succeeded: whatever grant was missing is there now.
    fn joined(&self) {
        self.group.set_granted(true);
    }

    /// An interest per tenant the group holds, read with the set's version
    /// under the state's lock: an interest added after this wakes the stream.
    fn interests(&self) -> Result<(Vec<SubscriptionInterest>, watch::Receiver<u64>), String> {
        let (tenants, changes) = {
            let state = self.group.state();
            let mut changes = self.group.interests.subscribe();
            changes.borrow_and_update();
            (state.tenants.clone(), changes)
        };
        let topic = GtsInstanceId::try_new(ACTIVATION_TOPIC_ID).map_err(|e| e.to_string())?;
        let pattern =
            GtsIdPattern::try_new(CHANGE_NOTIFICATION_EVENT_TYPE).map_err(|e| e.to_string())?;
        let interests = tenants
            .iter()
            .map(|tenant| {
                SubscriptionInterest::builder()
                    .topic(topic.clone())
                    .tenant_id(*tenant)
                    .types([pattern.clone()])
                    .build()
                    .map_err(|e| e.to_string())
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok((interests, changes))
    }

    /// A tenant interest was added.
    async fn changed(mark: &mut watch::Receiver<u64>) {
        // The sender lives as long as the group: a closed channel is not
        // expected, and joins again all the same.
        mark.changed().await.unwrap_or_default();
    }

    /// Reacted to at once, cut short by the stop.
    async fn on_event(&self, event: &EventV1, (): &mut (), cancel: &CancellationToken) -> bool {
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-react:p1:inst-ca-react-1
        // The broker delivered an event of the activation topic through one of
        // the group's tenant interests.
        let reacted = or_cancelled(cancel, self.react(event)).await;
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-react:p1:inst-ca-react-1
        reacted.is_some()
    }

    fn complete(&self, (): ()) {}
}

impl Reaction {
    /// A tenant's lane: its notifications one at a time, in the order they
    /// came, until the group is cancelled; a reaction in flight then is dropped.
    async fn lane(self: Arc<Self>, mut jobs: mpsc::Receiver<Job>, cancel: CancellationToken) {
        while let Some(Some(job)) = or_cancelled(&cancel, jobs.recv()).await {
            let Job {
                notification,
                tenant,
                span,
            } = job;
            let reacted = or_cancelled(
                &cancel,
                self.react_to(&notification, tenant).instrument(span),
            )
            .await;
            if reacted.is_none() {
                return;
            }
        }
    }

    /// The reaction to a notification of this gear's: re-read, hand over, report.
    async fn react_to(&self, notification: &ChangeNotification, tenant: Uuid) {
        let notified: Notified = notification
            .changes
            .iter()
            .filter_map(|n| SettingKey::parse(&n.key).ok().map(|k| (k, n.change_ref)))
            .collect();
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-react:p1:inst-ca-react-8
        // A key no live handle for the tenant holds is not read and owes nothing
        // until a handle for it exists: it is in no handle's `held` below.
        let held: Vec<Holder> = self
            .group
            .reactors()
            .into_iter()
            .filter(|r| r.tenant() == tenant)
            .map(|reactor| {
                let keys = reactor
                    .holds()
                    .into_iter()
                    .filter(|k| notified.contains_key(k))
                    .collect();
                Holder { reactor, keys }
            })
            .collect();
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-react:p1:inst-ca-react-8
        let fresh = self.reread(&held, tenant).await;
        for holder in held {
            Self::hand_over(&holder, &notified, &fresh, tenant);
        }
    }

    /// Re-read each notified key a handle holds, fresh, one store read per key,
    /// under the identity of the first handle that holds it; at most
    /// [`REREAD_CONCURRENCY`] at once, the results keyed back by key, so the
    /// bundles keep each handle's key order. All of them end within one
    /// fresh-read timeout: a key not read by then is left out, as a failed
    /// read is, which bounds how long the lane holds one notification. A key
    /// left out stays due and comes again, while the keys reported settle and
    /// leave the next notification, so a large one under a slow store is not
    /// cut at the same keys for ever.
    async fn reread(&self, held: &[Holder], tenant: Uuid) -> Fresh {
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-react:p1:inst-ca-react-6
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-react:p1:inst-ca-react-7
        // A key the re-read fails for, or answers `Retired` or `NotFound`, is
        // left out and reports nothing: its state stays `awaiting`, delivered
        // again, until the next change of the key or an unsubscribe.
        let mut wanted: BTreeMap<SettingKey, SecurityContext> = BTreeMap::new();
        for holder in held {
            let ctx = holder.reactor.ctx();
            for key in &holder.keys {
                wanted.entry(key.clone()).or_insert_with(|| ctx.clone());
            }
        }
        let deadline = tokio::time::Instant::now() + self.deps.timing.fresh_read;
        let total = wanted.len();
        let mut answered = 0_usize;
        let mut reads = futures_util::stream::iter(wanted)
            .map(|(key, ctx)| async move {
                let read = self.deps.read_fresh(&ctx, &key, tenant).await;
                (key, read)
            })
            .buffer_unordered(REREAD_CONCURRENCY);
        let mut fresh = Fresh::new();
        let all = tokio::time::timeout_at(deadline, async {
            while let Some((key, read)) = reads.next().await {
                answered += 1;
                match read {
                    Ok(value) => {
                        fresh.insert(key, value);
                    }
                    Err(error) => warn!(
                        key = %key,
                        kind = ?settings_service_sdk::SettingsError::from(error),
                        "fresh re-read failed; the key is left out of the bundle and reports nothing"
                    ),
                }
            }
        })
        .await;
        if all.is_err() {
            // The reads still in flight, or never started, are dropped.
            warn!(
                unread = total.saturating_sub(answered),
                "fresh re-reads not done within the fresh-read timeout; those keys are left out of the bundle and report nothing"
            );
        }
        fresh
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-react:p1:inst-ca-react-7
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-react:p1:inst-ca-react-6
    }

    /// Queue one handle its bundle. The handle's own task hands it to the
    /// handler and reports the outcomes it returns: the group does not wait.
    fn hand_over(holder: &Holder, notified: &Notified, fresh: &Fresh, tenant: Uuid) {
        let changes: Vec<BundledKey> = holder
            .keys
            .iter()
            .filter_map(|key| {
                Some(BundledKey {
                    key: key.clone(),
                    change_ref: notified.get(key).copied(),
                    value: fresh.get(key)?.clone(),
                })
            })
            .collect();
        if changes.is_empty() {
            return;
        }
        holder.reactor.enqueue(ChangeBundle {
            tenant: tenant_text(tenant),
            changes,
        });
    }
}
