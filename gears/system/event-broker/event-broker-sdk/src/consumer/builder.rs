use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use gts::{GtsIdError, GtsIdPattern, GtsInstanceId, GtsSchema};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::api::EventBrokerApi;
use crate::api::{BarrierMode, Filter, TenantTraversalDepth};
use crate::error::EventBrokerError;
use crate::sdk::EventBrokerSdk;

use super::runtime::{Consumer, ConsumerHandle};
use super::types::{
    BatchHandlerAdapter, DeliverBatch, EventHandlerAdapter, TypedBatchHandlerAdapter,
    TypedEventHandlerAdapter,
};
#[cfg(feature = "db")]
use super::types::{
    DeliverTxBatch, TxBatchHandlerAdapter, TxEventHandlerAdapter, TxTypedBatchHandlerAdapter,
    TxTypedEventHandlerAdapter, TxUndecodable,
};
use super::{
    BatchHandler, CommitOffset, ConsumerBatching, ConsumerBuffering, ConsumerCommitMode,
    ConsumerGroupRef, ConsumerListenerSettings, ConsumerProfile, ConsumerRetry,
    ConsumerRuntimeListener, ConsumerSettings, ConsumerSettingsOverrides, ConsumerSlowDetection,
    EventHandler, SubscriptionFilterRef, SubscriptionInterest, Undecodable,
};
#[cfg(feature = "db")]
use super::{CommitOffsetInTx, LocalDbOffsetManager, TxBatchHandler, TxEventHandler};
use crate::gts::EventPayload;

// ---- Typestate markers (zero-sized) ----

pub struct BrokerOnly<M: CommitOffset + 'static>(pub M);

#[cfg(feature = "db")]
pub struct WithTx<M: CommitOffsetInTx + 'static>(pub M);

pub trait ConsumerOffsetManager: 'static {
    type BuilderState;

    fn into_builder_state(self) -> Self::BuilderState;
}

impl<M> ConsumerOffsetManager for M
where
    M: CommitOffset + 'static,
{
    type BuilderState = BrokerOnly<M>;

    fn into_builder_state(self) -> Self::BuilderState {
        BrokerOnly(self)
    }
}

#[cfg(feature = "db")]
impl ConsumerOffsetManager for LocalDbOffsetManager {
    type BuilderState = WithTx<Self>;

    fn into_builder_state(self) -> Self::BuilderState {
        WithTx(self)
    }
}

// ---- Builder ----

pub struct ConsumerBuilder<M = ()> {
    /// Consumer group the subscription joins; required before the consumer can start.
    pub(crate) group: Option<ConsumerGroupRef>,
    /// Topics to consume; kept in sync with `subscription_interests` when those are set.
    pub(crate) topics: Vec<GtsInstanceId>,
    /// Per-topic interests (tenant scope, type patterns, filter) sent on JOIN.
    pub(crate) subscription_interests: Vec<SubscriptionInterest>,
    /// Root tenant whose events are consumed; `None` means the nil tenant id.
    pub(crate) tenant_id: Option<Uuid>,
    /// How far below `tenant_id` the tenant hierarchy is traversed.
    pub(crate) tenant_depth: TenantTraversalDepth,
    /// Whether traversal stops at self-managed tenant boundaries.
    pub(crate) barrier_mode: BarrierMode,
    /// Event type patterns applied to every topic without an explicit interest.
    pub(crate) event_type_patterns: Vec<GtsIdPattern>,
    /// Number of parallel consumer workers (sessions) the runtime spawns.
    pub(crate) parallelism: u32,
    /// Client agent string reported to the broker on JOIN.
    pub(crate) client_agent: String,
    /// Session timeout passed to the broker on JOIN; `None` leaves the broker default.
    pub(crate) session_timeout: Option<Duration>,
    /// Optional content filter applied to every subscription interest.
    pub(crate) filter: Option<Filter>,
    /// Settings profile providing the base buffering, batching and retry defaults.
    pub(crate) profile: ConsumerProfile,
    /// Explicit per-setting overrides layered on top of `profile`.
    pub(crate) settings_overrides: ConsumerSettingsOverrides,
    /// How offsets are committed after handlers complete.
    pub(crate) commit_mode: ConsumerCommitMode,
    /// Drop-on-Nth-heartbeat threshold: disconnect + re-JOIN after K consecutive heartbeats
    /// with no intervening events. Default: 10 (≈ 50 s of silence at 5 s broker cadence).
    pub(crate) heartbeat_drop_threshold: usize,
    /// Runtime listeners notified of consumer lifecycle events.
    pub(crate) listeners: Vec<Arc<dyn ConsumerRuntimeListener>>,
    /// Offset manager state; `()` until `offset_manager` selects one.
    pub(crate) offset_manager: M,
    /// Broker client resolved from ClientHub or supplied by tests.
    pub(crate) broker: Option<Arc<dyn EventBrokerApi>>,
    /// Security context used by the SDK runtime for EventBrokerApi calls.
    pub(crate) security_context: SecurityContext,
}

