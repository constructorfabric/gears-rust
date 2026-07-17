use std::sync::Arc;

use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::consumer::builder::ConsumerBuilder;
#[cfg(feature = "db")]
use crate::consumer::builder::WithTx;
use crate::consumer::dispatcher::SlotDispatcher;
#[cfg(feature = "db")]
use crate::consumer::dispatcher::TxSlotDispatcher;
use crate::consumer::offset_manager::CommitOffset;
use crate::consumer::{
    BatchHandlerOutcome, ConsumerGroupRef, ConsumerHandler, SingleEventHandler,
    SingleEventHandlerAdapter,
};
#[cfg(feature = "db")]
use crate::consumer::{CommitOffsetInTx, TxConsumerHandler, TxSingleEventHandlerAdapter};
use crate::consumer::{ConsumerRoute, EventTypeRef, TopicRef};
use crate::error::{ConsumerError, EventBrokerError};
use crate::ids::{EventTypeId, SubscriptionId, TopicId};

struct SlotHandle {
    subscription_id: Arc<tokio::sync::Mutex<Option<SubscriptionId>>>,
    cancel: CancellationToken,
    join: JoinHandle<Result<(), EventBrokerError>>,
}

/// Running consumer handle. Carries N subscription task handles.
pub struct Consumer {
    slots: Vec<SlotHandle>,
}

/// Public lifecycle handle returned by `ConsumerReady::start()`.
pub struct ConsumerHandle {
    consumer: Consumer,
}

pub(crate) struct RoutedHandlerEntry {
    route: ConsumerRoute,
    handler: Arc<dyn ConsumerHandler>,
}

pub(crate) struct RoutedBatchHandler {
    default_handler: Option<Arc<dyn ConsumerHandler>>,
    routes: Vec<RoutedHandlerEntry>,
}

impl RoutedBatchHandler {
    pub(crate) fn new(
        default_handler: Option<Arc<dyn ConsumerHandler>>,
        routes: Vec<ConsumerRoute>,
        route_handlers: Vec<Arc<dyn ConsumerHandler>>,
    ) -> Result<Self, EventBrokerError> {
        if routes.len() != route_handlers.len() {
            return Err(EventBrokerError::Internal(format!(
                "routed consumer has {} route descriptors but {} handlers",
                routes.len(),
                route_handlers.len()
            )));
        }

        let routes = routes
            .into_iter()
            .zip(route_handlers)
            .map(|(route, handler)| RoutedHandlerEntry { route, handler })
            .collect();
        Ok(Self {
            default_handler,
            routes,
        })
    }
}

#[cfg(feature = "db")]
pub(crate) struct TxRoutedHandlerEntry<OM: CommitOffsetInTx> {
    route: ConsumerRoute,
    handler: Arc<dyn TxConsumerHandler<OM>>,
}

#[cfg(feature = "db")]
pub(crate) struct TxRoutedBatchHandler<OM: CommitOffsetInTx> {
    default_handler: Option<Arc<dyn TxConsumerHandler<OM>>>,
    routes: Vec<TxRoutedHandlerEntry<OM>>,
}

#[cfg(feature = "db")]
impl<OM: CommitOffsetInTx> TxRoutedBatchHandler<OM> {
    pub(crate) fn new(
        default_handler: Option<Arc<dyn TxConsumerHandler<OM>>>,
        routes: Vec<ConsumerRoute>,
        route_handlers: Vec<Arc<dyn TxConsumerHandler<OM>>>,
    ) -> Result<Self, EventBrokerError> {
        if routes.len() != route_handlers.len() {
            return Err(EventBrokerError::Internal(format!(
                "transactional routed consumer has {} route descriptors but {} handlers",
                routes.len(),
                route_handlers.len()
            )));
        }

        let routes = routes
            .into_iter()
            .zip(route_handlers)
            .map(|(route, handler)| TxRoutedHandlerEntry { route, handler })
            .collect();
        Ok(Self {
            default_handler,
            routes,
        })
    }
}

