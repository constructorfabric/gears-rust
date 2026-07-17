//! Consumer builder and handle showcase tests.

use std::sync::Arc;

use event_broker_sdk::error::ConsumerError;
use event_broker_sdk::{
    Consumer, ConsumerBuilder, ConsumerGroupRef, Fallback, HandlerOutcome, InMemoryOffsetManager,
    RawEvent, SingleEventHandler,
};

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

#[test]
fn consumer_builder_chains_correctly() {
    // Verify that the typestate builder compiles to the expected quadrant.
    // This test doesn't run any async code — it just verifies the type-state
    // machinery routes correctly at compile time.
    let builder: ConsumerBuilder<()> = ConsumerBuilder::new_unbound()
        .group(ConsumerGroupRef::auto_anonymous("test"))
        .topics(["gts.cf.core.events.topic.v1~example.orders.v1"])
        .parallelism(3);

    let with_om = builder.offset_manager(InMemoryOffsetManager::new(Fallback::Earliest));
    let _ready = with_om.handler(NoopHandler);
}

#[test]
fn async_commit_om_does_not_implement_commit_offset_in_tx() {
    // Compile-time assertion: InMemoryOffsetManager implements CommitOffset (async)
    // but NOT CommitOffsetInTx. The line below MUST NOT compile:
    //
    //   let _: &dyn event_broker_sdk::CommitOffsetInTx = &InMemoryOffsetManager::new(Fallback::Earliest);
    //
    // The positive assertion (it implements CommitOffset) is tested here.
    let _om: Arc<dyn event_broker_sdk::CommitOffset> =
        Arc::new(InMemoryOffsetManager::new(Fallback::Earliest));
}

#[tokio::test]
async fn consumer_new_unbound_starts_empty() {
    let consumer = Consumer::new(3);
    // No slots were spawned; subscription_ids is empty.
    assert!(consumer.subscription_ids().is_empty());
    // Shutdown on an unbound consumer is a no-op.
    consumer.shutdown().await.unwrap();
}