impl ConsumerBuilder<()> {
    pub fn new(broker: Arc<dyn EventBrokerApi>) -> Self {
        Self {
            group: None,
            topics: Vec::new(),
            subscription_interests: Vec::new(),
            tenant_id: None,
            tenant_depth: TenantTraversalDepth::CurrentTenant,
            barrier_mode: BarrierMode::Respect,
            event_type_patterns: Vec::new(),
            parallelism: 1,
            client_agent: EventBrokerSdk::default_client_agent().into(),
            session_timeout: Some(Duration::from_secs(60)),
            filter: None,
            profile: ConsumerProfile::default_profile(),
            settings_overrides: ConsumerSettingsOverrides::default(),
            commit_mode: ConsumerCommitMode::default(),
            heartbeat_drop_threshold: 10,
            listeners: Vec::new(),
            offset_manager: (),
            broker: Some(broker),
            security_context: SecurityContext::anonymous(),
        }
    }

    pub fn new_unbound() -> Self {
        Self {
            group: None,
            topics: Vec::new(),
            subscription_interests: Vec::new(),
            tenant_id: None,
            tenant_depth: TenantTraversalDepth::CurrentTenant,
            barrier_mode: BarrierMode::Respect,
            event_type_patterns: Vec::new(),
            parallelism: 1,
            client_agent: EventBrokerSdk::default_client_agent().into(),
            session_timeout: Some(Duration::from_secs(60)),
            filter: None,
            profile: ConsumerProfile::default_profile(),
            settings_overrides: ConsumerSettingsOverrides::default(),
            commit_mode: ConsumerCommitMode::default(),
            heartbeat_drop_threshold: 10,
            listeners: Vec::new(),
            offset_manager: (),
            broker: None,
            security_context: SecurityContext::anonymous(),
        }
    }
}

// Common methods available in all commit-mode states.
impl<M> ConsumerBuilder<M> {
    pub fn group(mut self, group: ConsumerGroupRef) -> Self {
        self.group = Some(group);
        self
    }
    pub fn topics<I>(mut self, topics: I) -> Self
    where
        I: IntoIterator<Item = GtsInstanceId>,
    {
        self.topics.extend(topics);
        self
    }
    pub fn subscription_interests<I>(mut self, interests: I) -> Self
    where
        I: IntoIterator<Item = SubscriptionInterest>,
    {
        self.subscription_interests.extend(interests);
        self.topics = self
            .subscription_interests
            .iter()
            .map(|interest| interest.topic.clone())
            .collect();
        self
    }
    pub fn tenant_id(mut self, id: Uuid) -> Self {
        self.tenant_id = Some(id);
        self
    }
    /// Tenant hierarchy traversal scope. Defaults to current tenant only.
    pub fn tenant_depth(mut self, depth: TenantTraversalDepth) -> Self {
        self.tenant_depth = depth;
        self
    }
    /// Backward-compatible alias for tenant hierarchy traversal scope.
    pub fn max_depth(mut self, depth: TenantTraversalDepth) -> Self {
        self.tenant_depth = depth;
        self
    }
    /// Whether to stop at self-managed tenant boundaries. Default: `BarrierMode::Respect`.
    pub fn barrier_mode(mut self, mode: BarrierMode) -> Self {
        self.barrier_mode = mode;
        self
    }
    pub fn event_type_patterns<I>(mut self, pats: I) -> Self
    where
        I: IntoIterator<Item = GtsIdPattern>,
    {
        self.event_type_patterns.extend(pats);
        self
    }
    pub fn parallelism(mut self, n: u32) -> Self {
        self.parallelism = n;
        self
    }
    pub fn client_agent(mut self, ua: impl Into<String>) -> Self {
        self.client_agent = ua.into();
        self
    }
    /// Drop the streaming connection and re-JOIN after K consecutive
    /// `heartbeat` frames with no intervening events.
    /// Default: 10 (≈ 50 s of silence at the broker's 5 s default cadence).
    pub fn heartbeat_drop_threshold(mut self, k: usize) -> Self {
        self.heartbeat_drop_threshold = k;
        self
    }
    pub fn session_timeout(mut self, d: Duration) -> Self {
        self.session_timeout = Some(d);
        self
    }
    pub fn filter(mut self, engine: GtsInstanceId, expr: impl Into<String>) -> Self {
        self.filter = Some(Filter {
            engine,
            expression: expr.into(),
        });
        self
    }
    pub fn profile(mut self, profile: ConsumerProfile) -> Self {
        self.profile = profile;
        self
    }
    pub fn buffering(mut self, buffering: ConsumerBuffering) -> Self {
        self.settings_overrides.buffering = Some(buffering);
        self
    }
    pub fn batching(mut self, batching: ConsumerBatching) -> Self {
        self.settings_overrides.batching = Some(batching);
        self
    }
    pub fn slow_detection(mut self, slow_detection: ConsumerSlowDetection) -> Self {
        self.settings_overrides.slow_detection = Some(slow_detection);
        self
    }
    /// Overrides the profile's handler backoff: a delivery the handler retries or
    /// fails waits `base_delay`, doubling per attempt up to `max_delay`.
    pub fn retry(mut self, retry: ConsumerRetry) -> Self {
        self.settings_overrides.retry = Some(retry);
        self
    }
    pub fn listener_settings(mut self, listener: ConsumerListenerSettings) -> Self {
        self.settings_overrides.listener = Some(listener);
        self
    }
    pub fn register_listener<L>(mut self, listener: L) -> Self
    where
        L: ConsumerRuntimeListener + 'static,
    {
        self.listeners.push(Arc::new(listener));
        self
    }
    pub fn commit_mode(mut self, mode: ConsumerCommitMode) -> Self {
        self.commit_mode = mode;
        self
    }
    pub fn security_context(mut self, ctx: SecurityContext) -> Self {
        self.security_context = ctx;
        self
    }
}

