use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{Notify, Semaphore};
use tokio_util::sync::CancellationToken;

use super::core::Outbox;
use super::handler::{
    Handler, MessageHandler, PerMessageAdapter, TransactionalHandler, TransactionalMessageHandler,
};
use super::manager::{OutboxBuilder, QueueDeclaration};
use super::stats::StatsRegistry;
use super::strategy::{DecoupledStrategy, TransactionalStrategy, generate_worker_id};
use super::taskward::{
    BackoffConfig, Bulkhead, BulkheadConfig, ConcurrencyLimit, PanicPolicy, TracingListener,
    WorkerBuilder,
};
use super::types::{Partitions, WorkerTuning};
use super::workers::processor::{PartitionProcessor, ProcessorReport};
use crate::Db;

/// All runtime context needed to spawn a processor worker.
/// Constructed once per partition in [`OutboxBuilder::start()`].
pub struct SpawnContext {
    pub pid: i64,
    pub db: Db,
    pub cancel: CancellationToken,
    pub partition_notify: Arc<Notify>,
    pub processor_sem: Arc<Semaphore>,
    pub start_notify: Arc<Notify>,
    #[allow(dead_code)]
    pub outbox: Arc<Outbox>,
    /// Shared stats registry for processor workers. `None` when stats disabled.
    pub stats_registry: Option<Arc<std::sync::Mutex<StatsRegistry>>>,
    /// Processor worker tuning (batch size, pacing, retry backoff).
    pub tuning: WorkerTuning,
}

/// Trait for creating processor workers. One impl per processing mode.
pub trait ProcessorFactory: Send {
    fn spawn(&self, ctx: SpawnContext) -> (String, Pin<Box<dyn Future<Output = ()> + Send>>);
}

/// Shared worker assembly logic for all processor factories.
fn build_processor_worker<S: super::strategy::ProcessingStrategy + 'static>(
    ctx: &SpawnContext,
    strategy: S,
) -> (String, Pin<Box<dyn Future<Output = ()> + Send>>) {
    let processor = PartitionProcessor::new(strategy, ctx.pid, ctx.tuning.clone(), ctx.db.clone());
    let name = format!("processor-{}", ctx.pid);
    let (poker_notify, _poker_handle) =
        super::taskward::poker(ctx.tuning.idle_interval, ctx.cancel.clone());
    let mut builder = WorkerBuilder::<ProcessorReport>::new(&name, ctx.cancel.clone())
        .pacing(&ctx.tuning)
        .notifier(poker_notify)
        .notifier(Arc::clone(&ctx.partition_notify))
        .notifier(Arc::clone(&ctx.start_notify))
        .bulkhead(Bulkhead::new(
            &name,
            BulkheadConfig {
                semaphore: ConcurrencyLimit::Fixed(Arc::clone(&ctx.processor_sem)),
                backoff: BackoffConfig::default(),
            },
        ))
        .listener(TracingListener)
        .on_panic(PanicPolicy::CatchAndRetry);

    builder = super::manager::register_stats(
        builder,
        ctx.stats_registry.as_ref(),
        "processor",
        Box::new(|any| {
            any.downcast_ref::<ProcessorReport>()
                .map_or(0, |r| u64::from(r.messages_processed))
        }),
    );

    let worker = builder.build(processor);
    (name, Box::pin(worker.run()))
}

/// Builder for registering a queue with per-queue configuration.
///
/// Obtained via [`OutboxBuilder::queue`]. Terminal methods (`transactional`,
/// `decoupled`, `batch_transactional`, `batch_decoupled`) register the queue
/// and return the parent [`OutboxBuilder`] for chaining.
#[must_use = "a queue builder does nothing until a handler is registered via .transactional() or .decoupled()"]
pub struct QueueBuilder {
    builder: OutboxBuilder,
    name: String,
    partitions: Partitions,
}

impl QueueBuilder {
    pub(crate) fn new(builder: OutboxBuilder, name: String, partitions: Partitions) -> Self {
        Self {
            builder,
            name,
            partitions,
        }
    }

    // msg_batch_size removed — batch size now lives on WorkerTuning::batch_size.
    // backoff_base/backoff_max removed — retry backoff now lives on
    // WorkerTuning::retry_base/retry_max.
    // Use .processor_tuning() or .profile() on OutboxBuilder instead.

    /// Register a single-message transactional handler (common case).
    ///
    /// The `PerMessageAdapter` processes one message at a time.
    /// The processor factory overrides `WorkerTuning::batch_size` to 1
    /// so only one message is fetched per cycle.
    #[must_use]
    pub fn transactional(
        self,
        handler: impl TransactionalMessageHandler + 'static,
    ) -> OutboxBuilder {
        self.register_transactional(PerMessageAdapter::new(handler), true)
    }

    /// Register a single-message decoupled handler (common case).
    ///
    /// The `PerMessageAdapter` processes one message at a time.
    /// The processor factory overrides `WorkerTuning::batch_size` to 1.
    #[must_use]
    pub fn decoupled(self, handler: impl MessageHandler + 'static) -> OutboxBuilder {
        self.register_decoupled(PerMessageAdapter::new(handler), true, None)
    }

    /// Register a single-message decoupled handler with explicit configuration.
    ///
    /// Use this to customize `lease_duration`.
    #[must_use]
    pub fn decoupled_with(
        self,
        handler: impl MessageHandler + 'static,
        config: super::types::DecoupledConfig,
    ) -> OutboxBuilder {
        let super::types::DecoupledConfig { lease_duration } = config;
        self.register_decoupled(PerMessageAdapter::new(handler), true, Some(lease_duration))
    }