#[cfg(feature = "db")]
#[async_trait::async_trait]
impl<OM> TxConsumerHandler<OM> for TxRoutedBatchHandler<OM>
where
    OM: CommitOffsetInTx + 'static,
{
    async fn handle_batch(
        &self,
        batch: &crate::consumer::EventBatch<'_>,
        attempts: u16,
        commit: crate::consumer::TxCommitHandle<OM>,
    ) -> Result<crate::consumer::HandlerOutcome, ConsumerError> {
        let Some(event) = batch.next_event() else {
            return Ok(crate::consumer::HandlerOutcome::Success);
        };

        if let Some(entry) = self
            .routes
            .iter()
            .find(|entry| route_matches(&entry.route, event))
        {
            return entry.handler.handle_batch(batch, attempts, commit).await;
        }

        if let Some(default_handler) = &self.default_handler {
            return default_handler.handle_batch(batch, attempts, commit).await;
        }

        Err(EventBrokerError::InvalidConsumerOptions {
            detail: format!(
                "no transactional consumer route matched topic '{}' and event type '{}'",
                event.topic, event.type_id
            ),
            instance: String::new(),
        })
    }
}

#[async_trait::async_trait]
impl ConsumerHandler for RoutedBatchHandler {
    async fn handle_batch(
        &self,
        batch: &crate::consumer::EventBatch<'_>,
        attempts: u16,
    ) -> Result<BatchHandlerOutcome, ConsumerError> {
        let Some(event) = batch.next_event() else {
            return Ok(BatchHandlerOutcome::Success);
        };

        if let Some(entry) = self
            .routes
            .iter()
            .find(|entry| route_matches(&entry.route, event))
        {
            return entry.handler.handle_batch(batch, attempts).await;
        }

        if let Some(default_handler) = &self.default_handler {
            return default_handler.handle_batch(batch, attempts).await;
        }

        Err(EventBrokerError::InvalidConsumerOptions {
            detail: format!(
                "no consumer route matched topic '{}' and event type '{}'",
                event.topic, event.type_id
            ),
            instance: String::new(),
        })
    }
}

fn route_matches(route: &ConsumerRoute, event: &crate::consumer::RawEvent) -> bool {
    topic_matches(&route.topic, &event.topic)
        && route
            .event_type
            .as_ref()
            .is_none_or(|event_type| event_type_matches(event_type, &event.type_id))
}

fn topic_matches(route_topic: &TopicRef, event_topic: &str) -> bool {
    match route_topic {
        TopicRef::Gts(gts) => gts == event_topic,
        TopicRef::Id(id) => *id == TopicId::from_gts(event_topic),
    }
}

fn event_type_matches(route_type: &EventTypeRef, event_type: &str) -> bool {
    match route_type {
        EventTypeRef::Gts(gts) => gts == event_type,
        EventTypeRef::Id(id) => *id == EventTypeId::from_gts(event_type),
        EventTypeRef::GtsPattern(pattern) => gts_pattern_matches(pattern, event_type),
    }
}

fn gts_pattern_matches(pattern: &str, value: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    pattern
        .strip_suffix('*')
        .map_or(pattern == value, |prefix| value.starts_with(prefix))
}

impl ConsumerHandle {
    pub(crate) fn from_consumer(consumer: Consumer) -> Self {
        Self { consumer }
    }

    /// Gracefully stop the consumer and await all subscription slots.
    pub async fn stop(self) -> Result<(), EventBrokerError> {
        self.consumer.shutdown().await
    }

    /// Current subscription ids (one per active parallelism slot).
    pub fn subscription_ids(&self) -> Vec<SubscriptionId> {
        self.consumer.subscription_ids()
    }
}

impl Consumer {
    pub fn new(parallelism: u32) -> Self {
        let _ = parallelism;
        Self { slots: Vec::new() }
    }

    pub(crate) async fn new_with_slots<M, H>(
        builder: ConsumerBuilder<crate::consumer::builder::BrokerOnly<M>>,
        handler: H,
    ) -> Result<Self, EventBrokerError>
    where
        M: CommitOffset + 'static,
        H: SingleEventHandler + 'static,
    {
        let handler = SingleEventHandlerAdapter::new(Arc::new(handler));
        Self::new_with_batch_slots(builder, handler).await
    }

