//! Tests for [`ConsumerTypeCache`], the consumer's map from an event type to the
//! topic it publishes to.
//!
//! A second lookup is proved to make no broker call by passing a *different*
//! broker that does not know the type: if the lookup still succeeds, it was
//! served from what the cache already held.

use std::sync::Arc;

use super::type_cache::ConsumerTypeCache;
use crate::api::EventBrokerApi;
use crate::error::EventBrokerError;
use crate::mock::{MockBroker, MockBrokerHandle, stubs::test_ctx_for_tenant};

const TOPIC: &str = "gts.cf.core.events.topic.v1~example.cache.broker.orders.v1";
const OTHER_TOPIC: &str = "gts.cf.core.events.topic.v1~example.cache.broker.audit.v1";
const EVENT_TYPE: &str = "gts.cf.core.events.event.v1~example.cache.orders.created.v1~";
const OTHER_TYPE: &str = "gts.cf.core.events.event.v1~example.cache.audit.logged.v1~";

fn ctx() -> toolkit_security::SecurityContext {
    test_ctx_for_tenant(uuid::Uuid::from_u128(1))
}

/// A broker holding `topics` and, for each, one event type.
async fn broker_with(bindings: &[(&str, &str)]) -> Arc<dyn EventBrokerApi> {
    let mock = MockBroker::new();
    let handle = MockBrokerHandle::from_broker(&mock);
    for (topic, event_type) in bindings {
        handle.register_topic(topic, 2).await;
        handle
            .register_event_type(
                topic,
                event_type,
                serde_json::json!({ "type": "object" }),
                &[],
            )
            .await;
    }
    Arc::new(mock)
}

#[tokio::test]
async fn priming_resolves_the_types_of_declared_topics() {
    let broker = broker_with(&[(TOPIC, EVENT_TYPE), (OTHER_TOPIC, OTHER_TYPE)]).await;
    let cache = ConsumerTypeCache::default();

    cache
        .prime(&broker, &ctx(), &[TOPIC.to_owned()])
        .await
        .expect("priming succeeds");

    // Served from what priming held: a broker that knows nothing would fail.
    let empty = broker_with(&[]).await;
    assert_eq!(
        cache
            .topic_of(&empty, &ctx(), EVENT_TYPE)
            .await
            .expect("the declared topic's type was primed")
            .as_ref(),
        TOPIC
    );
}

#[tokio::test]
async fn priming_ignores_types_of_topics_the_consumer_did_not_declare() {
    let broker = broker_with(&[(TOPIC, EVENT_TYPE), (OTHER_TOPIC, OTHER_TYPE)]).await;
    let cache = ConsumerTypeCache::default();

    cache
        .prime(&broker, &ctx(), &[TOPIC.to_owned()])
        .await
        .expect("priming succeeds");

    let empty = broker_with(&[]).await;
    assert!(
        cache.topic_of(&empty, &ctx(), OTHER_TYPE).await.is_err(),
        "a type on an undeclared topic is not primed, so it needs the broker"
    );
}

#[tokio::test]
async fn a_consumer_declaring_a_topic_with_no_types_still_primes() {
    let mock = MockBroker::new();
    let handle = MockBrokerHandle::from_broker(&mock);
    handle.register_topic(TOPIC, 2).await;
    let broker: Arc<dyn EventBrokerApi> = Arc::new(mock);

    ConsumerTypeCache::default()
        .prime(&broker, &ctx(), &[TOPIC.to_owned()])
        .await
        .expect("a topic with no registered event types is a legitimate state");
}

#[tokio::test]
async fn a_type_absent_at_priming_is_resolved_on_first_sight() {
    let broker = broker_with(&[]).await;
    let cache = ConsumerTypeCache::default();
    cache
        .prime(&broker, &ctx(), &[TOPIC.to_owned()])
        .await
        .expect("priming an empty broker succeeds");

    // Registered after the consumer would have started.
    let later = broker_with(&[(TOPIC, EVENT_TYPE)]).await;
    assert_eq!(
        cache
            .topic_of(&later, &ctx(), EVENT_TYPE)
            .await
            .expect("a type registered later resolves on first sight")
            .as_ref(),
        TOPIC
    );
}

#[tokio::test]
async fn a_resolved_type_is_not_resolved_twice() {
    let broker = broker_with(&[(TOPIC, EVENT_TYPE)]).await;
    let cache = ConsumerTypeCache::default();

    cache
        .topic_of(&broker, &ctx(), EVENT_TYPE)
        .await
        .expect("first lookup resolves through the broker");

    let empty = broker_with(&[]).await;
    assert_eq!(
        cache
            .topic_of(&empty, &ctx(), EVENT_TYPE)
            .await
            .expect("the second lookup is served locally")
            .as_ref(),
        TOPIC
    );
}

#[tokio::test]
async fn an_unresolvable_type_is_reported_with_its_identifier() {
    let broker = broker_with(&[]).await;

    let err = ConsumerTypeCache::default()
        .topic_of(&broker, &ctx(), EVENT_TYPE)
        .await
        .expect_err("a type no broker knows cannot resolve to a topic");

    assert!(
        matches!(err, EventBrokerError::EventTypeUnknown { ref type_id, .. } if type_id == EVENT_TYPE),
        "the failure must name the type it could not resolve: {err:?}"
    );
}
