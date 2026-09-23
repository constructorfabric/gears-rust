//! Consumer-runtime behaviour (slow detection, listener delivery, handler
//! latency) against the real broker, with a sub-second harness heartbeat so the
//! detection/drop/rejoin cadence runs fast. Every assertion is on the runtime's
//! own reports over a real delivery - nothing fakes broker behaviour.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::Utc;
use event_broker::test_support::{EventBrokerHarness, StaticTypesRegistry};
use event_broker_sdk::{
    BatchHandlerOutcome, ConnectionDropReason, ConsumerBuffering, ConsumerBuilder,
    ConsumerCommitMode, ConsumerGroupRef, ConsumerListenerSettings, ConsumerRuntimeEvent,
    ConsumerSlowDetection, Event, Fallback, GtsTypeId, InMemoryOffsetManager, PartitionBufferState,
    Sequence, SlowConsumerTrigger, gts_id,
};
use serde_json::{Value, json};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::common::{SHOWCASE_SUBJECT_TYPE, topic};
use super::doubles::{
    AckAllBatchHandler, FailingRuntimeListener, FailingThenCommitBatchHandler,
    RecordingCommitOffsetManager, RecordingRuntimeListener, RuntimeEventKind,
    SequencedBatchHandler, SequencedOffsetManager, SharedRuntimeEvents, SharedTimeline,
    SleepingBatchHandler, SlowRuntimeListener, runtime_event_kind,
};

/// A harness with a 10ms heartbeat, so slow-detection and idle cadence run fast
/// instead of waiting out the production 1s tick.
async fn fast_harness(topic_id: &str, event_type: &str, partitions: u32) -> EventBrokerHarness {
    EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": topic_id, "partitions": partitions },
            {
                "id": event_type,
                "topic": topic_id,
                "allowed_subject_types": [SHOWCASE_SUBJECT_TYPE],
                "data_schema": { "type": "object" },
            },
        ])))
        .with_heartbeat(Duration::from_millis(10))
        .build()
        .await
}

async fn publish_one(
    broker: &Arc<dyn event_broker_sdk::EventBrokerApi>,
    ctx: &SecurityContext,
    event_type: &str,
    subject: &str,
    data: Value,
) {
    broker
        .publish(
            ctx,
            &Event {
                id: Uuid::new_v4(),
                type_id: GtsTypeId::try_new(event_type).expect("valid event type"),
                tenant_id: ctx.subject_tenant_id(),
                source: "runtime.behaviour.test".to_owned(),
                subject: subject.to_owned(),
                subject_type: GtsTypeId::try_new(SHOWCASE_SUBJECT_TYPE)
                    .expect("valid subject type"),
                occurred_at: Utc::now(),
                trace_parent: None,
                data: Some(data),
                partition: None,
                sequence: None,
                sequence_time: None,
                meta: None,
            },
        )
        .await
        .expect("event published");
}

