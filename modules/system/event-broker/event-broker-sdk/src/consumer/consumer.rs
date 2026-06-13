use std::sync::Arc;

use modkit_security::SecurityContext;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::consumer::ack::AckHandle;
use crate::consumer::backend::ConsumerBackend;
use crate::consumer::builder::ConsumerBuilder;
use crate::consumer::dispatcher::SlotDispatcher;
use crate::consumer::offset_manager::OffsetManager;
use crate::consumer::{ConsumerGroupRef, EventHandler, HandlerOutcome};
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

impl Consumer {
    pub fn new(parallelism: u32) -> Self {
        let _ = parallelism;
        Self { slots: Vec::new() }
    }

    pub(crate) async fn new_with_slots<M, H, D>(
        ctx: &SecurityContext,
        builder: ConsumerBuilder<D, crate::consumer::builder::BrokerOnly<M>>,
        handler: H,
    ) -> Result<Self, EventBrokerError>
    where
        M: OffsetManager + 'static,
        H: EventHandler<AckHandle, HandlerOutcome> + 'static,
    {
        let parallelism = builder.parallelism;
        let backend = builder.backend.ok_or_else(|| {
            EventBrokerError::Internal(
                "ConsumerBuilder: backend not wired; use EventBroker::consumer_builder()".into(),
            )
        })?;
        let handler = Arc::new(handler);
        let offset_manager = Arc::new(builder.offset_manager.0);

        let mut slots = Vec::with_capacity(parallelism as usize);
        let ctx_arc = Arc::new(ctx.clone());

        for idx in 0..parallelism {
            let sub_id = Arc::new(tokio::sync::Mutex::new(None));
            let cancel = CancellationToken::new();

            let dispatcher = SlotDispatcher {
                slot_idx: idx,
                backend: backend.clone(),
                offset_manager: offset_manager.clone(),
                handler: handler.clone(),
                group_ref: builder
                    .group
                    .clone()
                    .unwrap_or(ConsumerGroupRef::AutoAnonymous {
                        client_agent: builder.client_agent.clone(),
                        description: None,
                    }),
                topics: builder.topics.clone(),
                event_type_patterns: builder.event_type_patterns.clone(),
                client_agent: builder.client_agent.clone(),
                session_timeout: builder.session_timeout.clone(),
                filter: builder.filter.clone(),
                heartbeat_drop_threshold: builder.heartbeat_drop_threshold,
                retry_base: builder.retry_base,
                retry_max: builder.retry_max,
                auto_ack_interval: Some(
                    builder
                        .auto_ack_interval
                        .unwrap_or(std::time::Duration::from_secs(20)),
                ),
                manual_ack: builder.manual_ack,
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
        ctx: &SecurityContext,
        builder: ConsumerBuilder<D, crate::consumer::builder::BrokerOnly<M>>,
        handler: H,
    ) -> Result<Self, EventBrokerError>
    where
        M: OffsetManager + 'static,
        H: EventHandler<AckHandle, crate::consumer::RejectableOutcome> + 'static,
    {
        let _ = (ctx, builder, handler);
        // Shares most logic with new_with_slots; DLQ callback wiring deferred to task 19.6.
        Ok(Self { slots: Vec::new() })
    }

    /// Graceful shutdown: cancel all tasks and await drain.
    pub async fn shutdown(mut self, _ctx: &SecurityContext) -> Result<(), EventBrokerError> {
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
