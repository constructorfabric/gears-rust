use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::api::EventBroker;
use crate::api::{BarrierMode, Filter, TenantTraversalDepth};
use crate::error::EventBrokerError;
use crate::sdk::EventBrokerSdk;

use super::consumer::{Consumer, ConsumerHandle};
use super::{
    BatchEventHandler, CommitHandle, CommitOffset, ConsumerBatching, ConsumerBuffering,
    ConsumerCommitMode, ConsumerGroupRef, ConsumerListenerSettings, ConsumerProfile, ConsumerRetry,
    ConsumerSettings, ConsumerSettingsOverrides, ConsumerSlowDetection, DeadLetterEvent,
    EventHandler, EventTypeRef, HandlerOutcome, RejectableOutcome, TopicRef,
};
#[cfg(feature = "db")]
use super::{CommitOffsetInTx, TxCommitHandle};

// ---- Typestate markers (zero-sized) ----

pub struct NoDlq;
pub struct WithDlq;
pub struct BrokerOnly<M: CommitOffset + 'static>(pub M);

#[cfg(feature = "db")]
pub struct WithTx<M: CommitOffsetInTx + 'static>(pub M);

// ---- Builder ----

pub struct ConsumerBuilder<D = NoDlq, M = ()> {
    pub(crate) group: Option<ConsumerGroupRef>,
    pub(crate) topics: Vec<String>,
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
    pub(crate) _d: std::marker::PhantomData<D>,
    pub(crate) offset_manager: M,
    /// Broker client resolved from ClientHub or supplied by tests.
    pub(crate) broker: Option<Arc<dyn EventBroker>>,
    /// Security context used by the SDK runtime for EventBroker calls.
    pub(crate) security_context: SecurityContext,
}

impl ConsumerBuilder<NoDlq, ()> {
    pub fn new(broker: Arc<dyn EventBroker>) -> Self {
        Self {
            group: None,
            topics: Vec::new(),
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
            _d: std::marker::PhantomData,
            offset_manager: (),
            broker: Some(broker),
            security_context: SecurityContext::anonymous(),
        }
    }

    pub fn new_unbound() -> Self {
        Self {
            group: None,
            topics: Vec::new(),
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
            _d: std::marker::PhantomData,
            offset_manager: (),
            broker: None,
            security_context: SecurityContext::anonymous(),
        }
    }
}

// Common methods available in all typestate combinations.
macro_rules! common_builder_methods {
    ($D:ty, $M:ty) => {
        impl ConsumerBuilder<$D, $M> {
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
            pub fn commit_mode(mut self, mode: ConsumerCommitMode) -> Self {
                self.commit_mode = mode;
                self
            }
            pub fn security_context(mut self, ctx: SecurityContext) -> Self {
                self.security_context = ctx;
                self
            }
        }
    };
}

impl<D, M> ConsumerBuilder<D, M> {
    pub(crate) fn effective_settings(&self) -> Result<ConsumerSettings, EventBrokerError> {
        let settings = ConsumerSettings::resolve(self.profile.clone(), self.settings_overrides);
        settings.validate()?;
        Ok(settings)
    }
}

// Instantiate for all typestate combinations we need.
common_builder_methods!(NoDlq, ());
common_builder_methods!(WithDlq, ());

// on_dead_letter - transitions D from NoDlq to WithDlq
impl<M> ConsumerBuilder<NoDlq, M> {
    pub fn on_dead_letter<F, Fut>(self, _cb: F) -> ConsumerBuilder<WithDlq, M>
    where
        F: Fn(DeadLetterEvent) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<(), crate::error::ConsumerError>> + Send + 'static,
    {
        ConsumerBuilder {
            group: self.group,
            topics: self.topics,
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
            _d: std::marker::PhantomData,
            offset_manager: self.offset_manager,
            broker: self.broker,
            security_context: self.security_context,
        }
    }
}