impl<M> ConsumerBuilder<M> {
    /// Resolves the profile plus overrides into validated settings.
    pub(crate) fn effective_settings(&self) -> Result<ConsumerSettings, EventBrokerError> {
        let settings = ConsumerSettings::resolve(self.profile.clone(), self.settings_overrides);
        settings.validate()?;
        Ok(settings)
    }
}

// offset_manager transitions M and selects the commit mode.
impl ConsumerBuilder<()> {
    pub fn offset_manager<M>(self, m: M) -> ConsumerBuilder<M::BuilderState>
    where
        M: ConsumerOffsetManager,
    {
        ConsumerBuilder {
            group: self.group,
            topics: self.topics,
            subscription_interests: self.subscription_interests,
            tenant_id: self.tenant_id,
            tenant_depth: self.tenant_depth,
            barrier_mode: self.barrier_mode,
            event_type_patterns: self.event_type_patterns,
            parallelism: self.parallelism,
            client_agent: self.client_agent,
            heartbeat_drop_threshold: self.heartbeat_drop_threshold,
            session_timeout: self.session_timeout,
            filter: self.filter,
            profile: self.profile,
            settings_overrides: self.settings_overrides,
            commit_mode: self.commit_mode,
            listeners: self.listeners,
            offset_manager: m.into_builder_state(),
            broker: self.broker,
            security_context: self.security_context,
        }
    }
}

// ---- Terminal handler states ----

pub struct ConsumerReady<M, H> {
    /// Builder carrying the consumer configuration.
    pub(crate) builder: ConsumerBuilder<M>,
    /// Handler invoked for each event.
    pub(crate) handler: H,
}

pub struct ConsumerBatchReady<M, H> {
    /// Builder carrying the consumer configuration.
    pub(crate) builder: ConsumerBuilder<M>,
    /// Handler invoked for each batch of events.
    pub(crate) handler: H,
}

pub struct ConsumerRoutedReady<M, H> {
    /// Builder carrying the consumer configuration.
    pub(crate) builder: ConsumerBuilder<M>,
    /// Fallback handler for events matching no route.
    pub(crate) default_handler: H,
    /// Whether `default_handler` was explicitly supplied rather than a placeholder.
    pub(crate) has_default_handler: bool,
    /// Routes in registration order; `route_handlers[i]` serves `routes[i]`.
    pub(crate) routes: Vec<ConsumerRoute>,
    /// Handlers for the routes, index-aligned with `routes`.
    pub(crate) route_handlers: Vec<Arc<dyn DeliverBatch>>,
    /// Why each typed route that could not be registered was refused; `start()`
    /// reports the first.
    pub(crate) invalid_routes: Vec<String>,
}

#[cfg(feature = "db")]
pub struct TxConsumerRoutedReady<M: CommitOffsetInTx + 'static> {
    /// Builder carrying the consumer configuration.
    pub(crate) builder: ConsumerBuilder<WithTx<M>>,
    /// Handler for events no route matches.
    pub(crate) default_handler: Option<Arc<dyn DeliverTxBatch<M>>>,
    /// Routes in registration order; `route_handlers[i]` serves `routes[i]`.
    pub(crate) routes: Vec<ConsumerRoute>,
    /// Handlers for the routes, index-aligned with `routes`.
    pub(crate) route_handlers: Vec<Arc<dyn DeliverTxBatch<M>>>,
    /// Why each typed route that could not be registered was refused; `start()`
    /// reports the first.
    pub(crate) invalid_routes: Vec<String>,
}

pub struct NoDefaultHandler;

pub struct RouteMissingTopic;
pub struct RouteHasTopic;

pub struct ConsumerRouteBuilder<M, H, T = RouteMissingTopic> {
    ready: ConsumerRoutedReady<M, H>,
    topic: Option<GtsInstanceId>,
    event_type: Option<GtsIdPattern>,
    _topic_state: std::marker::PhantomData<T>,
}

#[cfg(feature = "db")]
pub struct TxConsumerRouteBuilder<M: CommitOffsetInTx + 'static, T = RouteMissingTopic> {
    ready: TxConsumerRoutedReady<M>,
    topic: Option<GtsInstanceId>,
    event_type: Option<GtsIdPattern>,
    _topic_state: std::marker::PhantomData<T>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ConsumerRoute {
    pub topic: GtsInstanceId,
    pub event_type: Option<GtsIdPattern>,
    pub handler_kind: ConsumerRouteHandlerKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsumerRouteHandlerKind {
    Single,
    Batch,
}

impl<M: CommitOffset + 'static> ConsumerBuilder<BrokerOnly<M>> {
    pub fn handler<H>(self, handler: H) -> ConsumerReady<BrokerOnly<M>, H>
    where
        H: EventHandler<serde_json::Value> + 'static,
    {
        ConsumerReady {
            builder: self,
            handler,
        }
    }

    pub fn batch_handler<H>(self, handler: H) -> ConsumerBatchReady<BrokerOnly<M>, H>
    where
        H: BatchHandler<serde_json::Value> + 'static,
    {
        ConsumerBatchReady {
            builder: self,
            handler,
        }
    }

    pub fn default_handler<H>(self, handler: H) -> ConsumerRoutedReady<BrokerOnly<M>, H>
    where
        H: EventHandler<serde_json::Value> + 'static,
    {
        ConsumerRoutedReady {
            builder: self,
            default_handler: handler,
            has_default_handler: true,
            routes: Vec::new(),
            route_handlers: Vec::new(),
            invalid_routes: Vec::new(),
        }
    }

    pub fn route(self) -> ConsumerRouteBuilder<BrokerOnly<M>, NoDefaultHandler, RouteMissingTopic> {
        ConsumerRouteBuilder {
            ready: ConsumerRoutedReady {
                builder: self,
                default_handler: NoDefaultHandler,
                has_default_handler: false,
                routes: Vec::new(),
                route_handlers: Vec::new(),
                invalid_routes: Vec::new(),
            },
            topic: None,
            event_type: None,
            _topic_state: std::marker::PhantomData,
        }
    }
}

