use crate::consumer::{
    BatchEventHandler, CommitHandle, ConsumerBuffering, ConsumerBuilder, ConsumerGroupRef,
    ConsumerHandle, ConsumerListenerSettings, ConsumerProfile, EventBatch, EventHandler,
    EventTypeRef, Fallback, HandlerOutcome, InMemoryOffsetManager, RawEvent, RejectableOutcome,
    TopicRef,
};
use crate::error::{ConsumerError, EventBrokerError};
use std::time::Duration;

struct NoopHandler;

#[async_trait::async_trait]
impl EventHandler<CommitHandle, HandlerOutcome> for NoopHandler {
    async fn handle(
        &self,
        _event: RawEvent,
        _attempts: u16,
        _commit: CommitHandle,
    ) -> Result<HandlerOutcome, ConsumerError> {
        Ok(HandlerOutcome::Success)
    }
}

struct NoopBatchHandler;

#[async_trait::async_trait]
impl BatchEventHandler<CommitHandle, HandlerOutcome> for NoopBatchHandler {
    async fn handle_batch(
        &self,
        _batch: &mut EventBatch<'_>,
        _attempts: u16,
        _commit: CommitHandle,
    ) -> Result<HandlerOutcome, ConsumerError> {
        Ok(HandlerOutcome::Success)
    }
}

struct NoopRejectableHandler;

#[async_trait::async_trait]
impl EventHandler<CommitHandle, RejectableOutcome> for NoopRejectableHandler {
    async fn handle(
        &self,
        _event: RawEvent,
        _attempts: u16,
        _commit: CommitHandle,
    ) -> Result<RejectableOutcome, ConsumerError> {
        Ok(RejectableOutcome::Success)
    }
}

struct NoopRejectableBatchHandler;

#[async_trait::async_trait]
impl BatchEventHandler<CommitHandle, RejectableOutcome> for NoopRejectableBatchHandler {
    async fn handle_batch(
        &self,
        _batch: &mut EventBatch<'_>,
        _attempts: u16,
        _commit: CommitHandle,
    ) -> Result<RejectableOutcome, ConsumerError> {
        Ok(RejectableOutcome::Success)
    }
}

#[tokio::test]
async fn consumer_ready_starts_without_context_argument() {
    let ready = ConsumerBuilder::new_unbound()
        .group(ConsumerGroupRef::auto_anonymous("builder-start"))
        .topics(["gts.cf.core.events.topic.v1~orders.v1"])
        .offset_manager(InMemoryOffsetManager::new(Fallback::Earliest))
        .handler(NoopHandler);

    let err = match ready.start().await {
        Ok(_) => panic!("unbound builder cannot open subscriptions"),
        Err(err) => err,
    };

    assert!(
        err.to_string().contains("broker not wired"),
        "unexpected error: {err}"
    );
}

#[test]
fn consumer_builder_accepts_batch_handler_terminal_method() {
    let _ready = ConsumerBuilder::new_unbound()
        .group(ConsumerGroupRef::auto_anonymous("builder-batch"))
        .topics(["gts.cf.core.events.topic.v1~orders.v1"])
        .offset_manager(InMemoryOffsetManager::new(Fallback::Earliest))
        .batch_handler(NoopBatchHandler);
}

#[test]
fn consumer_builder_accepts_default_and_routed_handlers() {
    let _ready = ConsumerBuilder::new_unbound()
        .group(ConsumerGroupRef::auto_anonymous("builder-routed"))
        .topics(["gts.cf.core.events.topic.v1~orders.v1"])
        .offset_manager(InMemoryOffsetManager::new(Fallback::Earliest))
        .default_handler(NoopHandler)
        .route()
        .topic(TopicRef::gts("gts.cf.core.events.topic.v1~orders.v1"))
        .event_type(EventTypeRef::gts(
            "gts.cf.core.events.event.v1~orders.OrderCreated",
        ))
        .handler(NoopHandler)
        .route()
        .topic(TopicRef::gts("gts.cf.core.events.topic.v1~orders.v1"))
        .event_type(EventTypeRef::gts_pattern(
            "gts.cf.core.events.event.v1~orders.*",
        ))
        .batch_handler(NoopBatchHandler);
}

#[test]
fn consumer_builder_accepts_route_only_with_topic_catch_all() {
    let _ready = ConsumerBuilder::new_unbound()
        .group(ConsumerGroupRef::auto_anonymous("builder-route-only"))
        .topics(["gts.cf.core.events.topic.v1~orders.v1"])
        .offset_manager(InMemoryOffsetManager::new(Fallback::Earliest))
        .route()
        .topic(TopicRef::gts("gts.cf.core.events.topic.v1~orders.v1"))
        .handler(NoopHandler);
}

