use crate::consumer::{
    BatchHandlerOutcome, ConsumerBuffering, ConsumerBuilder, ConsumerGroupRef, ConsumerHandle,
    ConsumerHandler, ConsumerListenerSettings, ConsumerProfile, EventBatch, Fallback,
    HandlerOutcome, InMemoryOffsetManager, RawEvent, SingleEventHandler,
};
use crate::error::{ConsumerError, EventBrokerError};
use gts::{GtsIdPattern, GtsInstanceId};
use std::time::Duration;
use toolkit_gts::{GTS_ID_PREFIX, gts_id};

fn ginst(id: impl AsRef<str>) -> GtsInstanceId {
    GtsInstanceId::try_new(id.as_ref()).expect("valid GTS instance id")
}

fn gpat(id: impl AsRef<str>) -> GtsIdPattern {
    GtsIdPattern::try_new(id.as_ref()).expect("valid GTS pattern")
}

struct NoopHandler;

#[async_trait::async_trait]
impl SingleEventHandler for NoopHandler {
    async fn handle(
        &self,
        _event: RawEvent,
        _attempts: u16,
    ) -> Result<HandlerOutcome, ConsumerError> {
        Ok(HandlerOutcome::Success)
    }
}

struct NoopBatchHandler;

#[async_trait::async_trait]
impl ConsumerHandler for NoopBatchHandler {
    async fn handle_batch(
        &self,
        _batch: &EventBatch<'_>,
        _attempts: u16,
    ) -> Result<BatchHandlerOutcome, ConsumerError> {
        Ok(BatchHandlerOutcome::Success)
    }
}

#[tokio::test]
async fn consumer_ready_starts_without_context_argument() {
    let ready = ConsumerBuilder::new_unbound()
        .group(ConsumerGroupRef::auto_anonymous("builder-start"))
        .topics([ginst(gts_id!(
            "cf.core.events.topic.v1~example.orders.x.x.v1"
        ))])
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
        .topics([ginst(gts_id!(
            "cf.core.events.topic.v1~example.orders.x.x.v1"
        ))])
        .offset_manager(InMemoryOffsetManager::new(Fallback::Earliest))
        .batch_handler(NoopBatchHandler);
}

#[test]
fn consumer_builder_accepts_default_and_routed_handlers() {
    let _ready = ConsumerBuilder::new_unbound()
        .group(ConsumerGroupRef::auto_anonymous("builder-routed"))
        .topics([ginst(gts_id!(
            "cf.core.events.topic.v1~example.orders.x.x.v1"
        ))])
        .offset_manager(InMemoryOffsetManager::new(Fallback::Earliest))
        .default_handler(NoopHandler)
        .route()
        .topic(ginst(gts_id!(
            "cf.core.events.topic.v1~example.orders.x.x.v1"
        )))
        .event_type(gpat(gts_id!(
            "cf.core.events.event.v1~example.orders.order_created.x.v1~"
        )))
        .handler(NoopHandler)
        .route()
        .topic(ginst(gts_id!(
            "cf.core.events.topic.v1~example.orders.x.x.v1"
        )))
        .event_type(gpat(format!(
            "{GTS_ID_PREFIX}cf.core.events.event.v1~example.orders.*"
        )))
        .batch_handler(NoopBatchHandler);
}

#[test]
fn consumer_builder_accepts_route_only_with_topic_catch_all() {
    let _ready = ConsumerBuilder::new_unbound()
        .group(ConsumerGroupRef::auto_anonymous("builder-route-only"))
        .topics([ginst(gts_id!(
            "cf.core.events.topic.v1~example.orders.x.x.v1"
        ))])
        .offset_manager(InMemoryOffsetManager::new(Fallback::Earliest))
        .route()
        .topic(ginst(gts_id!(
            "cf.core.events.topic.v1~example.orders.x.x.v1"
        )))
        .handler(NoopHandler);
}

