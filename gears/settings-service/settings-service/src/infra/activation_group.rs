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
//! handle goes or on stop. The group is driven through the broker API rather
//! than the SDK's `ConsumerBuilder`, as the replica's cache consumer's is: the
//! builder never hands the group id back, so the gear could neither delete it
//! nor keep a refused join on the same group.
//!
//! A delivery is dropped silently when it names another subscriber — every
//! process with an interest in a tenant receives that tenant's notifications
//! for its other subscribers — and otherwise handled: each notified key a live
//! handle holds is re-read fresh and the handle's bundle is queued to the
//! handle's own task, where the handler gets it and its outcomes are reported
//! (`activation_client`). The group never waits for a handler, so a slow one
//! delays neither another handle nor the rejoin for a new interest. A repeat is
//! handled like the first and never suppressed.
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
    ConsumerGroupId, ControlCode, CreateConsumerGroupRequest, EventBrokerApi, EventBrokerError,
    FrameStream, GtsIdPattern, GtsInstanceId, JoinRequest, Position, SeekPosition, SubscriptionId,
    WireEvent, WireFrame, api::SubscriptionInterest,
};
use futures_util::StreamExt as _;
use settings_service_sdk::gts::{ACTIVATION_TOPIC_ID, CHANGE_NOTIFICATION_EVENT_TYPE};
use settings_service_sdk::{BundledKey, ChangeBundle, ChangeNotification, SettingKey};
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use toolkit_security::SecurityContext;
use tracing::{Instrument as _, debug, info_span, warn};
use uuid::Uuid;

use super::activation_client::ConsumerDeps;
use super::cache_invalidation::{LEAVE_SHARE, SESSION_TIMEOUT, bounded, prove};
use super::event_broker::{Backoff, EventBrokerClient, or_cancelled};
use crate::domain::activation::tenant_text;

