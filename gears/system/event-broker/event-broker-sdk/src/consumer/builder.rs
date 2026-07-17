use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::api::EventBroker;
use crate::api::{BarrierMode, Filter, TenantTraversalDepth};
use crate::error::EventBrokerError;
use crate::sdk::EventBrokerSdk;

use super::runtime::{Consumer, ConsumerHandle};
use super::{
    CommitOffset, ConsumerBatching, ConsumerBuffering, ConsumerCommitMode, ConsumerGroupRef,
    ConsumerHandler, ConsumerListenerSettings, ConsumerProfile, ConsumerRetry,
    ConsumerRuntimeListener, ConsumerSettings, ConsumerSettingsOverrides, ConsumerSlowDetection,
    EventTypeRef, SingleEventHandler, SubscriptionFilterRef, SubscriptionInterest, TopicRef,
};
#[cfg(feature = "db")]
use super::{CommitOffsetInTx, LocalDbOffsetManager, TxConsumerHandler, TxSingleEventHandler};

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
    pub(crate) group: Option<ConsumerGroupRef>,
    pub(crate) topics: Vec<String>,
    pub(crate) subscription_interests: Vec<SubscriptionInterest>,
    pub(crate) tenant_id: Option<Uuid>,
    pub(crate) tenant_depth: TenantTraversalDepth,
    pub(crate) barrier_mode: BarrierMode,
    pub(crate) event_type_patterns: Vec<String>,
    pub(crate) parallelism: u32,
    pub(crate) client_agent: String,
    pub(crate) session_timeout: Option<Duration>,
    pub(crate) filter: Option<Filter>,
    pub(crate) retry_base: Duration,
    pub(crate) retry_max: Duration,
    pub(crate) profile: ConsumerProfile,
    pub(crate) settings_overrides: ConsumerSettingsOverrides,
    pub(crate) commit_mode: ConsumerCommitMode,
    /// Drop-on-Nth-heartbeat threshold: disconnect + re-JOIN after K consecutive heartbeats
    /// with no intervening events. Default: 10 (≈ 50 s of silence at 5 s broker cadence).
    pub(crate) heartbeat_drop_threshold: usize,
    pub(crate) listeners: Vec<Arc<dyn ConsumerRuntimeListener>>,
    pub(crate) offset_manager: M,
    /// Broker client resolved from ClientHub or supplied by tests.
    pub(crate) broker: Option<Arc<dyn EventBroker>>,
    /// Security context used by the SDK runtime for EventBroker calls.
    pub(crate) security_context: SecurityContext,
}

