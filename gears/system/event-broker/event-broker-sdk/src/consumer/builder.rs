use std::sync::Arc;
use std::time::Duration;

use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::consumer::backend::{BarrierMode, ConsumerBackend, Filter};
use crate::error::EventBrokerError;

use super::consumer::Consumer;
use super::{
    AckHandle, ConsumerGroupRef, DeadLetterEvent, EventHandler, HandlerOutcome, OffsetManager,
    RejectableOutcome,
};
#[cfg(feature = "outbox")]
use super::{TxAckHandle, TxOffsetManager};

// ---- Typestate markers (zero-sized) ----

pub struct NoDlq;
pub struct WithDlq;
pub struct BrokerOnly<M: OffsetManager + 'static>(pub M);

#[cfg(feature = "outbox")]
pub struct WithTx<M: TxOffsetManager + 'static>(pub M);

// ---- Builder ----

pub struct ConsumerBuilder<D = NoDlq, M = ()> {
    pub(crate) group: Option<ConsumerGroupRef>,
    pub(crate) topics: Vec<String>,
    pub(crate) tenant_id: Option<Uuid>,
    pub(crate) max_depth: Option<u32>,
    pub(crate) barrier_mode: BarrierMode,
    pub(crate) event_type_patterns: Vec<String>,
    pub(crate) parallelism: u32,
    pub(crate) client_agent: String,
    pub(crate) session_timeout: Option<String>,
    pub(crate) filter: Option<Filter>,
    pub(crate) retry_base: Duration,
    pub(crate) retry_max: Duration,
    pub(crate) auto_ack_interval: Option<Duration>,
    pub(crate) manual_ack: bool,
    /// Drop-on-Nth-heartbeat threshold: disconnect + re-JOIN after K consecutive heartbeats
    /// with no intervening events. Default: 10 (≈ 50 s of silence at 5 s broker cadence).
    pub(crate) heartbeat_drop_threshold: usize,
    pub(crate) _d: std::marker::PhantomData<D>,
    pub(crate) offset_manager: M,
    /// Wired by the impl crate's EventBroker. None when using new_unbound() for testing.
    pub(crate) backend: Option<Arc<dyn ConsumerBackend>>,
}

impl ConsumerBuilder<NoDlq, ()> {
    pub fn new(backend: Arc<dyn ConsumerBackend>) -> Self {
        Self {
            group: None,
            topics: Vec::new(),
            tenant_id: None,
            max_depth: Some(0),
            barrier_mode: BarrierMode::Respect,
            event_type_patterns: Vec::new(),
            parallelism: 1,
            client_agent: "event-broker-sdk/0.1".into(),
            session_timeout: Some("PT60S".into()),
            filter: None,
            retry_base: Duration::from_secs(1),
            retry_max: Duration::from_secs(60),
            auto_ack_interval: None,
            manual_ack: false,
            heartbeat_drop_threshold: 10,
            _d: std::marker::PhantomData,
            offset_manager: (),
            backend: Some(backend),
        }
    }

    pub fn new_unbound() -> Self {
        Self {
            group: None,
            topics: Vec::new(),
            tenant_id: None,
            max_depth: Some(0),
            barrier_mode: BarrierMode::Respect,
            event_type_patterns: Vec::new(),
            parallelism: 1,
            client_agent: "event-broker-sdk/0.1".into(),
            session_timeout: Some("PT60S".into()),
            filter: None,
            retry_base: Duration::from_secs(1),
            retry_max: Duration::from_secs(60),
            auto_ack_interval: None,
            manual_ack: false,
            heartbeat_drop_threshold: 10,
            _d: std::marker::PhantomData,
            offset_manager: (),
            backend: None,
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
            /// Descendant depth for tenant hierarchy traversal.
            /// `Some(0)` = current tenant only (default). `None` = unlimited descendants.
            pub fn max_depth(mut self, depth: Option<u32>) -> Self {
                self.max_depth = depth;
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
            pub fn session_timeout(mut self, d: impl Into<String>) -> Self {
                self.session_timeout = Some(d.into());
                self
            }
            pub fn filter(mut self, engine: impl Into<String>, expr: impl Into<String>) -> Self {
                self.filter = Some(Filter { engine: engine.into(), expression: expr.into() });
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
            pub fn auto_ack_interval(mut self, d: Duration) -> Self {
                self.auto_ack_interval = Some(d);
                self
            }
            pub fn manual_ack(mut self) -> Self {
                self.manual_ack = true;
                self
            }
        }
    };
}

// Instantiate for all typestate combinations we need.
common_builder_methods!(NoDlq, ());
common_builder_methods!(WithDlq, ());

// on_dead_letter — transitions D from NoDlq to WithDlq
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
            max_depth: self.max_depth,
            barrier_mode: self.barrier_mode,
            event_type_patterns: self.event_type_patterns,
            parallelism: self.parallelism,
            client_agent: self.client_agent,
            heartbeat_drop_threshold: self.heartbeat_drop_threshold,
            session_timeout: self.session_timeout,
            filter: self.filter,
            retry_base: self.retry_base,
            retry_max: self.retry_max,
            auto_ack_interval: self.auto_ack_interval,
            manual_ack: self.manual_ack,
            _d: std::marker::PhantomData,
            offset_manager: self.offset_manager,
            backend: self.backend,
        }
    }
}