// offset_manager / tx_offset_manager - transitions M
impl<D> ConsumerBuilder<D, ()> {
    pub fn offset_manager<M: CommitOffset + 'static>(
        self,
        m: M,
    ) -> ConsumerBuilder<D, BrokerOnly<M>> {
        ConsumerBuilder {
            group: self.group,
            topics: self.topics,
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
            _d: std::marker::PhantomData,
            offset_manager: BrokerOnly(m),
            broker: self.broker,
            security_context: self.security_context,
        }
    }

    #[cfg(feature = "db")]
    pub fn tx_offset_manager<M: CommitOffsetInTx + 'static>(
        self,
        m: M,
    ) -> ConsumerBuilder<D, WithTx<M>> {
        ConsumerBuilder {
            group: self.group,
            topics: self.topics,
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
            _d: std::marker::PhantomData,
            offset_manager: WithTx(m),
            broker: self.broker,
            security_context: self.security_context,
        }
    }
}

// ---- Four terminal `handler()` impls - one per (D, M) quadrant ----

pub struct ConsumerReady<D, M, H> {
    pub(crate) builder: ConsumerBuilder<D, M>,
    pub(crate) handler: H,
}

pub struct ConsumerBatchReady<D, M, H> {
    pub(crate) builder: ConsumerBuilder<D, M>,
    pub(crate) handler: H,
}

pub struct ConsumerRoutedReady<D, M, H> {
    pub(crate) builder: ConsumerBuilder<D, M>,
    pub(crate) default_handler: H,
    pub(crate) has_default_handler: bool,
    pub(crate) routes: Vec<ConsumerRoute>,
}

pub struct NoDefaultHandler;

pub struct RouteMissingTopic;
pub struct RouteHasTopic;

pub struct ConsumerRouteBuilder<D, M, H, T = RouteMissingTopic> {
    ready: ConsumerRoutedReady<D, M, H>,
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

impl<M: CommitOffset + 'static> ConsumerBuilder<NoDlq, BrokerOnly<M>> {
    pub fn handler<H>(self, handler: H) -> ConsumerReady<NoDlq, BrokerOnly<M>, H>
    where
        H: EventHandler<CommitHandle, HandlerOutcome> + 'static,
    {
        ConsumerReady {
            builder: self,
            handler,
        }
    }

    pub fn batch_handler<H>(self, handler: H) -> ConsumerBatchReady<NoDlq, BrokerOnly<M>, H>
    where
        H: BatchEventHandler<CommitHandle, HandlerOutcome> + 'static,
    {
        ConsumerBatchReady {
            builder: self,
            handler,
        }
    }

    pub fn default_handler<H>(self, handler: H) -> ConsumerRoutedReady<NoDlq, BrokerOnly<M>, H>
    where
        H: EventHandler<CommitHandle, HandlerOutcome> + 'static,
    {
        ConsumerRoutedReady {
            builder: self,
            default_handler: handler,
            has_default_handler: true,
            routes: Vec::new(),
        }
    }

    pub fn route(
        self,
    ) -> ConsumerRouteBuilder<NoDlq, BrokerOnly<M>, NoDefaultHandler, RouteMissingTopic> {
        ConsumerRouteBuilder {
            ready: ConsumerRoutedReady {
                builder: self,
                default_handler: NoDefaultHandler,
                has_default_handler: false,
                routes: Vec::new(),
            },
            topic: None,
            event_type: None,
            _topic_state: std::marker::PhantomData,
        }
    }
}

impl<M: CommitOffset + 'static> ConsumerBuilder<WithDlq, BrokerOnly<M>> {
    pub fn handler<H>(self, handler: H) -> ConsumerReady<WithDlq, BrokerOnly<M>, H>
    where
        H: EventHandler<CommitHandle, RejectableOutcome> + 'static,
    {
        ConsumerReady {
            builder: self,
            handler,
        }
    }

    pub fn batch_handler<H>(self, handler: H) -> ConsumerBatchReady<WithDlq, BrokerOnly<M>, H>
    where
        H: BatchEventHandler<CommitHandle, RejectableOutcome> + 'static,
    {
        ConsumerBatchReady {
            builder: self,
            handler,
        }
    }

    pub fn default_handler<H>(self, handler: H) -> ConsumerRoutedReady<WithDlq, BrokerOnly<M>, H>
    where
        H: EventHandler<CommitHandle, RejectableOutcome> + 'static,
    {
        ConsumerRoutedReady {
            builder: self,
            default_handler: handler,
            has_default_handler: true,
            routes: Vec::new(),
        }
    }

    pub fn route(
        self,
    ) -> ConsumerRouteBuilder<WithDlq, BrokerOnly<M>, NoDefaultHandler, RouteMissingTopic> {
        ConsumerRouteBuilder {
            ready: ConsumerRoutedReady {
                builder: self,
                default_handler: NoDefaultHandler,
                has_default_handler: false,
                routes: Vec::new(),
            },
            topic: None,
            event_type: None,
            _topic_state: std::marker::PhantomData,
        }
    }
}