#[cfg(feature = "db")]
impl<M: CommitOffsetInTx + 'static> ConsumerBuilder<WithTx<M>> {
    pub fn handler<H>(self, handler: H) -> ConsumerReady<WithTx<M>, H>
    where
        H: TxEventHandler<serde_json::Value, M> + 'static,
    {
        ConsumerReady {
            builder: self,
            handler,
        }
    }

    pub fn batch_handler<H>(self, handler: H) -> ConsumerBatchReady<WithTx<M>, H>
    where
        H: TxBatchHandler<serde_json::Value, M> + 'static,
    {
        ConsumerBatchReady {
            builder: self,
            handler,
        }
    }

    pub fn default_handler<H>(self, handler: H) -> TxConsumerRoutedReady<M>
    where
        H: TxEventHandler<serde_json::Value, M> + 'static,
    {
        let handler = TxEventHandlerAdapter::<_, M>::new(handler);
        TxConsumerRoutedReady {
            builder: self,
            default_handler: Some(Arc::new(handler)),
            routes: Vec::new(),
            route_handlers: Vec::new(),
            invalid_routes: Vec::new(),
        }
    }

    pub fn route(self) -> TxConsumerRouteBuilder<M, RouteMissingTopic> {
        TxConsumerRouteBuilder {
            ready: TxConsumerRoutedReady {
                builder: self,
                default_handler: None,
                routes: Vec::new(),
                route_handlers: Vec::new(),
                invalid_routes: Vec::new(),
            },
            topic: None,
            event_type: None,
            _topic_state: std::marker::PhantomData,
        }
    }
}

impl<M, H> ConsumerRoutedReady<M, H> {
    pub fn route(self) -> ConsumerRouteBuilder<M, H, RouteMissingTopic> {
        ConsumerRouteBuilder {
            ready: self,
            topic: None,
            event_type: None,
            _topic_state: std::marker::PhantomData,
        }
    }

    fn validate_routes(&self) -> Result<(), EventBrokerError> {
        if let Some(detail) = self.invalid_routes.first() {
            return Err(EventBrokerError::InvalidConsumerOptions {
                detail: detail.clone(),
            });
        }
        if self.builder.topics.is_empty() {
            return Err(EventBrokerError::InvalidConsumerOptions {
                detail: "routed consumer requires at least one configured topic".to_owned(),
            });
        }

        let mut seen = HashSet::new();
        for route in &self.routes {
            if !self.route_topic_is_configured(&route.topic) {
                return Err(EventBrokerError::InvalidConsumerOptions {
                    detail: format!(
                        "route topic {:?} is not part of the configured subscription topics",
                        route.topic
                    ),
                });
            }

            let key = (
                route.topic.clone(),
                route.event_type.as_ref().map(|p| p.pattern().to_owned()),
            );
            if !seen.insert(key) {
                return Err(EventBrokerError::InvalidConsumerOptions {
                    detail: format!(
                        "duplicate consumer route for topic {:?} and event type {:?}",
                        route.topic, route.event_type
                    ),
                });
            }
        }

        if !self.has_default_handler {
            for configured in &self.builder.topics {
                let has_topic_catch_all = self
                    .routes
                    .iter()
                    .any(|route| route.event_type.is_none() && &route.topic == configured);
                if !has_topic_catch_all {
                    return Err(EventBrokerError::InvalidConsumerOptions {
                        detail: format!(
                            "routed consumer without a default handler requires a topic catch-all route for configured topic {configured}"
                        ),
                    });
                }
            }
        }

        Ok(())
    }

    fn route_topic_is_configured(&self, route_topic: &GtsInstanceId) -> bool {
        self.builder
            .topics
            .iter()
            .any(|configured| route_topic == configured)
    }
}