async fn wait_for_runtime_event_kinds(
    recorded: &SharedRuntimeEvents,
    expected: &BTreeSet<RuntimeEventKind>,
) {
    for _ in 0..300 {
        let observed = recorded
            .lock()
            .unwrap()
            .iter()
            .map(runtime_event_kind)
            .collect::<BTreeSet<_>>();
        if expected.is_subset(&observed) {
            return;
        }
        drop(observed);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!(
        "missing representative runtime event kinds; expected={expected:?}, events={:?}",
        recorded.lock().unwrap()
    );
}

#[tokio::test]
async fn slow_detection_emits_listener_events_and_drops_subscription_stream() {
    const TOPIC: &str = gts_id!("cf.core.events.topic.v1~example.mock.showcase.slow.v1");
    const EVENT_TYPE: &str = gts_id!("cf.core.events.event.v1~example.mock.showcase.slow.v1~");

    let fx = fast_harness(TOPIC, EVENT_TYPE, 1).await;
    let broker = fx.broker();
    let ctx = SecurityContext::anonymous();
    let listener = RecordingRuntimeListener::default();
    let recorded = listener.events.clone();
    let calls = Arc::new(Mutex::new(Vec::new()));

    let handle = ConsumerBuilder::new(broker.clone())
        .group(ConsumerGroupRef::auto_anonymous("slow-drop"))
        .topics([topic(TOPIC)])
        .buffering(ConsumerBuffering {
            partition_capacity: 8,
            high_watermark: 1,
            low_watermark: 0,
        })
        .register_listener(listener)
        .offset_manager(InMemoryOffsetManager::new(Fallback::Earliest))
        .batch_handler(AckAllBatchHandler {
            calls: calls.clone(),
        })
        .start()
        .await
        .expect("consumer starts");

    for _ in 0..100 {
        if handle.subscription_ids().len() == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    publish_one(
        &broker,
        &ctx,
        EVENT_TYPE,
        "slow-subject",
        json!({ "slow": true }),
    )
    .await;

    for _ in 0..300 {
        let events = recorded.lock().unwrap().clone();
        let has_state = events.iter().any(|event| {
            matches!(
                event,
                ConsumerRuntimeEvent::PartitionBufferStateChanged { state }
                    if state.state == PartitionBufferState::SlowDetected
            )
        });
        let has_drop = events.iter().any(|event| {
            matches!(
                event,
                ConsumerRuntimeEvent::SubscriptionConnectionDropped {
                    reason: ConnectionDropReason::SlowConsumer { .. },
                    affected,
                    ..
                } if !affected.is_empty()
            )
        });
        if has_state && has_drop {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    handle.stop().await.expect("consumer stops");

    let events = recorded.lock().unwrap();
    assert!(
        events.iter().any(|event| {
            matches!(
                event,
                ConsumerRuntimeEvent::PartitionBufferStateChanged { state }
                    if state.state == PartitionBufferState::SlowDetected
                        && state.topic == TOPIC
                        && state.buffered_count == 1
            )
        }),
        "missing slow buffer state event: {events:?}"
    );
    assert!(
        events.iter().any(|event| {
            matches!(
                event,
                ConsumerRuntimeEvent::SubscriptionConnectionDropped {
                    reason: ConnectionDropReason::SlowConsumer { topic, .. },
                    affected,
                    ..
                } if topic == TOPIC && !affected.is_empty()
            )
        }),
        "missing slow connection drop event: {events:?}"
    );
    assert!(
        calls
            .lock()
            .unwrap()
            .iter()
            .any(|(t, partition, _)| t.as_str() == TOPIC && *partition == 0),
        "slow event should be drained through the handler before rejoin"
    );
}

#[tokio::test]
async fn runtime_listener_observes_representative_non_dlq_event_variants() {
    const TOPIC: &str = gts_id!("cf.core.events.topic.v1~example.mock.showcase.listenerall.v1");
    const EVENT_TYPE: &str =
        gts_id!("cf.core.events.event.v1~example.mock.showcase.listenerall.v1~");

    let fx = fast_harness(TOPIC, EVENT_TYPE, 1).await;
    let broker = fx.broker();
    let ctx = SecurityContext::anonymous();
    let listener = RecordingRuntimeListener::default();
    let recorded = listener.events.clone();
    let handler_calls = Arc::new(Mutex::new(Vec::new()));

    let handle = ConsumerBuilder::new(broker.clone())
        .group(ConsumerGroupRef::auto_anonymous("listener-all"))
        .topics([topic(TOPIC)])
        .buffering(ConsumerBuffering {
            partition_capacity: 8,
            high_watermark: 1,
            low_watermark: 0,
        })
        .retry_base(Duration::from_millis(1))
        .retry_max(Duration::from_millis(1))
        .register_listener(listener)
        .offset_manager(InMemoryOffsetManager::new(Fallback::Earliest))
        .batch_handler(FailingThenCommitBatchHandler {
            failures_remaining: Arc::new(Mutex::new(1)),
            calls: handler_calls.clone(),
        })
        .start()
        .await
        .expect("consumer starts");

    for _ in 0..100 {
        if handle.subscription_ids().len() == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    publish_one(
        &broker,
        &ctx,
        EVENT_TYPE,
        "listener-all-subject",
        json!({ "representative": true }),
    )
    .await;

    let expected_before_stop = BTreeSet::from([
        RuntimeEventKind::SubscriptionJoining,
        RuntimeEventKind::SubscriptionStarted,
        RuntimeEventKind::SubscriptionRejoining,
        RuntimeEventKind::SubscriptionConnectionDropped,
        RuntimeEventKind::AssignmentChanged,
        RuntimeEventKind::ProgressAdvanced,
        RuntimeEventKind::PartitionBufferStateChanged,
        RuntimeEventKind::HandlerBatchStarted,
        RuntimeEventKind::HandlerBatchCompleted,
        RuntimeEventKind::HandlerFailed,
        RuntimeEventKind::OffsetLoaded,
        RuntimeEventKind::OffsetCommitted,
        RuntimeEventKind::RetryScheduled,
    ]);
    wait_for_runtime_event_kinds(&recorded, &expected_before_stop).await;

    handle.stop().await.expect("consumer stops");

    let expected_after_stop = expected_before_stop
        .into_iter()
        .chain([RuntimeEventKind::SubscriptionTerminated])
        .collect::<BTreeSet<_>>();
    wait_for_runtime_event_kinds(&recorded, &expected_after_stop).await;

    assert_eq!(*handler_calls.lock().unwrap(), vec![2]);
}

#[tokio::test]
async fn listener_failure_does_not_commit_drop_or_stop_consumer_events() {
    const TOPIC: &str = gts_id!("cf.core.events.topic.v1~example.mock.showcase.listenerfail.v1");
    const EVENT_TYPE: &str =
        gts_id!("cf.core.events.event.v1~example.mock.showcase.listenerfail.v1~");

    let fx = fast_harness(TOPIC, EVENT_TYPE, 1).await;
    let broker = fx.broker();
    let ctx = SecurityContext::anonymous();
    let recorder = RecordingRuntimeListener::default();
    let recorded_events = recorder.events.clone();
    let handler_calls = Arc::new(Mutex::new(Vec::new()));
    let offset_manager = RecordingCommitOffsetManager::default();
    let commits = offset_manager.commits.clone();

    let handle = ConsumerBuilder::new(broker.clone())
        .group(ConsumerGroupRef::auto_anonymous("listener-failure"))
        .topics([topic(TOPIC)])
        .commit_mode(ConsumerCommitMode::manual())
        .register_listener(FailingRuntimeListener)
        .register_listener(recorder)
        .offset_manager(offset_manager)
        .batch_handler(AckAllBatchHandler {
            calls: handler_calls.clone(),
        })
        .start()
        .await
        .expect("consumer starts despite listener that will fail");

    for _ in 0..100 {
        if handle.subscription_ids().len() == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    for offset in 0..2 {
        publish_one(
            &broker,
            &ctx,
            EVENT_TYPE,
            &format!("listener-failure-{offset}"),
            json!({ "offset": offset }),
        )
        .await;
    }

    for _ in 0..300 {
        if handler_calls.lock().unwrap().len() >= 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let active_subscriptions = handle.subscription_ids();
    handle.stop().await.expect("consumer stops cleanly");

    let calls = handler_calls.lock().unwrap().clone();
    assert_eq!(
        calls.len(),
        2,
        "listener failure must not drop consumer events before handler dispatch"
    );
    let commits = commits.lock().unwrap().clone();
    assert!(
        commits
            .iter()
            .all(|(_, _, _, offset)| *offset == Sequence::NONE.as_i64()),
        "listener failure must not durably advance delivered event offsets by itself: {commits:?}"
    );
    assert!(
        !active_subscriptions.is_empty(),
        "listener failure must not stop the consumer by itself"
    );
    let events = recorded_events.lock().unwrap();
    assert!(
        !events.iter().any(|event| {
            matches!(
                event,
                ConsumerRuntimeEvent::SubscriptionConnectionDropped { .. }
            )
        }),
        "listener failure must not drop the subscription stream: {events:?}"
    );
    assert!(
        events.iter().any(|event| {
            matches!(
                event,
                ConsumerRuntimeEvent::HandlerBatchCompleted {
                    outcome: BatchHandlerOutcome::AdvanceThrough { .. },
                    ..
                }
            )
        }),
        "the recording listener should still observe handler success after another listener fails"
    );
}

#[tokio::test]
async fn slow_listener_timeout_does_not_block_runtime_delivery_or_handler_processing() {
    const TOPIC: &str = gts_id!("cf.core.events.topic.v1~example.mock.showcase.listenerto.v1");
    const EVENT_TYPE: &str =
        gts_id!("cf.core.events.event.v1~example.mock.showcase.listenerto.v1~");

    let fx = fast_harness(TOPIC, EVENT_TYPE, 1).await;
    let broker = fx.broker();
    let ctx = SecurityContext::anonymous();
    let recorder = RecordingRuntimeListener::default();
    let recorded_events = recorder.events.clone();
    let handler_calls = Arc::new(Mutex::new(Vec::new()));

    let handle = ConsumerBuilder::new(broker.clone())
        .group(ConsumerGroupRef::auto_anonymous("listener-timeout"))
        .topics([topic(TOPIC)])
        .listener_settings(ConsumerListenerSettings {
            channel_capacity: 8,
            timeout: Duration::from_millis(1),
        })
        .register_listener(SlowRuntimeListener {
            delay: Duration::from_secs(60),
        })
        .register_listener(recorder)
        .offset_manager(InMemoryOffsetManager::new(Fallback::Earliest))
        .batch_handler(AckAllBatchHandler {
            calls: handler_calls.clone(),
        })
        .start()
        .await
        .expect("consumer starts");

    for _ in 0..100 {
        if handle.subscription_ids().len() == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    publish_one(
        &broker,
        &ctx,
        EVENT_TYPE,
        "listener-timeout-subject",
        json!({ "timeout": true }),
    )
    .await;

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let handler_done = !handler_calls.lock().unwrap().is_empty();
            let listener_done = recorded_events.lock().unwrap().iter().any(|event| {
                matches!(
                    event,
                    ConsumerRuntimeEvent::HandlerBatchCompleted {
                        outcome: BatchHandlerOutcome::AdvanceThrough { .. },
                        ..
                    }
                )
            });
            if handler_done && listener_done {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("slow listener timeout should let processing continue");

    handle.stop().await.expect("consumer stops");

    assert_eq!(handler_calls.lock().unwrap().len(), 1);
    assert!(
        recorded_events
            .lock()
            .unwrap()
            .iter()
            .any(|event| matches!(event, ConsumerRuntimeEvent::SubscriptionStarted { .. })),
        "recording listener should receive events after the slow listener times out"
    );
}

#[tokio::test]
async fn handler_latency_strikes_emit_listener_events_and_drop_subscription_stream() {
    const TOPIC: &str = gts_id!("cf.core.events.topic.v1~example.mock.showcase.latency.v1");
    const EVENT_TYPE: &str = gts_id!("cf.core.events.event.v1~example.mock.showcase.latency.v1~");

    let fx = fast_harness(TOPIC, EVENT_TYPE, 1).await;
    let broker = fx.broker();
    let ctx = SecurityContext::anonymous();
    let listener = RecordingRuntimeListener::default();
    let recorded = listener.events.clone();
    let calls = Arc::new(Mutex::new(Vec::new()));

    let handle = ConsumerBuilder::new(broker.clone())
        .group(ConsumerGroupRef::auto_anonymous("latency-drop"))
        .topics([topic(TOPIC)])
        .buffering(ConsumerBuffering {
            partition_capacity: 8,
            high_watermark: 8,
            low_watermark: 0,
        })
        .slow_detection(ConsumerSlowDetection {
            handler_latency: Duration::from_millis(1),
            handler_strikes: 1,
        })
        .register_listener(listener)
        .offset_manager(InMemoryOffsetManager::new(Fallback::Earliest))
        .batch_handler(SleepingBatchHandler {
            calls: calls.clone(),
            delay: Duration::from_millis(10),
        })
        .start()
        .await
        .expect("consumer starts");

    for _ in 0..100 {
        if handle.subscription_ids().len() == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    publish_one(
        &broker,
        &ctx,
        EVENT_TYPE,
        "latency-subject",
        json!({ "slow": "handler" }),
    )
    .await;

    for _ in 0..300 {
        let events = recorded.lock().unwrap().clone();
        let has_latency_drop = events.iter().any(|event| {
            matches!(
                event,
                ConsumerRuntimeEvent::SubscriptionConnectionDropped {
                    reason: ConnectionDropReason::SlowConsumer {
                        trigger: SlowConsumerTrigger::HandlerLatencyStrikes,
                        ..
                    },
                    affected,
                    ..
                } if !affected.is_empty()
            )
        });
        if has_latency_drop {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    handle.stop().await.expect("consumer stops");

    assert!(
        calls
            .lock()
            .unwrap()
            .iter()
            .any(|(t, partition, _)| t.as_str() == TOPIC && *partition == 0),
        "slow handler should process the event before latency drop"
    );
    let events = recorded.lock().unwrap();
    assert!(
        events.iter().any(|event| {
            matches!(
                event,
                ConsumerRuntimeEvent::PartitionBufferStateChanged { state }
                    if state.state == PartitionBufferState::SlowDetected
                        && state.topic == TOPIC
                        && state.trigger == Some(SlowConsumerTrigger::HandlerLatencyStrikes)
                        && state.consecutive_slow_handlers == 1
            )
        }),
        "missing latency slow buffer state event: {events:?}"
    );
    assert!(
        events.iter().any(|event| {
            matches!(
                event,
                ConsumerRuntimeEvent::SubscriptionConnectionDropped {
                    reason: ConnectionDropReason::SlowConsumer {
                        topic,
                        trigger: SlowConsumerTrigger::HandlerLatencyStrikes,
                        ..
                    },
                    affected,
                    ..
                } if topic == TOPIC && !affected.is_empty()
            )
        }),
        "missing latency connection drop event: {events:?}"
    );
}

#[tokio::test]
async fn slow_drop_reports_other_assignments_owned_by_same_subscription_slot() {
    const TOPIC: &str = gts_id!("cf.core.events.topic.v1~example.mock.showcase.affected.v1");
    const EVENT_TYPE: &str = gts_id!("cf.core.events.event.v1~example.mock.showcase.affected.v1~");

    let fx = fast_harness(TOPIC, EVENT_TYPE, 2).await;
    let broker = fx.broker();
    let ctx = SecurityContext::anonymous();
    let listener = RecordingRuntimeListener::default();
    let recorded = listener.events.clone();

    let handle = ConsumerBuilder::new(broker.clone())
        .group(ConsumerGroupRef::auto_anonymous("affected-assignments"))
        .topics([topic(TOPIC)])
        .buffering(ConsumerBuffering {
            partition_capacity: 8,
            high_watermark: 1,
            low_watermark: 0,
        })
        .register_listener(listener)
        .offset_manager(InMemoryOffsetManager::new(Fallback::Earliest))
        .batch_handler(AckAllBatchHandler {
            calls: Arc::new(Mutex::new(Vec::new())),
        })
        .start()
        .await
        .expect("consumer starts");

    for _ in 0..100 {
        if handle.subscription_ids().len() == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    publish_one(
        &broker,
        &ctx,
        EVENT_TYPE,
        "affected-subject",
        json!({ "slow": true }),
    )
    .await;

    let affected = wait_for_slow_drop_affected_assignments(&recorded).await;
    handle.stop().await.expect("consumer stops");

    // The single anonymous consumer owns both partitions on one subscription
    // slot, so dropping that slot's stream must report every partition it holds,
    // not only the one that overflowed.
    assert_eq!(
        affected,
        BTreeSet::from([(TOPIC.to_owned(), 0), (TOPIC.to_owned(), 1)]),
        "slow partition should report every assignment on the dropped subscription slot"
    );
}

async fn wait_for_slow_drop_affected_assignments(
    recorded: &SharedRuntimeEvents,
) -> BTreeSet<(String, u32)> {
    for _ in 0..300 {
        let events = recorded.lock().unwrap().clone();
        if let Some(affected) = events.iter().find_map(|event| {
            if let ConsumerRuntimeEvent::SubscriptionConnectionDropped {
                reason: ConnectionDropReason::SlowConsumer { .. },
                affected,
                ..
            } = event
            {
                Some(
                    affected
                        .iter()
                        .map(|slot| (slot.topic.to_string(), slot.partition))
                        .collect::<BTreeSet<_>>(),
                )
            } else {
                None
            }
        }) {
            return affected;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!(
        "did not observe slow-drop affected assignments; events={:?}",
        recorded.lock().unwrap()
    );
}

#[tokio::test]
async fn slow_drop_drains_buffer_before_rejoin_load_position() {
    const TOPIC: &str = gts_id!("cf.core.events.topic.v1~example.mock.showcase.drainrejoin.v1");
    const EVENT_TYPE: &str =
        gts_id!("cf.core.events.event.v1~example.mock.showcase.drainrejoin.v1~");

    let fx = fast_harness(TOPIC, EVENT_TYPE, 1).await;
    let broker = fx.broker();
    let ctx = SecurityContext::anonymous();
    let timeline: SharedTimeline = Arc::new(Mutex::new(Vec::new()));

    let handle = ConsumerBuilder::new(broker.clone())
        .group(ConsumerGroupRef::auto_anonymous("drain-rejoin"))
        .topics([topic(TOPIC)])
        .buffering(ConsumerBuffering {
            partition_capacity: 8,
            high_watermark: 1,
            low_watermark: 0,
        })
        .commit_mode(ConsumerCommitMode::manual())
        .offset_manager(SequencedOffsetManager {
            timeline: timeline.clone(),
        })
        .batch_handler(SequencedBatchHandler {
            timeline: timeline.clone(),
        })
        .start()
        .await
        .expect("consumer starts");

    for _ in 0..100 {
        if handle.subscription_ids().len() == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    publish_one(
        &broker,
        &ctx,
        EVENT_TYPE,
        "drain-rejoin-subject",
        json!({ "slow": "high-watermark" }),
    )
    .await;

    let observed = wait_for_drain_rejoin_timeline(&timeline).await;
    handle.stop().await.expect("consumer stops");

    let first_load = observed
        .iter()
        .position(|entry| *entry == "load")
        .expect("initial load recorded");
    let first_handle = observed
        .iter()
        .position(|entry| *entry == "handle")
        .expect("buffer drain handler recorded");
    let second_load_after_handle = observed
        .iter()
        .enumerate()
        .skip(first_handle + 1)
        .find_map(|(idx, entry)| (*entry == "load").then_some(idx))
        .expect("rejoin load recorded after drain");

    assert!(
        first_load < first_handle && first_handle < second_load_after_handle,
        "expected load -> handle -> load ordering, got {observed:?}"
    );
}

async fn wait_for_drain_rejoin_timeline(timeline: &SharedTimeline) -> Vec<&'static str> {
    for _ in 0..300 {
        let observed = timeline.lock().unwrap().clone();
        let Some(first_handle) = observed.iter().position(|entry| *entry == "handle") else {
            drop(observed);
            tokio::time::sleep(Duration::from_millis(10)).await;
            continue;
        };
        if observed
            .iter()
            .skip(first_handle + 1)
            .any(|entry| *entry == "load")
        {
            return observed;
        }
        drop(observed);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!(
        "did not observe drain-before-rejoin sequence; timeline={:?}",
        timeline.lock().unwrap()
    );
}

/// Documents a real gap, so it is ignored rather than asserting the wrong
/// behaviour: the single-process broker's group coordinator assigns every
/// partition to every member, so two parallel slots each receive all four
/// partitions instead of two disjoint halves. When the coordinator distributes
/// partitions exclusively across a group's members, drop the `#[ignore]`.
#[ignore = "single-process broker assigns every partition to every group member; parallel slots are not yet disjoint"]
#[tokio::test]
async fn parallelism_creates_independent_slots_with_shared_group_and_interests() {
    use std::collections::BTreeMap;

    const TOPIC: &str = gts_id!("cf.core.events.topic.v1~example.mock.showcase.parallel.v1");
    const EVENT_TYPE: &str = gts_id!("cf.core.events.event.v1~example.mock.showcase.parallel.v1~");

    let fx = fast_harness(TOPIC, EVENT_TYPE, 4).await;
    let broker = fx.broker();
    let listener = RecordingRuntimeListener::default();
    let recorded = listener.events.clone();

    let handle = ConsumerBuilder::new(broker.clone())
        .group(ConsumerGroupRef::auto_anonymous("parallel-slots"))
        .topics([topic(TOPIC)])
        .parallelism(2)
        .register_listener(listener)
        .offset_manager(InMemoryOffsetManager::new(Fallback::Earliest))
        .batch_handler(AckAllBatchHandler {
            calls: Arc::new(Mutex::new(Vec::new())),
        })
        .start()
        .await
        .expect("consumer starts");

    // The assignment each slot holds is read from its own topology frame, i.e.
    // the AssignmentChanged the runtime emits per subscription - never a
    // control-plane re-read, which the real broker does not offer.
    let mut per_slot: BTreeMap<String, BTreeSet<(String, u32)>> = BTreeMap::new();
    for _ in 0..300 {
        per_slot.clear();
        for event in recorded.lock().unwrap().iter() {
            if let ConsumerRuntimeEvent::AssignmentChanged {
                subscription_id,
                assigned,
            } = event
            {
                per_slot.insert(
                    format!("{subscription_id:?}"),
                    assigned
                        .iter()
                        .map(|slot| (slot.topic.to_string(), slot.partition))
                        .collect(),
                );
            }
        }
        let non_empty = per_slot.values().filter(|set| !set.is_empty()).count();
        let covered = per_slot
            .values()
            .flatten()
            .cloned()
            .collect::<BTreeSet<_>>();
        if non_empty == 2 && covered.len() == 4 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    handle.stop().await.expect("consumer stops");

    let non_empty_slots = per_slot
        .values()
        .filter(|set| !set.is_empty())
        .collect::<Vec<_>>();
    assert_eq!(
        non_empty_slots.len(),
        2,
        "two parallel slots should each own a share of the partitions; got {per_slot:?}"
    );
    for assignment in &non_empty_slots {
        assert_eq!(
            assignment.len(),
            2,
            "each of two slots should own half of the four partitions; got {per_slot:?}"
        );
    }
    let covered = per_slot
        .values()
        .flatten()
        .cloned()
        .collect::<BTreeSet<_>>();
    assert_eq!(
        covered,
        (0..4)
            .map(|partition| (TOPIC.to_owned(), partition))
            .collect::<BTreeSet<_>>(),
        "parallel slots should cover every partition exactly once"
    );
}