impl ConsumerBuilder<()> {
    pub fn new(broker: Arc<dyn EventBroker>) -> Self {
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
            retry_base: Duration::from_secs(1),
            retry_max: Duration::from_secs(60),
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
            retry_base: Duration::from_secs(1),
            retry_max: Duration::from_secs(60),
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
    pub fn topics<I, S>(mut self, topics: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.topics.extend(topics.into_iter().map(Into::into));
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
            .map(|interest| topic_ref_to_string(&interest.topic))
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
    pub fn event_type_patterns<I, S>(mut self, pats: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.event_type_patterns
            .extend(pats.into_iter().map(Into::into));
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
    pub fn filter(mut self, engine: impl Into<String>, expr: impl Into<String>) -> Self {
        self.filter = Some(Filter {
            engine: engine.into(),
            expression: expr.into(),
        });
        self
    }
    pub fn retry_base(mut self, d: Duration) -> Self {
        self.retry_base = d;
        self
    }
    pub fn retry_max(mut self, d: Duration) -> Self {
        self.retry_max = d;
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
    pub fn retry(mut self, retry: ConsumerRetry) -> Self {
        self.settings_overrides.retry = Some(retry);
        self.retry_base = retry.base_delay;
        self.retry_max = retry.max_delay;
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
            retry_base: self.retry_base,
            retry_max: self.retry_max,
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
    pub(crate) builder: ConsumerBuilder<M>,
    pub(crate) handler: H,
}

pub struct ConsumerBatchReady<M, H> {
    pub(crate) builder: ConsumerBuilder<M>,
    pub(crate) handler: H,
}

pub struct ConsumerRoutedReady<M, H> {
    pub(crate) builder: ConsumerBuilder<M>,
    pub(crate) default_handler: H,
    pub(crate) has_default_handler: bool,
    pub(crate) routes: Vec<ConsumerRoute>,
    pub(crate) route_handlers: Vec<Arc<dyn ConsumerHandler>>,
}

#[cfg(feature = "db")]
pub struct TxConsumerRoutedReady<M: CommitOffsetInTx + 'static> {
    pub(crate) builder: ConsumerBuilder<WithTx<M>>,
    pub(crate) default_handler: Option<Arc<dyn TxConsumerHandler<M>>>,
    pub(crate) routes: Vec<ConsumerRoute>,
    pub(crate) route_handlers: Vec<Arc<dyn TxConsumerHandler<M>>>,
}

pub struct NoDefaultHandler;

pub struct RouteMissingTopic;
pub struct RouteHasTopic;

pub struct ConsumerRouteBuilder<M, H, T = RouteMissingTopic> {
    ready: ConsumerRoutedReady<M, H>,
    topic: Option<TopicRef>,
    event_type: Option<EventTypeRef>,
    _topic_state: std::marker::PhantomData<T>,
}

#[cfg(feature = "db")]
pub struct TxConsumerRouteBuilder<M: CommitOffsetInTx + 'static, T = RouteMissingTopic> {
    ready: TxConsumerRoutedReady<M>,
    topic: Option<TopicRef>,
    event_type: Option<EventTypeRef>,
    _topic_state: std::marker::PhantomData<T>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsumerRoute {
    pub topic: TopicRef,
    pub event_type: Option<EventTypeRef>,
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
        H: SingleEventHandler + 'static,
    {
        ConsumerReady {
            builder: self,
            handler,
        }
    }

    pub fn batch_handler<H>(self, handler: H) -> ConsumerBatchReady<BrokerOnly<M>, H>
    where
        H: ConsumerHandler + 'static,
    {
        ConsumerBatchReady {
            builder: self,
            handler,
        }
    }

    pub fn default_handler<H>(self, handler: H) -> ConsumerRoutedReady<BrokerOnly<M>, H>
    where
        H: SingleEventHandler + 'static,
    {
        ConsumerRoutedReady {
            builder: self,
            default_handler: handler,
            has_default_handler: true,
            routes: Vec::new(),
            route_handlers: Vec::new(),
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
        H: TxSingleEventHandler<M> + 'static,
    {
        ConsumerReady {
            builder: self,
            handler,
        }
    }

    pub fn batch_handler<H>(self, handler: H) -> ConsumerBatchReady<WithTx<M>, H>
    where
        H: TxConsumerHandler<M> + 'static,
    {
        ConsumerBatchReady {
            builder: self,
            handler,
        }
    }

    pub fn default_handler<H>(self, handler: H) -> TxConsumerRoutedReady<M>
    where
        H: TxSingleEventHandler<M> + 'static,
    {
        let handler = super::TxSingleEventHandlerAdapter::<_, M>::new(Arc::new(handler));
        TxConsumerRoutedReady {
            builder: self,
            default_handler: Some(Arc::new(handler)),
            routes: Vec::new(),
            route_handlers: Vec::new(),
        }
    }

    pub fn route(self) -> TxConsumerRouteBuilder<M, RouteMissingTopic> {
        TxConsumerRouteBuilder {
            ready: TxConsumerRoutedReady {
                builder: self,
                default_handler: None,
                routes: Vec::new(),
                route_handlers: Vec::new(),
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
        if self.builder.topics.is_empty() {
            return Err(EventBrokerError::InvalidConsumerOptions {
                detail: "routed consumer requires at least one configured topic".to_owned(),
                instance: String::new(),
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
                    instance: String::new(),
                });
            }

            let key = (route.topic.clone(), route.event_type.clone());
            if !seen.insert(key) {
                return Err(EventBrokerError::InvalidConsumerOptions {
                    detail: format!(
                        "duplicate consumer route for topic {:?} and event type {:?}",
                        route.topic, route.event_type
                    ),
                    instance: String::new(),
                });
            }
        }

        if !self.has_default_handler {
            for configured in &self.builder.topics {
                let has_topic_catch_all = self.routes.iter().any(|route| {
                    route.event_type.is_none()
                        && topic_ref_matches_configured(&route.topic, configured)
                });
                if !has_topic_catch_all {
                    return Err(EventBrokerError::InvalidConsumerOptions {
                        detail: format!(
                            "routed consumer without a default handler requires a topic catch-all route for configured topic {configured}"
                        ),
                        instance: String::new(),
                    });
                }
            }
        }

        Ok(())
    }

    fn route_topic_is_configured(&self, route_topic: &TopicRef) -> bool {
        self.builder
            .topics
            .iter()
            .any(|configured| topic_ref_matches_configured(route_topic, configured))
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
        if self.builder.topics.is_empty() {
            return Err(EventBrokerError::InvalidConsumerOptions {
                detail: "routed consumer requires at least one configured topic".to_owned(),
                instance: String::new(),
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
                    instance: String::new(),
                });
            }

            let key = (route.topic.clone(), route.event_type.clone());
            if !seen.insert(key) {
                return Err(EventBrokerError::InvalidConsumerOptions {
                    detail: format!(
                        "duplicate consumer route for topic {:?} and event type {:?}",
                        route.topic, route.event_type
                    ),
                    instance: String::new(),
                });
            }
        }

        if self.default_handler.is_none() {
            for configured in &self.builder.topics {
                let has_topic_catch_all = self.routes.iter().any(|route| {
                    route.event_type.is_none()
                        && topic_ref_matches_configured(&route.topic, configured)
                });
                if !has_topic_catch_all {
                    return Err(EventBrokerError::InvalidConsumerOptions {
                        detail: format!(
                            "routed consumer without a default handler requires a topic catch-all route for configured topic {configured}"
                        ),
                        instance: String::new(),
                    });
                }
            }
        }

        Ok(())
    }

    fn route_topic_is_configured(&self, route_topic: &TopicRef) -> bool {
        self.builder
            .topics
            .iter()
            .any(|configured| topic_ref_matches_configured(route_topic, configured))
    }
}

fn topic_ref_matches_configured(route_topic: &TopicRef, configured: &str) -> bool {
    match route_topic {
        TopicRef::Gts(gts) => gts == configured,
        TopicRef::Id(id) => *id == crate::ids::TopicId::from_gts(configured),
    }
}

pub(crate) fn topic_ref_to_string(topic: &TopicRef) -> String {
    match topic {
        TopicRef::Gts(gts) => gts.clone(),
        TopicRef::Id(id) => id.as_uuid().to_string(),
    }
}

pub(crate) fn event_type_ref_to_string(event_type: &EventTypeRef) -> String {
    match event_type {
        EventTypeRef::Gts(gts) | EventTypeRef::GtsPattern(gts) => gts.clone(),
        EventTypeRef::Id(id) => id.as_uuid().to_string(),
    }
}

pub(crate) fn subscription_filter_ref_to_filter(filter: &SubscriptionFilterRef) -> Filter {
    Filter {
        engine: match &filter.engine {
            super::FilterEngineRef::Gts(gts) => gts.clone(),
            super::FilterEngineRef::Id(id) => id.to_string(),
        },
        expression: filter.expression.clone(),
    }
}

impl<M, H> ConsumerRouteBuilder<M, H, RouteMissingTopic> {
    pub fn topic(self, topic: impl Into<TopicRef>) -> ConsumerRouteBuilder<M, H, RouteHasTopic> {
        ConsumerRouteBuilder {
            ready: self.ready,
            topic: Some(topic.into()),
            event_type: self.event_type,
            _topic_state: std::marker::PhantomData,
        }
    }
}

impl<M, H> ConsumerRouteBuilder<M, H, RouteHasTopic> {
    pub fn topic(mut self, topic: impl Into<TopicRef>) -> Self {
        self.topic = Some(topic.into());
        self
    }

    pub fn event_type(mut self, event_type: impl Into<EventTypeRef>) -> Self {
        self.event_type = Some(event_type.into());
        self
    }

    fn push_broker_route(
        mut self,
        handler_kind: ConsumerRouteHandlerKind,
        handler: Arc<dyn ConsumerHandler>,
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
    pub fn topic(self, topic: impl Into<TopicRef>) -> TxConsumerRouteBuilder<M, RouteHasTopic> {
        TxConsumerRouteBuilder {
            ready: self.ready,
            topic: Some(topic.into()),
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
    pub fn topic(mut self, topic: impl Into<TopicRef>) -> Self {
        self.topic = Some(topic.into());
        self
    }

    pub fn event_type(mut self, event_type: impl Into<EventTypeRef>) -> Self {
        self.event_type = Some(event_type.into());
        self
    }

    fn push_tx_route(
        mut self,
        handler_kind: ConsumerRouteHandlerKind,
        handler: Arc<dyn TxConsumerHandler<M>>,
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
    pub fn handler<RH>(self, _handler: RH) -> ConsumerRoutedReady<BrokerOnly<M>, H>
    where
        RH: SingleEventHandler + 'static,
    {
        let handler = super::SingleEventHandlerAdapter::new(Arc::new(_handler));
        self.push_broker_route(ConsumerRouteHandlerKind::Single, Arc::new(handler))
    }

    pub fn batch_handler<RH>(self, _handler: RH) -> ConsumerRoutedReady<BrokerOnly<M>, H>
    where
        RH: ConsumerHandler + 'static,
    {
        self.push_broker_route(ConsumerRouteHandlerKind::Batch, Arc::new(_handler))
    }
}

#[cfg(feature = "db")]
impl<M> TxConsumerRouteBuilder<M, RouteHasTopic>
where
    M: CommitOffsetInTx + 'static,
{
    pub fn handler<RH>(self, _handler: RH) -> TxConsumerRoutedReady<M>
    where
        RH: TxSingleEventHandler<M> + 'static,
    {
        let handler = super::TxSingleEventHandlerAdapter::new(Arc::new(_handler));
        self.push_tx_route(ConsumerRouteHandlerKind::Single, Arc::new(handler))
    }

    pub fn batch_handler<RH>(self, _handler: RH) -> TxConsumerRoutedReady<M>
    where
        RH: TxConsumerHandler<M> + 'static,
    {
        self.push_tx_route(ConsumerRouteHandlerKind::Batch, Arc::new(_handler))
    }
}

// ---- Terminal build methods on ConsumerReady ----

// -- BrokerOnly (async-commit) --

impl<M, H> ConsumerReady<BrokerOnly<M>, H>
where
    M: CommitOffset + 'static,
    H: SingleEventHandler + 'static,
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
    H: ConsumerHandler + 'static,
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
    H: TxConsumerHandler<M> + 'static,
{
    pub async fn start(self) -> Result<ConsumerHandle, EventBrokerError> {
        let consumer = Consumer::new_with_tx_batch_slots(self.builder, self.handler).await?;
        Ok(ConsumerHandle::from_consumer(consumer))
    }
}

impl<M, H> ConsumerRoutedReady<BrokerOnly<M>, H>
where
    M: CommitOffset + 'static,
    H: SingleEventHandler + 'static,
{
    pub async fn start(self) -> Result<ConsumerHandle, EventBrokerError> {
        self.validate_routes()?;
        let default_handler = super::SingleEventHandlerAdapter::new(Arc::new(self.default_handler));
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
    H: TxSingleEventHandler<M> + 'static,
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
