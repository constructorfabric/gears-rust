//! The runtime never mixes topics or partitions within a handler batch. Driven
//! against the real broker: two topics x two partitions, with partition keys
//! hunted so each event lands on a distinct partition (the real broker's
//! `(murmur3_x86_32(key,0) & 0x7FFFFFFF) % count`, reproduced here), so the four
//! events occupy four distinct `(topic, partition)` buffers.

use std::collections::BTreeSet;
use std::time::Duration;

use chrono::Utc;
use event_broker::test_support::StaticTypesRegistry;
use event_broker_sdk::{
    ConsumerBuilder, ConsumerGroupRef, Event, Fallback, GtsTypeId, InMemoryOffsetManager, gts_id,
};
use serde_json::json;
use toolkit_stable_hash::murmur3_x86_32;
use uuid::Uuid;

use super::common::{SHOWCASE_SUBJECT_TYPE, fixture_from_catalog, topic};
use super::doubles::BatchScopeRecorder;

const ORDERS_TOPIC: &str = gts_id!("cf.core.events.topic.v1~example.mock.showcase.orders.v1");
const PAYMENTS_TOPIC: &str = gts_id!("cf.core.events.topic.v1~example.mock.showcase.payments.v1");
const ORDERS_EVENT: &str = gts_id!("cf.core.events.event.v1~example.mock.showcase.order.v1~");
const PAYMENTS_EVENT: &str = gts_id!("cf.core.events.event.v1~example.mock.showcase.payment.v1~");

/// The broker's partition function (`domain::ingest::partition_for`), reproduced
/// so the test can choose keys that land on a chosen partition.
fn partition_for(key: &str, count: u32) -> u32 {
    (murmur3_x86_32(key.as_bytes(), 0) & 0x7FFF_FFFF) % count.max(1)
}

/// A partition-key value (an event's `subject`, which the fixture's types point
/// their partition key at) that the broker maps onto `target` of `count`.
fn key_for_partition(target: u32, count: u32) -> String {
    (0..)
        .map(|i| format!("pk-{i}"))
        .find(|key| partition_for(key, count) == target)
        .expect("a key hashing onto the target partition exists")
}

#[tokio::test]
async fn runtime_dispatch_never_mixes_topics_or_partitions_in_handler_batches() {
    // Two topics, each with an event type partitioned by `/subject`, admitting
    // the showcase subject type.
    let event_type = |id: &str, topic: &str| {
        json!({
            "id": id,
            "topic": topic,
            "partition_key": "/subject",
            "allowed_subject_types": [SHOWCASE_SUBJECT_TYPE],
            "data_schema": { "type": "object" },
        })
    };
    let fx = fixture_from_catalog(StaticTypesRegistry::of(json!([
        { "id": ORDERS_TOPIC, "partitions": 2 },
        { "id": PAYMENTS_TOPIC, "partitions": 2 },
        event_type(ORDERS_EVENT, ORDERS_TOPIC),
        event_type(PAYMENTS_EVENT, PAYMENTS_TOPIC),
    ])))
    .await;

    let subject_type = GtsTypeId::try_new(SHOWCASE_SUBJECT_TYPE).expect("valid subject type");
    let on_partition_0 = key_for_partition(0, 2);
    let on_partition_1 = key_for_partition(1, 2);

    let recorder = BatchScopeRecorder::default();
    let scopes = recorder.scopes.clone();
    let violations = recorder.violations.clone();

    let handle = ConsumerBuilder::new(fx.broker.clone())
        .group(ConsumerGroupRef::auto_anonymous("batch-scope"))
        .topics([topic(ORDERS_TOPIC), topic(PAYMENTS_TOPIC)])
        .offset_manager(InMemoryOffsetManager::new(Fallback::Earliest))
        .batch_handler(recorder)
        .start()
        .await
        .expect("consumer starts");

    for _ in 0..100 {
        if handle.subscription_ids().len() == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    // Four events across two topics x two partitions - four distinct buffers.
    for (event_type_id, subject) in [
        (ORDERS_EVENT, &on_partition_0),
        (ORDERS_EVENT, &on_partition_1),
        (PAYMENTS_EVENT, &on_partition_0),
        (PAYMENTS_EVENT, &on_partition_1),
    ] {
        fx.broker
            .publish(
                &fx.ctx,
                &Event {
                    id: Uuid::new_v4(),
                    type_id: GtsTypeId::try_new(event_type_id).expect("valid event type"),
                    tenant_id: fx.ctx.subject_tenant_id(),
                    source: "runtime.dispatch.test".to_owned(),
                    subject: subject.clone(),
                    subject_type: subject_type.clone(),
                    occurred_at: Utc::now(),
                    trace_parent: None,
                    data: Some(json!({})),
                    partition: None,
                    sequence: None,
                    sequence_time: None,
                    meta: None,
                },
            )
            .await
            .expect("event published");
    }

    for _ in 0..300 {
        if scopes.lock().unwrap().len() >= 4 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    handle.stop().await.expect("consumer stops");

    assert_eq!(*violations.lock().unwrap(), Vec::<String>::new());
    let recorded = scopes.lock().unwrap().clone();
    assert_eq!(
        recorded.len(),
        4,
        "four distinct buffers -> four batches: {recorded:?}"
    );
    assert!(
        recorded.iter().all(|(_, _, len)| *len == 1),
        "each buffer holds one event: {recorded:?}"
    );
    let topics = recorded
        .iter()
        .map(|(topic, _, _)| topic.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(topics, BTreeSet::from([ORDERS_TOPIC, PAYMENTS_TOPIC]));
}
