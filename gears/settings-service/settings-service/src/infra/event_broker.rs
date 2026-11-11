//! The Event Broker client: the broker as a soft dependency.
//!
//! The broker is never in the gear's `deps` and init never waits on it. The
//! client looks it up in the `ClientHub` with an answer of "none" rather than a
//! failure, and on the first success registers the cache topic and the
//! `cache_invalidate` type and learns the root tenant. Everything the publish
//! hook needs is read back from that cached state without an await.

use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use event_broker_sdk::{Ack, EventBrokerApi, EventBrokerError, EventV1};
use serde_json::Value;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use toolkit::ClientHub;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;
use tracing::{debug, warn};
use types_registry_sdk::{RegisterResult, TypesRegistryClient};
use uuid::Uuid;

use crate::domain::activation::{
    BuiltNotification, NotificationGateway, NotifiedChange, SubscriberTenant,
};
use crate::domain::coherence::{
    BroadcastContext, BroadcastGateway, BroadcastSink, BroadcastState, PublishError, TenantKeys,
};
use crate::domain::platform_scope::PlatformScope;
use crate::infra::broadcast::{build_broadcasts, build_notifications};
use crate::infra::event_schemas::{
    activation_topic_instance, cache_invalidate_event_type_schema, cache_topic_instance,
    change_notification_event_type_schema,
};
use crate::infra::service_identity::service_context;

/// The first wait of every backoff in the broker flows.
const BACKOFF_BASE: Duration = Duration::from_secs(1);

/// The longest a backoff ceiling grows to.
const BACKOFF_CAP: Duration = Duration::from_secs(30);

/// How long the request path publishes nothing after a request's publish
/// failed or exceeded its own timeout. Short on purpose: during it even a recovered broker
/// gets no broadcast, and peers converge only within `cache_ttl_seconds`. Not
/// configurable.
pub(crate) const FAILURE_COOLING: Duration = Duration::from_secs(5);

/// What the client remembers of the broker across requests: a request whose
/// publish failed or exceeded its own timeout mutes the request path for
/// [`FAILURE_COOLING`], so a sick broker costs one request the publish budget
/// rather than every request. A request that merely ran out of its budget
/// mutes nothing.
#[derive(Default)]
struct FailureMemory {
    /// The end of the cooling window, once a publish failed.
    cooling_until: Mutex<Option<Instant>>,
}

impl FailureMemory {
    fn remember(&self, cooling: Duration) {
        let until = Instant::now() + cooling;
        let mut held = self
            .cooling_until
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *held = Some(held.map_or(until, |earlier| earlier.max(until)));
    }

    /// Why the request path publishes nothing now, if it does not.
    fn muted(&self) -> Option<&'static str> {
        let until = *self
            .cooling_until
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        until
            .is_some_and(|until| Instant::now() < until)
            .then_some("a publish failed or timed out moments ago")
    }
}

/// Whether a refusal says the broker itself is unwell, rather than that this
/// event or this identity is refused. A refusal of the event or of a grant
/// recurs for every request whatever the broker's health, and remembering it
/// would mute the cache broadcasts behind, say, a missing activation grant.
fn broker_unwell(error: &EventBrokerError) -> bool {
    !matches!(
        error,
        EventBrokerError::Unauthorized { .. }
            | EventBrokerError::InvalidProducerOptions { .. }
            | EventBrokerError::EventTypeNotDeclared { .. }
            | EventBrokerError::EventTypeUnknown { .. }
            | EventBrokerError::TypeNotInDeclaredTopic { .. }
            | EventBrokerError::SchemaNotPrepared { .. }
            | EventBrokerError::InvalidEventField { .. }
            | EventBrokerError::InvalidTextField { .. }
            | EventBrokerError::EventDataInvalid { .. }
            | EventBrokerError::TopicNotFound { .. }
            | EventBrokerError::BatchTooLarge { .. }
    )
}