#[cfg(feature = "db")]
impl<M> TxConsumerRoutedReady<M>
where
    M: CommitOffsetInTx + 'static,
{
    pub fn route(self) -> TxConsumerRouteBuilder<M, RouteMissingTopic> {
        TxConsumerRouteBuilder {
            ready: self,
            topic: None,
            event_type: None,
            _topic_state: std::marker::PhantomData,
        }
    }

    fn validate_routes(&self) -> Result<(), EventBrokerError> {
        if let Some(detail) = self.invalid_routes.first() {
            return Err(EventBrokerError::InvalidConsumerOptions {
                detail: detail.clone(),
            });
        }
        if self.builder.topics.is_empty() {
            return Err(EventBrokerError::InvalidConsumerOptions {
                detail: "routed consumer requires at least one configured topic".to_owned(),
            });
        }

        let mut seen = HashSet::new();
        for route in &self.routes {
            if !self.route_topic_is_configured(&route.topic) {
                return Err(EventBrokerError::InvalidConsumerOptions {
                    detail: format!(
                        "route topic {:?} is not part of the configured subscription topics",
                        route.topic
                    ),
                });
            }

            let key = (
                route.topic.clone(),
                route.event_type.as_ref().map(|p| p.pattern().to_owned()),
            );
            if !seen.insert(key) {
                return Err(EventBrokerError::InvalidConsumerOptions {
                    detail: format!(
                        "duplicate consumer route for topic {:?} and event type {:?}",
                        route.topic, route.event_type
                    ),
                });
            }
        }

        if self.default_handler.is_none() {
            for configured in &self.builder.topics {
                let has_topic_catch_all = self
                    .routes
                    .iter()
                    .any(|route| route.event_type.is_none() && &route.topic == configured);
                if !has_topic_catch_all {
                    return Err(EventBrokerError::InvalidConsumerOptions {
                        detail: format!(
                            "routed consumer without a default handler requires a topic catch-all route for configured topic {configured}"
                        ),
                    });
                }
            }
        }

        Ok(())
    }

    fn route_topic_is_configured(&self, route_topic: &GtsInstanceId) -> bool {
        self.builder
            .topics
            .iter()
            .any(|configured| route_topic == configured)
    }
}

pub(crate) fn subscription_filter_ref_to_filter(filter: &SubscriptionFilterRef) -> Filter {
    Filter {
        engine: filter.engine.clone(),
        expression: filter.expression.clone(),
    }
}

impl<M, H> ConsumerRouteBuilder<M, H, RouteMissingTopic> {
    pub fn topic(self, topic: GtsInstanceId) -> ConsumerRouteBuilder<M, H, RouteHasTopic> {
        ConsumerRouteBuilder {
            ready: self.ready,
            topic: Some(topic),
            event_type: self.event_type,
            _topic_state: std::marker::PhantomData,
        }
    }
}

impl<M, H> ConsumerRouteBuilder<M, H, RouteHasTopic> {
    pub fn topic(mut self, topic: GtsInstanceId) -> Self {
        self.topic = Some(topic);
        self
    }

    pub fn event_type(mut self, event_type: GtsIdPattern) -> Self {
        self.event_type = Some(event_type);
        self
    }

    fn push_broker_route(
        mut self,
        handler_kind: ConsumerRouteHandlerKind,
        handler: Arc<dyn DeliverBatch>,
    ) -> ConsumerRoutedReady<M, H> {
        let topic = self
            .topic
            .expect("route topic is required before registering a handler");
        self.ready.routes.push(ConsumerRoute {
            topic,
            event_type: self.event_type,
            handler_kind,
        });
        self.ready.route_handlers.push(handler);
        self.ready
    }
}

#[cfg(feature = "db")]
impl<M> TxConsumerRouteBuilder<M, RouteMissingTopic>
where
    M: CommitOffsetInTx + 'static,
{
    pub fn topic(self, topic: GtsInstanceId) -> TxConsumerRouteBuilder<M, RouteHasTopic> {
        TxConsumerRouteBuilder {
            ready: self.ready,
            topic: Some(topic),
            event_type: self.event_type,
            _topic_state: std::marker::PhantomData,
        }
    }
}

#[cfg(feature = "db")]
impl<M> TxConsumerRouteBuilder<M, RouteHasTopic>
where
    M: CommitOffsetInTx + 'static,
{
    pub fn topic(mut self, topic: GtsInstanceId) -> Self {
        self.topic = Some(topic);
        self
    }

    pub fn event_type(mut self, event_type: GtsIdPattern) -> Self {
        self.event_type = Some(event_type);
        self
    }

    fn push_tx_route(
        mut self,
        handler_kind: ConsumerRouteHandlerKind,
        handler: Arc<dyn DeliverTxBatch<M>>,
    ) -> TxConsumerRoutedReady<M> {
        let topic = self
            .topic
            .expect("route topic is required before registering a handler");
        self.ready.routes.push(ConsumerRoute {
            topic,
            event_type: self.event_type,
            handler_kind,
        });
        self.ready.route_handlers.push(handler);
        self.ready
    }
}

impl<M, H> ConsumerRouteBuilder<BrokerOnly<M>, H, RouteHasTopic>
where
    M: CommitOffset + 'static,
{
    pub fn handler<RH>(self, handler: RH) -> ConsumerRoutedReady<BrokerOnly<M>, H>
    where
        RH: EventHandler<serde_json::Value> + 'static,
    {
        let handler = EventHandlerAdapter::new(handler);
        self.push_broker_route(ConsumerRouteHandlerKind::Single, Arc::new(handler))
    }

    pub fn batch_handler<RH>(self, handler: RH) -> ConsumerRoutedReady<BrokerOnly<M>, H>
    where
        RH: BatchHandler<serde_json::Value> + 'static,
    {
        let handler = BatchHandlerAdapter::new(handler);
        self.push_broker_route(ConsumerRouteHandlerKind::Batch, Arc::new(handler))
    }

    /// Makes this a typed route for payload `P`: it serves the events of `P`'s type and
    /// of the types derived from it, replacing any type an earlier
    /// [`event_type`](Self::event_type) set, and its handler gets each event decoded as
    /// `P`. `undecodable` says what happens to an event whose data does not decode as
    /// `P`.
    pub fn event<P: EventPayload>(
        self,
        undecodable: Undecodable,
    ) -> TypedRouteBuilder<BrokerOnly<M>, H, P> {
        TypedRouteBuilder {
            route: self,
            event_type: GtsIdPattern::try_new(P::innermost_type_id()),
            undecodable,
            _payload: std::marker::PhantomData,
        }
    }
}

