#![cfg(feature = "integration")]
#![allow(unknown_lints, de0901_gts_string_pattern)]
//! Integration tests for `ConsumerBuilder` typestate and `Consumer` handle.
//!
//! Run with: `cargo test -p cyberware-event-broker-sdk --features integration`

use std::sync::Arc;

use event_broker_sdk::error::ConsumerError;
use event_broker_sdk::{
    AckHandle, BrokerOffsetManager, Consumer, ConsumerBuilder, ConsumerGroupId, ConsumerGroupRef,
    EventHandler, Fallback, HandlerOutcome, InMemoryOffsetManager, NoDlq, RawEvent,
};

struct NoopHandler;

#[async_trait::async_trait]
impl EventHandler<AckHandle, HandlerOutcome> for NoopHandler {
    async fn handle(
        &self,
        _ctx: &modkit_security::SecurityContext,
        _event: RawEvent,
        _attempts: u16,
        _ack: AckHandle,
    ) -> Result<HandlerOutcome, ConsumerError> {
        Ok(HandlerOutcome::Success)
    }
}

#[test]
fn consumer_builder_chains_correctly() {
    // Verify that the typestate builder compiles to the expected quadrant.
    // This test doesn't run any async code — it just verifies the type-state
    // machinery routes correctly at compile time.
    let builder: ConsumerBuilder<NoDlq, ()> = ConsumerBuilder::new_unbound()
        .group(ConsumerGroupRef::auto_anonymous("test"))
        .topics(["gts.cf.core.events.topic.v1~orders.v1"])
        .parallelism(3);

    let with_om = builder.offset_manager(InMemoryOffsetManager::new(Fallback::Earliest));
    let _ready = with_om.handler(NoopHandler);
}

#[test]
fn broker_only_om_does_not_implement_tx_offset_manager() {
    // Compile-time assertion: BrokerOffsetManager only implements OffsetManager.
    // The line below MUST NOT compile:
    //
    //   let _: &dyn event_broker_sdk::TxOffsetManager = &BrokerOffsetManager::new(Fallback::Earliest);
    //
    // The positive assertion (BrokerOffsetManager implements OffsetManager) is tested here.
    let _om: Arc<dyn event_broker_sdk::OffsetManager> =
        Arc::new(BrokerOffsetManager::new(Fallback::Earliest));
}

#[tokio::test]
async fn consumer_new_unbound_starts_empty() {
    let consumer = Consumer::new(3);
    // No slots were spawned; subscription_ids is empty.
    assert!(consumer.subscription_ids().is_empty());
    // Shutdown on an unbound consumer is a no-op.
    let ctx = modkit_security::SecurityContext::anonymous();
    consumer.shutdown(&ctx).await.unwrap();
}