/// Exponential backoff with equal jitter: the ceiling doubles from the base to
/// the cap, and each wait is drawn uniformly from the upper half of the
/// ceiling, `[ceiling / 2, ceiling]`. The floor is what keeps a broker that
/// ends every stream at once from being retried in a hot loop: the creation
/// rate decays to about one per cap. Not configurable; the schedule is
/// injectable so tests run in milliseconds.
#[derive(Debug, Clone)]
pub struct Backoff {
    base: Duration,
    cap: Duration,
    ceiling: Duration,
}

impl Backoff {
    /// The production schedule: 1 s doubling to a 30 s cap.
    #[must_use]
    pub const fn standard() -> Self {
        Self::with_steps(BACKOFF_BASE, BACKOFF_CAP)
    }

    /// A schedule from `base` doubling to `cap`; tests pass milliseconds.
    #[must_use]
    pub const fn with_steps(base: Duration, cap: Duration) -> Self {
        Self {
            base,
            cap,
            ceiling: base,
        }
    }

    /// The ceiling the next wait is drawn under.
    #[must_use]
    pub const fn ceiling(&self) -> Duration {
        self.ceiling
    }

    /// The next wait: uniform in `ceiling / 2 ..= ceiling`, after which the
    /// ceiling doubles up to the cap.
    #[allow(
        clippy::should_implement_trait,
        reason = "a schedule that never ends is not an Iterator; `next` is the natural name"
    )]
    pub fn next(&mut self) -> Duration {
        let ceiling = self.ceiling;
        self.ceiling = (ceiling * 2).min(self.cap);
        let floor = ceiling / 2;
        // A 64-bit draw scaled to the span above the floor by a shift:
        // `nanos * draw / 2^64`, which stays in `0..nanos`.
        let draw = u64::try_from(Uuid::new_v4().as_u128() >> 64).unwrap_or(0);
        let span = (ceiling.saturating_sub(floor).as_nanos() * u128::from(draw)) >> 64;
        floor + Duration::from_nanos(u64::try_from(span).unwrap_or(u64::MAX))
    }

    /// Start over from the base: once a stream has proved healthy (its first
    /// frame arrived, or it stayed open for the consumer's healthy period).
    /// A join that merely succeeded proves nothing.
    pub const fn reset(&mut self) {
        self.ceiling = self.base;
    }
}

/// Run `call` unless `cancel` fires first: `None` when it did. The awaited
/// calls of the background tasks go through this, so a broker, a registry or a
/// resolver that never answers cannot hold the task past the lifecycle's stop.
pub(crate) async fn or_cancelled<T>(
    cancel: &CancellationToken,
    call: impl Future<Output = T>,
) -> Option<T> {
    tokio::select! {
        biased;
        () = cancel.cancelled() => None,
        out = call => Some(out),
    }
}

/// The Types Registry as the client needs it: register a topic instance and an
/// event type schema — the cache topic and the `cache_invalidate` type, the
/// activation topic and the `change_notification` type — each as one document
/// with one per-item result.
#[async_trait]
pub trait CacheSchemaRegistry: Send + Sync {
    /// Register a topic instance.
    ///
    /// # Errors
    /// The registry's own, when the call itself fails.
    async fn register_topic(&self, topic: Value) -> Result<Vec<RegisterResult>, CanonicalError>;

    /// Register an event type schema.
    ///
    /// # Errors
    /// The registry's own, when the call itself fails.
    async fn register_event_type(
        &self,
        schema: Value,
    ) -> Result<Vec<RegisterResult>, CanonicalError>;
}

#[async_trait]
impl CacheSchemaRegistry for Arc<dyn TypesRegistryClient> {
    async fn register_topic(&self, topic: Value) -> Result<Vec<RegisterResult>, CanonicalError> {
        self.register_instances(vec![topic]).await
    }

    async fn register_event_type(
        &self,
        schema: Value,
    ) -> Result<Vec<RegisterResult>, CanonicalError> {
        self.register_type_schemas(vec![schema]).await
    }
}