/// A route with its topic, a payload type `P` and an [`Undecodable`] policy, waiting
/// for its handler. Made by [`ConsumerRouteBuilder::event`].
pub struct TypedRouteBuilder<M, H, P> {
    route: ConsumerRouteBuilder<M, H, RouteHasTopic>,
    /// `P`'s type id as a route pattern, or why it is not one.
    event_type: Result<GtsIdPattern, GtsIdError>,
    undecodable: Undecodable,
    _payload: std::marker::PhantomData<fn() -> P>,
}

// Prints the route's topic, type and policy; the routes and handlers gathered so far
// are left out.
impl<M, H, P: GtsSchema> std::fmt::Debug for TypedRouteBuilder<M, H, P> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TypedRouteBuilder")
            .field(
                "topic",
                &self.route.topic.as_ref().map_or("", AsRef::<str>::as_ref),
            )
            .field("event_type", &P::innermost_type_id())
            .field("undecodable", &self.undecodable)
            .finish()
    }
}

impl<M, H, P> TypedRouteBuilder<BrokerOnly<M>, H, P>
where
    M: CommitOffset + 'static,
    P: EventPayload,
{
    /// Registers `handler` for the route's events. An event that decodes as `P` goes to
    /// `handler`: `Success` advances through it, `Retry` retries it. One that does not
    /// decode goes by the route's [`Undecodable`] policy.
    ///
    /// A `P` whose type id is not a GTS type id registers no route; `start()` then
    /// refuses the consumer with [`EventBrokerError::InvalidConsumerOptions`].
    pub fn handler<RH>(self, handler: RH) -> ConsumerRoutedReady<BrokerOnly<M>, H>
    where
        RH: EventHandler<P> + 'static,
    {
        self.push_typed_route(ConsumerRouteHandlerKind::Single, |undecodable| {
            Arc::new(TypedEventHandlerAdapter::<_, P>::new(handler, undecodable))
        })
    }

    /// Registers `handler` for the route's batches: the events of `P`'s type and of the
    /// types derived from it. Each batch is decoded as `P` in delivery order, up to the
    /// first event that does not decode; `handler` gets the events before it, the typed
    /// prefix.
    ///
    /// - A non-empty batch whose events all decode goes to `handler` whole, and its
    ///   outcome passes through unchanged; an empty batch succeeds without `handler`.
    /// - After a typed prefix, `handler`'s `Retry`, and an `AdvanceThrough` short of the
    ///   prefix's last event, are returned as they are. An `AdvanceThrough` to an offset
    ///   outside the prefix, the undecodable event's included, is a handler error: it is
    ///   reported and the batch retried. `handler`'s `Success`, or an `AdvanceThrough` to
    ///   the prefix's last event, hands the undecodable event to the route's
    ///   [`Undecodable`] policy.
    /// - `Stall` advances through the prefix and stops at the event; with no prefix it
    ///   retries. `Skip` advances past the event and reports it. `Fallback(h)` hands the
    ///   raw event to `h`: its `Success` advances past the event, its `Retry` advances
    ///   through the prefix, or with no prefix retries.
    /// - The events after the undecodable one are not decoded, and no outcome advances
    ///   through them.
    /// - An error from `handler` or from a fallback handler is reported and the whole
    ///   batch retried, so a prefix `handler` already handled is handed to it again.
    ///
    /// A `P` whose type id is not a GTS type id registers no route; `start()` then
    /// refuses the consumer with [`EventBrokerError::InvalidConsumerOptions`].
    pub fn batch_handler<RH>(self, handler: RH) -> ConsumerRoutedReady<BrokerOnly<M>, H>
    where
        RH: BatchHandler<P> + 'static,
    {
        self.push_typed_route(ConsumerRouteHandlerKind::Batch, |undecodable| {
            Arc::new(TypedBatchHandlerAdapter::<_, P>::new(handler, undecodable))
        })
    }

    /// Registers the adapter `adapter` builds from the route's policy, under `P`'s type
    /// id; a `P` whose id is no GTS type id records why in `invalid_routes` instead.
    fn push_typed_route(
        self,
        handler_kind: ConsumerRouteHandlerKind,
        adapter: impl FnOnce(Undecodable) -> Arc<dyn DeliverBatch>,
    ) -> ConsumerRoutedReady<BrokerOnly<M>, H> {
        let mut route = self.route;
        match self.event_type {
            Ok(pattern) => {
                route.event_type = Some(pattern);
                route.push_broker_route(handler_kind, adapter(self.undecodable))
            }
            Err(err) => {
                route.ready.invalid_routes.push(format!(
                    "typed route payload type id {:?} is not a GTS type id: {err}",
                    P::innermost_type_id()
                ));
                route.ready
            }
        }
    }
}

