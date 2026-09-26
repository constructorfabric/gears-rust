//! Consumer-handle lifecycle against the real broker: independent handles, each
//! exposing the subscription slots its parallelism created.

use std::time::Duration;

use async_trait::async_trait;
use event_broker::test_support::StaticTypesRegistry;
use event_broker_sdk::{
    ConsumerBuilder, ConsumerError, ConsumerGroupRef, ConsumerProfile, Fallback, HandlerOutcome,
    InMemoryOffsetManager, RawEvent, SingleEventHandler, gts_id,
};
use serde_json::json;

use super::common::{fixture_from_catalog, topic};

const ORDERS_TOPIC: &str = gts_id!("cf.core.events.topic.v1~cf.core.orders.topic.v1");
const PAYMENTS_TOPIC: &str = gts_id!("cf.core.events.topic.v1~cf.core.payments.topic.v1");

struct NoopHandler;

#[async_trait]
impl SingleEventHandler for NoopHandler {
    async fn handle(
        &self,
        _event: RawEvent,
        _attempts: u16,
    ) -> Result<HandlerOutcome, ConsumerError> {
        Ok(HandlerOutcome::Success)
    }
}

#[tokio::test]
async fn multiple_consumer_handles_run_with_independent_lifecycle() {
    let fixture = fixture_from_catalog(StaticTypesRegistry::of(json!([
        { "id": ORDERS_TOPIC, "partitions": 4 },
        { "id": PAYMENTS_TOPIC, "partitions": 4 },
    ])))
    .await;

    let orders = ConsumerBuilder::new(fixture.broker.clone())
        .group(ConsumerGroupRef::auto_anonymous("orders-handle"))
        .topics([topic(ORDERS_TOPIC)])
        .profile(ConsumerProfile::low_latency())
        .parallelism(2)
        .offset_manager(InMemoryOffsetManager::new(Fallback::Earliest))
        .handler(NoopHandler)
        .start()
        .await
        .expect("orders consumer starts");
    let payments = ConsumerBuilder::new(fixture.broker.clone())
        .group(ConsumerGroupRef::auto_anonymous("payments-handle"))
        .topics([topic(PAYMENTS_TOPIC)])
        .profile(ConsumerProfile::high_throughput())
        .parallelism(1)
        .offset_manager(InMemoryOffsetManager::new(Fallback::Latest))
        .handler(NoopHandler)
        .start()
        .await
        .expect("payments consumer starts");

    for _ in 0..200 {
        if orders.subscription_ids().len() == 2 && payments.subscription_ids().len() == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

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