#[cfg(feature = "db")]
impl<M: CommitOffsetInTx + 'static> ConsumerBuilder<NoDlq, WithTx<M>> {
    pub fn handler<H>(self, handler: H) -> ConsumerReady<NoDlq, WithTx<M>, H>
    where
        H: EventHandler<TxCommitHandle<M>, HandlerOutcome> + 'static,
    {
        ConsumerReady {
            builder: self,
            handler,
        }
    }

    pub fn batch_handler<H>(self, handler: H) -> ConsumerBatchReady<NoDlq, WithTx<M>, H>
    where
        H: BatchEventHandler<TxCommitHandle<M>, HandlerOutcome> + 'static,
    {
        ConsumerBatchReady {
            builder: self,
            handler,
        }
    }

    pub fn default_handler<H>(self, handler: H) -> ConsumerRoutedReady<NoDlq, WithTx<M>, H>
    where
        H: EventHandler<TxCommitHandle<M>, HandlerOutcome> + 'static,
    {
        ConsumerRoutedReady {
            builder: self,
            default_handler: handler,
            has_default_handler: true,
            routes: Vec::new(),
        }
    }

    pub fn route(
        self,
    ) -> ConsumerRouteBuilder<NoDlq, WithTx<M>, NoDefaultHandler, RouteMissingTopic> {
        ConsumerRouteBuilder {
            ready: ConsumerRoutedReady {
                builder: self,
                default_handler: NoDefaultHandler,
                has_default_handler: false,
                routes: Vec::new(),
            },
            topic: None,
            event_type: None,
            _topic_state: std::marker::PhantomData,
        }
    }
}

#[cfg(feature = "db")]
impl<M: CommitOffsetInTx + 'static> ConsumerBuilder<WithDlq, WithTx<M>> {
    pub fn handler<H>(self, handler: H) -> ConsumerReady<WithDlq, WithTx<M>, H>
    where
        H: EventHandler<TxCommitHandle<M>, RejectableOutcome> + 'static,
    {
        ConsumerReady {
            builder: self,
            handler,
        }
    }

    pub fn batch_handler<H>(self, handler: H) -> ConsumerBatchReady<WithDlq, WithTx<M>, H>
    where
        H: BatchEventHandler<TxCommitHandle<M>, RejectableOutcome> + 'static,
    {
        ConsumerBatchReady {
            builder: self,
            handler,
        }
    }

    pub fn default_handler<H>(self, handler: H) -> ConsumerRoutedReady<WithDlq, WithTx<M>, H>
    where
        H: EventHandler<TxCommitHandle<M>, RejectableOutcome> + 'static,
    {
        ConsumerRoutedReady {
            builder: self,
            default_handler: handler,
            has_default_handler: true,
            routes: Vec::new(),
        }
    }

    pub fn route(
        self,
    ) -> ConsumerRouteBuilder<WithDlq, WithTx<M>, NoDefaultHandler, RouteMissingTopic> {
        ConsumerRouteBuilder {
            ready: ConsumerRoutedReady {
                builder: self,
                default_handler: NoDefaultHandler,
                has_default_handler: false,
                routes: Vec::new(),
            },
            topic: None,
            event_type: None,
            _topic_state: std::marker::PhantomData,
        }
    }
}