#[cfg(feature = "db")]
impl<M> TxConsumerRouteBuilder<M, RouteHasTopic>
where
    M: CommitOffsetInTx + 'static,
{
    pub fn handler<RH>(self, handler: RH) -> TxConsumerRoutedReady<M>
    where
        RH: TxEventHandler<serde_json::Value, M> + 'static,
    {
        let handler = TxEventHandlerAdapter::new(handler);
        self.push_tx_route(ConsumerRouteHandlerKind::Single, Arc::new(handler))
    }

    pub fn batch_handler<RH>(self, handler: RH) -> TxConsumerRoutedReady<M>
    where
        RH: TxBatchHandler<serde_json::Value, M> + 'static,
    {
        let handler = TxBatchHandlerAdapter::new(handler);
        self.push_tx_route(ConsumerRouteHandlerKind::Batch, Arc::new(handler))
    }

    /// Makes this a typed transactional route for payload `P`: it serves the events of
    /// `P`'s type and of the types derived from it, replacing any type an earlier
    /// [`event_type`](Self::event_type) set. `undecodable` says what happens to an
    /// event whose data does not decode as `P`.
    pub fn event<P: EventPayload>(
        self,
        undecodable: TxUndecodable<M>,
    ) -> TxTypedRouteBuilder<M, P> {
        TxTypedRouteBuilder {
            route: self,
            event_type: GtsIdPattern::try_new(P::innermost_type_id()),
            undecodable,
            _payload: std::marker::PhantomData,
        }
    }
}

/// A transactional route with its topic, a payload type `P` and a [`TxUndecodable`]
/// policy, waiting for its handler. Made by [`TxConsumerRouteBuilder::event`].
#[cfg(feature = "db")]
pub struct TxTypedRouteBuilder<M: CommitOffsetInTx + 'static, P> {
    route: TxConsumerRouteBuilder<M, RouteHasTopic>,
    /// `P`'s type id as a route pattern, or why it is not one.
    event_type: Result<GtsIdPattern, GtsIdError>,
    undecodable: TxUndecodable<M>,
    _payload: std::marker::PhantomData<fn() -> P>,
}

// Prints the route's topic, type and policy; the routes and handlers gathered so far
// are left out.
#[cfg(feature = "db")]
impl<M: CommitOffsetInTx + 'static, P: GtsSchema> std::fmt::Debug for TxTypedRouteBuilder<M, P> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TxTypedRouteBuilder")
            .field(
                "topic",
                &self.route.topic.as_ref().map_or("", AsRef::<str>::as_ref),
            )
            .field("event_type", &P::innermost_type_id())
            .field("undecodable", &self.undecodable)
            .finish()
    }
}

#[cfg(feature = "db")]
impl<M, P> TxTypedRouteBuilder<M, P>
where
    M: CommitOffsetInTx + 'static,
    P: EventPayload,
{
    /// Registers `handler` for the route's events: the events of `P`'s type and of the
    /// types derived from it. An event that decodes as `P` goes to `handler` with the
    /// delivery's commit handle, and its outcome passes through: the consumer advances
    /// through what `handler` committed. One that does not decode goes by the route's
    /// [`TxUndecodable`] policy.
    ///
    /// A `P` whose type id is not a GTS type id registers no route; `start()` then
    /// refuses the consumer with [`EventBrokerError::InvalidConsumerOptions`].
    pub fn handler<RH>(self, handler: RH) -> TxConsumerRoutedReady<M>
    where
        RH: TxEventHandler<P, M> + 'static,
    {
        self.push_typed_tx_route(ConsumerRouteHandlerKind::Single, |undecodable| {
            Arc::new(TxTypedEventHandlerAdapter::<_, P, M>::new(
                handler,
                undecodable,
            ))
        })
    }

    /// Registers `handler` for the route's batches: the events of `P`'s type and of the
    /// types derived from it. Each batch is decoded as `P` in delivery order, up to the
    /// first event that does not decode; `handler` gets the events before it, the typed
    /// prefix, with a commit handle that commits only offsets of the prefix.
    ///
    /// - A non-empty batch whose events all decode goes to `handler` whole, with the
    ///   delivery's commit handle, and its outcome passes through; an empty batch
    ///   succeeds without `handler`.
    /// - After a typed prefix, `handler`'s `Retry` is returned as it is. After its
    ///   `Success`, the undecodable event goes to the route's [`TxUndecodable`] policy
    ///   only if `handler` committed the prefix's last event. Had it committed an
    ///   earlier one, the consumer advances through that one; had it committed none,
    ///   the consumer reports the missing commit, as for any transactional handler.
    /// - `Stall` succeeds after the committed prefix, and the consumer advances through
    ///   it; with no prefix it retries. The runtime delivers one event per call, so
    ///   `Stall` stops at the undecodable event.
    /// - `Fallback(h)` hands the raw event to `h` with a commit handle that commits only
    ///   that event's offset. Its `Success` passes through; its `Retry` succeeds after
    ///   the committed prefix, or with no prefix retries.
    /// - An error from `handler`, or from a fallback handler with no prefix, is reported
    ///   and the batch retried. A fallback error after the committed prefix is logged
    ///   and the call succeeds: the prefix is committed in its own transaction, so while
    ///   the consumer owns the partition it is not handed to `handler` again.
    ///
    /// A `P` whose type id is not a GTS type id registers no route; `start()` then
    /// refuses the consumer with [`EventBrokerError::InvalidConsumerOptions`].
    pub fn batch_handler<RH>(self, handler: RH) -> TxConsumerRoutedReady<M>
    where
        RH: TxBatchHandler<P, M> + 'static,
    {
        self.push_typed_tx_route(ConsumerRouteHandlerKind::Batch, |undecodable| {
            Arc::new(TxTypedBatchHandlerAdapter::<_, P, M>::new(
                handler,
                undecodable,
            ))
        })
    }

    /// Registers the adapter `adapter` builds from the route's policy, under `P`'s type
    /// id; a `P` whose id is no GTS type id records why in `invalid_routes` instead.
    fn push_typed_tx_route(
        self,
        handler_kind: ConsumerRouteHandlerKind,
        adapter: impl FnOnce(TxUndecodable<M>) -> Arc<dyn DeliverTxBatch<M>>,
    ) -> TxConsumerRoutedReady<M> {
        let mut route = self.route;
        match self.event_type {
            Ok(pattern) => {
                route.event_type = Some(pattern);
                route.push_tx_route(handler_kind, adapter(self.undecodable))
            }
            Err(err) => {
                route.ready.invalid_routes.push(format!(
                    "typed route payload type id {:?} is not a GTS type id: {err}",
                    P::innermost_type_id()
                ));
                route.ready
            }
        }
    }
}