// offset_manager / tx_offset_manager — transitions M
impl<D> ConsumerBuilder<D, ()> {
    pub fn offset_manager<M: OffsetManager + 'static>(
        self,
        m: M,
    ) -> ConsumerBuilder<D, BrokerOnly<M>> {
        ConsumerBuilder {
            group: self.group,
            topics: self.topics,
            tenant_id: self.tenant_id,
            max_depth: self.max_depth,
            barrier_mode: self.barrier_mode,
            event_type_patterns: self.event_type_patterns,
            parallelism: self.parallelism,
            client_agent: self.client_agent,
            heartbeat_drop_threshold: self.heartbeat_drop_threshold,
            session_timeout: self.session_timeout,
            filter: self.filter,
            retry_base: self.retry_base,
            retry_max: self.retry_max,
            auto_ack_interval: self.auto_ack_interval,
            manual_ack: self.manual_ack,
            _d: std::marker::PhantomData,
            offset_manager: BrokerOnly(m),
            backend: self.backend,
        }
    }

    #[cfg(feature = "outbox")]
    pub fn tx_offset_manager<M: TxOffsetManager + 'static>(
        self,
        m: M,
    ) -> ConsumerBuilder<D, WithTx<M>> {
        ConsumerBuilder {
            group: self.group,
            topics: self.topics,
            tenant_id: self.tenant_id,
            max_depth: self.max_depth,
            barrier_mode: self.barrier_mode,
            event_type_patterns: self.event_type_patterns,
            parallelism: self.parallelism,
            client_agent: self.client_agent,
            heartbeat_drop_threshold: self.heartbeat_drop_threshold,
            session_timeout: self.session_timeout,
            filter: self.filter,
            retry_base: self.retry_base,
            retry_max: self.retry_max,
            auto_ack_interval: self.auto_ack_interval,
            manual_ack: self.manual_ack,
            _d: std::marker::PhantomData,
            offset_manager: WithTx(m),
            backend: self.backend,
        }
    }
}

// ---- Four terminal `handler()` impls — one per (D, M) quadrant ----

pub struct ConsumerReady<D, M, H> {
    pub(crate) builder: ConsumerBuilder<D, M>,
    pub(crate) handler: H,
}

impl<M: OffsetManager + 'static> ConsumerBuilder<NoDlq, BrokerOnly<M>> {
    pub fn handler<H>(self, handler: H) -> ConsumerReady<NoDlq, BrokerOnly<M>, H>
    where
        H: EventHandler<AckHandle, HandlerOutcome> + 'static,
    {
        ConsumerReady {
            builder: self,
            handler,
        }
    }
}

impl<M: OffsetManager + 'static> ConsumerBuilder<WithDlq, BrokerOnly<M>> {
    pub fn handler<H>(self, handler: H) -> ConsumerReady<WithDlq, BrokerOnly<M>, H>
    where
        H: EventHandler<AckHandle, RejectableOutcome> + 'static,
    {
        ConsumerReady {
            builder: self,
            handler,
        }
    }
}

