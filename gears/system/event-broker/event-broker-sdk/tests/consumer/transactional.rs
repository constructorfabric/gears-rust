//! Transactional-consumer lifecycle and commit semantics against the real
//! broker. The tx offset-store/handler doubles impl the public
//! `event_broker_sdk::consumer` typestate traits; there is no mock.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use event_broker_sdk::consumer::{
    CommitOffsetInTx, ConsumerOffsetManager, OffsetManagerError, OffsetStore, Position,
    TxCommitHandle, TxConsumerHandler, TxSingleEventHandler, WithTx,
};
use event_broker_sdk::{
    ConsumerBuilder, ConsumerCommitMode, ConsumerError, ConsumerGroupId, ConsumerGroupRef,
    EventBatch, Fallback, HandlerOutcome, RawEvent, Sequence, TopicId, gts_id,
};
use serde_json::json;

use super::common::{publish_json, topic, topic_fixture};

const TOPIC: &str = gts_id!("cf.core.events.topic.v1~cf.core.orders.topic.v1");
const EVENT_TYPE: &str = gts_id!("cf.core.events.event.v1~example.mock.showcase.tx.v1~");

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

#[derive(Clone, Default)]
struct CountingTxOffsetManager {
    commits: Arc<AtomicUsize>,
}

#[async_trait]
impl OffsetStore for CountingTxOffsetManager {
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
impl CommitOffsetInTx for CountingTxOffsetManager {
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
        self.commits.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

impl ConsumerOffsetManager for CountingTxOffsetManager {
    type BuilderState = WithTx<Self>;

    fn into_builder_state(self) -> Self::BuilderState {
        WithTx(self)
    }
}

/// Processes the event (so delivery is proven) but never calls `commit`, so the
/// runtime must not auto-commit on its behalf.
struct NoCommitTxHandler {
    handled: Arc<AtomicUsize>,
}

#[async_trait]
impl TxSingleEventHandler<CountingTxOffsetManager> for NoCommitTxHandler {
    async fn handle(
        &self,
        _event: RawEvent,
        _attempts: u16,
        _commit: TxCommitHandle<CountingTxOffsetManager>,
    ) -> Result<HandlerOutcome, ConsumerError> {
        self.handled.fetch_add(1, Ordering::SeqCst);
        Ok(HandlerOutcome::Success)
    }
}

#[tokio::test]
async fn transactional_consumer_start_paths_return_lifecycle_handles() {
    let fx = topic_fixture(TOPIC, EVENT_TYPE, 3).await;

    let single = ConsumerBuilder::new(fx.broker.clone())
        .group(ConsumerGroupRef::auto_anonymous("tx-single-start"))
        .topics([topic(TOPIC)])
        .parallelism(1)
        .offset_manager(RecordingTxOffsetManager)
        .handler(NoopTxHandler)
        .start()
        .await
        .expect("transactional single consumer starts");
    let batch = ConsumerBuilder::new(fx.broker.clone())
        .group(ConsumerGroupRef::auto_anonymous("tx-batch-start"))
        .topics([topic(TOPIC)])
        .parallelism(1)
        .offset_manager(RecordingTxOffsetManager)
        .batch_handler(NoopTxBatchHandler)
        .start()
        .await
        .expect("transactional batch consumer starts");
    let routed = ConsumerBuilder::new(fx.broker.clone())
        .group(ConsumerGroupRef::auto_anonymous("tx-routed-start"))
        .topics([topic(TOPIC)])
        .parallelism(1)
        .offset_manager(RecordingTxOffsetManager)
        .default_handler(NoopTxHandler)
        .route()
        .topic(topic(TOPIC))
        .batch_handler(NoopTxBatchHandler)
        .start()
        .await
        .expect("transactional routed consumer starts");

    for _ in 0..200 {
        if single.subscription_ids().len() == 1
            && batch.subscription_ids().len() == 1
            && routed.subscription_ids().len() == 1
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(single.subscription_ids().len(), 1);
    assert_eq!(batch.subscription_ids().len(), 1);
    assert_eq!(routed.subscription_ids().len(), 1);

    single.stop().await.expect("single stops");
    batch.stop().await.expect("batch stops");
    routed.stop().await.expect("routed stops");
}

#[tokio::test]
async fn transactional_consumer_does_not_auto_commit_when_handler_omits_commit_in_tx() {
    let fx = topic_fixture(TOPIC, EVENT_TYPE, 1).await;
    let manager = CountingTxOffsetManager::default();
    let commits = manager.commits.clone();
    let handled = Arc::new(AtomicUsize::new(0));

    let handle = ConsumerBuilder::new(fx.broker.clone())
        .group(ConsumerGroupRef::auto_anonymous("tx-no-auto-commit"))
        .topics([topic(TOPIC)])
        .commit_mode(ConsumerCommitMode::auto(Duration::from_millis(5)))
        .offset_manager(manager)
        .handler(NoCommitTxHandler {
            handled: handled.clone(),
        })
        .start()
        .await
        .expect("transactional consumer starts");

    for _ in 0..100 {
        if handle.subscription_ids().len() == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    publish_json(
        &fx.broker,
        &fx.ctx,
        EVENT_TYPE,
        "subject-1",
        None,
        json!({ "ok": true }),
    )
    .await;

    // Wait for the event to actually be handled - otherwise `commits == 0` would
    // pass trivially on a consumer that received nothing.
    for _ in 0..300 {
        if handled.load(Ordering::SeqCst) >= 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    // Give any (erroneous) auto-commit a chance to fire before asserting it didn't.
    tokio::time::sleep(Duration::from_millis(50)).await;

    handle.stop().await.expect("consumer stops");
    assert!(
        handled.load(Ordering::SeqCst) >= 1,
        "the event must be delivered for this assertion to mean anything"
    );
    assert_eq!(
        commits.load(Ordering::SeqCst),
        0,
        "a handler that omits commit_in_tx must not be auto-committed"
    );
}