#[test]
fn consumer_builders_keep_independent_profiles_and_listener_settings() {
    let low_latency = ConsumerBuilder::new_unbound()
        .group(ConsumerGroupRef::auto_anonymous("builder-independent-low"))
        .topics(["gts.cf.core.events.topic.v1~orders.v1"])
        .profile(ConsumerProfile::low_latency())
        .listener_settings(ConsumerListenerSettings {
            timeout: Duration::from_millis(25),
            channel_capacity: 8,
        })
        .offset_manager(InMemoryOffsetManager::new(Fallback::Earliest))
        .handler(NoopHandler);

    let high_throughput = ConsumerBuilder::new_unbound()
        .group(ConsumerGroupRef::auto_anonymous("builder-independent-high"))
        .topics(["gts.cf.core.events.topic.v1~payments.v1"])
        .profile(ConsumerProfile::high_throughput())
        .buffering(ConsumerBuffering {
            partition_capacity: 1024,
            high_watermark: 900,
            low_watermark: 512,
        })
        .listener_settings(ConsumerListenerSettings {
            timeout: Duration::from_millis(250),
            channel_capacity: 64,
        })
        .offset_manager(InMemoryOffsetManager::new(Fallback::Latest))
        .handler(NoopHandler);

    let low_settings = low_latency.builder.effective_settings().unwrap();
    let high_settings = high_throughput.builder.effective_settings().unwrap();

    assert_ne!(low_latency.builder.topics, high_throughput.builder.topics);
    assert_ne!(low_settings.batching, high_settings.batching);
    assert_ne!(low_settings.listener, high_settings.listener);
    assert_eq!(high_settings.buffering.partition_capacity, 1024);
}

#[test]
fn consumer_builder_accepts_dlq_typestate_quadrants() {
    let _single = ConsumerBuilder::new_unbound()
        .group(ConsumerGroupRef::auto_anonymous("builder-dlq-single"))
        .topics(["gts.cf.core.events.topic.v1~orders.v1"])
        .on_dead_letter(|_event| async { Ok(()) })
        .offset_manager(InMemoryOffsetManager::new(Fallback::Earliest))
        .handler(NoopRejectableHandler);

    let _batch = ConsumerBuilder::new_unbound()
        .group(ConsumerGroupRef::auto_anonymous("builder-dlq-batch"))
        .topics(["gts.cf.core.events.topic.v1~orders.v1"])
        .on_dead_letter(|_event| async { Ok(()) })
        .offset_manager(InMemoryOffsetManager::new(Fallback::Earliest))
        .batch_handler(NoopRejectableBatchHandler);

    let _routed = ConsumerBuilder::new_unbound()
        .group(ConsumerGroupRef::auto_anonymous("builder-dlq-routed"))
        .topics(["gts.cf.core.events.topic.v1~orders.v1"])
        .on_dead_letter(|_event| async { Ok(()) })
        .offset_manager(InMemoryOffsetManager::new(Fallback::Earliest))
        .default_handler(NoopRejectableHandler)
        .route()
        .topic(TopicRef::gts("gts.cf.core.events.topic.v1~orders.v1"))
        .event_type(EventTypeRef::gts(
            "gts.cf.core.events.event.v1~orders.OrderCreated",
        ))
        .handler(NoopRejectableHandler);
}

#[tokio::test]
async fn routed_consumer_rejects_route_outside_subscription_topics() {
    let ready = ConsumerBuilder::new_unbound()
        .group(ConsumerGroupRef::auto_anonymous(
            "builder-routed-invalid-topic",
        ))
        .topics(["gts.cf.core.events.topic.v1~orders.v1"])
        .offset_manager(InMemoryOffsetManager::new(Fallback::Earliest))
        .default_handler(NoopHandler)
        .route()
        .topic(TopicRef::gts("gts.cf.core.events.topic.v1~payments.v1"))
        .handler(NoopHandler);

    let err = match ready.start().await {
        Ok(_) => panic!("route validation must fail"),
        Err(err) => err,
    };

    assert!(
        matches!(err, EventBrokerError::InvalidConsumerOptions { .. }),
        "unexpected error: {err}"
    );
    assert!(
        err.to_string().contains("not part of the configured"),
        "unexpected error: {err}"
    );
}