    pub(crate) async fn new_with_batch_slots<M, H>(
        builder: ConsumerBuilder<crate::consumer::builder::BrokerOnly<M>>,
        handler: H,
    ) -> Result<Self, EventBrokerError>
    where
        M: CommitOffset + 'static,
        H: ConsumerHandler + 'static,
    {
        Self::new_with_batch_slots_with_cancel(builder, handler, None).await
    }

    async fn new_with_batch_slots_with_cancel<M, H>(
        builder: ConsumerBuilder<crate::consumer::builder::BrokerOnly<M>>,
        handler: H,
        shared_cancel: Option<CancellationToken>,
    ) -> Result<Self, EventBrokerError>
    where
        M: CommitOffset + 'static,
        H: ConsumerHandler + 'static,
    {
        let settings = builder.effective_settings()?;
        let parallelism = builder.parallelism;
        let broker = builder.broker.ok_or_else(|| {
            EventBrokerError::Internal(
                "ConsumerBuilder: broker not wired; use EventBroker::consumer_builder()".into(),
            )
        })?;
        let handler = Arc::new(handler);
        let offset_manager = Arc::new(builder.offset_manager.0);

        let mut slots = Vec::with_capacity(parallelism as usize);
        let ctx_arc = Arc::new(builder.security_context);

        for idx in 0..parallelism {
            let sub_id = Arc::new(tokio::sync::Mutex::new(None));
            let cancel = shared_cancel.clone().unwrap_or_default();

            let dispatcher = SlotDispatcher {
                slot_idx: idx,
                broker: broker.clone(),
                offset_manager: offset_manager.clone(),
                handler: handler.clone(),
                group_ref: builder
                    .group
                    .clone()
                    .unwrap_or(ConsumerGroupRef::AutoAnonymous {
                        alias: builder.client_agent.clone(),
                    }),
                topics: builder.topics.clone(),
                subscription_interests: builder.subscription_interests.clone(),
                tenant_id: builder.tenant_id,
                tenant_depth: builder.tenant_depth,
                barrier_mode: builder.barrier_mode,
                event_type_patterns: builder.event_type_patterns.clone(),
                client_agent: builder.client_agent.clone(),
                session_timeout: builder.session_timeout,
                filter: builder.filter.clone(),
                heartbeat_drop_threshold: builder.heartbeat_drop_threshold,
                retry_base: settings.retry.base_delay,
                retry_max: settings.retry.max_delay,
                commit_mode: builder.commit_mode,
                partition_buffer_capacity: settings.buffering.partition_capacity,
                buffer_high_watermark: settings.buffering.high_watermark,
                buffer_low_watermark: settings.buffering.low_watermark,
                batch_max_events: settings.batching.max_events,
                handler_latency: settings.slow_detection.handler_latency,
                handler_strikes: settings.slow_detection.handler_strikes,
                listeners: builder.listeners.clone(),
                listener_timeout: settings.listener.timeout,
                max_rejoin_attempts: 16,
                subscription_id: sub_id.clone(),
            };

            let task_ctx = ctx_arc.clone();
            let task_cancel = cancel.clone();
            let join = tokio::spawn(async move { dispatcher.run(task_ctx, task_cancel).await });

            slots.push(SlotHandle {
                subscription_id: sub_id,
                cancel,
                join,
            });
        }

        Ok(Self { slots })
    }

    pub(crate) async fn new_with_routed_slots<M>(
        builder: ConsumerBuilder<crate::consumer::builder::BrokerOnly<M>>,
        default_handler: Option<Arc<dyn ConsumerHandler>>,
        routes: Vec<ConsumerRoute>,
        route_handlers: Vec<Arc<dyn ConsumerHandler>>,
    ) -> Result<Self, EventBrokerError>
    where
        M: CommitOffset + 'static,
    {
        let handler = RoutedBatchHandler::new(default_handler, routes, route_handlers)?;

        Self::new_with_batch_slots(builder, handler).await
    }

    #[cfg(feature = "db")]
    pub(crate) async fn new_with_tx_slots<M, H>(
        builder: ConsumerBuilder<WithTx<M>>,
        handler: H,
    ) -> Result<Self, EventBrokerError>
    where
        M: CommitOffsetInTx + 'static,
        H: crate::consumer::TxSingleEventHandler<M> + 'static,
    {
        let handler = TxSingleEventHandlerAdapter::new(Arc::new(handler));
        Self::new_with_tx_batch_slots(builder, handler).await
    }

