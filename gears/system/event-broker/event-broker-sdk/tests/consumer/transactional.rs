//! Transactional-consumer lifecycle and commit semantics against the real
//! broker. The tx offset-store/handler doubles impl the public
//! `event_broker_sdk::consumer` typestate traits; there is no mock.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use event_broker::test_support::StaticTypesRegistry;
use event_broker_sdk::consumer::{
    CommitOffsetInTx, ConsumerOffsetManager, OffsetManagerError, OffsetStore, Position,
    TxBatchHandler, TxCommitHandle, TxEventHandler, TxUndecodable, WithTx,
};
use event_broker_sdk::{
    Ack, BatchHandlerOutcome, ConsumerBuilder, ConsumerCommitMode, ConsumerError, ConsumerGroupId,
    ConsumerGroupRef, ConsumerRetry, ConsumerRuntimeEvent, Delivery, EventBatch, EventBrokerApi,
    EventV1, Fallback, GtsInstanceId, GtsTypeId, HandlerOutcome, Sequence, SubscriptionId, TopicId,
    WireFrame, gts_id,
};
use gts::GtsSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use super::common::{
    SHOWCASE_SUBJECT_TYPE, TopicFixture, event_type, fixture_from_catalog, publish_json, topic,
    topic_fixture, two_partition_event, two_topic_fixture, wait_until,
};
use super::doubles::{
    RecordingRuntimeListener, SeekFaultBroker, SharedSubjectOffsets, WarnCapture,
};

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
impl TxEventHandler<serde_json::Value, RecordingTxOffsetManager> for NoopTxHandler {
    async fn handle(
        &self,
        _event: EventV1,
        _delivery: &Delivery,
        _commit: TxCommitHandle<RecordingTxOffsetManager>,
    ) -> Result<HandlerOutcome, ConsumerError> {
        Ok(HandlerOutcome::Success)
    }
}

struct NoopTxBatchHandler;