/// The one result of a one-document registration, as success or a reason.
/// "Already registered" is success: registration is idempotent.
fn registered(
    what: &str,
    answer: Result<Vec<RegisterResult>, CanonicalError>,
) -> Result<(), String> {
    let results = answer.map_err(|e| format!("registering the {what} failed: {e}"))?;
    let [result] = <[RegisterResult; 1]>::try_from(results).map_err(|r| {
        format!(
            "the registry answered {} results for the one {what}",
            r.len()
        )
    })?;
    match result {
        RegisterResult::Ok { .. }
        | RegisterResult::Err {
            error: CanonicalError::AlreadyExists { .. },
            ..
        } => Ok(()),
        RegisterResult::Err { error, .. } => {
            Err(format!("the registry refused the {what}: {error}"))
        }
    }
}

/// The Event Broker as this gear uses it. See the module documentation.
pub struct EventBrokerClient {
    hub: Arc<ClientHub>,
    registry: Arc<dyn CacheSchemaRegistry>,
    scope: Arc<dyn PlatformScope>,
    publish_timeout: Duration,
    schedule: Backoff,
    group_alias: String,
    topic_registered: AtomicBool,
    type_registered: AtomicBool,
    activation_topic_registered: AtomicBool,
    activation_type_registered: AtomicBool,
    root: OnceLock<Uuid>,
    memory: Arc<FailureMemory>,
}

impl EventBrokerClient {
    /// A client over the hub the broker will be looked up in. Resolves nothing:
    /// init never waits on the broker.
    ///
    /// `instance_id` is the runtime's instance identity; it names this
    /// replica's consumer group.
    #[must_use]
    pub fn new(
        hub: Arc<ClientHub>,
        registry: Arc<dyn CacheSchemaRegistry>,
        scope: Arc<dyn PlatformScope>,
        publish_timeout: Duration,
        instance_id: Uuid,
    ) -> Self {
        Self {
            hub,
            registry,
            scope,
            publish_timeout,
            schedule: Backoff::standard(),
            group_alias: format!("settings-service:{instance_id}"),
            topic_registered: AtomicBool::new(false),
            type_registered: AtomicBool::new(false),
            activation_topic_registered: AtomicBool::new(false),
            activation_type_registered: AtomicBool::new(false),
            root: OnceLock::new(),
            memory: Arc::new(FailureMemory::default()),
        }
    }

    /// Replace the backoff schedule; for tests.
    #[must_use]
    pub const fn with_backoff(mut self, schedule: Backoff) -> Self {
        self.schedule = schedule;
        self
    }

    /// A fresh backoff on this client's schedule, for one waiting task.
    #[must_use]
    pub fn backoff(&self) -> Backoff {
        self.schedule.clone()
    }

    /// The broker, when the hub holds one now. A hub lookup, not a call.
    #[must_use]
    pub fn broker(&self) -> Option<Arc<dyn EventBrokerApi>> {
        self.hub.try_get::<dyn EventBrokerApi>()
    }

    /// The root tenant's id, once learned.
    #[must_use]
    pub fn root_tenant(&self) -> Option<Uuid> {
        self.root.get().copied()
    }

    /// What the publish hook reads when a request ends.
    #[must_use]
    pub fn state(&self) -> BroadcastState {
        BroadcastState {
            types_registered: self.topic_registered.load(Ordering::Acquire)
                && self.type_registered.load(Ordering::Acquire),
            root_tenant_known: self.root.get().is_some(),
        }
    }

    /// The publishing identity: this gear's service principal with the root
    /// tenant as subject tenant. `None` until the root tenant is known.
    // @cpt-dod:cpt-cf-settings-service-dod-replica-coherence-identity:p1
    #[must_use]
    pub fn publishing_context(&self) -> Option<SecurityContext> {
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-lifecycle:p1:inst-ca-lifecycle-16
        // Every publish of the activation feature — the end of a request and the
        // delivery loop — is made under this one identity, which holds the
        // `produce` grants of the activation topic, the `change_notification`
        // type and the activation subject type; a missing grant refuses each
        // publish, logged at `warn` by its caller, leaving the gear up.
        let root = self.root_tenant()?;
        service_context(root).ok()
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-lifecycle:p1:inst-ca-lifecycle-16
    }