#[tokio::test]
async fn routed_consumer_rejects_duplicate_routes() {
    let ready = ConsumerBuilder::new_unbound()
        .group(ConsumerGroupRef::auto_anonymous("builder-routed-duplicate"))
        .topics(["gts.cf.core.events.topic.v1~orders.v1"])
        .offset_manager(InMemoryOffsetManager::new(Fallback::Earliest))
        .default_handler(NoopHandler)
        .route()
        .topic(TopicRef::gts("gts.cf.core.events.topic.v1~orders.v1"))
        .event_type(EventTypeRef::gts(
            "gts.cf.core.events.event.v1~orders.OrderCreated",
        ))
        .handler(NoopHandler)
        .route()
        .topic(TopicRef::gts("gts.cf.core.events.topic.v1~orders.v1"))
        .event_type(EventTypeRef::gts(
            "gts.cf.core.events.event.v1~orders.OrderCreated",
        ))
        .handler(NoopHandler);

    let err = match ready.start().await {
        Ok(_) => panic!("duplicate route validation must fail"),
        Err(err) => err,
    };

    assert!(
        matches!(err, EventBrokerError::InvalidConsumerOptions { .. }),
        "unexpected error: {err}"
    );
    assert!(
        err.to_string().contains("duplicate consumer route"),
        "unexpected error: {err}"
    );
}

#[tokio::test]
async fn routed_consumer_without_default_rejects_incomplete_routes() {
    let ready = ConsumerBuilder::new_unbound()
        .group(ConsumerGroupRef::auto_anonymous(
            "builder-routed-missing-default",
        ))
        .topics(["gts.cf.core.events.topic.v1~orders.v1"])
        .offset_manager(InMemoryOffsetManager::new(Fallback::Earliest))
        .route()
        .topic(TopicRef::gts("gts.cf.core.events.topic.v1~orders.v1"))
        .event_type(EventTypeRef::gts(
            "gts.cf.core.events.event.v1~orders.OrderCreated",
        ))
        .handler(NoopHandler);

    let err = match ready.start().await {
        Ok(_) => panic!("route-only consumer without catch-all must fail"),
        Err(err) => err,
    };

    assert!(
        matches!(err, EventBrokerError::InvalidConsumerOptions { .. }),
        "unexpected error: {err}"
    );
    assert!(
        err.to_string().contains("without a default handler"),
        "unexpected error: {err}"
    );
}

#[tokio::test]
async fn routed_consumer_rejects_missing_subscription_topics() {
    let ready = ConsumerBuilder::new_unbound()
        .group(ConsumerGroupRef::auto_anonymous("builder-routed-no-topics"))
        .offset_manager(InMemoryOffsetManager::new(Fallback::Earliest))
        .route()
        .topic(TopicRef::gts("gts.cf.core.events.topic.v1~orders.v1"))
        .handler(NoopHandler);

    let err = match ready.start().await {
        Ok(_) => panic!("routed consumer without topics must fail"),
        Err(err) => err,
    };

    assert!(
        matches!(err, EventBrokerError::InvalidConsumerOptions { .. }),
        "unexpected error: {err}"
    );
    assert!(
        err.to_string()
            .contains("requires at least one configured topic"),
        "unexpected error: {err}"
    );
}

#[tokio::test]
async fn consumer_handle_exposes_subscription_inspection_and_stop() {
    let handle = ConsumerHandle::from_consumer(super::consumer::Consumer::new(2));

    assert!(handle.subscription_ids().is_empty());
    handle.stop().await.expect("empty handle stop");
}

#[cfg(feature = "mock")]
#[tokio::test]
async fn multiple_consumer_handles_run_with_independent_lifecycle() {
    use crate::EventBroker;
    use crate::mock::{MockBroker, MockBrokerHandle};

    const ORDERS_TOPIC: &str = "gts.cf.core.events.topic.v1~cf.core.orders.topic.v1";
    const PAYMENTS_TOPIC: &str = "gts.cf.core.events.topic.v1~cf.core.payments.topic.v1";

    let mock = MockBroker::new();
    let control = MockBrokerHandle::from_broker(&mock);
    control.register_topic(ORDERS_TOPIC, 4).await;
    control.register_topic(PAYMENTS_TOPIC, 4).await;
    control
        .set_heartbeat_interval(Duration::from_millis(10))
        .await;

    let broker: std::sync::Arc<dyn EventBroker> = std::sync::Arc::new(mock);
    let orders = ConsumerBuilder::new(broker.clone())
        .group(ConsumerGroupRef::auto_anonymous("orders-handle"))
        .topics([ORDERS_TOPIC])
        .profile(ConsumerProfile::low_latency())
        .parallelism(2)
        .offset_manager(InMemoryOffsetManager::new(Fallback::Earliest))
        .handler(NoopHandler)
        .start()
        .await
        .expect("orders consumer starts");
    let payments = ConsumerBuilder::new(broker)
        .group(ConsumerGroupRef::auto_anonymous("payments-handle"))
        .topics([PAYMENTS_TOPIC])
        .profile(ConsumerProfile::high_throughput())
        .parallelism(1)
        .offset_manager(InMemoryOffsetManager::new(Fallback::Latest))
        .handler(NoopHandler)
        .start()
        .await
        .expect("payments consumer starts");

    wait_for_subscription_count(&orders, 2).await;
    wait_for_subscription_count(&payments, 1).await;

    let order_subscriptions = orders.subscription_ids();
    let payment_subscriptions = payments.subscription_ids();
    assert_eq!(order_subscriptions.len(), 2);
    assert_eq!(payment_subscriptions.len(), 1);
    assert!(
        order_subscriptions
            .iter()
            .all(|id| !payment_subscriptions.contains(id))
    );

    orders.stop().await.expect("orders handle stops");
    payments.stop().await.expect("payments handle stops");
}

