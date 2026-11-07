//! Consumer recovery against a real broker whose seek/stream is fault-injected
//! by `SeekFaultBroker` (a decorator over `harness.broker()`). The faults are
//! the decorator's; delivery and normal seek come from the real broker.

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use event_broker_sdk::gts_id;
use event_broker_sdk::{
    Ack, BatchHandler, BatchHandlerOutcome, ConsumerBuilder, ConsumerCommitMode, ConsumerError,
    ConsumerGroupRef, ConsumerRuntimeEvent, Delivery, EventBatch, EventBrokerApi, EventV1,
    Fallback, GtsTypeId, InMemoryOffsetManager, SubscriptionId, TopicId, WireFrame,
};
use serde_json::json;
use uuid::Uuid;

use super::common::{
    SHOWCASE_SUBJECT_TYPE, TopicFixture, event_type, publish_json, topic, topic_fixture,
    two_partition_event, wait_until,
};
use super::doubles::{
    BatchScopeRecorder, RecordingCommitOffsetManager, RecordingRuntimeListener, SeekFaultBroker,
    SharedSubjectOffsets, WarnCapture,
};

const TOPIC: &str = gts_id!("cf.core.events.topic.v1~example.mock.showcase.reseek.v1");
const EVENT: &str = gts_id!("cf.core.events.event.v1~example.mock.showcase.reseek.v1~");

/// Bounds each await of a test that waits on delivery or `stop()`: 5 s against healthy
/// runs of about 1.3 s. For the re-join tests it is also far below the 60 s default
/// `session_timeout`, the longest a member left behind by a re-join keeps its share
/// once its stream has closed.
const BOUND: Duration = Duration::from_secs(5);

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

/// Records `(subject, offset)` for every event it is handed and advances
/// through the last one.
#[derive(Clone, Default)]
struct SubjectRecorder {
    handled: SharedSubjectOffsets,
}

#[async_trait]
impl BatchHandler<serde_json::Value> for SubjectRecorder {
    async fn handle_batch(
        &self,
        batch: &EventBatch<'_, serde_json::Value>,
        _delivery: &Delivery,
    ) -> Result<BatchHandlerOutcome, ConsumerError> {
        let chunk = batch.next_chunk(batch.len());
        self.handled
            .lock()
            .unwrap()
            .extend(chunk.iter().map(|event| {
                (
                    event.subject().to_owned(),
                    event
                        .sequence()
                        .expect("a delivered event is stamped")
                        .as_i64(),
                )
            }));
        Ok(chunk
            .last()
            .map(|event| BatchHandlerOutcome::AdvanceThrough {
                offset: event.sequence().expect("a delivered event is stamped"),
            })
            .unwrap_or(BatchHandlerOutcome::Success))
    }
}

/// A transport stamps every event it can place; one that arrives without a stamped
/// position is skipped like an unattributable event, with no cursor move, and the
/// stream goes on.
#[tokio::test]
async fn a_delivered_event_without_a_stamped_position_is_skipped() {
    let fx = topic_fixture(TOPIC, EVENT, 1).await;
    let unstamped = EventV1::builder_for(event_type(EVENT), json!({}))
        .id(Uuid::from_u128(1))
        .tenant_id(Uuid::nil())
        .source("injected")
        .subject("unstamped")
        .subject_type_id(GtsTypeId::new(SHOWCASE_SUBJECT_TYPE))
        .occurred_at("2026-01-02T03:04:05Z".parse().unwrap())
        .build()
        .unwrap();
    let faulty = SeekFaultBroker::new(fx.broker.clone(), 0, false)
        .with_stream_prefix(vec![WireFrame::Event(unstamped)]);
    let broker: Arc<dyn EventBrokerApi> = Arc::new(faulty);

    let recorder = SubjectRecorder::default();
    let handled = recorder.handled.clone();
    let offset_manager = RecordingCommitOffsetManager::default();
    let commits = offset_manager.commits.clone();

    let handle = ConsumerBuilder::new(broker.clone())
        .group(ConsumerGroupRef::auto_anonymous("unstamped"))
        .topics([topic(TOPIC)])
        .offset_manager(offset_manager)
        .commit_mode(ConsumerCommitMode::auto(Duration::from_millis(20)))
        .batch_handler(recorder)
        .start()
        .await
        .expect("consumer starts");

    publish_json(&fx.broker, &fx.ctx, EVENT, "stamped", None, json!({})).await;

    wait_until(|| commits.lock().unwrap().iter().any(|commit| commit.3 == 1)).await;
    tokio::time::timeout(BOUND, handle.stop())
        .await
        .expect("the consumer stops within the bound")
        .expect("consumer stops");

    // The unstamped frame came first on the same topic: had it been buffered, the
    // handler would have been handed it ahead of the stamped event.
    assert_eq!(*handled.lock().unwrap(), vec![("stamped".to_owned(), 1)]);
    // Control frames may also commit the broker's position (0 here), so the check is
    // the cursor's furthest point: the stamped event's offset, which the skip did
    // not move past.
    let furthest: Option<(TopicId, u32, i64)> = commits
        .lock()
        .unwrap()
        .iter()
        .map(|(_group, topic_id, partition, offset)| (*topic_id, *partition, *offset))
        .max_by_key(|commit| commit.2);
    assert_eq!(furthest, Some((TopicId::from_gts(TOPIC), 0, 1)));
}