#[cfg(feature = "outbox")]
impl<M: TxOffsetManager + 'static> ConsumerBuilder<NoDlq, WithTx<M>> {
    pub fn handler<H>(self, handler: H) -> ConsumerReady<NoDlq, WithTx<M>, H>
    where
        H: EventHandler<TxAckHandle<M>, HandlerOutcome> + 'static,
    {
        ConsumerReady {
            builder: self,
            handler,
        }
    }
}

#[cfg(feature = "outbox")]
impl<M: TxOffsetManager + 'static> ConsumerBuilder<WithDlq, WithTx<M>> {
    pub fn handler<H>(self, handler: H) -> ConsumerReady<WithDlq, WithTx<M>, H>
    where
        H: EventHandler<TxAckHandle<M>, RejectableOutcome> + 'static,
    {
        ConsumerReady {
            builder: self,
            handler,
        }
    }
}

// ---- Terminal build methods on ConsumerReady ----

// -- NoDlq + BrokerOnly (no DLQ, broker-ack) --

impl<M, H> ConsumerReady<NoDlq, BrokerOnly<M>, H>
where
    M: OffsetManager + 'static,
    H: EventHandler<AckHandle, HandlerOutcome> + 'static,
{
    pub async fn build_background(
        self,
        ctx: &SecurityContext,
    ) -> Result<Consumer, EventBrokerError> {
        Consumer::new_with_slots(ctx, self.builder, self.handler).await
    }

    pub async fn run_blocking(
        self,
        ctx: &SecurityContext,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<(), EventBrokerError> {
        let consumer = self.build_background(ctx).await?;
        cancel.cancelled().await;
        consumer.shutdown(&SecurityContext::anonymous()).await
    }
}

// -- WithDlq + BrokerOnly --

impl<M, H> ConsumerReady<WithDlq, BrokerOnly<M>, H>
where
    M: OffsetManager + 'static,
    H: EventHandler<AckHandle, RejectableOutcome> + 'static,
{
    pub async fn build_background(
        self,
        ctx: &SecurityContext,
    ) -> Result<Consumer, EventBrokerError> {
        // DLQ path reuses the same dispatcher structure; handler outcome is different.
        // Full DLQ callback wiring is task 19.6; placeholder here so the crate compiles.
        Consumer::new_with_slots_dlq(ctx, self.builder, self.handler).await
    }

    pub async fn run_blocking(
        self,
        ctx: &SecurityContext,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<(), EventBrokerError> {
        let consumer = self.build_background(ctx).await?;
        cancel.cancelled().await;
        consumer.shutdown(&SecurityContext::anonymous()).await
    }
}

// -- NoDlq + WithTx (outbox feature) --

#[cfg(feature = "outbox")]
impl<M, H> ConsumerReady<NoDlq, WithTx<M>, H>
where
    M: TxOffsetManager + 'static,
    H: EventHandler<TxAckHandle<M>, HandlerOutcome> + 'static,
{
    pub async fn build_background(
        self,
        ctx: &SecurityContext,
    ) -> Result<Consumer, EventBrokerError> {
        let _ = ctx;
        todo!("NoDlq + WithTx build_background — task 19")
    }

    pub async fn run_blocking(
        self,
        ctx: &SecurityContext,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<(), EventBrokerError> {
        let _ = (ctx, cancel);
        todo!("NoDlq + WithTx run_blocking — task 19")
    }
}

// -- WithDlq + WithTx (outbox feature) --

#[cfg(feature = "outbox")]
impl<M, H> ConsumerReady<WithDlq, WithTx<M>, H>
where
    M: TxOffsetManager + 'static,
    H: EventHandler<TxAckHandle<M>, RejectableOutcome> + 'static,
{
    pub async fn build_background(
        self,
        ctx: &SecurityContext,
    ) -> Result<Consumer, EventBrokerError> {
        let _ = ctx;
        todo!("WithDlq + WithTx build_background — task 19")
    }

    pub async fn run_blocking(
        self,
        ctx: &SecurityContext,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<(), EventBrokerError> {
        let _ = (ctx, cancel);
        todo!("WithDlq + WithTx run_blocking — task 19")
    }
}