    /// The alias of this replica's anonymous consumer group:
    /// `settings-service:<instance id>`.
    #[must_use]
    pub fn group_alias(&self) -> String {
        self.group_alias.clone()
    }

    /// One resolution step. When the hub holds a broker: learn the root tenant
    /// (once) and register the cache topic and the `cache_invalidate` type
    /// (once each). `true` when the broker is present and both are done; any
    /// failure is logged and answered `false`, and the caller waits and calls
    /// again. Each call into the Tenant Resolver or the Types Registry is cut
    /// short by `cancel`, answered `false` as well.
    pub async fn resolve_once(&self, cancel: &CancellationToken) -> bool {
        if self.broker().is_none() {
            return false;
        }
        let root = self.resolve_root(cancel).await;
        let registered = self.register_once(cancel).await;
        root && registered
    }

    async fn resolve_root(&self, cancel: &CancellationToken) -> bool {
        // @cpt-begin:cpt-cf-settings-service-flow-replica-coherence-lifecycle:p1:inst-rc-lifecycle-4
        if self.root.get().is_some() {
            return true;
        }
        let Some(answer) = or_cancelled(cancel, self.scope.root_tenant()).await else {
            debug!("root tenant lookup cancelled");
            return false;
        };
        match answer {
            Ok(root) => {
                self.root.get_or_init(|| root);
                true
            }
            Err(error) => {
                warn!(%error, "root tenant not yet known; cache broadcast waits");
                false
            }
        }
        // @cpt-end:cpt-cf-settings-service-flow-replica-coherence-lifecycle:p1:inst-rc-lifecycle-4
    }

    async fn register_once(&self, cancel: &CancellationToken) -> bool {
        // @cpt-begin:cpt-cf-settings-service-flow-replica-coherence-lifecycle:p1:inst-rc-lifecycle-5
        self.register_topic_once(cancel).await && self.register_type_once(cancel).await
        // @cpt-end:cpt-cf-settings-service-flow-replica-coherence-lifecycle:p1:inst-rc-lifecycle-5
    }

    /// Register one document unless `registered` already says it is: the
    /// registry call is cut short by `cancel`, a refusal is logged at `warn` and
    /// answered `false`.
    async fn register_document(
        cancel: &CancellationToken,
        registered_flag: &AtomicBool,
        what: &'static str,
        call: impl Future<Output = Result<Vec<RegisterResult>, CanonicalError>>,
    ) -> bool {
        if registered_flag.load(Ordering::Acquire) {
            return true;
        }
        let Some(answer) = or_cancelled(cancel, call).await else {
            debug!(what, "registration cancelled");
            return false;
        };
        match registered(what, answer) {
            Ok(()) => {
                registered_flag.store(true, Ordering::Release);
                true
            }
            Err(reason) => {
                warn!(%reason, what, "not registered; retrying");
                false
            }
        }
    }

    async fn register_topic_once(&self, cancel: &CancellationToken) -> bool {
        Self::register_document(
            cancel,
            &self.topic_registered,
            "cache topic",
            self.registry.register_topic(cache_topic_instance()),
        )
        .await
    }

    async fn register_type_once(&self, cancel: &CancellationToken) -> bool {
        Self::register_document(
            cancel,
            &self.type_registered,
            "cache_invalidate type",
            self.registry
                .register_event_type(cache_invalidate_event_type_schema()),
        )
        .await
    }

    /// Whether the activation topic and the `change_notification` type are
    /// registered: until they are, no notification is published and no scan
    /// runs.
    #[must_use]
    pub fn notifications_registered(&self) -> bool {
        self.activation_topic_registered.load(Ordering::Acquire)
            && self.activation_type_registered.load(Ordering::Acquire)
    }