    #[cfg(feature = "db")]
    pub(crate) async fn new_with_tx_batch_slots<M, H>(
        builder: ConsumerBuilder<WithTx<M>>,
        handler: H,
    ) -> Result<Self, EventBrokerError>
    where
        M: CommitOffsetInTx + 'static,
        H: TxConsumerHandler<M> + 'static,
    {
        let settings = builder.effective_settings()?;
        let parallelism = builder.parallelism;
        let broker = builder.broker.ok_or_else(|| {
            EventBrokerError::Internal(
                "ConsumerBuilder: broker not wired; use EventBroker::consumer_builder()".into(),
            )
        })?;
        let handler = Arc::new(handler);
        let offset_manager = Arc::new(builder.offset_manager.0);

        let mut slots = Vec::with_capacity(parallelism as usize);
        let ctx_arc = Arc::new(builder.security_context);

        for idx in 0..parallelism {
            let sub_id = Arc::new(tokio::sync::Mutex::new(None));
            let cancel = CancellationToken::new();

            let dispatcher = TxSlotDispatcher {
                slot_idx: idx,
                broker: broker.clone(),
                offset_manager: offset_manager.clone(),
                handler: handler.clone(),
                group_ref: builder
                    .group
                    .clone()
                    .unwrap_or(ConsumerGroupRef::AutoAnonymous {
                        alias: builder.client_agent.clone(),
                    }),
                topics: builder.topics.clone(),
                subscription_interests: builder.subscription_interests.clone(),
                tenant_id: builder.tenant_id,
                tenant_depth: builder.tenant_depth,
                barrier_mode: builder.barrier_mode,
                event_type_patterns: builder.event_type_patterns.clone(),
                client_agent: builder.client_agent.clone(),
                session_timeout: builder.session_timeout,
                filter: builder.filter.clone(),
                heartbeat_drop_threshold: builder.heartbeat_drop_threshold,
                retry_base: settings.retry.base_delay,
                retry_max: settings.retry.max_delay,
                partition_buffer_capacity: settings.buffering.partition_capacity,
                buffer_high_watermark: settings.buffering.high_watermark,
                buffer_low_watermark: settings.buffering.low_watermark,
                batch_max_events: settings.batching.max_events,
                handler_latency: settings.slow_detection.handler_latency,
                handler_strikes: settings.slow_detection.handler_strikes,
                listeners: builder.listeners.clone(),
                listener_timeout: settings.listener.timeout,
                max_rejoin_attempts: 16,
                subscription_id: sub_id.clone(),
            };

            let task_ctx = ctx_arc.clone();
            let task_cancel = cancel.clone();
            let join = tokio::spawn(async move { dispatcher.run(task_ctx, task_cancel).await });

            slots.push(SlotHandle {
                subscription_id: sub_id,
                cancel,
                join,
            });
        }

        Ok(Self { slots })
    }

    #[cfg(feature = "db")]
    pub(crate) async fn new_with_tx_routed_slots<M>(
        builder: ConsumerBuilder<WithTx<M>>,
        default_handler: Option<Arc<dyn TxConsumerHandler<M>>>,
        routes: Vec<ConsumerRoute>,
        route_handlers: Vec<Arc<dyn TxConsumerHandler<M>>>,
    ) -> Result<Self, EventBrokerError>
    where
        M: CommitOffsetInTx + 'static,
    {
        let handler = TxRoutedBatchHandler::new(default_handler, routes, route_handlers)?;

        Self::new_with_tx_batch_slots(builder, handler).await
    }

    /// Graceful shutdown: cancel all tasks and await drain.
    pub async fn shutdown(mut self) -> Result<(), EventBrokerError> {
        for slot in &self.slots {
            slot.cancel.cancel();
        }
        for slot in self.slots.drain(..) {
            let _ = slot.join.await;
        }
        Ok(())
    }

    /// Current subscription ids (one per parallelism slot).
    pub fn subscription_ids(&self) -> Vec<SubscriptionId> {
        self.slots
            .iter()
            .filter_map(|s| s.subscription_id.try_lock().ok().and_then(|g| *g))
            .collect()
    }
}
