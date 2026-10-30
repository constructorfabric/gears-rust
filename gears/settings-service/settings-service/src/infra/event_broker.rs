//! The Event Broker client: the broker as a soft dependency.
//!
//! The broker is never in the gear's `deps` and init never waits on it. The
//! client looks it up in the `ClientHub` with an answer of "none" rather than a
//! failure, and on the first success registers the cache topic and the
//! `cache_invalidate` type and learns the root tenant. Everything the publish
//! hook needs is read back from that cached state without an await.

use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use event_broker_sdk::{Event, EventBrokerApi};
use serde_json::Value;
use tokio_util::sync::CancellationToken;
use toolkit::ClientHub;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;
use tracing::{debug, warn};
use types_registry_sdk::{RegisterResult, TypesRegistryClient};
use uuid::Uuid;

use crate::domain::coherence::{
    BroadcastContext, BroadcastGateway, BroadcastSink, BroadcastState, PublishError, TenantKeys,
};
use crate::domain::platform_scope::PlatformScope;
use crate::infra::broadcast::build_broadcasts;
use crate::infra::event_schemas::{cache_invalidate_event_type_schema, cache_topic_instance};
use crate::infra::service_identity::service_context;

/// The first wait of every backoff in the broker flows.
const BACKOFF_BASE: Duration = Duration::from_secs(1);

/// The longest a backoff ceiling grows to.
const BACKOFF_CAP: Duration = Duration::from_secs(30);

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

/// The Types Registry as the client needs it: register the cache topic and the
/// `cache_invalidate` type, each as one document with one per-item result.
#[async_trait]
pub trait CacheSchemaRegistry: Send + Sync {
    /// Register the cache topic instance.
    ///
    /// # Errors
    /// The registry's own, when the call itself fails.
    async fn register_topic(&self, topic: Value) -> Result<Vec<RegisterResult>, CanonicalError>;

    /// Register the `cache_invalidate` event type schema.
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
    root: OnceLock<Uuid>,
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
            root: OnceLock::new(),
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
        let root = self.root_tenant()?;
        service_context(root).ok()
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

    async fn register_topic_once(&self, cancel: &CancellationToken) -> bool {
        if self.topic_registered.load(Ordering::Acquire) {
            return true;
        }
        let Some(answer) =
            or_cancelled(cancel, self.registry.register_topic(cache_topic_instance())).await
        else {
            debug!("cache topic registration cancelled");
            return false;
        };
        match registered("cache topic", answer) {
            Ok(()) => {
                self.topic_registered.store(true, Ordering::Release);
                true
            }
            Err(reason) => {
                warn!(%reason, "cache topic not registered; retrying");
                false
            }
        }
    }

    async fn register_type_once(&self, cancel: &CancellationToken) -> bool {
        if self.type_registered.load(Ordering::Acquire) {
            return true;
        }
        let Some(answer) = or_cancelled(
            cancel,
            self.registry
                .register_event_type(cache_invalidate_event_type_schema()),
        )
        .await
        else {
            debug!("cache_invalidate type registration cancelled");
            return false;
        };
        match registered("cache_invalidate type", answer) {
            Ok(()) => {
                self.type_registered.store(true, Ordering::Release);
                true
            }
            Err(reason) => {
                warn!(%reason, "cache_invalidate type not registered; retrying");
                false
            }
        }
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
    /// request's publishes are made through. `None` while no broker is in the
    /// hub or the root tenant is unknown.
    #[must_use]
    pub fn session(&self) -> Option<PublishSession> {
        Some(PublishSession {
            broker: self.broker()?,
            ctx: self.publishing_context()?,
            timeout: self.publish_timeout,
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
}

#[async_trait]
impl BroadcastSink for PublishSession {
    async fn publish(&self, event: Event) -> Result<(), PublishError> {
        match tokio::time::timeout(self.timeout, self.broker.publish(&self.ctx, &event)).await {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(error)) => {
                // The broker's error names the refusal, never the event body.
                // The outcome of the publish logs it once, at `warn`.
                let reason = error.to_string();
                debug!(%reason, "cache_invalidate refused by the broker");
                Err(PublishError::Refused(reason))
            }
            Err(_elapsed) => {
                debug!("cache_invalidate not accepted within the publish timeout");
                Err(PublishError::TimedOut)
            }
        }
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

    fn broadcasts(&self, touched: &[TenantKeys], ctx: &BroadcastContext) -> Vec<Event> {
        build_broadcasts(touched, ctx)
    }
}

#[cfg(test)]
#[path = "event_broker_tests.rs"]
mod event_broker_tests;