    /// One registration step of the activation half, run by the delivery loop
    /// on its tick once the broker resolves: the activation topic, then the
    /// `change_notification` type as a derived event schema that names the
    /// activation subject type and keeps the default partition key, each once
    /// and idempotently. `true` when both are registered; a refusal is logged
    /// at `warn` and answered `false`, and the next tick registers again.
    pub async fn register_notifications_once(&self, cancel: &CancellationToken) -> bool {
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-lifecycle:p1:inst-ca-lifecycle-11
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-lifecycle:p1:inst-ca-lifecycle-12
        let topic = Self::register_document(
            cancel,
            &self.activation_topic_registered,
            "activation topic",
            self.registry.register_topic(activation_topic_instance()),
        )
        .await;
        let registered = topic
            && Self::register_document(
                cancel,
                &self.activation_type_registered,
                "change_notification type",
                self.registry
                    .register_event_type(change_notification_event_type_schema()),
            )
            .await;
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-lifecycle:p1:inst-ca-lifecycle-12
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-lifecycle:p1:inst-ca-lifecycle-11
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-lifecycle:p1:inst-ca-lifecycle-19
        // Recorded in the flags above: the publish and the scan stop skipping
        // for that reason. A publish the broker refuses within its
        // specification-refresh interval after the first registration is
        // retried like any refusal, at the next due tick.
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-lifecycle:p1:inst-ca-lifecycle-19
        registered
    }

    /// One wait of a background task: the next backoff delay, cut short by
    /// `cancel`. `false` when the token fired.
    pub async fn wait_cycle(backoff: &mut Backoff, cancel: &CancellationToken) -> bool {
        tokio::select! {
            biased;
            () = cancel.cancelled() => false,
            () = tokio::time::sleep(backoff.next()) => true,
        }
    }

    /// Resolve until ready or cancelled: repeat [`Self::resolve_once`], waiting
    /// a backoff between attempts. `true` when ready, `false` when cancelled.
    pub async fn resolve_until_ready(&self, cancel: &CancellationToken) -> bool {
        let mut backoff = self.backoff();
        while !cancel.is_cancelled() {
            if self.resolve_once(cancel).await {
                return true;
            }
            if !Self::wait_cycle(&mut backoff, cancel).await {
                break;
            }
        }
        false
    }

    /// The broker and the publishing context, taken together: what one
    /// request's publishes are made through, the cache broadcasts and the
    /// notifications that follow them alike.
    ///
    /// The session carries the request's publish budget: one
    /// `publish_timeout_seconds` for the whole publish phase, from now. Once
    /// it is spent the session is exhausted and the request's remaining
    /// publishes are skipped. A publish of the session that the broker fails
    /// or that exceeds its own timeout mutes the request path for
    /// [`FAILURE_COOLING`]; one cut by the end of the budget after the broker
    /// accepted an earlier publish of the session mutes nothing and is skipped
    /// — the request ran out of time, the broker did not fail.
    ///
    /// `None` while no broker is in the hub, the root tenant is unknown, or the
    /// cooling window of a recent failure runs: the request's publishes are
    /// then counted `skipped`.
    #[must_use]
    pub fn session(&self) -> Option<PublishSession> {
        if let Some(why) = self.memory.muted() {
            debug!(why, "the request publishes nothing");
            return None;
        }
        let mut session = self.delivery_session()?;
        session.deadline = Some(Instant::now() + self.publish_timeout);
        session.memory = Some(Arc::clone(&self.memory));
        Some(session)
    }

    /// A session for the delivery loop: each publish bounded by
    /// `publish_timeout_seconds` on its own, no budget across publishes, and
    /// neither muted by nor feeding the request path's failure memory — the
    /// loop's waits hold no request, and it retries at its next tick.
    fn delivery_session(&self) -> Option<PublishSession> {
        Some(PublishSession {
            broker: self.broker()?,
            ctx: self.publishing_context()?,
            timeout: self.publish_timeout,
            deadline: None,
            memory: None,
            accepted: AtomicBool::new(false),
        })
    }
}