// ---- Terminal build methods on ConsumerReady ----

// -- BrokerOnly (async-commit) --

impl<M, H> ConsumerReady<BrokerOnly<M>, H>
where
    M: CommitOffset + 'static,
    H: EventHandler<serde_json::Value> + 'static,
{
    pub async fn start(self) -> Result<ConsumerHandle, EventBrokerError> {
        let consumer = Consumer::new_with_slots(self.builder, self.handler).await?;
        Ok(ConsumerHandle::from_consumer(consumer))
    }

    pub async fn run_blocking(
        self,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<(), EventBrokerError> {
        let handle = self.start().await?;
        cancel.cancelled().await;
        handle.stop().await
    }
}

impl<M, H> ConsumerBatchReady<BrokerOnly<M>, H>
where
    M: CommitOffset + 'static,
    H: BatchHandler<serde_json::Value> + 'static,
{
    pub async fn start(self) -> Result<ConsumerHandle, EventBrokerError> {
        let consumer = Consumer::new_with_batch_slots(self.builder, self.handler).await?;
        Ok(ConsumerHandle::from_consumer(consumer))
    }

    pub async fn run_blocking(
        self,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<(), EventBrokerError> {
        let handle = self.start().await?;
        cancel.cancelled().await;
        handle.stop().await
    }
}

#[cfg(feature = "db")]
impl<M, H> ConsumerBatchReady<WithTx<M>, H>
where
    M: CommitOffsetInTx + 'static,
    H: TxBatchHandler<serde_json::Value, M> + 'static,
{
    pub async fn start(self) -> Result<ConsumerHandle, EventBrokerError> {
        let consumer = Consumer::new_with_tx_batch_slots(self.builder, self.handler).await?;
        Ok(ConsumerHandle::from_consumer(consumer))
    }
}

impl<M, H> ConsumerRoutedReady<BrokerOnly<M>, H>
where
    M: CommitOffset + 'static,
    H: EventHandler<serde_json::Value> + 'static,
{
    pub async fn start(self) -> Result<ConsumerHandle, EventBrokerError> {
        self.validate_routes()?;
        let default_handler = EventHandlerAdapter::new(self.default_handler);
        let consumer = Consumer::new_with_routed_slots(
            self.builder,
            Some(Arc::new(default_handler)),
            self.routes,
            self.route_handlers,
        )
        .await?;
        Ok(ConsumerHandle::from_consumer(consumer))
    }
}

impl<M> ConsumerRoutedReady<BrokerOnly<M>, NoDefaultHandler>
where
    M: CommitOffset + 'static,
{
    pub async fn start(self) -> Result<ConsumerHandle, EventBrokerError> {
        self.validate_routes()?;
        let consumer =
            Consumer::new_with_routed_slots(self.builder, None, self.routes, self.route_handlers)
                .await?;
        Ok(ConsumerHandle::from_consumer(consumer))
    }
}

// -- WithTx (db feature) --

#[cfg(feature = "db")]
impl<M> TxConsumerRoutedReady<M>
where
    M: CommitOffsetInTx + 'static,
{
    pub async fn start(self) -> Result<ConsumerHandle, EventBrokerError> {
        self.validate_routes()?;
        let consumer = Consumer::new_with_tx_routed_slots(
            self.builder,
            self.default_handler,
            self.routes,
            self.route_handlers,
        )
        .await?;
        Ok(ConsumerHandle::from_consumer(consumer))
    }
}

#[cfg(feature = "db")]
impl<M, H> ConsumerReady<WithTx<M>, H>
where
    M: CommitOffsetInTx + 'static,
    H: TxEventHandler<serde_json::Value, M> + 'static,
{
    pub async fn start(self) -> Result<ConsumerHandle, EventBrokerError> {
        let consumer = Consumer::new_with_tx_slots(self.builder, self.handler).await?;
        Ok(ConsumerHandle::from_consumer(consumer))
    }

    pub async fn run_blocking(
        self,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<(), EventBrokerError> {
        let handle = self.start().await?;
        cancel.cancelled().await;
        handle.stop().await
    }
}