/// The broker's limit on the interests of one subscription: a gear holds at
/// most this many tenant interests per process, and the client refuses one past
/// it before registering anything.
pub const MAX_TENANT_INTERESTS: usize = 64;

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
    /// Whether the handle is live: not dropped and not retired.
    fn is_live(&self) -> bool;
    /// The keys the handle holds: every key that is not `refused`.
    fn holds(&self) -> Vec<SettingKey>;
    /// Queue a bundle for the handle's own task, which hands it to the handler
    /// and reports the outcomes; returns at once, the order kept per handle.
    fn enqueue(&self, bundle: ChangeBundle);
    /// The broker grants the group (`true`), or refuses it a grant (`false`).
    fn broker_grant(&self, granted: bool);
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
        });
        group.add(tenant, ctx, reactor)?;
        let task = GroupTask {
            group: Arc::clone(&group),
            deps: Arc::clone(&self.deps),
            create_warned: Throttle::new(),
            join_warned: Throttle::new(),
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

    /// Stop: refuse further handles, tell every group to leave and delete itself
    /// and wait for them, within the cleanup deadline — a group still at it after
    /// that is abandoned and stays in the broker, as a crashed process's does.
    pub async fn stop(&self) {
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-retire:p1:inst-ca-retire-12
        self.stopped.store(true, Ordering::SeqCst);
        self.stop.cancel();
        self.groups().clear();
        let tasks: Vec<JoinHandle<()>> = std::mem::take(
            &mut *self
                .finishing
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        let deadline = self.deps.timing.cleanup_deadline;
        let joined = futures_util::future::join_all(tasks);
        if tokio::time::timeout(deadline, joined).await.is_err() {
            warn!(
                deadline_secs = deadline.as_secs(),
                "consumer groups not left within the cleanup deadline; they stay in the broker"
            );
        }
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-retire:p1:inst-ca-retire-12
    }
}

/// Why the task stopped holding a group.
enum Ended {
    Cancelled,
    Lost,
}

/// What ended one stream.
enum Taken {
    Ended(Ended),
    /// The broker closed the subscription with a terminal control frame: join
    /// again through the same group, after a backoff.
    Rejoin,
    /// A new tenant interest was added: join again at once with the full set.
    Reinterest,
}

/// Why a join did not yield a stream.
enum OpenFailed {
    /// The broker refused a step; `grant_missing` says it was for a grant.
    Refused { reason: String, grant_missing: bool },
    /// The stop fired while a step was in flight.
    Cancelled,
}

/// Whether the broker's refusal is a missing grant (`define` or `consume`).
fn grant_missing(error: &EventBrokerError) -> bool {
    matches!(error, EventBrokerError::Unauthorized { .. })
}

/// Await one broker call, cut short by the stop.
async fn step<T>(
    cancel: &CancellationToken,
    call: &'static str,
    work: impl std::future::Future<Output = Result<T, EventBrokerError>>,
) -> Result<T, OpenFailed> {
    match or_cancelled(cancel, work).await {
        None => {
            warn!(call, "stop interrupted an in-flight broker call");
            Err(OpenFailed::Cancelled)
        }
        Some(Ok(value)) => Ok(value),
        Some(Err(error)) => Err(OpenFailed::Refused {
            grant_missing: grant_missing(&error),
            reason: error.to_string(),
        }),
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

/// The group the task holds and the subscription it has on it, if any.
struct Held {
    broker: Arc<dyn EventBrokerApi>,
    group: ConsumerGroupId,
    subscription: Option<SubscriptionId>,
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

/// The background task of one group.
struct GroupTask {
    group: Arc<Group>,
    deps: Arc<ConsumerDeps>,
    /// The warning of a refused group creation.
    create_warned: Throttle,
    /// The warning of a refused join.
    join_warned: Throttle,
}

impl GroupTask {
    fn broker(&self) -> Option<Arc<dyn EventBrokerApi>> {
        self.deps.hub.try_get::<dyn EventBrokerApi>()
    }

    fn alias(&self) -> String {
        format!("settings-consumer:{}", self.group.subscriber)
    }

    /// Run until the group is cancelled — its last handle gone, or the stop —
    /// then leave and delete the group best-effort.
    async fn run(self) {
        let mut pause = self.deps.timing.backoff.clone();
        while !self.group.cancel.is_cancelled() && self.cycle(&mut pause).await {}
    }

    /// One wait on the backoff schedule; `false` when the stop fired.
    async fn pause(&self, pause: &mut Backoff) -> bool {
        EventBrokerClient::wait_cycle(pause, &self.group.cancel).await
    }

    /// One pass: resolve the broker, hold a group on it, release the group.
    /// `false` when the group was cancelled and the task is done.
    async fn cycle(&self, pause: &mut Backoff) -> bool {
        let Some(broker) = self.broker() else {
            // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-subscribe:p1:inst-ca-subscribe-25
            // @cpt-begin:cpt-cf-settings-service-algo-consumer-activation-subscribe-state:p1:inst-ca-substate-5
            // No broker resolvable: the registered keys stay `registered` and
            // the service's delivery loop catches up from the `awaiting`
            // states once the group joins.
            return self.pause(pause).await;
            // @cpt-end:cpt-cf-settings-service-algo-consumer-activation-subscribe-state:p1:inst-ca-substate-5
            // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-subscribe:p1:inst-ca-subscribe-25
        };
        let group = match self.create_group(&broker, pause).await {
            Ok(group) => group,
            Err(Ended::Cancelled) => return false,
            Err(Ended::Lost) => return self.pause(pause).await,
        };
        let mut held = Held {
            broker,
            group,
            subscription: None,
        };
        match self.hold(&mut held, pause).await {
            Ended::Cancelled => {
                self.release(&held, "stop").await;
                false
            }
            Ended::Lost => {
                warn!("event broker lost; the activation consumer group is paused");
                self.release(&held, "loss").await;
                self.pause(pause).await
            }
        }
    }

    /// Create the gear's anonymous group once, under the subscriber's identity,
    /// retrying a refusal with backoff until it holds or the broker is lost. A
    /// refusal for a missing `define` grant moves the handles' keys to `pending`.
    async fn create_group(
        &self,
        broker: &Arc<dyn EventBrokerApi>,
        pause: &mut Backoff,
    ) -> Result<ConsumerGroupId, Ended> {
        let cancel = &self.group.cancel;
        loop {
            let request = CreateConsumerGroupRequest {
                client_agent: self.alias(),
                description: Some(format!(
                    "settings activation consumer of `{}`",
                    self.group.subscriber
                )),
            };
            match step(
                cancel,
                "create_consumer_group",
                broker.create_consumer_group(&self.group.ctx(), request),
            )
            .await
            {
                Ok(group) => return Ok(group.id),
                Err(OpenFailed::Cancelled) => return Err(Ended::Cancelled),
                Err(OpenFailed::Refused {
                    reason,
                    grant_missing,
                }) => {
                    if grant_missing {
                        self.group.set_granted(false);
                    }
                    if self.create_warned.due(self.deps.timing.warn_every) {
                        warn!(%reason, grant_missing, "consumer group refused by the broker; retrying");
                    }
                    if !EventBrokerClient::wait_cycle(pause, cancel).await {
                        return Err(Ended::Cancelled);
                    }
                    if self.broker().is_none() {
                        return Err(Ended::Lost);
                    }
                }
            }
        }
    }

    /// Join through the held group and take deliveries; a refused join is
    /// retried against the same group.
    async fn hold(&self, held: &mut Held, pause: &mut Backoff) -> Ended {
        loop {
            match self.session(held, pause).await {
                Ok(Taken::Ended(ended)) => return ended,
                Ok(Taken::Reinterest) => {
                    debug!("a tenant interest was added; joining again");
                    continue;
                }
                Ok(Taken::Rejoin) => debug!("subscription closed by the broker; joining again"),
                Err(OpenFailed::Cancelled) => return Ended::Cancelled,
                Err(OpenFailed::Refused {
                    reason,
                    grant_missing,
                }) => self.join_refused(&reason, grant_missing),
            }
            if !self.pause(pause).await {
                return Ended::Cancelled;
            }
            if self.broker().is_none() {
                return Ended::Lost;
            }
        }
    }

    /// The broker refused the join.
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

    /// One join and the deliveries taken through it.
    async fn session(&self, held: &mut Held, pause: &mut Backoff) -> Result<Taken, OpenFailed> {
        let mut changes = self.group.interests.subscribe();
        let stream = self.open(held, &mut changes).await?;
        // The join succeeded: whatever grant was missing is there now.
        self.group.set_granted(true);
        Ok(self.take(stream, &mut changes, pause).await)
    }

    /// One join: an interest per tenant, the seek to the latest position, the
    /// stream.
    async fn open(
        &self,
        held: &mut Held,
        changes: &mut watch::Receiver<u64>,
    ) -> Result<FrameStream, OpenFailed> {
        let cancel = &self.group.cancel;
        // Best effort: the previous subscription may already be gone. It stays
        // on `held` until the leave settles, so a stop that cancels the leave
        // still finds it and `release` leaves it before the group is deleted.
        if let Some(previous) = held.subscription {
            match step(
                cancel,
                "leave",
                held.broker.leave(&self.group.ctx(), previous),
            )
            .await
            {
                Ok(()) => held.subscription = None,
                Err(OpenFailed::Cancelled) => return Err(OpenFailed::Cancelled),
                Err(OpenFailed::Refused { reason, .. }) => {
                    debug!(%reason, "previous subscription not left");
                    held.subscription = None;
                }
            }
        }
        // The set the join carries and the version it was read at, together: an
        // interest added after this point wakes the stream.
        let tenants = {
            let state = self.group.state();
            changes.borrow_and_update();
            state.tenants.clone()
        };
        let refused = |e: &dyn fmt::Display| OpenFailed::Refused {
            reason: e.to_string(),
            grant_missing: false,
        };
        let topic = GtsInstanceId::try_new(ACTIVATION_TOPIC_ID).map_err(|e| refused(&e))?;
        let pattern =
            GtsIdPattern::try_new(CHANGE_NOTIFICATION_EVENT_TYPE).map_err(|e| refused(&e))?;
        let interests = tenants
            .iter()
            .map(|tenant| {
                SubscriptionInterest::builder()
                    .topic(topic.clone())
                    .tenant_id(*tenant)
                    .types([pattern.clone()])
                    .build()
                    .map_err(|e| refused(&e))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let assignment = step(
            cancel,
            "join",
            held.broker.join(
                &self.group.ctx(),
                JoinRequest {
                    group: held.group,
                    client_agent: self.alias(),
                    interests,
                    session_timeout: Some(SESSION_TIMEOUT),
                },
            ),
        )
        .await?;
        held.subscription = Some(assignment.subscription_id);
        // Offsets live in memory and start from the latest position: what was
        // published before this join is the service's delivery loop's to catch up.
        let positions: Vec<SeekPosition> = assignment
            .assigned
            .iter()
            .map(|a| SeekPosition {
                topic: a.topic.clone(),
                partition: a.partition,
                value: Position::Latest,
            })
            .collect();
        step(
            cancel,
            "seek",
            held.broker.seek(
                &self.group.ctx(),
                assignment.subscription_id,
                assignment.topology_version,
                &positions,
            ),
        )
        .await?;
        step(
            cancel,
            "stream",
            held.broker
                .stream(&self.group.ctx(), assignment.subscription_id),
        )
        .await
    }

    /// Take deliveries until the group is cancelled, the broker is lost or an
    /// interest is added, looking the broker up again before every wait for the
    /// next frame.
    async fn take(
        &self,
        mut stream: FrameStream,
        changes: &mut watch::Receiver<u64>,
        pause: &mut Backoff,
    ) -> Taken {
        let cancel = &self.group.cancel;
        let timing = &self.deps.timing;
        let opened = Instant::now();
        let mut proven = false;
        loop {
            if self.broker().is_none() {
                return Taken::Ended(Ended::Lost);
            }
            tokio::select! {
                biased;
                () = cancel.cancelled() => return Taken::Ended(Ended::Cancelled),
                _ = changes.changed() => return Taken::Reinterest,
                () = tokio::time::sleep(timing.liveness) => {
                    if opened.elapsed() >= timing.healthy {
                        prove(pause, &mut proven);
                    }
                }
                frame = stream.next() => {
                    if let Some(taken) = self.on_frame(frame, pause, &mut proven).await {
                        return taken;
                    }
                }
            }
        }
    }

    /// What the stream yielded: its end or an error is the broker lost; a
    /// delivery is reacted to and, like any other non-terminal frame, proves the
    /// stream healthy; a terminal control frame ends the subscription and proves
    /// nothing.
    async fn on_frame(
        &self,
        frame: Option<Result<WireFrame, EventBrokerError>>,
        pause: &mut Backoff,
        proven: &mut bool,
    ) -> Option<Taken> {
        match frame {
            None => {
                warn!("delivery stream ended");
                Some(Taken::Ended(Ended::Lost))
            }
            Some(Err(error)) => {
                warn!(%error, "delivery stream failed");
                Some(Taken::Ended(Ended::Lost))
            }
            Some(Ok(WireFrame::Event(event))) => {
                prove(pause, proven);
                // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-react:p1:inst-ca-react-1
                // The broker delivered an event of the activation topic through
                // one of the group's tenant interests.
                let reacted = or_cancelled(&self.group.cancel, self.react(&event)).await;
                // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-react:p1:inst-ca-react-1
                reacted.is_none().then_some(Taken::Ended(Ended::Cancelled))
            }
            Some(Ok(WireFrame::Control {
                code: ControlCode::Terminal,
                ..
            })) => Some(Taken::Rejoin),
            Some(Ok(_)) => {
                prove(pause, proven);
                None
            }
        }
    }

    /// One delivery, inside a `change_notification.consume` span that continues
    /// the publisher's trace when the event carries a `trace_parent`.
    async fn react(&self, event: &WireEvent) {
        let span = info_span!(
            "change_notification.consume",
            event_id = %event.id,
            tenant = tracing::field::Empty,
        );
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-react:p1:inst-ca-react-14
        if let Some(traceparent) = event.trace_parent.as_deref()
            && let Ok(value) = http::HeaderValue::from_str(traceparent)
        {
            let mut headers = http::HeaderMap::new();
            headers.insert(toolkit_trace_context::TRACEPARENT, value);
            toolkit_trace_context::set_parent_from_headers(&span, &headers);
        }
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-react:p1:inst-ca-react-14
        self.dispatch(event, &span).instrument(span.clone()).await;
    }

    async fn dispatch(&self, event: &WireEvent, span: &tracing::Span) {
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-react:p1:inst-ca-react-2
        // At receipt, before the malformed and foreign-subscriber drops below,
        // from the envelope's `occurred_at`, so a dropped delivery is observed too.
        self.deps.metrics.publish_to_deliver(
            "change_notification",
            (chrono::Utc::now() - event.occurred_at)
                .to_std()
                .unwrap_or(Duration::ZERO),
        );
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-react:p1:inst-ca-react-2
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-react:p1:inst-ca-react-3
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-react:p1:inst-ca-react-5
        // Attributes of `data` this version does not know, and the optional
        // subject pair, absent in v1, are ignored by the tolerant reader.
        let parsed = (event.type_id.as_ref() == CHANGE_NOTIFICATION_EVENT_TYPE)
            .then(|| serde_json::from_value::<ChangeNotification>(event.data.clone()).ok())
            .flatten()
            .and_then(|n| Uuid::parse_str(&n.tenant).ok().map(|tenant| (n, tenant)));
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-react:p1:inst-ca-react-5
        let Some((notification, tenant)) = parsed else {
            warn!(event_id = %event.id, "a malformed change_notification was dropped");
            return;
        };
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-react:p1:inst-ca-react-3
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-react:p1:inst-ca-react-4
        // Every process with an interest in a tenant receives that tenant's
        // notifications for its other subscribers: dropped, without a log line.
        if notification.subscriber != self.group.subscriber {
            return;
        }
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-react:p1:inst-ca-react-4
        span.record("tenant", tracing::field::display(tenant));
        self.react_to(&notification, tenant).await;
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-react:p1:inst-ca-react-13
        // Completed: the group's offset advances in memory and nothing is
        // acknowledged to the broker.
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-react:p1:inst-ca-react-13
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

    /// Re-read each notified key a handle holds, fresh, one store read per key.
    async fn reread(&self, held: &[Holder], tenant: Uuid) -> Fresh {
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-react:p1:inst-ca-react-6
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-react:p1:inst-ca-react-7
        // A key the re-read fails for, or answers `Retired` or `NotFound`, is
        // left out and reports nothing: its state stays `awaiting`, delivered
        // again, until the next change of the key or a retire.
        let mut fresh = Fresh::new();
        for holder in held {
            for key in &holder.keys {
                if fresh.contains_key(key) {
                    continue;
                }
                match self
                    .deps
                    .read_fresh(&holder.reactor.ctx(), key, tenant)
                    .await
                {
                    Ok(value) => {
                        fresh.insert(key.clone(), value);
                    }
                    Err(error) => warn!(
                        key = %key,
                        kind = ?settings_service_sdk::SettingsError::from(error),
                        "fresh re-read failed; the key is left out of the bundle and reports nothing"
                    ),
                }
            }
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

    /// Leave the subscription and delete the group, once, best-effort, within
    /// the cleanup deadline. A failure is a warning and nothing else: the group
    /// stays in the broker, as a crashed process's does.
    async fn release(&self, held: &Held, why: &str) {
        let deadline = self.deps.timing.cleanup_deadline;
        let started = Instant::now();
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-retire:p1:inst-ca-retire-13
        if let Some(subscription) = held.subscription {
            bounded(
                deadline / LEAVE_SHARE,
                "leaving the activation topic failed",
                why,
                held.broker.leave(&self.group.ctx(), subscription),
            )
            .await;
        }
        bounded(
            deadline.saturating_sub(started.elapsed()),
            "consumer group not deleted; it stays in the broker",
            why,
            held.broker
                .delete_consumer_group(&self.group.ctx(), &held.group),
        )
        .await;
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-retire:p1:inst-ca-retire-13
    }
}