#[cfg(feature = "mock")]
async fn wait_for_subscription_count(handle: &ConsumerHandle, expected: usize) {
    for _ in 0..100 {
        if handle.subscription_ids().len() == expected {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!(
        "consumer handle exposed {} subscription ids, expected {expected}",
        handle.subscription_ids().len()
    );
}

#[cfg(feature = "db")]
mod tx_typestate {
    use async_trait::async_trait;

    use super::*;
    use crate::consumer::{
        CommitOffsetInTx, OffsetManagerError, OffsetStore, ResolvedPosition, TxCommitHandle,
    };
    use crate::ids::{ConsumerGroupId, TopicId};

    #[derive(Default)]
    struct RecordingTxOffsetManager;

    #[async_trait]
    impl OffsetStore for RecordingTxOffsetManager {
        async fn load_position(
            &self,
            _group: &ConsumerGroupId,
            _topic: &TopicId,
            _partition: u32,
        ) -> Result<ResolvedPosition, OffsetManagerError> {
            Ok(Fallback::Earliest.into())
        }
    }

    #[async_trait]
    impl CommitOffsetInTx for RecordingTxOffsetManager {
        async fn commit_in_tx<TX>(
            &self,
            _txn: &TX,
            _group: &ConsumerGroupId,
            _topic: &TopicId,
            _partition: u32,
            _offset: i64,
        ) -> Result<(), OffsetManagerError>
        where
            TX: toolkit_db::secure::DBRunner + Sync,
        {
            Ok(())
        }
    }

    struct NoopTxHandler;

    #[async_trait]
    impl EventHandler<TxCommitHandle<RecordingTxOffsetManager>, HandlerOutcome> for NoopTxHandler {
        async fn handle(
            &self,
            _event: RawEvent,
            _attempts: u16,
            _commit: TxCommitHandle<RecordingTxOffsetManager>,
        ) -> Result<HandlerOutcome, ConsumerError> {
            Ok(HandlerOutcome::Success)
        }
    }

    struct NoopTxBatchHandler;

    #[async_trait]
    impl BatchEventHandler<TxCommitHandle<RecordingTxOffsetManager>, HandlerOutcome>
        for NoopTxBatchHandler
    {
        async fn handle_batch(
            &self,
            _batch: &mut EventBatch<'_>,
            _attempts: u16,
            _commit: TxCommitHandle<RecordingTxOffsetManager>,
        ) -> Result<HandlerOutcome, ConsumerError> {
            Ok(HandlerOutcome::Success)
        }
    }

    #[test]
    fn consumer_builder_accepts_transactional_typestate_quadrants() {
        let _single = ConsumerBuilder::new_unbound()
            .group(ConsumerGroupRef::auto_anonymous("builder-tx-single"))
            .topics(["gts.cf.core.events.topic.v1~orders.v1"])
            .tx_offset_manager(RecordingTxOffsetManager)
            .handler(NoopTxHandler);

        let _batch = ConsumerBuilder::new_unbound()
            .group(ConsumerGroupRef::auto_anonymous("builder-tx-batch"))
            .topics(["gts.cf.core.events.topic.v1~orders.v1"])
            .tx_offset_manager(RecordingTxOffsetManager)
            .batch_handler(NoopTxBatchHandler);

        let _routed = ConsumerBuilder::new_unbound()
            .group(ConsumerGroupRef::auto_anonymous("builder-tx-routed"))
            .topics(["gts.cf.core.events.topic.v1~orders.v1"])
            .tx_offset_manager(RecordingTxOffsetManager)
            .default_handler(NoopTxHandler)
            .route()
            .topic(TopicRef::gts("gts.cf.core.events.topic.v1~orders.v1"))
            .event_type(EventTypeRef::gts(
                "gts.cf.core.events.event.v1~orders.OrderCreated",
            ))
            .batch_handler(NoopTxBatchHandler);
    }
}