#[test]
fn consumer_builders_keep_independent_profiles_and_listener_settings() {
    let low_latency = ConsumerBuilder::new_unbound()
        .group(ConsumerGroupRef::auto_anonymous("builder-independent-low"))
        .topics([ginst(gts_id!(
            "cf.core.events.topic.v1~example.orders.x.x.v1"
        ))])
        .profile(ConsumerProfile::low_latency())
        .listener_settings(ConsumerListenerSettings {
            timeout: Duration::from_millis(25),
            channel_capacity: 8,
        })
        .offset_manager(InMemoryOffsetManager::new(Fallback::Earliest))
        .handler(NoopHandler);

    let high_throughput = ConsumerBuilder::new_unbound()
        .group(ConsumerGroupRef::auto_anonymous("builder-independent-high"))
        .topics([ginst(gts_id!(
            "cf.core.events.topic.v1~example.payments.x.x.v1"
        ))])
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

#[tokio::test]
async fn routed_consumer_rejects_route_outside_subscription_topics() {
    let ready = ConsumerBuilder::new_unbound()
        .group(ConsumerGroupRef::auto_anonymous(
            "builder-routed-invalid-topic",
        ))
        .topics([ginst(gts_id!(
            "cf.core.events.topic.v1~example.orders.x.x.v1"
        ))])
        .offset_manager(InMemoryOffsetManager::new(Fallback::Earliest))
        .default_handler(NoopHandler)
        .route()
        .topic(ginst(gts_id!(
            "cf.core.events.topic.v1~example.payments.x.x.v1"
        )))
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
        .topics([ginst(gts_id!(
            "cf.core.events.topic.v1~example.orders.x.x.v1"
        ))])
        .offset_manager(InMemoryOffsetManager::new(Fallback::Earliest))
        .default_handler(NoopHandler)
        .route()
        .topic(ginst(gts_id!(
            "cf.core.events.topic.v1~example.orders.x.x.v1"
        )))
        .event_type(gpat(gts_id!(
            "cf.core.events.event.v1~example.orders.order_created.x.v1~"
        )))
        .handler(NoopHandler)
        .route()
        .topic(ginst(gts_id!(
            "cf.core.events.topic.v1~example.orders.x.x.v1"
        )))
        .event_type(gpat(gts_id!(
            "cf.core.events.event.v1~example.orders.order_created.x.v1~"
        )))
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
        .topics([ginst(gts_id!(
            "cf.core.events.topic.v1~example.orders.x.x.v1"
        ))])
        .offset_manager(InMemoryOffsetManager::new(Fallback::Earliest))
        .route()
        .topic(ginst(gts_id!(
            "cf.core.events.topic.v1~example.orders.x.x.v1"
        )))
        .event_type(gpat(gts_id!(
            "cf.core.events.event.v1~example.orders.order_created.x.v1~"
        )))
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
        .topic(ginst(gts_id!(
            "cf.core.events.topic.v1~example.orders.x.x.v1"
        )))
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
    let handle = ConsumerHandle::from_consumer(super::runtime::Consumer::new());

    assert!(handle.subscription_ids().is_empty());
    handle.stop().await.expect("empty handle stop");
}

#[cfg(feature = "db")]
mod tx_typestate {

    use async_trait::async_trait;

    use super::*;
    use crate::consumer::{
        CommitOffsetInTx, ConsumerOffsetManager, OffsetManagerError, OffsetStore, Position,
        TxCommitHandle, TxConsumerHandler, TxSingleEventHandler, WithTx,
    };
    use crate::ids::{ConsumerGroupId, TopicId};
    use crate::sequence::Sequence;

    #[derive(Default)]
    struct RecordingTxOffsetManager;

    #[async_trait]
    impl OffsetStore for RecordingTxOffsetManager {
        async fn load_position(
            &self,
            _group: &ConsumerGroupId,
            _topic: &TopicId,
            _partition: u32,
        ) -> Result<Position, OffsetManagerError> {
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
            _offset: Sequence,
        ) -> Result<(), OffsetManagerError>
        where
            TX: toolkit_db::secure::DBRunner + Sync,
        {
            Ok(())
        }
    }

    impl ConsumerOffsetManager for RecordingTxOffsetManager {
        type BuilderState = WithTx<Self>;

        fn into_builder_state(self) -> Self::BuilderState {
            WithTx(self)
        }
    }

    struct NoopTxHandler;

    #[async_trait]
    impl TxSingleEventHandler<RecordingTxOffsetManager> for NoopTxHandler {
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
    impl TxConsumerHandler<RecordingTxOffsetManager> for NoopTxBatchHandler {
        async fn handle_batch(
            &self,
            _batch: &EventBatch<'_>,
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
            .topics([ginst(gts_id!(
                "cf.core.events.topic.v1~example.orders.x.x.v1"
            ))])
            .offset_manager(RecordingTxOffsetManager)
            .handler(NoopTxHandler);

        let _batch = ConsumerBuilder::new_unbound()
            .group(ConsumerGroupRef::auto_anonymous("builder-tx-batch"))
            .topics([ginst(gts_id!(
                "cf.core.events.topic.v1~example.orders.x.x.v1"
            ))])
            .offset_manager(RecordingTxOffsetManager)
            .batch_handler(NoopTxBatchHandler);

        let _routed = ConsumerBuilder::new_unbound()
            .group(ConsumerGroupRef::auto_anonymous("builder-tx-routed"))
            .topics([ginst(gts_id!(
                "cf.core.events.topic.v1~example.orders.x.x.v1"
            ))])
            .offset_manager(RecordingTxOffsetManager)
            .default_handler(NoopTxHandler)
            .route()
            .topic(ginst(gts_id!(
                "cf.core.events.topic.v1~example.orders.x.x.v1"
            )))
            .event_type(gpat(gts_id!(
                "cf.core.events.event.v1~example.orders.order_created.x.v1~"
            )))
            .batch_handler(NoopTxBatchHandler);
    }
}