impl<D, M, H> ConsumerRoutedReady<D, M, H> {
    pub fn route(self) -> ConsumerRouteBuilder<D, M, H, RouteMissingTopic> {
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

fn topic_ref_matches_configured(route_topic: &TopicRef, configured: &str) -> bool {
    match route_topic {
        TopicRef::Gts(gts) => gts == configured,
        TopicRef::Id(id) => *id == crate::ids::TopicId::from_gts(configured),
    }
}

impl<D, M, H> ConsumerRouteBuilder<D, M, H, RouteMissingTopic> {
    pub fn topic(self, topic: impl Into<TopicRef>) -> ConsumerRouteBuilder<D, M, H, RouteHasTopic> {
        ConsumerRouteBuilder {
            ready: self.ready,
            topic: Some(topic.into()),
            event_type: self.event_type,
            _topic_state: std::marker::PhantomData,
        }
    }
}

impl<D, M, H> ConsumerRouteBuilder<D, M, H, RouteHasTopic> {
    pub fn topic(mut self, topic: impl Into<TopicRef>) -> Self {
        self.topic = Some(topic.into());
        self
    }

    pub fn event_type(mut self, event_type: impl Into<EventTypeRef>) -> Self {
        self.event_type = Some(event_type.into());
        self
    }

    fn push_route(
        mut self,
        handler_kind: ConsumerRouteHandlerKind,
    ) -> ConsumerRoutedReady<D, M, H> {
        let topic = self
            .topic
            .expect("route topic is required before registering a handler");
        self.ready.routes.push(ConsumerRoute {
            topic,
            event_type: self.event_type,
            handler_kind,
        });
        self.ready
    }
}

impl<M, H> ConsumerRouteBuilder<NoDlq, BrokerOnly<M>, H, RouteHasTopic>
where
    M: CommitOffset + 'static,
{
    pub fn handler<RH>(self, _handler: RH) -> ConsumerRoutedReady<NoDlq, BrokerOnly<M>, H>
    where
        RH: EventHandler<CommitHandle, HandlerOutcome> + 'static,
    {
        self.push_route(ConsumerRouteHandlerKind::Single)
    }

    pub fn batch_handler<RH>(self, _handler: RH) -> ConsumerRoutedReady<NoDlq, BrokerOnly<M>, H>
    where
        RH: BatchEventHandler<CommitHandle, HandlerOutcome> + 'static,
    {
        self.push_route(ConsumerRouteHandlerKind::Batch)
    }
}

impl<M, H> ConsumerRouteBuilder<WithDlq, BrokerOnly<M>, H, RouteHasTopic>
where
    M: CommitOffset + 'static,
{
    pub fn handler<RH>(self, _handler: RH) -> ConsumerRoutedReady<WithDlq, BrokerOnly<M>, H>
    where
        RH: EventHandler<CommitHandle, RejectableOutcome> + 'static,
    {
        self.push_route(ConsumerRouteHandlerKind::Single)
    }

    pub fn batch_handler<RH>(self, _handler: RH) -> ConsumerRoutedReady<WithDlq, BrokerOnly<M>, H>
    where
        RH: BatchEventHandler<CommitHandle, RejectableOutcome> + 'static,
    {
        self.push_route(ConsumerRouteHandlerKind::Batch)
    }
}

#[cfg(feature = "db")]
impl<M, H> ConsumerRouteBuilder<NoDlq, WithTx<M>, H, RouteHasTopic>
where
    M: CommitOffsetInTx + 'static,
{
    pub fn handler<RH>(self, _handler: RH) -> ConsumerRoutedReady<NoDlq, WithTx<M>, H>
    where
        RH: EventHandler<TxCommitHandle<M>, HandlerOutcome> + 'static,
    {
        self.push_route(ConsumerRouteHandlerKind::Single)
    }

    pub fn batch_handler<RH>(self, _handler: RH) -> ConsumerRoutedReady<NoDlq, WithTx<M>, H>
    where
        RH: BatchEventHandler<TxCommitHandle<M>, HandlerOutcome> + 'static,
    {
        self.push_route(ConsumerRouteHandlerKind::Batch)
    }
}

#[cfg(feature = "db")]
impl<M, H> ConsumerRouteBuilder<WithDlq, WithTx<M>, H, RouteHasTopic>
where
    M: CommitOffsetInTx + 'static,
{
    pub fn handler<RH>(self, _handler: RH) -> ConsumerRoutedReady<WithDlq, WithTx<M>, H>
    where
        RH: EventHandler<TxCommitHandle<M>, RejectableOutcome> + 'static,
    {
        self.push_route(ConsumerRouteHandlerKind::Single)
    }

    pub fn batch_handler<RH>(self, _handler: RH) -> ConsumerRoutedReady<WithDlq, WithTx<M>, H>
    where
        RH: BatchEventHandler<TxCommitHandle<M>, RejectableOutcome> + 'static,
    {
        self.push_route(ConsumerRouteHandlerKind::Batch)
    }
}

// ---- Terminal build methods on ConsumerReady ----

// -- NoDlq + BrokerOnly (no DLQ, async-commit) --

impl<M, H> ConsumerReady<NoDlq, BrokerOnly<M>, H>
where
    M: CommitOffset + 'static,
    H: EventHandler<CommitHandle, HandlerOutcome> + 'static,
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

impl<M, H> ConsumerBatchReady<NoDlq, BrokerOnly<M>, H>
where
    M: CommitOffset + 'static,
    H: BatchEventHandler<CommitHandle, HandlerOutcome> + 'static,
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

impl<M, H> ConsumerBatchReady<WithDlq, BrokerOnly<M>, H>
where
    M: CommitOffset + 'static,
    H: BatchEventHandler<CommitHandle, RejectableOutcome> + 'static,
{
    pub async fn start(self) -> Result<ConsumerHandle, EventBrokerError> {
        let _ = (self.builder, self.handler);
        Err(EventBrokerError::Internal(
            "DLQ batch consumer runtime is not implemented yet".into(),
        ))
    }
}

#[cfg(feature = "db")]
impl<M, H> ConsumerBatchReady<NoDlq, WithTx<M>, H>
where
    M: CommitOffsetInTx + 'static,
    H: BatchEventHandler<TxCommitHandle<M>, HandlerOutcome> + 'static,
{
    pub async fn start(self) -> Result<ConsumerHandle, EventBrokerError> {
        let _ = (self.builder, self.handler);
        todo!("NoDlq + WithTx batch start - task 19")
    }
}

#[cfg(feature = "db")]
impl<M, H> ConsumerBatchReady<WithDlq, WithTx<M>, H>
where
    M: CommitOffsetInTx + 'static,
    H: BatchEventHandler<TxCommitHandle<M>, RejectableOutcome> + 'static,
{
    pub async fn start(self) -> Result<ConsumerHandle, EventBrokerError> {
        let _ = (self.builder, self.handler);
        todo!("WithDlq + WithTx batch start - task 19")
    }
}

impl<D, M, H> ConsumerRoutedReady<D, M, H> {
    pub async fn start(self) -> Result<ConsumerHandle, EventBrokerError> {
        self.validate_routes()?;
        let _ = (self.builder, self.default_handler, self.routes);
        Err(EventBrokerError::Internal(
            "routed consumer runtime is not implemented yet".into(),
        ))
    }
}

// -- WithDlq + BrokerOnly --

impl<M, H> ConsumerReady<WithDlq, BrokerOnly<M>, H>
where
    M: CommitOffset + 'static,
    H: EventHandler<CommitHandle, RejectableOutcome> + 'static,
{
    pub async fn start(self) -> Result<ConsumerHandle, EventBrokerError> {
        // DLQ path reuses the same dispatcher structure; handler outcome is different.
        // Full DLQ callback wiring is task 19.6; placeholder here so the crate compiles.
        let consumer = Consumer::new_with_slots_dlq(self.builder, self.handler).await?;
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

// -- NoDlq + WithTx (db feature) --

#[cfg(feature = "db")]
impl<M, H> ConsumerReady<NoDlq, WithTx<M>, H>
where
    M: CommitOffsetInTx + 'static,
    H: EventHandler<TxCommitHandle<M>, HandlerOutcome> + 'static,
{
    pub async fn start(self) -> Result<ConsumerHandle, EventBrokerError> {
        todo!("NoDlq + WithTx start - task 19")
    }

    pub async fn run_blocking(
        self,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<(), EventBrokerError> {
        let _ = cancel;
        todo!("NoDlq + WithTx run_blocking - task 19")
    }
}

// -- WithDlq + WithTx (db feature) --

#[cfg(feature = "db")]
impl<M, H> ConsumerReady<WithDlq, WithTx<M>, H>
where
    M: CommitOffsetInTx + 'static,
    H: EventHandler<TxCommitHandle<M>, RejectableOutcome> + 'static,
{
    pub async fn start(self) -> Result<ConsumerHandle, EventBrokerError> {
        todo!("WithDlq + WithTx start - task 19")
    }

    pub async fn run_blocking(
        self,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<(), EventBrokerError> {
        let _ = cancel;
        todo!("WithDlq + WithTx run_blocking - task 19")
    }
}
