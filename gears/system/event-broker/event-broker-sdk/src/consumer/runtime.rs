use std::sync::Arc;

use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::consumer::ConsumerRoute;
use crate::consumer::builder::ConsumerBuilder;
#[cfg(feature = "db")]
use crate::consumer::builder::WithTx;
#[cfg(feature = "db")]
use crate::consumer::dispatcher::TxSlotDispatcher;
use crate::consumer::dispatcher::{SlotDispatcher, SlotSettings};
use crate::consumer::offset_manager::CommitOffset;
use crate::consumer::types::{BatchHandlerAdapter, DeliverBatch, Delivered, EventHandlerAdapter};
#[cfg(feature = "db")]
use crate::consumer::types::{DeliverTxBatch, TxBatchHandlerAdapter, TxEventHandlerAdapter};
use crate::consumer::{BatchHandler, BatchHandlerOutcome, Delivery, EventBatch, EventHandler};
#[cfg(feature = "db")]
use crate::consumer::{
    CommitOffsetInTx, HandlerOutcome, TxBatchHandler, TxCommitHandle, TxEventHandler,
};
use crate::error::{ConsumerError, EventBrokerError};
use crate::gts::StoredPosition;
use crate::ids::SubscriptionId;
use gts::{GtsId, GtsIdPattern, GtsInstanceId, GtsTypeId};

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
    handler: Arc<dyn DeliverBatch>,
}

pub(crate) struct RoutedBatchHandler {
    default_handler: Option<Arc<dyn DeliverBatch>>,
    routes: Vec<RoutedHandlerEntry>,
}