/// The broker handle and the publishing context of one request, resolved once.
/// A broker that leaves the hub afterwards is a refusal on this handle, not a
/// missing broker.
pub struct PublishSession {
    broker: Arc<dyn EventBrokerApi>,
    ctx: SecurityContext,
    timeout: Duration,
    /// The end of the request's publish budget; `None` for the delivery loop.
    deadline: Option<Instant>,
    /// The request path's failure memory, fed by this session's failures;
    /// `None` for the delivery loop.
    memory: Option<Arc<FailureMemory>>,
    /// Whether the broker accepted a publish of this session: a publish cut
    /// by the budget after one is the request's running out of time, not the
    /// broker's failing.
    accepted: AtomicBool,
}

impl PublishSession {
    fn remember_failure(&self) {
        if let Some(memory) = &self.memory {
            memory.remember(FAILURE_COOLING);
        }
    }
}

#[async_trait]
impl BroadcastSink for PublishSession {
    async fn publish(&self, event: EventV1) -> Result<(), PublishError> {
        // One publish waits at most the timeout, and never past the request's
        // budget: a broker that accepts each publish just inside the timeout
        // still costs the request one timeout, not one per publish.
        let own = Instant::now() + self.timeout;
        let budget_cut = self.deadline.filter(|deadline| *deadline < own);
        let limit = budget_cut.unwrap_or(own);
        // `Ack::Accepted`: a broadcast or a notification is never waited on
        // until stored, so the answer is the broker's admission; the timeout
        // and the budget bound how long that answer may take.
        let publish = self.broker.publish(&self.ctx, &event, Ack::Accepted);
        match tokio::time::timeout_at(limit, publish).await {
            Ok(Ok(_)) => {
                self.accepted.store(true, Ordering::Relaxed);
                Ok(())
            }
            Ok(Err(error)) => {
                // The broker's error names the refusal, never the event body.
                // The outcome of the publish logs it once, at `warn`.
                if broker_unwell(&error) {
                    self.remember_failure();
                }
                let reason = error.to_string();
                debug!(%reason, "event refused by the broker");
                Err(PublishError::Refused(reason))
            }
            // Cut by the end of the request's budget, the broker having accepted
            // a publish of the session: the request ran out of time — slow
            // publishes, or its own reads between them — and the broker did not
            // fail. Nothing is remembered. Had it accepted nothing, the broker
            // let the request's whole budget pass without one acceptance: that
            // is its own timeout, remembered below.
            Err(_elapsed) if budget_cut.is_some() && self.accepted.load(Ordering::Relaxed) => {
                debug!("event cut by the end of the request's publish budget");
                Err(PublishError::BudgetSpent)
            }
            Err(_elapsed) => {
                self.remember_failure();
                debug!("event not accepted within the publish timeout");
                Err(PublishError::TimedOut)
            }
        }
    }

    fn exhausted(&self) -> bool {
        self.deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
    }
}

#[async_trait]
impl NotificationGateway for EventBrokerClient {
    fn broker_present(&self) -> bool {
        self.broker().is_some()
    }

    fn root_tenant(&self) -> Option<Uuid> {
        Self::root_tenant(self)
    }

    fn registered(&self) -> bool {
        self.notifications_registered()
    }

    async fn register(&self, cancel: &CancellationToken) -> bool {
        self.register_notifications_once(cancel).await
    }

    fn session(&self) -> Option<Box<dyn BroadcastSink>> {
        Self::delivery_session(self).map(|session| Box::new(session) as Box<dyn BroadcastSink>)
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

impl BroadcastGateway for EventBrokerClient {
    fn state(&self) -> BroadcastState {
        Self::state(self)
    }

    fn root_tenant(&self) -> Option<Uuid> {
        Self::root_tenant(self)
    }

    fn session(&self) -> Option<Box<dyn BroadcastSink>> {
        Self::session(self).map(|session| Box::new(session) as Box<dyn BroadcastSink>)
    }

    fn broadcasts(&self, touched: &[TenantKeys], ctx: &BroadcastContext) -> Vec<EventV1> {
        build_broadcasts(touched, ctx)
    }
}

#[cfg(test)]
#[path = "event_broker_tests.rs"]
mod event_broker_tests;
