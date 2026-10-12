//! Consumer recovery against a real broker whose seek/stream is fault-injected
//! by `SeekFaultBroker` (a decorator over `harness.broker()`). The faults are
//! the decorator's; delivery and normal seek come from the real broker.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use event_broker_sdk::gts_id;
use event_broker_sdk::{
    ConsumerBuilder, ConsumerGroupRef, EventBrokerApi, Fallback, InMemoryOffsetManager,
};
use serde_json::json;

use super::common::{publish_json, topic, topic_fixture};
use super::doubles::{BatchScopeRecorder, SeekFaultBroker};

const TOPIC: &str = gts_id!("cf.core.events.topic.v1~example.mock.showcase.reseek.v1");
const EVENT: &str = gts_id!("cf.core.events.event.v1~example.mock.showcase.reseek.v1~");

#[tokio::test]
async fn seek_recovers_from_one_shot_topology_version_mismatch() {
    let fx = topic_fixture(TOPIC, EVENT, 1).await;

    // Fault only the first seek: the consumer must re-read the subscription and
    // re-seek to recover, then deliver the event.
    let faulty = SeekFaultBroker::new(fx.broker.clone(), 1, false);
    let seek_calls = faulty.seek_calls.clone();
    let get_sub_calls = faulty.get_subscription_calls.clone();
    let broker: Arc<dyn EventBrokerApi> = Arc::new(faulty);

    let recorder = BatchScopeRecorder::default();
    let scopes = recorder.scopes.clone();

    let handle = ConsumerBuilder::new(broker.clone())
        .group(ConsumerGroupRef::auto_anonymous("reseek"))
        .topics([topic(TOPIC)])
        .offset_manager(InMemoryOffsetManager::new(Fallback::Earliest))
        .batch_handler(recorder)
        .start()
        .await
        .expect("consumer starts");

    publish_json(&fx.broker, &fx.ctx, EVENT, "s", None, json!({})).await;

    for _ in 0..300 {
        if !scopes.lock().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    handle.stop().await.expect("consumer stops");

    // The event was delivered despite the injected mismatch - recovery worked.
    assert_eq!(scopes.lock().unwrap().len(), 1);
    // The first seek faulted, the retry re-read the subscription and re-seeked.
    assert!(
        seek_calls.load(Ordering::SeqCst) >= 2,
        "expected a re-seek after the mismatch, saw {}",
        seek_calls.load(Ordering::SeqCst)
    );
    assert!(
        get_sub_calls.load(Ordering::SeqCst) >= 1,
        "expected a subscription re-read on the mismatch"
    );
}

#[tokio::test]
async fn seek_topology_mismatch_is_bounded_and_surfaces() {
    // Mirrors `dispatcher::RESEEK_ON_TOPOLOGY_MISMATCH_ATTEMPTS` (a pub(crate)
    // tuning constant not reachable from an integration test).
    const RESEEK_ATTEMPTS: u32 = 5;

    let fx = topic_fixture(TOPIC, EVENT, 1).await;

    // Every seek mismatches: recovery must give up after a bounded number of
    // re-reads and surface the error instead of looping forever.
    let faulty = SeekFaultBroker::new(fx.broker.clone(), usize::MAX, false);
    let seek_calls = faulty.seek_calls.clone();
    let get_sub_calls = faulty.get_subscription_calls.clone();
    let broker: Arc<dyn EventBrokerApi> = Arc::new(faulty);

    let recorder = BatchScopeRecorder::default();
    let scopes = recorder.scopes.clone();

    let handle = ConsumerBuilder::new(broker.clone())
        .group(ConsumerGroupRef::auto_anonymous("reseekfail"))
        .topics([topic(TOPIC)])
        .offset_manager(InMemoryOffsetManager::new(Fallback::Earliest))
        .batch_handler(recorder)
        .start()
        .await
        .expect("consumer starts");

    publish_json(&fx.broker, &fx.ctx, EVENT, "s", None, json!({})).await;

    // One resolve_and_seek issues `RESEEK_ATTEMPTS + 1` seeks (each retry
    // re-reads the subscription) then surfaces the mismatch.
    let want_seeks = (RESEEK_ATTEMPTS + 1) as usize;
    for _ in 0..300 {
        if seek_calls.load(Ordering::SeqCst) >= want_seeks {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let _ = handle.stop().await;

    assert!(
        seek_calls.load(Ordering::SeqCst) >= want_seeks,
        "expected the seek to be retried up to the budget, saw {}",
        seek_calls.load(Ordering::SeqCst)
    );
    assert!(
        get_sub_calls.load(Ordering::SeqCst) >= RESEEK_ATTEMPTS as usize,
        "expected one subscription re-read per retry"
    );
    // A persistent mismatch never masquerades as success: nothing was delivered.
    assert!(scopes.lock().unwrap().is_empty());
}

#[tokio::test]
async fn positions_not_set_at_open_is_not_masked() {
    let fx = topic_fixture(TOPIC, EVENT, 1).await;

    // No seek fault; the first stream open reports PositionsNotSet. A compliant
    // consumer seeks before opening, so this is a protocol violation the runtime
    // must surface rather than silently re-seed-and-retry.
    let faulty = SeekFaultBroker::new(fx.broker.clone(), 0, true);
    let stream_calls = faulty.stream_calls.clone();
    let broker: Arc<dyn EventBrokerApi> = Arc::new(faulty);

    let recorder = BatchScopeRecorder::default();
    let scopes = recorder.scopes.clone();

    let handle = ConsumerBuilder::new(broker.clone())
        .group(ConsumerGroupRef::auto_anonymous("pns"))
        .topics([topic(TOPIC)])
        .offset_manager(InMemoryOffsetManager::new(Fallback::Earliest))
        .batch_handler(recorder)
        .start()
        .await
        .expect("consumer starts");

    publish_json(&fx.broker, &fx.ctx, EVENT, "s", None, json!({})).await;

    for _ in 0..100 {
        if stream_calls.load(Ordering::SeqCst) >= 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    let _ = handle.stop().await;

    // Fail-fast: the run loop returned on the first PositionsNotSet - it did not
    // re-seed and re-open (which would have opened the stream a second time),
    // and nothing was delivered.
    assert_eq!(stream_calls.load(Ordering::SeqCst), 1);
    assert!(scopes.lock().unwrap().is_empty());
}