impl RoutedBatchHandler {
    pub(crate) fn new(
        default_handler: Option<Arc<dyn DeliverBatch>>,
        routes: Vec<ConsumerRoute>,
        route_handlers: Vec<Arc<dyn DeliverBatch>>,
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
    handler: Arc<dyn DeliverTxBatch<OM>>,
}

#[cfg(feature = "db")]
pub(crate) struct TxRoutedBatchHandler<OM: CommitOffsetInTx> {
    default_handler: Option<Arc<dyn DeliverTxBatch<OM>>>,
    routes: Vec<TxRoutedHandlerEntry<OM>>,
}

#[cfg(feature = "db")]
impl<OM: CommitOffsetInTx> TxRoutedBatchHandler<OM> {
    pub(crate) fn new(
        default_handler: Option<Arc<dyn DeliverTxBatch<OM>>>,
        routes: Vec<ConsumerRoute>,
        route_handlers: Vec<Arc<dyn DeliverTxBatch<OM>>>,
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
impl<OM> DeliverTxBatch<OM> for TxRoutedBatchHandler<OM>
where
    OM: CommitOffsetInTx + 'static,
{
    async fn deliver(
        &self,
        batch: &EventBatch<'_, serde_json::Value>,
        positions: &[StoredPosition],
        delivery: &Delivery,
        commit: TxCommitHandle<OM>,
    ) -> Result<HandlerOutcome, ConsumerError> {
        let Some(event) = batch.next_event() else {
            return Ok(HandlerOutcome::Success);
        };

        if let Some(entry) = self
            .routes
            .iter()
            .find(|entry| entry.route.matches(delivery.topic(), event.event_type()))
        {
            return entry
                .handler
                .deliver(batch, positions, delivery, commit)
                .await;
        }

        if let Some(default_handler) = &self.default_handler {
            return default_handler
                .deliver(batch, positions, delivery, commit)
                .await;
        }

        Err(EventBrokerError::InvalidConsumerOptions {
            detail: format!(
                "no transactional consumer route matched topic '{}' and event type '{}'",
                delivery.topic(),
                event.event_type()
            ),
        })
    }
}

#[async_trait::async_trait]
impl DeliverBatch for RoutedBatchHandler {
    async fn deliver(
        &self,
        batch: &EventBatch<'_, serde_json::Value>,
        positions: &[StoredPosition],
        delivery: &Delivery,
    ) -> Result<Delivered, ConsumerError> {
        let Some(event) = batch.next_event() else {
            return Ok(BatchHandlerOutcome::Success.into());
        };

        if let Some(entry) = self
            .routes
            .iter()
            .find(|entry| entry.route.matches(delivery.topic(), event.event_type()))
        {
            return entry.handler.deliver(batch, positions, delivery).await;
        }

        if let Some(default_handler) = &self.default_handler {
            return default_handler.deliver(batch, positions, delivery).await;
        }

        Err(EventBrokerError::InvalidConsumerOptions {
            detail: format!(
                "no consumer route matched topic '{}' and event type '{}'",
                delivery.topic(),
                event.event_type()
            ),
        })
    }
}

impl ConsumerRoute {
    /// Whether this route serves an event of `event_type` read from `topic`: the topic
    /// is the route's, and the type matches its pattern or the route has none.
    pub(crate) fn matches(&self, topic: &GtsInstanceId, event_type: &GtsTypeId) -> bool {
        self.topic == *topic
            && self
                .event_type
                .as_ref()
                .is_none_or(|pattern| Self::event_type_matches(pattern, event_type))
    }

    /// Whether a delivered event's type matches a route's GTS pattern, via the
    /// canonical `gts` matcher. The type id is already validated, so a parse failure
    /// simply does not match rather than aborting dispatch.
    fn event_type_matches(pattern: &GtsIdPattern, event_type: &GtsTypeId) -> bool {
        GtsId::try_new(event_type.as_ref()).is_ok_and(|id| id.matches_pattern(pattern))
    }
}

impl ConsumerHandle {
    /// Wraps a started `Consumer` in its public handle.
    pub(crate) fn from_consumer(consumer: Consumer) -> Self {
        Self { consumer }
    }

    /// Gracefully stop the consumer and await all subscription slots.
    ///
    /// # Errors
    ///
    /// None: it always returns `Ok(())`. A slot whose run ended with an error, or
    /// panicked, is awaited like the others and its result is not returned.
    pub async fn stop(self) -> Result<(), EventBrokerError> {
        self.consumer.shutdown().await
    }

    /// The subscription each parallelism slot joined last. A slot that has not joined yet,
    /// or is recording a new subscription at that moment, is left out.
    pub fn subscriptions(&self) -> Vec<SubscriptionId> {
        self.consumer.subscriptions()
    }
}

impl Default for Consumer {
    fn default() -> Self {
        Self::new()
    }
}

impl Consumer {
    pub fn new() -> Self {
        Self { slots: Vec::new() }
    }

    /// Starts one subscription slot per parallel worker, each dispatching events one at a time to `handler`.
    pub(crate) async fn new_with_slots<M, H>(
        builder: ConsumerBuilder<crate::consumer::builder::BrokerOnly<M>>,
        handler: H,
    ) -> Result<Self, EventBrokerError>
    where
        M: CommitOffset + 'static,
        H: EventHandler<serde_json::Value> + 'static,
    {
        let handler = EventHandlerAdapter::new(handler);
        Self::new_with_batch_slots_with_cancel(builder, handler, None).await
    }

    /// Starts one subscription slot per parallel worker, each dispatching event batches to `handler`.
    pub(crate) async fn new_with_batch_slots<M, H>(
        builder: ConsumerBuilder<crate::consumer::builder::BrokerOnly<M>>,
        handler: H,
    ) -> Result<Self, EventBrokerError>
    where
        M: CommitOffset + 'static,
        H: BatchHandler<serde_json::Value> + 'static,
    {
        let handler = BatchHandlerAdapter::new(handler);
        Self::new_with_batch_slots_with_cancel(builder, handler, None).await
    }

    /// Starts one subscription slot per parallel worker, each handing batches to
    /// `handler`. The consumer's group is resolved once, after every step that can
    /// refuse the start, and all its slots join that group.
    async fn new_with_batch_slots_with_cancel<M, H>(
        builder: ConsumerBuilder<crate::consumer::builder::BrokerOnly<M>>,
        handler: H,
        shared_cancel: Option<CancellationToken>,
    ) -> Result<Self, EventBrokerError>
    where
        M: CommitOffset + 'static,
        H: DeliverBatch + 'static,
    {
        let settings = builder.effective_settings()?;
        builder.require_identity()?;
        let parallelism = builder.parallelism;
        let broker = builder.broker.clone().ok_or_else(|| {
            EventBrokerError::Internal(
                "ConsumerBuilder: broker not wired; construct it with ConsumerBuilder::new(broker)"
                    .into(),
            )
        })?;
        // Resolved once, before any slot starts, so attributing a delivered event
        // to its topic is a local read rather than a call on the delivery path.
        let type_cache = Arc::new(super::type_cache::ConsumerTypeCache::default());
        type_cache
            .prime(&broker, &builder.security_context, &builder.topics)
            .await?;
        // Resolved once, so every slot joins the same group and the group splits the
        // partitions between them. It comes after the last step that can fail, so a
        // failed start creates no anonymous group.
        let group = builder
            .effective_group()
            .resolve(broker.as_ref(), &builder.security_context)
            .await?;
        let slot_settings = SlotSettings::for_consumer(
            &builder,
            &settings,
            broker.clone(),
            type_cache.clone(),
            group,
        );
        let handler = Arc::new(handler);
        let offset_manager = Arc::new(builder.offset_manager.0);

        let mut slots = Vec::with_capacity(parallelism as usize);
        let ctx_arc = Arc::new(builder.security_context);

        for idx in 0..parallelism {
            let sub_id = Arc::new(tokio::sync::Mutex::new(None));
            let cancel = shared_cancel.clone().unwrap_or_default();

            let dispatcher = SlotDispatcher {
                settings: slot_settings.for_slot(idx, sub_id.clone()),
                offset_manager: offset_manager.clone(),
                handler: handler.clone(),
                commit_mode: builder.commit_mode,
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

    /// Starts one subscription slot per parallel worker, dispatching events to the route handler matching topic and type, else the default handler.
    pub(crate) async fn new_with_routed_slots<M>(
        builder: ConsumerBuilder<crate::consumer::builder::BrokerOnly<M>>,
        default_handler: Option<Arc<dyn DeliverBatch>>,
        routes: Vec<ConsumerRoute>,
        route_handlers: Vec<Arc<dyn DeliverBatch>>,
    ) -> Result<Self, EventBrokerError>
    where
        M: CommitOffset + 'static,
    {
        let handler = RoutedBatchHandler::new(default_handler, routes, route_handlers)?;

        Self::new_with_batch_slots_with_cancel(builder, handler, None).await
    }

    /// Builds a tx-capable runtime that delivers events to `handler` one at a time.
    #[cfg(feature = "db")]
    pub(crate) async fn new_with_tx_slots<M, H>(
        builder: ConsumerBuilder<WithTx<M>>,
        handler: H,
    ) -> Result<Self, EventBrokerError>
    where
        M: CommitOffsetInTx + 'static,
        H: TxEventHandler<serde_json::Value, M> + 'static,
    {
        let handler = TxEventHandlerAdapter::new(handler);
        Self::new_with_tx_deliver_slots(builder, handler).await
    }

    /// Builds a tx-capable runtime that delivers batches to `handler`.
    #[cfg(feature = "db")]
    pub(crate) async fn new_with_tx_batch_slots<M, H>(
        builder: ConsumerBuilder<WithTx<M>>,
        handler: H,
    ) -> Result<Self, EventBrokerError>
    where
        M: CommitOffsetInTx + 'static,
        H: TxBatchHandler<serde_json::Value, M> + 'static,
    {
        let handler = TxBatchHandlerAdapter::new(handler);
        Self::new_with_tx_deliver_slots(builder, handler).await
    }

    /// Builds a tx-capable runtime that hands each batch to `handler`. The consumer's
    /// group is resolved once, after every step that can refuse the start, and all its
    /// slots join that group.
    #[cfg(feature = "db")]
    async fn new_with_tx_deliver_slots<M, H>(
        builder: ConsumerBuilder<WithTx<M>>,
        handler: H,
    ) -> Result<Self, EventBrokerError>
    where
        M: CommitOffsetInTx + 'static,
        H: DeliverTxBatch<M> + 'static,
    {
        let settings = builder.effective_settings()?;
        builder.require_identity()?;
        let parallelism = builder.parallelism;
        let broker = builder.broker.clone().ok_or_else(|| {
            EventBrokerError::Internal(
                "ConsumerBuilder: broker not wired; construct it with ConsumerBuilder::new(broker)"
                    .into(),
            )
        })?;
        // Resolved once, before any slot starts, so attributing a delivered event
        // to its topic is a local read rather than a call on the delivery path.
        let type_cache = Arc::new(super::type_cache::ConsumerTypeCache::default());
        type_cache
            .prime(&broker, &builder.security_context, &builder.topics)
            .await?;
        // Resolved once, so every slot joins the same group and the group splits the
        // partitions between them. It comes after the last step that can fail, so a
        // failed start creates no anonymous group.
        let group = builder
            .effective_group()
            .resolve(broker.as_ref(), &builder.security_context)
            .await?;
        let slot_settings = SlotSettings::for_consumer(
            &builder,
            &settings,
            broker.clone(),
            type_cache.clone(),
            group,
        );
        let handler = Arc::new(handler);
        let offset_manager = Arc::new(builder.offset_manager.0);

        let mut slots = Vec::with_capacity(parallelism as usize);
        let ctx_arc = Arc::new(builder.security_context);

        for idx in 0..parallelism {
            let sub_id = Arc::new(tokio::sync::Mutex::new(None));
            let cancel = CancellationToken::new();

            let dispatcher = TxSlotDispatcher {
                settings: slot_settings.for_slot(idx, sub_id.clone()),
                offset_manager: offset_manager.clone(),
                handler: handler.clone(),
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

    /// Builds a tx-capable runtime that dispatches events by route, else to the default handler.
    #[cfg(feature = "db")]
    pub(crate) async fn new_with_tx_routed_slots<M>(
        builder: ConsumerBuilder<WithTx<M>>,
        default_handler: Option<Arc<dyn DeliverTxBatch<M>>>,
        routes: Vec<ConsumerRoute>,
        route_handlers: Vec<Arc<dyn DeliverTxBatch<M>>>,
    ) -> Result<Self, EventBrokerError>
    where
        M: CommitOffsetInTx + 'static,
    {
        let handler = TxRoutedBatchHandler::new(default_handler, routes, route_handlers)?;

        Self::new_with_tx_deliver_slots(builder, handler).await
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

    /// The subscription each parallelism slot joined last. A slot that has not joined yet,
    /// or is recording a new subscription at that moment, is left out.
    pub fn subscriptions(&self) -> Vec<SubscriptionId> {
        self.slots
            .iter()
            .filter_map(|s| s.subscription_id.try_lock().ok().and_then(|g| *g))
            .collect()
    }
}