#[async_trait]
impl TxBatchHandler<serde_json::Value, RecordingTxOffsetManager> for NoopTxBatchHandler {
    async fn handle_batch(
        &self,
        _batch: &EventBatch<'_, serde_json::Value>,
        _delivery: &Delivery,
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
impl TxEventHandler<serde_json::Value, CountingTxOffsetManager> for NoCommitTxHandler {
    async fn handle(
        &self,
        _event: EventV1,
        _delivery: &Delivery,
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

/// Records `(subject, offset)` for every event it is handed.
#[derive(Clone, Default)]
struct SubjectTxRecorder {
    handled: SharedSubjectOffsets,
}

#[async_trait]
impl TxEventHandler<serde_json::Value, RecordingTxOffsetManager> for SubjectTxRecorder {
    async fn handle(
        &self,
        event: EventV1,
        _delivery: &Delivery,
        _commit: TxCommitHandle<RecordingTxOffsetManager>,
    ) -> Result<HandlerOutcome, ConsumerError> {
        self.handled.lock().unwrap().push((
            event.subject().to_owned(),
            event
                .sequence()
                .expect("a delivered event is stamped")
                .as_i64(),
        ));
        Ok(HandlerOutcome::Success)
    }
}

/// Records `(subject, offset)` for every event it is handed and commits its offset in
/// a transaction.
struct CommittingSubjectTxRecorder {
    db: toolkit_db::Db,
    handled: SharedSubjectOffsets,
}

#[async_trait]
impl TxEventHandler<serde_json::Value, RecordingTxOffsetManager> for CommittingSubjectTxRecorder {
    async fn handle(
        &self,
        event: EventV1,
        _delivery: &Delivery,
        commit: TxCommitHandle<RecordingTxOffsetManager>,
    ) -> Result<HandlerOutcome, ConsumerError> {
        let offset = event.sequence().expect("a delivered event is stamped");
        self.db
            .transaction_ref(move |tx| {
                Box::pin(async move {
                    commit.commit_offset_in_tx(tx, offset).await.map_err(|err| {
                        toolkit_db::DbError::Sea(sea_orm::DbErr::Custom(err.to_string()))
                    })
                })
            })
            .await
            .map_err(|err| ConsumerError::Internal(err.to_string()))?;
        self.handled
            .lock()
            .unwrap()
            .push((event.subject().to_owned(), offset.as_i64()));
        Ok(HandlerOutcome::Success)
    }
}

/// A sqlite database that only has to open transactions: `RecordingTxOffsetManager`
/// writes nothing.
async fn transaction_db(name: &str) -> toolkit_db::Db {
    let dsn = format!("sqlite:file:{name}?mode=memory&cache=shared");
    toolkit_db::connect_db(&dsn, toolkit_db::ConnectOpts::default())
        .await
        .expect("sqlite opens")
}

/// The transactional twin of `recovery::a_delivered_event_without_a_stamped_position_is_skipped`:
/// an event without a stamped position is skipped, with no cursor move, and the
/// stream goes on.
#[tokio::test]
async fn a_transactionally_delivered_event_without_a_stamped_position_is_skipped() {
    let fx = topic_fixture(TOPIC, EVENT_TYPE, 1).await;
    let unstamped = EventV1::builder_for(event_type(EVENT_TYPE), json!({}))
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

    let handled = SharedSubjectOffsets::default();
    let recorder = CommittingSubjectTxRecorder {
        db: transaction_db("tx_unstamped").await,
        handled: handled.clone(),
    };
    let listener = RecordingRuntimeListener::default();
    let runtime_events = listener.events.clone();

    let handle = ConsumerBuilder::new(broker.clone())
        .group(ConsumerGroupRef::auto_anonymous("tx-unstamped"))
        .topics([topic(TOPIC)])
        .register_listener(listener)
        .offset_manager(RecordingTxOffsetManager)
        .handler(recorder)
        .start()
        .await
        .expect("transactional consumer starts");

    publish_json(&fx.broker, &fx.ctx, EVENT_TYPE, "stamped", None, json!({})).await;

    let committed = || -> Vec<Sequence> {
        runtime_events
            .lock()
            .unwrap()
            .iter()
            .filter_map(|event| match event {
                ConsumerRuntimeEvent::OffsetCommitted { offset, .. } => Some(*offset),
                _ => None,
            })
            .collect()
    };
    // The skipped frame came first on the stream, so once the stamped event's commit
    // is observed the skip is done. A miss shows in the asserts below.
    let _ = tokio::time::timeout(BOUND, async {
        while committed().is_empty() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    tokio::time::timeout(BOUND, handle.stop())
        .await
        .expect("the consumer stops within the bound")
        .expect("consumer stops");

    // The unstamped frame came first on the same topic: had it been buffered, the
    // handler would have been handed it ahead of the stamped event.
    assert_eq!(*handled.lock().unwrap(), vec![("stamped".to_owned(), 1)]);
    // The handler commits what it is handed, so only the stamped event's sequence
    // is committed: the skip committed no position of its own.
    assert_eq!(committed(), vec![Sequence::assigned(1)]);
}

/// Answers `Retry` on its first call; on the next it commits the event's offset in a
/// transaction and answers `Success`. Records the attempt of every call.
struct RetryOnceTxRecorder {
    db: toolkit_db::Db,
    attempts: Arc<Mutex<Vec<u16>>>,
}

#[async_trait]
impl TxEventHandler<serde_json::Value, RecordingTxOffsetManager> for RetryOnceTxRecorder {
    async fn handle(
        &self,
        event: EventV1,
        delivery: &Delivery,
        commit: TxCommitHandle<RecordingTxOffsetManager>,
    ) -> Result<HandlerOutcome, ConsumerError> {
        let first_call = {
            let mut attempts = self.attempts.lock().unwrap();
            attempts.push(delivery.attempts());
            attempts.len() == 1
        };
        if first_call {
            return Ok(HandlerOutcome::Retry {
                reason: "not yet".to_owned(),
            });
        }
        let offset = event.sequence().expect("a delivered event is stamped");
        self.db
            .transaction_ref(move |tx| {
                Box::pin(async move {
                    commit.commit_offset_in_tx(tx, offset).await.map_err(|err| {
                        toolkit_db::DbError::Sea(sea_orm::DbErr::Custom(err.to_string()))
                    })
                })
            })
            .await
            .map_err(|err| ConsumerError::Internal(err.to_string()))?;
        Ok(HandlerOutcome::Success)
    }
}

/// The transactional twin of
/// `single_handler::a_retried_event_is_redelivered_with_the_next_attempt`.
#[tokio::test]
async fn a_retried_transactional_event_is_redelivered_with_the_next_attempt() {
    let fx = topic_fixture(TOPIC, EVENT_TYPE, 1).await;
    let attempts = Arc::new(Mutex::new(Vec::new()));
    let recorder = RetryOnceTxRecorder {
        db: transaction_db("tx_retry_attempts").await,
        attempts: attempts.clone(),
    };
    let listener = RecordingRuntimeListener::default();
    let runtime_events = listener.events.clone();

    let handle = ConsumerBuilder::new(fx.broker.clone())
        .group(ConsumerGroupRef::auto_anonymous("tx-retry-attempts"))
        .topics([topic(TOPIC)])
        .register_listener(listener)
        .retry(ConsumerRetry {
            base_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(1),
        })
        .offset_manager(RecordingTxOffsetManager)
        .handler(recorder)
        .start()
        .await
        .expect("transactional consumer starts");

    publish_json(&fx.broker, &fx.ctx, EVENT_TYPE, "retry", None, json!({})).await;

    wait_until(|| attempts.lock().unwrap().len() == 2).await;
    tokio::time::timeout(BOUND, handle.stop())
        .await
        .expect("the consumer stops within the bound")
        .expect("consumer stops");

    assert_eq!(*attempts.lock().unwrap(), vec![1, 2]);
    // `RetryScheduled` comes from the handler's `Retry` and from a handler error; the
    // handler retries once and returns no error. The configured 1 ms backoff is the
    // emitted delay, not the default profile's 1 s.
    let retries: Vec<ConsumerRuntimeEvent> = runtime_events
        .lock()
        .unwrap()
        .iter()
        .filter(|event| matches!(event, ConsumerRuntimeEvent::RetryScheduled { .. }))
        .cloned()
        .collect();
    let [ConsumerRuntimeEvent::RetryScheduled { attempt, delay, .. }] = retries.as_slice() else {
        panic!("expected exactly one RetryScheduled, got {retries:?}");
    };
    assert_eq!((*attempt, *delay), (1, Duration::from_millis(1)));
}

/// The transactional twin of
/// `recovery::a_transport_error_on_the_stream_is_logged_before_the_rejoin`.
#[tokio::test]
async fn a_transactional_transport_error_on_the_stream_is_logged_before_the_rejoin() {
    // Diverges from docs/features/0004-consumption-transport.md:104-106, :131 (a stream
    // that breaks off resumes the subscription); the code re-JOINs. Pending NEW-RESUME
    // (owner Q26).
    let capture = WarnCapture::install();
    let lines = capture.lines();
    let fx = topic_fixture(TOPIC, EVENT_TYPE, 1).await;
    let faulty = SeekFaultBroker::new(fx.broker.clone(), 0, false).with_stream_error(
        ConsumerError::Transport("PLANTED-transport-fault".to_owned()),
    );
    let broker: Arc<dyn EventBrokerApi> = Arc::new(faulty);

    let listener = RecordingRuntimeListener::default();
    let runtime_events = listener.events.clone();

    let handle = ConsumerBuilder::new(broker.clone())
        .group(ConsumerGroupRef::auto_anonymous("tx-transport-fault"))
        .topics([topic(TOPIC)])
        .register_listener(listener)
        .offset_manager(RecordingTxOffsetManager)
        .handler(SubjectTxRecorder::default())
        .start()
        .await
        .expect("transactional consumer starts");

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
            "WARN transactional stream transport error; re-joining slot_idx=0 error=transport: PLANTED-transport-fault"
                .to_owned()
        ]
    );
}

/// Bounds each await of a test that waits on delivery or `stop()`: 5 s against healthy
/// runs of about 1.3 s. For the re-join tests it is also far below the 60 s default
/// `session_timeout`, the longest a member left behind by a re-join keeps its share
/// once its stream has closed.
const BOUND: Duration = Duration::from_secs(5);

const FIRST: Uuid = Uuid::from_u128(0x1701);
const SECOND: Uuid = Uuid::from_u128(0x1702);

type SharedPlacements = Arc<Mutex<Vec<(Uuid, u32, i64)>>>;

/// Records `(id, partition, offset)` for every event it is handed and commits its
/// offset in a transaction, so the consumer goes on streaming.
struct PlacementTxRecorder {
    db: toolkit_db::Db,
    handled: SharedPlacements,
}

#[async_trait]
impl TxEventHandler<serde_json::Value, RecordingTxOffsetManager> for PlacementTxRecorder {
    async fn handle(
        &self,
        event: EventV1,
        delivery: &Delivery,
        commit: TxCommitHandle<RecordingTxOffsetManager>,
    ) -> Result<HandlerOutcome, ConsumerError> {
        let offset = event.sequence().expect("a delivered event is stamped");
        self.db
            .transaction_ref(move |tx| {
                Box::pin(async move {
                    commit.commit_offset_in_tx(tx, offset).await.map_err(|err| {
                        toolkit_db::DbError::Sea(sea_orm::DbErr::Custom(err.to_string()))
                    })
                })
            })
            .await
            .map_err(|err| ConsumerError::Internal(err.to_string()))?;
        self.handled.lock().unwrap().push((
            event.id(),
            delivery.partition(),
            event
                .sequence()
                .expect("a delivered event is stamped")
                .as_i64(),
        ));
        Ok(HandlerOutcome::Success)
    }
}

/// The transactional twin of `recovery::deliveries_after_a_first_stream_fault`.
async fn deliveries_after_a_first_stream_fault(
    fx: &TopicFixture,
    faulty: SeekFaultBroker,
    group: &str,
) -> Vec<(Uuid, u32, i64)> {
    run_after_a_first_stream_fault(fx, faulty, group)
        .await
        .placements
}

/// The transactional twin of `recovery::FaultRun`.
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

/// The transactional twin of `recovery::run_after_a_first_stream_fault`.
async fn run_after_a_first_stream_fault(
    fx: &TopicFixture,
    faulty: SeekFaultBroker,
    group: &str,
) -> FaultRun {
    let leave_calls = faulty.leave_calls.clone();
    let broker: Arc<dyn EventBrokerApi> = Arc::new(faulty);
    let listener = RecordingRuntimeListener::default();
    let runtime_events = listener.events.clone();
    // `RecordingTxOffsetManager` writes nothing, so the database only has to open a
    // transaction.
    let dsn = format!("sqlite:file:tx_resume_{group}?mode=memory&cache=shared");
    let db = toolkit_db::connect_db(&dsn, toolkit_db::ConnectOpts::default())
        .await
        .expect("sqlite opens");
    let handled = SharedPlacements::default();
    let recorder = PlacementTxRecorder {
        db,
        handled: handled.clone(),
    };

    let handle = ConsumerBuilder::new(broker)
        .group(ConsumerGroupRef::auto_anonymous(group))
        .topics([topic(TOPIC)])
        .register_listener(listener)
        .offset_manager(RecordingTxOffsetManager)
        .handler(recorder)
        .start()
        .await
        .expect("transactional consumer starts");

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
        let event = two_partition_event(&fx.ctx, EVENT_TYPE, id, subject, partition);
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

/// The transactional twin of
/// `recovery::a_consumer_resumes_delivery_promptly_after_a_transport_fault`.
#[tokio::test]
async fn a_transactional_consumer_resumes_delivery_promptly_after_a_transport_fault() {
    // Diverges from docs/features/0004-consumption-transport.md:104-106, :131 (a stream
    // that breaks off resumes the subscription); the code re-JOINs. Pending NEW-RESUME
    // (owner Q26).
    let fx = topic_fixture(TOPIC, EVENT_TYPE, 2).await;
    let faulty = SeekFaultBroker::new(fx.broker.clone(), 0, false)
        .with_stream_error(ConsumerError::Transport("injected".to_owned()));

    let handled = deliveries_after_a_first_stream_fault(&fx, faulty, "tx-transport-resume").await;

    assert_eq!(handled, vec![(FIRST, 0, 1), (SECOND, 1, 1)]);
}

/// The transactional twin of
/// `recovery::a_consumer_resumes_delivery_promptly_after_its_stream_ends`.
#[tokio::test]
async fn a_transactional_consumer_resumes_delivery_promptly_after_its_stream_ends() {
    // Diverges from docs/features/0004-consumption-transport.md:104-106, :131 (bare close
    // resumes the subscription); the code re-JOINs. Pending NEW-RESUME (owner Q26).
    let fx = topic_fixture(TOPIC, EVENT_TYPE, 2).await;
    let faulty = SeekFaultBroker::new(fx.broker.clone(), 0, false).with_stream_end();

    let handled = deliveries_after_a_first_stream_fault(&fx, faulty, "tx-end-resume").await;

    assert_eq!(handled, vec![(FIRST, 0, 1), (SECOND, 1, 1)]);
}

/// The transactional twin of
/// `recovery::a_consumer_resumes_delivery_promptly_after_a_failed_stream_open`.
#[tokio::test]
async fn a_transactional_consumer_resumes_delivery_promptly_after_a_failed_stream_open() {
    // Diverges from docs/features/0004-consumption-transport.md:131 (an error reopens
    // the same subscription); the code re-JOINs. Pending NEW-RESUME (owner Q26).
    let fx = topic_fixture(TOPIC, EVENT_TYPE, 2).await;
    let faulty = SeekFaultBroker::new(fx.broker.clone(), 0, false)
        .with_open_failure(ConsumerError::Internal("injected".to_owned()));

    let run = run_after_a_first_stream_fault(&fx, faulty, "tx-open-failure-resume").await;

    assert_eq!(run.placements, vec![(FIRST, 0, 1), (SECOND, 1, 1)]);
    assert_eq!(run.leave_calls, vec![run.rejoined_from()]);
}

/// The transactional twin of
/// `recovery::a_consumer_resumes_delivery_promptly_after_an_error_before_the_first_frame`.
#[tokio::test]
async fn a_transactional_consumer_resumes_delivery_promptly_after_an_error_before_the_first_frame()
{
    // Diverges from docs/features/0004-consumption-transport.md:131 (an error reopens
    // the same subscription); the code re-JOINs. Pending NEW-RESUME (owner Q26).
    let fx = topic_fixture(TOPIC, EVENT_TYPE, 2).await;
    let faulty = SeekFaultBroker::new(fx.broker.clone(), 0, false)
        .with_open_error(ConsumerError::Transport("injected".to_owned()));

    let run = run_after_a_first_stream_fault(&fx, faulty, "tx-open-error-resume").await;

    assert_eq!(run.placements, vec![(FIRST, 0, 1), (SECOND, 1, 1)]);
    assert_eq!(run.leave_calls, vec![run.rejoined_from()]);
}

/// The transactional twin of the `recovery` test
/// `a_consumer_resumes_delivery_promptly_after_a_stream_that_ends_before_its_first_frame`.
#[tokio::test]
async fn a_transactional_consumer_resumes_delivery_promptly_after_a_stream_that_ends_before_its_first_frame()
 {
    // Diverges from docs/features/0004-consumption-transport.md:104-106, :131 (bare close
    // resumes the subscription); the code re-JOINs. Pending NEW-RESUME (owner Q26).
    let fx = topic_fixture(TOPIC, EVENT_TYPE, 2).await;
    let faulty = SeekFaultBroker::new(fx.broker.clone(), 0, false).with_open_end();

    let run = run_after_a_first_stream_fault(&fx, faulty, "tx-open-end-resume").await;

    assert_eq!(run.placements, vec![(FIRST, 0, 1), (SECOND, 1, 1)]);
    assert_eq!(run.leave_calls, vec![run.rejoined_from()]);
}

/// The transactional twin of `recovery::run_after_a_refused_stream_open`.
async fn run_after_a_refused_stream_open(
    fx: &TopicFixture,
    faulty: SeekFaultBroker,
    group: &str,
) -> (FaultRun, Result<(), ConsumerError>) {
    let leave_calls = faulty.leave_calls.clone();
    let broker: Arc<dyn EventBrokerApi> = Arc::new(faulty);
    let listener = RecordingRuntimeListener::default();
    let runtime_events = listener.events.clone();
    // `RecordingTxOffsetManager` writes nothing, so the database only has to open a
    // transaction.
    let dsn = format!("sqlite:file:tx_resume_{group}?mode=memory&cache=shared");
    let db = toolkit_db::connect_db(&dsn, toolkit_db::ConnectOpts::default())
        .await
        .expect("sqlite opens");
    let handled = SharedPlacements::default();
    let recorder = PlacementTxRecorder {
        db,
        handled: handled.clone(),
    };

    let handle = ConsumerBuilder::new(broker)
        .group(ConsumerGroupRef::auto_anonymous(group))
        .topics([topic(TOPIC)])
        .register_listener(listener)
        .offset_manager(RecordingTxOffsetManager)
        .handler(recorder)
        .start()
        .await
        .expect("transactional consumer starts");

    for (id, subject, partition) in [(FIRST, "first", 0), (SECOND, "second", 1)] {
        let event = two_partition_event(&fx.ctx, EVENT_TYPE, id, subject, partition);
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

/// The transactional twin of
/// `recovery::positions_not_set_at_open_is_recovered_by_a_fresh_join`.
#[tokio::test]
async fn a_transactional_positions_not_set_at_open_is_recovered_by_a_fresh_join() {
    // Diverges from docs/openapi.yaml:310-313 (seek the unseeded partitions and retry
    // the same subscription); the code re-JOINs. Pending NEW-RESUME (owner Q26).
    let fx = topic_fixture(TOPIC, EVENT_TYPE, 2).await;
    let faulty = SeekFaultBroker::new(fx.broker.clone(), 0, true);
    let stream_calls = faulty.stream_calls.clone();

    let (run, stopped) = run_after_a_refused_stream_open(&fx, faulty, "tx-pns-open-resume").await;

    assert_eq!(run.placements, vec![(FIRST, 0, 1), (SECOND, 1, 1)]);
    // The refused open and the one after the re-join; the second stream stays open.
    assert_eq!(stream_calls.load(Ordering::SeqCst), 2);
    assert_eq!(run.leave_calls, vec![run.rejoined_from()]);
    let Ok(()) = stopped else {
        panic!("expected the consumer to stop cleanly, got {stopped:?}");
    };
}

/// The transactional twin of
/// `recovery::a_positions_not_set_before_the_first_frame_is_recovered_by_a_fresh_join`.
#[tokio::test]
async fn a_transactional_positions_not_set_before_the_first_frame_is_recovered_by_a_fresh_join() {
    // Diverges from docs/openapi.yaml:310-313 (seek the unseeded partitions and retry
    // the same subscription); the code re-JOINs. Pending NEW-RESUME (owner Q26).
    let fx = topic_fixture(TOPIC, EVENT_TYPE, 2).await;
    let faulty = SeekFaultBroker::new(fx.broker.clone(), 0, false).with_open_error(
        ConsumerError::PositionsNotSet {
            unseeded: vec![],
            detail: "injected".into(),
        },
    );

    let (run, stopped) =
        run_after_a_refused_stream_open(&fx, faulty, "tx-pns-first-item-resume").await;

    assert_eq!(run.placements, vec![(FIRST, 0, 1), (SECOND, 1, 1)]);
    assert_eq!(run.leave_calls, vec![run.rejoined_from()]);
    let Ok(()) = stopped else {
        panic!("expected the consumer to stop cleanly, got {stopped:?}");
    };
}

const ORDERS_TOPIC: &str = gts_id!("cf.core.events.topic.v1~example.mock.showcase.tx_orders.v1");
const ORDERS_EVENT: &str = gts_id!("cf.core.events.event.v1~example.mock.showcase.tx_orders.v1~");
const AUDIT_TOPIC: &str = gts_id!("cf.core.events.topic.v1~example.mock.showcase.tx_audit.v1");
const AUDIT_EVENT: &str = gts_id!("cf.core.events.event.v1~example.mock.showcase.tx_audit.v1~");
const ORDER_ID: Uuid = Uuid::from_u128(0x7160_0201);
const AUDIT_ID: Uuid = Uuid::from_u128(0x7160_0202);

type TopicPlacements = Arc<Mutex<Vec<(Uuid, GtsInstanceId, u32)>>>;

/// Records `(id, topic, partition)` of every event, the topic and partition taken
/// from its delivery, and commits its offset in a transaction so the consumer goes
/// on streaming.
struct TopicTxRecorder {
    db: toolkit_db::Db,
    placements: TopicPlacements,
}

#[async_trait]
impl TxEventHandler<serde_json::Value, RecordingTxOffsetManager> for TopicTxRecorder {
    async fn handle(
        &self,
        event: EventV1,
        delivery: &Delivery,
        commit: TxCommitHandle<RecordingTxOffsetManager>,
    ) -> Result<HandlerOutcome, ConsumerError> {
        let offset = event.sequence().expect("a delivered event is stamped");
        self.db
            .transaction_ref(move |tx| {
                Box::pin(async move {
                    commit.commit_offset_in_tx(tx, offset).await.map_err(|err| {
                        toolkit_db::DbError::Sea(sea_orm::DbErr::Custom(err.to_string()))
                    })
                })
            })
            .await
            .map_err(|err| ConsumerError::Internal(err.to_string()))?;
        self.placements.lock().unwrap().push((
            event.id(),
            delivery.topic().clone(),
            delivery.partition(),
        ));
        Ok(HandlerOutcome::Success)
    }
}

/// The transactional twin of
/// `batch_handler::a_batch_handler_sees_one_delivery_per_topic_partition_batch`. Both
/// topics use partition 0: only the topic tells the two deliveries apart.
#[tokio::test]
async fn a_transactional_handler_sees_the_topic_of_each_delivery() {
    let fixture =
        two_topic_fixture((ORDERS_TOPIC, ORDERS_EVENT), (AUDIT_TOPIC, AUDIT_EVENT), 1).await;
    let placements = TopicPlacements::default();
    let recorder = TopicTxRecorder {
        db: transaction_db("tx_delivery_topic").await,
        placements: placements.clone(),
    };

    let handle = ConsumerBuilder::new(fixture.broker.clone())
        .group(ConsumerGroupRef::auto_anonymous("tx-delivery-topic"))
        .topics([topic(ORDERS_TOPIC), topic(AUDIT_TOPIC)])
        .offset_manager(RecordingTxOffsetManager)
        .handler(recorder)
        .start()
        .await
        .expect("transactional consumer starts");

    for (id, event_type) in [(ORDER_ID, ORDERS_EVENT), (AUDIT_ID, AUDIT_EVENT)] {
        let event = EventV1::builder_for(GtsTypeId::new(event_type), json!({}))
            .id(id)
            .tenant_id(fixture.ctx.subject_tenant_id())
            .source("tx-delivery-topic")
            .subject("subject-1")
            .subject_type_id(GtsTypeId::new(SHOWCASE_SUBJECT_TYPE))
            .build()
            .expect("a valid event");
        fixture
            .broker
            .publish(&fixture.ctx, &event, Ack::Accepted)
            .await
            .expect("event published");
    }

    let both_placed = || placements.lock().unwrap().len() == 2;
    // A stall shows as a missing event in the assert below, not as a panic here.
    let _ = tokio::time::timeout(BOUND, async {
        while !both_placed() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    tokio::time::timeout(BOUND, handle.stop())
        .await
        .expect("the consumer stops within the bound")
        .expect("consumer stops");

    let mut placed = placements.lock().unwrap().clone();
    placed.sort_by_key(|&(id, _, _)| id);
    assert_eq!(
        placed,
        vec![
            (ORDER_ID, topic(ORDERS_TOPIC), 0),
            (AUDIT_ID, topic(AUDIT_TOPIC), 0),
        ]
    );
}

// ---------------------------------------------------------------------------
// Typed transactional routes: `.event::<P>(TxUndecodable)` decodes each event as `P`
// and applies the policy to an event that does not decode.
// ---------------------------------------------------------------------------

const TYPED_TOPIC: &str = gts_id!("cf.core.events.topic.v1~example.mock.showcase.tx_typed.v1");
const ORDER_PLACED: &str =
    gts_id!("cf.core.events.event.v1~example.mock.tx_typed.order_placed.v1~");

const BAD_ID: Uuid = Uuid::from_u128(0xBAD);
const GOOD_ID: Uuid = Uuid::from_u128(0x600D);

/// The typed route's payload; `order_id` and `quantity` are required.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct OrderPlacedV1 {
    order_id: String,
    quantity: u32,
}

impl GtsSchema for OrderPlacedV1 {
    const TYPE_ID: &'static str = ORDER_PLACED;

    fn gts_schema_with_refs() -> serde_json::Value {
        json!({ "type": "object" })
    }
}

fn good_order() -> OrderPlacedV1 {
    OrderPlacedV1 {
        order_id: "o-2".to_owned(),
        quantity: 2,
    }
}

/// One topic of one partition whose `ORDER_PLACED` type admits any object, so the
/// broker stores a body that does not decode as `OrderPlacedV1`.
async fn typed_fixture() -> TopicFixture {
    fixture_from_catalog(StaticTypesRegistry::of(json!([
        { "id": TYPED_TOPIC, "partitions": 1 },
        { "id": ORDER_PLACED, "topic": TYPED_TOPIC, "data_schema": { "type": "object" }, "allowed_subject_types": [SHOWCASE_SUBJECT_TYPE], "partition_key": "/source" },
    ])))
    .await
}

/// Publishes an `ORDER_PLACED` event with a fixed `id` carrying `data`.
async fn publish_order(fixture: &TopicFixture, id: Uuid, data: serde_json::Value) {
    let event = EventV1::builder_for(GtsTypeId::new(ORDER_PLACED), data)
        .id(id)
        .tenant_id(fixture.ctx.subject_tenant_id())
        .source("tx-typed-route")
        .subject(id.to_string())
        .subject_type_id(GtsTypeId::new(SHOWCASE_SUBJECT_TYPE))
        .build()
        .expect("a valid event");
    fixture
        .broker
        .publish(&fixture.ctx, &event, Ack::Accepted)
        .await
        .expect("the broker accepts a body its type's schema admits");
}

fn bad_order_body() -> serde_json::Value {
    json!({ "order_id": "o-1" })
}

fn good_order_body() -> serde_json::Value {
    json!({ "order_id": "o-2", "quantity": 2 })
}

/// Commits `offset` through `commit` in a transaction of its own.
async fn commit_in_own_tx(
    db: &toolkit_db::Db,
    commit: TxCommitHandle<CountingTxOffsetManager>,
    offset: Sequence,
) -> Result<(), ConsumerError> {
    db.transaction_ref(move |tx| {
        Box::pin(async move {
            commit
                .commit_offset_in_tx(tx, offset)
                .await
                .map_err(|err| toolkit_db::DbError::Sea(sea_orm::DbErr::Custom(err.to_string())))
        })
    })
    .await
    .map_err(|err| ConsumerError::Internal(err.to_string()))
}

type TypedTxCalls = Arc<Mutex<Vec<(Uuid, Option<OrderPlacedV1>)>>>;
type TypedTxBatchCalls = Arc<Mutex<Vec<Vec<(Uuid, Option<OrderPlacedV1>)>>>>;
type RawTxCalls = Arc<Mutex<Vec<(Uuid, Option<serde_json::Value>)>>>;

/// Commits each typed event's offset in a transaction and records it as `(id, data)`.
struct TypedTxCommitter {
    db: toolkit_db::Db,
    calls: TypedTxCalls,
}

#[async_trait]
impl TxEventHandler<OrderPlacedV1, CountingTxOffsetManager> for TypedTxCommitter {
    async fn handle(
        &self,
        event: EventV1<OrderPlacedV1>,
        _delivery: &Delivery,
        commit: TxCommitHandle<CountingTxOffsetManager>,
    ) -> Result<HandlerOutcome, ConsumerError> {
        let offset = event.sequence().expect("a delivered event is stamped");
        commit_in_own_tx(&self.db, commit, offset).await?;
        self.calls
            .lock()
            .unwrap()
            .push((event.id(), event.data().cloned()));
        Ok(HandlerOutcome::Success)
    }
}

/// Commits the offset of each typed batch's last event in a transaction and records
/// the batch as `(id, data)` per event.
struct TypedTxBatchCommitter {
    db: toolkit_db::Db,
    calls: TypedTxBatchCalls,
}

#[async_trait]
impl TxBatchHandler<OrderPlacedV1, CountingTxOffsetManager> for TypedTxBatchCommitter {
    async fn handle_batch(
        &self,
        batch: &EventBatch<'_, OrderPlacedV1>,
        _delivery: &Delivery,
        commit: TxCommitHandle<CountingTxOffsetManager>,
    ) -> Result<HandlerOutcome, ConsumerError> {
        let last = batch
            .iter()
            .last()
            .and_then(EventV1::sequence)
            .expect("a delivered batch has a stamped last event");
        commit_in_own_tx(&self.db, commit, last).await?;
        self.calls.lock().unwrap().push(
            batch
                .iter()
                .map(|event| (event.id(), event.data().cloned()))
                .collect(),
        );
        Ok(HandlerOutcome::Success)
    }
}

/// Commits each raw event's offset in a transaction and records it as `(id, data)`.
struct RawTxCommitter {
    db: toolkit_db::Db,
    calls: RawTxCalls,
}

#[async_trait]
impl TxEventHandler<serde_json::Value, CountingTxOffsetManager> for RawTxCommitter {
    async fn handle(
        &self,
        event: EventV1,
        _delivery: &Delivery,
        commit: TxCommitHandle<CountingTxOffsetManager>,
    ) -> Result<HandlerOutcome, ConsumerError> {
        let offset = event.sequence().expect("a delivered event is stamped");
        commit_in_own_tx(&self.db, commit, offset).await?;
        self.calls
            .lock()
            .unwrap()
            .push((event.id(), event.data().cloned()));
        Ok(HandlerOutcome::Success)
    }
}

impl RawTxCommitter {
    fn new(db: &toolkit_db::Db) -> Self {
        Self {
            db: db.clone(),
            calls: RawTxCalls::default(),
        }
    }
}

/// The offsets in `events`' `OffsetCommitted`, in order. In a transactional consumer
/// only the handler path emits it, after a handler's committed `Success`.
fn offsets_committed(events: &Mutex<Vec<ConsumerRuntimeEvent>>) -> Vec<Sequence> {
    events
        .lock()
        .unwrap()
        .iter()
        .filter_map(|event| match event {
            ConsumerRuntimeEvent::OffsetCommitted { offset, .. } => Some(*offset),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_typed_transactional_route_hands_the_decoded_payload_and_commits() {
    let fixture = typed_fixture().await;
    let db = transaction_db("tx_typed_decoded").await;
    let typed = TypedTxCommitter {
        db: db.clone(),
        calls: TypedTxCalls::default(),
    };
    let typed_calls = typed.calls.clone();
    let default = RawTxCommitter::new(&db);
    let default_calls = default.calls.clone();
    let manager = CountingTxOffsetManager::default();
    let commits = manager.commits.clone();
    let listener = RecordingRuntimeListener::default();
    let runtime_events = listener.events.clone();

    let handle = ConsumerBuilder::new(fixture.broker.clone())
        .group(ConsumerGroupRef::auto_anonymous("tx-typed-route-decoded"))
        .topics([topic(TYPED_TOPIC)])
        .register_listener(listener)
        .offset_manager(manager)
        .default_handler(default)
        .route()
        .topic(topic(TYPED_TOPIC))
        .event::<OrderPlacedV1>(TxUndecodable::Stall)
        .handler(typed)
        .start()
        .await
        .expect("transactional consumer starts");

    publish_order(&fixture, GOOD_ID, good_order_body()).await;

    wait_for("an offset is committed", || {
        !offsets_committed(&runtime_events).is_empty()
    })
    .await;
    tokio::time::timeout(BOUND, handle.stop())
        .await
        .expect("the consumer stops within the bound")
        .expect("consumer stops");

    assert_eq!(
        *typed_calls.lock().unwrap(),
        vec![(GOOD_ID, Some(good_order()))]
    );
    assert_eq!(*default_calls.lock().unwrap(), vec![]);
    assert_eq!(commits.load(Ordering::SeqCst), 1);
    assert_eq!(
        offsets_committed(&runtime_events),
        vec![Sequence::assigned(1)]
    );
}

#[tokio::test]
async fn an_undecodable_event_on_a_transactional_stall_route_is_retried_and_never_handed_over() {
    let fixture = typed_fixture().await;
    let db = transaction_db("tx_typed_stall").await;
    let typed = TypedTxCommitter {
        db: db.clone(),
        calls: TypedTxCalls::default(),
    };
    let typed_calls = typed.calls.clone();
    let manager = CountingTxOffsetManager::default();
    let commits = manager.commits.clone();
    let listener = RecordingRuntimeListener::default();
    let runtime_events = listener.events.clone();

    let handle = ConsumerBuilder::new(fixture.broker.clone())
        .group(ConsumerGroupRef::auto_anonymous("tx-typed-route-stall"))
        .topics([topic(TYPED_TOPIC)])
        .register_listener(listener)
        .retry(ConsumerRetry {
            base_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(1),
        })
        .offset_manager(manager)
        .default_handler(RawTxCommitter::new(&db))
        .route()
        .topic(topic(TYPED_TOPIC))
        .event::<OrderPlacedV1>(TxUndecodable::Stall)
        .handler(typed)
        .start()
        .await
        .expect("transactional consumer starts");

    publish_order(&fixture, BAD_ID, bad_order_body()).await;
    publish_order(&fixture, GOOD_ID, good_order_body()).await;

    // In a transactional consumer `RetryScheduled` comes from the handler's `Retry`
    // and from a handler error; no handler here returns an error.
    let retries = || -> Vec<(TopicId, String, u32, u16, Duration)> {
        runtime_events
            .lock()
            .unwrap()
            .iter()
            .filter_map(|event| match event {
                ConsumerRuntimeEvent::RetryScheduled {
                    topic_id,
                    topic,
                    partition,
                    attempt,
                    delay,
                } => Some((*topic_id, topic.to_string(), *partition, *attempt, *delay)),
                _ => None,
            })
            .collect()
    };
    wait_for("the third retry is scheduled", || {
        retries().iter().any(|retry| retry.3 >= 3)
    })
    .await;
    tokio::time::timeout(BOUND, handle.stop())
        .await
        .expect("the consumer stops within the bound")
        .expect("consumer stops");

    let retries = retries();
    let [first, second, third, ..] = retries.as_slice() else {
        panic!("expected at least three RetryScheduled, got {retries:?}");
    };
    let retry = |attempt| {
        (
            TopicId::from_gts(TYPED_TOPIC),
            TYPED_TOPIC.to_owned(),
            0,
            attempt,
            Duration::from_millis(1),
        )
    };
    assert_eq!([first, second, third], [&retry(1), &retry(2), &retry(3)]);
    assert_eq!(*typed_calls.lock().unwrap(), vec![]);
    assert_eq!(commits.load(Ordering::SeqCst), 0);
    assert_eq!(offsets_committed(&runtime_events), vec![]);
    let events = runtime_events.lock().unwrap();
    let reasons: Vec<&str> = events
        .iter()
        .filter_map(|event| match event {
            ConsumerRuntimeEvent::HandlerBatchCompleted {
                outcome: BatchHandlerOutcome::Retry { reason },
                ..
            } => Some(reason.as_str()),
            _ => None,
        })
        .take(1)
        .collect();
    assert_eq!(
        reasons,
        vec![
            "event data of gts.cf.core.events.event.v1~example.mock.tx_typed.order_placed.v1~ \
             does not decode as the payload type (data)"
        ]
    );
}

#[tokio::test]
async fn an_undecodable_event_on_a_transactional_fallback_route_goes_to_the_raw_handler() {
    let fixture = typed_fixture().await;
    let db = transaction_db("tx_typed_fallback").await;
    let typed = TypedTxCommitter {
        db: db.clone(),
        calls: TypedTxCalls::default(),
    };
    let typed_calls = typed.calls.clone();
    let fallback = RawTxCommitter::new(&db);
    let fallback_calls = fallback.calls.clone();
    let default = RawTxCommitter::new(&db);
    let default_calls = default.calls.clone();
    let manager = CountingTxOffsetManager::default();
    let commits = manager.commits.clone();
    let listener = RecordingRuntimeListener::default();
    let runtime_events = listener.events.clone();

    let handle = ConsumerBuilder::new(fixture.broker.clone())
        .group(ConsumerGroupRef::auto_anonymous("tx-typed-route-fallback"))
        .topics([topic(TYPED_TOPIC)])
        .register_listener(listener)
        .offset_manager(manager)
        .default_handler(default)
        .route()
        .topic(topic(TYPED_TOPIC))
        .event::<OrderPlacedV1>(TxUndecodable::Fallback(Arc::new(fallback)))
        .handler(typed)
        .start()
        .await
        .expect("transactional consumer starts");

    publish_order(&fixture, BAD_ID, bad_order_body()).await;
    publish_order(&fixture, GOOD_ID, good_order_body()).await;

    wait_for("both offsets are committed", || {
        offsets_committed(&runtime_events).len() == 2
    })
    .await;
    tokio::time::timeout(BOUND, handle.stop())
        .await
        .expect("the consumer stops within the bound")
        .expect("consumer stops");

    assert_eq!(
        *fallback_calls.lock().unwrap(),
        vec![(BAD_ID, Some(bad_order_body()))]
    );
    assert_eq!(
        *typed_calls.lock().unwrap(),
        vec![(GOOD_ID, Some(good_order()))]
    );
    assert_eq!(*default_calls.lock().unwrap(), vec![]);
    assert_eq!(commits.load(Ordering::SeqCst), 2);
    assert_eq!(
        offsets_committed(&runtime_events),
        vec![Sequence::assigned(1), Sequence::assigned(2)]
    );
}

/// Batches of one event (the default profile), so every batch decodes whole (`k = n`).
#[tokio::test]
async fn a_typed_transactional_batch_route_hands_decoded_batches_and_commits() {
    let fixture = typed_fixture().await;
    let db = transaction_db("tx_typed_batch").await;
    let typed = TypedTxBatchCommitter {
        db: db.clone(),
        calls: TypedTxBatchCalls::default(),
    };
    let typed_calls = typed.calls.clone();
    let default = RawTxCommitter::new(&db);
    let default_calls = default.calls.clone();
    let manager = CountingTxOffsetManager::default();
    let commits = manager.commits.clone();
    let listener = RecordingRuntimeListener::default();
    let runtime_events = listener.events.clone();

    let handle = ConsumerBuilder::new(fixture.broker.clone())
        .group(ConsumerGroupRef::auto_anonymous("tx-typed-batch-route"))
        .topics([topic(TYPED_TOPIC)])
        .register_listener(listener)
        .offset_manager(manager)
        .default_handler(default)
        .route()
        .topic(topic(TYPED_TOPIC))
        .event::<OrderPlacedV1>(TxUndecodable::Stall)
        .batch_handler(typed)
        .start()
        .await
        .expect("transactional consumer starts");

    publish_order(&fixture, GOOD_ID, good_order_body()).await;

    wait_for("an offset is committed", || {
        !offsets_committed(&runtime_events).is_empty()
    })
    .await;
    tokio::time::timeout(BOUND, handle.stop())
        .await
        .expect("the consumer stops within the bound")
        .expect("consumer stops");

    assert_eq!(
        *typed_calls.lock().unwrap(),
        vec![vec![(GOOD_ID, Some(good_order()))]]
    );
    assert_eq!(*default_calls.lock().unwrap(), vec![]);
    assert_eq!(commits.load(Ordering::SeqCst), 1);
    assert_eq!(
        offsets_committed(&runtime_events),
        vec![Sequence::assigned(1)]
    );
}

// ---------------------------------------------------------------------------
// Stopping a transactional consumer whose handler keeps retrying: `stop()` cancels
// each slot, and the retry loop gives the batch up instead of backing off again.
// ---------------------------------------------------------------------------

/// What a retrying transactional handler answers on every delivery.
#[derive(Clone, Copy)]
enum TxRetryAnswer {
    Retry,
    Err,
}

impl TxRetryAnswer {
    fn outcome(self) -> Result<HandlerOutcome, ConsumerError> {
        match self {
            Self::Retry => Ok(HandlerOutcome::Retry {
                reason: "not yet".to_owned(),
            }),
            Self::Err => Err(ConsumerError::Internal("handler failed".to_owned())),
        }
    }
}

/// Answers every delivery with `answer` and never commits.
struct RetryingTxHandler {
    answer: TxRetryAnswer,
}

#[async_trait]
impl TxEventHandler<serde_json::Value, RecordingTxOffsetManager> for RetryingTxHandler {
    async fn handle(
        &self,
        _event: EventV1,
        _delivery: &Delivery,
        _commit: TxCommitHandle<RecordingTxOffsetManager>,
    ) -> Result<HandlerOutcome, ConsumerError> {
        self.answer.outcome()
    }
}

/// Signals `entered` on each delivery, holds it until `release` is notified, then
/// answers with `answer` and never commits.
struct HeldTxHandler {
    answer: TxRetryAnswer,
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}

#[async_trait]
impl TxEventHandler<serde_json::Value, RecordingTxOffsetManager> for HeldTxHandler {
    async fn handle(
        &self,
        _event: EventV1,
        _delivery: &Delivery,
        _commit: TxCommitHandle<RecordingTxOffsetManager>,
    ) -> Result<HandlerOutcome, ConsumerError> {
        self.entered.notify_one();
        self.release.notified().await;
        self.answer.outcome()
    }
}

/// Every `RetryScheduled` recorded so far, as `(topic_id, topic, partition, attempt, delay)`.
fn retries_scheduled(
    events: &Mutex<Vec<ConsumerRuntimeEvent>>,
) -> Vec<(TopicId, GtsInstanceId, u32, u16, Duration)> {
    events
        .lock()
        .unwrap()
        .iter()
        .filter_map(|event| match event {
            ConsumerRuntimeEvent::RetryScheduled {
                topic_id,
                topic,
                partition,
                attempt,
                delay,
            } => Some((*topic_id, topic.clone(), *partition, *attempt, *delay)),
            _ => None,
        })
        .collect()
}

/// Waits, bounded by `BOUND`, until `predicate` holds; parks 10 ms between checks.
async fn wait_for(what: &str, mut predicate: impl FnMut() -> bool) {
    tokio::time::timeout(BOUND, async {
        while !predicate() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{what} within the bound"));
}

/// Starts a transactional consumer of `TOPIC` with `handler`, retry backoff `delay`
/// and a recording listener, and publishes one event to it. The fixture is returned
/// with the handle because it owns the running broker.
async fn start_retrying_tx_consumer<H>(
    group: &str,
    delay: Duration,
    handler: H,
) -> (
    TopicFixture,
    event_broker_sdk::consumer::ConsumerHandle,
    Arc<Mutex<Vec<ConsumerRuntimeEvent>>>,
)
where
    H: TxEventHandler<serde_json::Value, RecordingTxOffsetManager> + 'static,
{
    let fx = topic_fixture(TOPIC, EVENT_TYPE, 1).await;
    let listener = RecordingRuntimeListener::default();
    let runtime_events = listener.events.clone();
    let handle = ConsumerBuilder::new(fx.broker.clone())
        .group(ConsumerGroupRef::auto_anonymous(group))
        .topics([topic(TOPIC)])
        .register_listener(listener)
        .retry(ConsumerRetry {
            base_delay: delay,
            max_delay: delay,
        })
        .offset_manager(RecordingTxOffsetManager)
        .handler(handler)
        .start()
        .await
        .expect("transactional consumer starts");
    publish_json(&fx.broker, &fx.ctx, EVENT_TYPE, "retry", None, json!({})).await;
    (fx, handle, runtime_events)
}

/// The retry loop gives the batch up once `stop()` cancels the slot. The slot then
/// leaves at its next stream frame (a heartbeat, 1 s in this fixture), well inside
/// `BOUND`; a loop that ignores the cancel retries every 1 ms and `stop()` never ends.
#[tokio::test]
async fn a_transactional_consumer_whose_handler_keeps_retrying_stops_promptly() {
    let delay = Duration::from_millis(1);
    let (_fixture, handle, runtime_events) = start_retrying_tx_consumer(
        "tx-retrying-stop",
        delay,
        RetryingTxHandler {
            answer: TxRetryAnswer::Retry,
        },
    )
    .await;

    wait_for("the third retry is scheduled", || {
        retries_scheduled(&runtime_events).len() >= 3
    })
    .await;
    let stopped = tokio::time::timeout(BOUND, handle.stop()).await;

    assert!(
        matches!(stopped, Ok(Ok(()))),
        "the consumer stops within the bound: {stopped:?}"
    );
    let retries = retries_scheduled(&runtime_events);
    let [first, second, third, ..] = retries.as_slice() else {
        panic!("at least three retries were scheduled: {retries:?}");
    };
    let topic_id = TopicId::from_gts(TOPIC);
    assert_eq!(
        [first, second, third],
        [
            &(topic_id, topic(TOPIC), 0, 1, delay),
            &(topic_id, topic(TOPIC), 0, 2, delay),
            &(topic_id, topic(TOPIC), 0, 3, delay),
        ]
    );
}

/// `stop()` during a 60 s backoff ends the backoff: the slot leaves at its next
/// heartbeat (1 s), far below the backoff, so a backoff that ignores the cancel
/// shows as `Elapsed` at `BOUND`.
async fn stop_during_a_long_backoff(group: &str, answer: TxRetryAnswer) {
    let delay = Duration::from_secs(60);
    let (_fixture, handle, runtime_events) =
        start_retrying_tx_consumer(group, delay, RetryingTxHandler { answer }).await;

    wait_for("the first retry is scheduled", || {
        !retries_scheduled(&runtime_events).is_empty()
    })
    .await;
    let stopped = tokio::time::timeout(BOUND, handle.stop()).await;

    assert!(
        matches!(stopped, Ok(Ok(()))),
        "the consumer stops within the bound: {stopped:?}"
    );
    assert_eq!(
        retries_scheduled(&runtime_events),
        vec![(TopicId::from_gts(TOPIC), topic(TOPIC), 0, 1, delay)]
    );
}

#[tokio::test]
async fn a_transactional_consumer_stops_during_a_long_retry_backoff() {
    stop_during_a_long_backoff("tx-long-backoff-retry", TxRetryAnswer::Retry).await;
}

#[tokio::test]
async fn a_transactional_consumer_stops_during_a_long_retry_backoff_after_a_handler_error() {
    stop_during_a_long_backoff("tx-long-backoff-err", TxRetryAnswer::Err).await;
}

/// `stop()` while the handler runs: the handler's answer schedules no retry. The
/// handler holds its first delivery until released; one poll of `stop()` runs
/// `Consumer::shutdown` to its first await, which comes after every slot's token is
/// cancelled and before the slot is joined, so the cancel is in place before the
/// handler answers. The held delivery lasts far below the default 5 s
/// `handler_latency`, so no slow-consumer strike ends the batch first. Returns the
/// runtime events.
async fn stop_while_the_handler_runs(
    group: &str,
    answer: TxRetryAnswer,
) -> Vec<ConsumerRuntimeEvent> {
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let (_fixture, handle, runtime_events) = start_retrying_tx_consumer(
        group,
        Duration::from_millis(1),
        HeldTxHandler {
            answer,
            entered: entered.clone(),
            release: release.clone(),
        },
    )
    .await;

    tokio::time::timeout(BOUND, entered.notified())
        .await
        .expect("the handler receives the event within the bound");
    let mut stop = Box::pin(handle.stop());
    let first_poll =
        std::future::poll_fn(|cx| std::task::Poll::Ready(stop.as_mut().poll(cx))).await;
    assert!(
        first_poll.is_pending(),
        "stop() waits for the held slot: {first_poll:?}"
    );
    let retries_before_release = retries_scheduled(&runtime_events);
    release.notify_one();
    let stopped = tokio::time::timeout(BOUND, stop).await;

    assert!(
        matches!(stopped, Ok(Ok(()))),
        "the consumer stops within the bound: {stopped:?}"
    );
    assert_eq!(retries_before_release, vec![]);
    assert_eq!(retries_scheduled(&runtime_events), retries_before_release);
    runtime_events.lock().unwrap().clone()
}

#[tokio::test]
async fn a_transactional_consumer_stopped_while_its_handler_runs_schedules_no_retry() {
    let events = stop_while_the_handler_runs("tx-held-retry", TxRetryAnswer::Retry).await;

    let completed: Vec<&BatchHandlerOutcome> = events
        .iter()
        .filter_map(|event| match event {
            ConsumerRuntimeEvent::HandlerBatchCompleted { outcome, .. } => Some(outcome),
            _ => None,
        })
        .collect();
    let [BatchHandlerOutcome::Retry { reason }] = completed.as_slice() else {
        panic!("the handler's one answer is a Retry: {completed:?}");
    };
    assert_eq!(reason, "not yet");
}

#[tokio::test]
async fn a_transactional_consumer_stopped_while_its_handler_fails_schedules_no_retry() {
    let events = stop_while_the_handler_runs("tx-held-err", TxRetryAnswer::Err).await;

    let failed: Vec<String> = events
        .iter()
        .filter_map(|event| match event {
            ConsumerRuntimeEvent::HandlerFailed { error, .. } => Some(error.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(failed, vec!["internal: handler failed".to_owned()]);
}