    /// Register a batch transactional handler (advanced).
    #[must_use]
    pub fn batch_transactional(
        self,
        handler: impl TransactionalHandler + 'static,
    ) -> OutboxBuilder {
        self.register_transactional(handler, false)
    }

    /// Internal: register a transactional handler with `per_message` flag.
    fn register_transactional(
        self,
        handler: impl TransactionalHandler + 'static,
        per_message: bool,
    ) -> OutboxBuilder {
        let factory = TransactionalProcessorFactory {
            handler: Arc::new(handler),
            per_message,
        };

        let mut builder = self.builder;
        builder.queue_declarations.push(QueueDeclaration {
            name: self.name,
            partitions: self.partitions,
            factory: Box::new(factory),
        });
        builder
    }

    /// Register a batch decoupled handler (advanced).
    ///
    /// Uses `WorkerTuning::batch_size` as-is (not forced to 1).
    #[must_use]
    pub fn batch_decoupled(self, handler: impl Handler + 'static) -> OutboxBuilder {
        self.register_decoupled(handler, false, None)
    }

    /// Internal: register a decoupled handler with `per_message` flag.
    fn register_decoupled(
        self,
        handler: impl Handler + 'static,
        per_message: bool,
        lease_duration_override: Option<Duration>,
    ) -> OutboxBuilder {
        let factory = DecoupledProcessorFactory {
            handler: Arc::new(handler),
            queue_name: self.name.clone(),
            per_message,
            lease_duration_override,
        };

        let mut builder = self.builder;
        builder.queue_declarations.push(QueueDeclaration {
            name: self.name,
            partitions: self.partitions,
            factory: Box::new(factory),
        });
        builder
    }

    /// Register a batch decoupled handler with explicit configuration (advanced).
    #[must_use]
    pub fn batch_decoupled_with(
        self,
        handler: impl Handler + 'static,
        config: super::types::DecoupledConfig,
    ) -> OutboxBuilder {
        let super::types::DecoupledConfig { lease_duration } = config;
        self.register_decoupled(handler, false, Some(lease_duration))
    }
}

// --- Factory implementations ---

struct TransactionalProcessorFactory<H: TransactionalHandler> {
    handler: Arc<H>,
    /// When true, override `tuning.batch_size` to 1 (per-message adapter).
    per_message: bool,
}

impl<H: TransactionalHandler + 'static> ProcessorFactory for TransactionalProcessorFactory<H> {
    fn spawn(&self, mut ctx: SpawnContext) -> (String, Pin<Box<dyn Future<Output = ()> + Send>>) {
        if self.per_message {
            // Per-message handlers process one message at a time via PerMessageAdapter,
            // so force batch_size=1 to avoid fetching messages that won't be processed.
            ctx.tuning.batch_size = 1;
        }
        let strategy = TransactionalStrategy::new(Box::new(ArcTransactionalHandler(Arc::clone(
            &self.handler,
        ))));
        build_processor_worker(&ctx, strategy)
    }
}

struct DecoupledProcessorFactory<H: Handler> {
    handler: Arc<H>,
    queue_name: String,
    /// When true, override `tuning.batch_size` to 1 (per-message adapter).
    per_message: bool,
    /// Optional lease duration override from `DecoupledConfig`.
    lease_duration_override: Option<Duration>,
}

impl<H: Handler + 'static> ProcessorFactory for DecoupledProcessorFactory<H> {
    fn spawn(&self, mut ctx: SpawnContext) -> (String, Pin<Box<dyn Future<Output = ()> + Send>>) {
        if self.per_message {
            ctx.tuning.batch_size = 1;
        }
        if let Some(ld) = self.lease_duration_override {
            ctx.tuning.lease_duration = ld;
        }
        let worker_id = generate_worker_id(&self.queue_name);
        let strategy =
            DecoupledStrategy::new(Box::new(ArcHandler(Arc::clone(&self.handler))), worker_id);
        build_processor_worker(&ctx, strategy)
    }
}

// Arc wrappers to share handler across partitions

struct ArcTransactionalHandler<H: TransactionalHandler>(Arc<H>);

#[async_trait::async_trait]
impl<H: TransactionalHandler> TransactionalHandler for ArcTransactionalHandler<H> {
    async fn handle(
        &self,
        txn: &dyn sea_orm::ConnectionTrait,
        msgs: &[super::handler::OutboxMessage],
        cancel: CancellationToken,
    ) -> super::handler::HandlerResult {
        self.0.handle(txn, msgs, cancel).await
    }

    fn processed_count(&self) -> Option<usize> {
        self.0.processed_count()
    }
}

struct ArcHandler<H: Handler>(Arc<H>);

#[async_trait::async_trait]
impl<H: Handler> Handler for ArcHandler<H> {
    async fn handle(
        &self,
        msgs: &[super::handler::OutboxMessage],
        cancel: CancellationToken,
    ) -> super::handler::HandlerResult {
        self.0.handle(msgs, cancel).await
    }

    fn processed_count(&self) -> Option<usize> {
        self.0.processed_count()
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn partitions_count() {
        assert_eq!(Partitions::of(1).count(), 1);
        assert_eq!(Partitions::of(2).count(), 2);
        assert_eq!(Partitions::of(4).count(), 4);
        assert_eq!(Partitions::of(8).count(), 8);
        assert_eq!(Partitions::of(16).count(), 16);
        assert_eq!(Partitions::of(32).count(), 32);
        assert_eq!(Partitions::of(64).count(), 64);
    }
}
