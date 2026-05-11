use std::sync::Arc;

use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::consumer::builder::ConsumerBuilder;
use crate::consumer::commit::CommitHandle;
use crate::consumer::dispatcher::SlotDispatcher;
use crate::consumer::offset_manager::CommitOffset;
use crate::consumer::{
    BatchEventHandler, ConsumerGroupRef, EventHandler, HandlerOutcome, SingleEventHandlerAdapter,
};
use crate::error::EventBrokerError;
use crate::ids::SubscriptionId;

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

    pub(crate) async fn new_with_slots<M, H, D>(
        builder: ConsumerBuilder<D, crate::consumer::builder::BrokerOnly<M>>,
        handler: H,
    ) -> Result<Self, EventBrokerError>
    where
        M: CommitOffset + 'static,
        H: EventHandler<CommitHandle, HandlerOutcome> + 'static,
    {
        let handler = SingleEventHandlerAdapter::new(Arc::new(handler));
        Self::new_with_batch_slots(builder, handler).await
    }

    pub(crate) async fn new_with_batch_slots<M, H, D>(
        builder: ConsumerBuilder<D, crate::consumer::builder::BrokerOnly<M>>,
        handler: H,
    ) -> Result<Self, EventBrokerError>
    where
        M: CommitOffset + 'static,
        H: BatchEventHandler<CommitHandle, HandlerOutcome> + 'static,
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
                tenant_id: builder.tenant_id,
                tenant_depth: builder.tenant_depth,
                barrier_mode: builder.barrier_mode,
                event_type_patterns: builder.event_type_patterns.clone(),
                client_agent: builder.client_agent.clone(),
                session_timeout: builder.session_timeout.clone(),
                filter: builder.filter.clone(),
                heartbeat_drop_threshold: builder.heartbeat_drop_threshold,
                retry_base: settings.retry.base_delay,
                retry_max: settings.retry.max_delay,
                commit_mode: builder.commit_mode,
                max_rejoin_attempts: 16,
                subscription_id: sub_id.clone(),
                on_dead_letter: None,
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

    /// Stub for WithDlq + BrokerOnly consumers. DLQ wiring in task 19.6.
    pub(crate) async fn new_with_slots_dlq<M, H, D>(
        builder: ConsumerBuilder<D, crate::consumer::builder::BrokerOnly<M>>,
        handler: H,
    ) -> Result<Self, EventBrokerError>
    where
        M: CommitOffset + 'static,
        H: EventHandler<CommitHandle, crate::consumer::RejectableOutcome> + 'static,
    {
        builder.effective_settings()?;
        let _ = (builder, handler);
        // Shares most logic with new_with_slots; DLQ callback wiring deferred to task 19.6.
        Ok(Self { slots: Vec::new() })
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