/// A transport error ends the stream and the consumer re-joins; the error is
/// logged first, so a fault that recurs on every re-join is visible.
#[tokio::test]
async fn a_transport_error_on_the_stream_is_logged_before_the_rejoin() {
    // Diverges from docs/features/0004-consumption-transport.md:104-106, :131 (a stream
    // that breaks off resumes the subscription); the code re-JOINs. Pending NEW-RESUME
    // (owner Q26).
    let capture = WarnCapture::install();
    let lines = capture.lines();
    let fx = topic_fixture(TOPIC, EVENT, 1).await;
    let faulty = SeekFaultBroker::new(fx.broker.clone(), 0, false).with_stream_error(
        ConsumerError::Transport("PLANTED-transport-fault".to_owned()),
    );
    let broker: Arc<dyn EventBrokerApi> = Arc::new(faulty);

    let listener = RecordingRuntimeListener::default();
    let runtime_events = listener.events.clone();

    let handle = ConsumerBuilder::new(broker.clone())
        .group(ConsumerGroupRef::auto_anonymous("transport-fault"))
        .topics([topic(TOPIC)])
        .register_listener(listener)
        .offset_manager(InMemoryOffsetManager::new(Fallback::Earliest))
        .batch_handler(SubjectRecorder::default())
        .start()
        .await
        .expect("consumer starts");

    // The re-join follows the log line, so once it is under way the line is written
    // or never will be. Each wait parks for 10 ms; 500 of them bound it at 5 s.
    let rejoining = || {
        runtime_events
            .lock()
            .unwrap()
            .iter()
            .any(|event| matches!(event, ConsumerRuntimeEvent::SubscriptionRejoining { .. }))
    };
    for _ in 0..500 {
        if rejoining() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    tokio::time::timeout(BOUND, handle.stop())
        .await
        .expect("the consumer stops within the bound")
        .expect("consumer stops");

    assert!(
        rejoining(),
        "the consumer re-joins after the transport error"
    );

    let planted: Vec<String> = lines
        .lock()
        .unwrap()
        .iter()
        .filter(|line| line.contains("PLANTED"))
        .cloned()
        .collect();
    assert_eq!(
        planted,
        vec![
            "WARN stream transport error; re-joining slot_idx=0 error=transport: PLANTED-transport-fault"
                .to_owned()
        ]
    );
}

/// A test's capture holds only its own thread's lines, even when another thread hits
/// the same `warn!` call site first while the capture is live.
#[test]
fn a_warn_from_another_thread_does_not_reach_this_tests_capture() {
    fn warn_with_marker(marker: &str) {
        tracing::warn!(marker, "a warn from a test");
    }

    let capture = WarnCapture::install();
    let lines = capture.lines();

    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let other = std::thread::spawn(move || {
        warn_with_marker("PLANTED-other-thread");
        done_tx.send(()).expect("the test thread waits");
    });
    done_rx
        .recv_timeout(BOUND)
        .expect("the other thread emits its warn");
    other.join().expect("the other thread ends");

    warn_with_marker("PLANTED-this-thread");

    assert_eq!(
        *lines.lock().unwrap(),
        vec!["WARN a warn from a test marker=\"PLANTED-this-thread\"".to_owned()]
    );
}

const FIRST: Uuid = Uuid::from_u128(0x1701);
const SECOND: Uuid = Uuid::from_u128(0x1702);

type SharedPlacements = Arc<Mutex<Vec<(Uuid, u32, i64)>>>;

/// Records `(id, partition, offset)` for every event it is handed and advances
/// through the last one.
#[derive(Clone, Default)]
struct PlacementRecorder {
    handled: SharedPlacements,
}

#[async_trait]
impl BatchHandler<serde_json::Value> for PlacementRecorder {
    async fn handle_batch(
        &self,
        batch: &EventBatch<'_, serde_json::Value>,
        delivery: &Delivery,
    ) -> Result<BatchHandlerOutcome, ConsumerError> {
        let chunk = batch.next_chunk(batch.len());
        self.handled
            .lock()
            .unwrap()
            .extend(chunk.iter().map(|event| {
                (
                    event.id(),
                    delivery.partition(),
                    event
                        .sequence()
                        .expect("a delivered event is stamped")
                        .as_i64(),
                )
            }));
        Ok(chunk
            .last()
            .map(|event| BatchHandlerOutcome::AdvanceThrough {
                offset: event.sequence().expect("a delivered event is stamped"),
            })
            .unwrap_or(BatchHandlerOutcome::Success))
    }
}

/// Starts a consumer on `faulty`, whose first stream breaks off, waits for
/// the re-join, publishes `FIRST` to partition 0 and `SECOND` to partition 1, and
/// returns what the handler was handed within `BOUND` of that, by partition.
async fn deliveries_after_a_first_stream_fault(
    fx: &TopicFixture,
    faulty: SeekFaultBroker,
    group: &str,
) -> Vec<(Uuid, u32, i64)> {
    run_after_a_first_stream_fault(fx, faulty, group)
        .await
        .placements
}

/// What a consumer did around a fault on its first stream.
struct FaultRun {
    /// `(id, partition, offset)` of every event handled, by partition.
    placements: Vec<(Uuid, u32, i64)>,
    runtime_events: Vec<ConsumerRuntimeEvent>,
    /// The subscriptions left before the consumer was stopped.
    leave_calls: Vec<SubscriptionId>,
}

impl FaultRun {
    /// The subscription the one re-join replaced.
    fn rejoined_from(&self) -> SubscriptionId {
        let rejoins: Vec<SubscriptionId> = self
            .runtime_events
            .iter()
            .filter_map(|event| match event {
                ConsumerRuntimeEvent::SubscriptionRejoining {
                    previous_subscription_id,
                    ..
                } => Some(*previous_subscription_id),
                _ => None,
            })
            .collect();
        let [previous] = rejoins.as_slice() else {
            panic!("expected one re-join, saw {rejoins:?}");
        };
        *previous
    }
}

/// `deliveries_after_a_first_stream_fault`, also returning the runtime events and
/// the subscriptions left before the stop (whose own leave is not counted).
async fn run_after_a_first_stream_fault(
    fx: &TopicFixture,
    faulty: SeekFaultBroker,
    group: &str,
) -> FaultRun {
    let leave_calls = faulty.leave_calls.clone();
    let broker: Arc<dyn EventBrokerApi> = Arc::new(faulty);
    let listener = RecordingRuntimeListener::default();
    let runtime_events = listener.events.clone();
    let recorder = PlacementRecorder::default();
    let handled = recorder.handled.clone();

    let handle = ConsumerBuilder::new(broker)
        .group(ConsumerGroupRef::auto_anonymous(group))
        .topics([topic(TOPIC)])
        .register_listener(listener)
        .offset_manager(InMemoryOffsetManager::new(Fallback::Earliest))
        .batch_handler(recorder)
        .start()
        .await
        .expect("consumer starts");

    let rejoining = || {
        runtime_events
            .lock()
            .unwrap()
            .iter()
            .any(|event| matches!(event, ConsumerRuntimeEvent::SubscriptionRejoining { .. }))
    };
    tokio::time::timeout(BOUND, async {
        while !rejoining() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the consumer re-joins after the fault");

    for (id, subject, partition) in [(FIRST, "first", 0), (SECOND, "second", 1)] {
        let event = two_partition_event(&fx.ctx, EVENT, id, subject, partition);
        fx.broker
            .publish(&fx.ctx, &event, Ack::Accepted)
            .await
            .expect("event published");
    }
    let both_handled = || handled.lock().unwrap().len() >= 2;
    // A stall shows as a missing event in the caller's assert, not as a panic here.
    let _ = tokio::time::timeout(BOUND, async {
        while !both_handled() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    let left_before_stop = leave_calls.lock().unwrap().clone();
    tokio::time::timeout(BOUND, handle.stop())
        .await
        .expect("the consumer stops within the bound")
        .expect("consumer stops");

    let mut handled = handled.lock().unwrap().clone();
    handled.sort_by_key(|&(_, partition, _)| partition);
    let runtime_events = runtime_events.lock().unwrap().clone();
    FaultRun {
        placements: handled,
        runtime_events,
        leave_calls: left_before_stop,
    }
}

/// A transport fault ends the stream and the consumer re-joins. The broker hands
/// the re-joined member every partition only once the old member is gone; one the
/// re-join left behind would keep its share until its stream closes, then
/// `session_timeout`.
#[tokio::test]
async fn a_consumer_resumes_delivery_promptly_after_a_transport_fault() {
    // Diverges from docs/features/0004-consumption-transport.md:104-106, :131 (a stream
    // that breaks off resumes the subscription); the code re-JOINs. Pending NEW-RESUME
    // (owner Q26).
    let fx = topic_fixture(TOPIC, EVENT, 2).await;
    let faulty = SeekFaultBroker::new(fx.broker.clone(), 0, false)
        .with_stream_error(ConsumerError::Transport("injected".to_owned()));

    let handled = deliveries_after_a_first_stream_fault(&fx, faulty, "transport-resume").await;

    assert_eq!(handled, vec![(FIRST, 0, 1), (SECOND, 1, 1)]);
}

/// A stream that ends takes the same way out as a transport fault: the consumer
/// re-joins and delivers on every partition again well within `session_timeout`.
#[tokio::test]
async fn a_consumer_resumes_delivery_promptly_after_its_stream_ends() {
    // Diverges from docs/features/0004-consumption-transport.md:104-106, :131 (bare close
    // resumes the subscription); the code re-JOINs. Pending NEW-RESUME (owner Q26).
    let fx = topic_fixture(TOPIC, EVENT, 2).await;
    let faulty = SeekFaultBroker::new(fx.broker.clone(), 0, false).with_stream_end();

    let handled = deliveries_after_a_first_stream_fault(&fx, faulty, "end-resume").await;

    assert_eq!(handled, vec![(FIRST, 0, 1), (SECOND, 1, 1)]);
}

/// A subscription whose stream fails to open is left before the re-join. Its member
/// never streamed, and the broker keeps such a member, sharing out the partitions to
/// it, until `join_timeout` (60 s by default); only a leave removes it at once.
#[tokio::test]
async fn a_consumer_resumes_delivery_promptly_after_a_failed_stream_open() {
    // Diverges from docs/features/0004-consumption-transport.md:131 (an error reopens
    // the same subscription); the code re-JOINs. Pending NEW-RESUME (owner Q26).
    let fx = topic_fixture(TOPIC, EVENT, 2).await;
    let faulty = SeekFaultBroker::new(fx.broker.clone(), 0, false)
        .with_open_failure(ConsumerError::Internal("injected".to_owned()));

    let run = run_after_a_first_stream_fault(&fx, faulty, "open-failure-resume").await;

    assert_eq!(run.placements, vec![(FIRST, 0, 1), (SECOND, 1, 1)]);
    assert_eq!(run.leave_calls, vec![run.rejoined_from()]);
}

/// A transport that opens lazily (REST) reports a refused open as the stream's
/// first item. The stream counts as open only at its first frame, so the consumer
/// takes the failed-open way out and leaves the member that never streamed.
#[tokio::test]
async fn a_consumer_resumes_delivery_promptly_after_an_error_before_the_first_frame() {
    // Diverges from docs/features/0004-consumption-transport.md:131 (an error reopens
    // the same subscription); the code re-JOINs. Pending NEW-RESUME (owner Q26).
    let fx = topic_fixture(TOPIC, EVENT, 2).await;
    let faulty = SeekFaultBroker::new(fx.broker.clone(), 0, false)
        .with_open_error(ConsumerError::Transport("injected".to_owned()));

    let run = run_after_a_first_stream_fault(&fx, faulty, "open-error-resume").await;

    assert_eq!(run.placements, vec![(FIRST, 0, 1), (SECOND, 1, 1)]);
    assert_eq!(run.leave_calls, vec![run.rejoined_from()]);
}

/// A stream that ends before its first frame never opened either: the consumer
/// leaves the member that never streamed before the re-join.
#[tokio::test]
async fn a_consumer_resumes_delivery_promptly_after_a_stream_that_ends_before_its_first_frame() {
    // Diverges from docs/features/0004-consumption-transport.md:104-106, :131 (bare close
    // resumes the subscription); the code re-JOINs. Pending NEW-RESUME (owner Q26).
    let fx = topic_fixture(TOPIC, EVENT, 2).await;
    let faulty = SeekFaultBroker::new(fx.broker.clone(), 0, false).with_open_end();

    let run = run_after_a_first_stream_fault(&fx, faulty, "open-end-resume").await;

    assert_eq!(run.placements, vec![(FIRST, 0, 1), (SECOND, 1, 1)]);
    assert_eq!(run.leave_calls, vec![run.rejoined_from()]);
}

/// Starts a consumer on `faulty`, whose first stream open is refused, publishes
/// `FIRST` to partition 0 and `SECOND` to partition 1 at once, and returns what the
/// handler was handed within `BOUND` of that, with the outcome of the stop. It waits
/// for the deliveries, not for a re-join, so a run that ends on the refusal fails on
/// the caller's assert.
async fn run_after_a_refused_stream_open(
    fx: &TopicFixture,
    faulty: SeekFaultBroker,
    group: &str,
) -> (FaultRun, Result<(), ConsumerError>) {
    let leave_calls = faulty.leave_calls.clone();
    let broker: Arc<dyn EventBrokerApi> = Arc::new(faulty);
    let listener = RecordingRuntimeListener::default();
    let runtime_events = listener.events.clone();
    let recorder = PlacementRecorder::default();
    let handled = recorder.handled.clone();

    let handle = ConsumerBuilder::new(broker)
        .group(ConsumerGroupRef::auto_anonymous(group))
        .topics([topic(TOPIC)])
        .register_listener(listener)
        .offset_manager(InMemoryOffsetManager::new(Fallback::Earliest))
        .batch_handler(recorder)
        .start()
        .await
        .expect("consumer starts");

    for (id, subject, partition) in [(FIRST, "first", 0), (SECOND, "second", 1)] {
        let event = two_partition_event(&fx.ctx, EVENT, id, subject, partition);
        fx.broker
            .publish(&fx.ctx, &event, Ack::Accepted)
            .await
            .expect("event published");
    }
    let both_handled = || handled.lock().unwrap().len() >= 2;
    // A stall shows as a missing event in the caller's assert, not as a panic here.
    let _ = tokio::time::timeout(BOUND, async {
        while !both_handled() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    let left_before_stop = leave_calls.lock().unwrap().clone();
    let stopped = tokio::time::timeout(BOUND, handle.stop())
        .await
        .expect("the consumer stops within the bound");

    let mut handled = handled.lock().unwrap().clone();
    handled.sort_by_key(|&(_, partition, _)| partition);
    let runtime_events = runtime_events.lock().unwrap().clone();
    let run = FaultRun {
        placements: handled,
        runtime_events,
        leave_calls: left_before_stop,
    };
    (run, stopped)
}

/// A stream open refused with `PositionsNotSet` can be transient for a member that
/// seeked but has not streamed yet: a peer's JOIN or LEAVE in between can move an
/// unseeded partition onto it. The consumer leaves that member, re-joins and seeks
/// the fresh assignment, then delivers on every partition.
#[tokio::test]
async fn positions_not_set_at_open_is_recovered_by_a_fresh_join() {
    // Diverges from docs/openapi.yaml:310-313 (seek the unseeded partitions and retry
    // the same subscription); the code re-JOINs. Pending NEW-RESUME (owner Q26).
    let fx = topic_fixture(TOPIC, EVENT, 2).await;
    let faulty = SeekFaultBroker::new(fx.broker.clone(), 0, true);
    let stream_calls = faulty.stream_calls.clone();

    let (run, stopped) = run_after_a_refused_stream_open(&fx, faulty, "pns-open-resume").await;

    assert_eq!(run.placements, vec![(FIRST, 0, 1), (SECOND, 1, 1)]);
    // The refused open and the one after the re-join; the second stream stays open.
    assert_eq!(stream_calls.load(Ordering::SeqCst), 2);
    assert_eq!(run.leave_calls, vec![run.rejoined_from()]);
    let Ok(()) = stopped else {
        panic!("expected the consumer to stop cleanly, got {stopped:?}");
    };
}

/// A transport that opens lazily (REST) reports a refused open as the stream's first
/// item. `PositionsNotSet` there takes the same way out as at open.
#[tokio::test]
async fn a_positions_not_set_before_the_first_frame_is_recovered_by_a_fresh_join() {
    // Diverges from docs/openapi.yaml:310-313 (seek the unseeded partitions and retry
    // the same subscription); the code re-JOINs. Pending NEW-RESUME (owner Q26).
    let fx = topic_fixture(TOPIC, EVENT, 2).await;
    let faulty = SeekFaultBroker::new(fx.broker.clone(), 0, false).with_open_error(
        ConsumerError::PositionsNotSet {
            unseeded: vec![],
            detail: "injected".into(),
        },
    );

    let (run, stopped) =
        run_after_a_refused_stream_open(&fx, faulty, "pns-first-item-resume").await;

    assert_eq!(run.placements, vec![(FIRST, 0, 1), (SECOND, 1, 1)]);
    assert_eq!(run.leave_calls, vec![run.rejoined_from()]);
    let Ok(()) = stopped else {
        panic!("expected the consumer to stop cleanly, got {stopped:?}");
    };
}
