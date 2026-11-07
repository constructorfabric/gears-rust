//! The transactional typed routes: the single-event and batch adapters that decode
//! delivered events as the route's payload type, hand each handler a commit handle
//! scoped to the events it got, and apply the route's [`TxUndecodable`] policy; and
//! the route builder that sets the type and the policy.

use std::sync::{Arc, Mutex};

use gts::{GtsIdPattern, GtsInstanceId, GtsSchema, GtsTypeId};
use serde::{Deserialize, Serialize};
use toolkit_gts::gts_id;
use uuid::Uuid;

use super::commit::TxCommitHandleParts;
use super::types::{DeliverTxBatch, TxTypedBatchHandlerAdapter, TxTypedEventHandlerAdapter};
use super::{
    CommitOffsetInTx, ConsumerBuilder, ConsumerGroupRef, ConsumerOffsetManager, ConsumerRoute,
    ConsumerRouteHandlerKind, Delivery, EventBatch, Fallback, HandlerOutcome, OffsetManagerError,
    OffsetStore, Position, TxBatchHandler, TxCommitHandle, TxEventHandler, TxUndecodable, WithTx,
};
use crate::error::{ConsumerError, EventBrokerError};
use crate::gts::{EventV1, StoredPosition};
use crate::ids::{ConsumerGroupId, TopicId};
use crate::sequence::Sequence;
use crate::warn_capture::WarnCapture;

const TOPIC: &str = gts_id!("cf.core.events.topic.v1~example.orders.x.x.v1");
const ORDER_PLACED: &str = gts_id!("cf.core.events.event.v1~example.orders.order_placed.x.v1~");
const ORDER_CREATED: &str = gts_id!("cf.core.events.event.v1~example.orders.order_created.x.v1~");

/// The consumer group every handle in these tests commits for.
fn group() -> ConsumerGroupId {
    ConsumerGroupId::new(Uuid::from_u128(0x7501))
}

/// The topic id every handle in these tests commits for.
fn topic_id() -> TopicId {
    TopicId::new(Uuid::from_u128(0x7502))
}

/// The typed payload of the route under test; `order_id` and `quantity` are required.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct OrderPlacedV1 {
    order_id: String,
    quantity: u32,
}

impl GtsSchema for OrderPlacedV1 {
    const TYPE_ID: &'static str = ORDER_PLACED;

    fn gts_schema_with_refs() -> serde_json::Value {
        serde_json::json!({ "type": "object" })
    }
}

fn ginst(id: &str) -> GtsInstanceId {
    GtsInstanceId::try_new(id).expect("valid GTS instance id")
}

fn gpat(id: &str) -> GtsIdPattern {
    GtsIdPattern::try_new(id).expect("valid GTS pattern")
}

/// An `ORDER_PLACED` event with id `id` carrying `data`, stamped at `sequence` of
/// partition 7.
fn order_event(id: u128, sequence: i64, data: serde_json::Value) -> EventV1 {
    EventV1::builder_for(GtsTypeId::new(ORDER_PLACED), data)
        .id(Uuid::from_u128(id))
        .tenant_id(Uuid::nil())
        .subject("order-1")
        .subject_type_id(GtsTypeId::new("gts.x.eb.test.subject.v1~"))
        .occurred_at("2026-01-02T03:04:05Z".parse().unwrap())
        .build()
        .unwrap()
        .stamp_position(StoredPosition {
            partition: 7,
            sequence: Sequence::assigned(sequence),
            sequence_time: "2026-01-02T03:04:06Z".parse().unwrap(),
        })
}

/// The payload `good_at(id, ..)` carries.
fn order(id: u128) -> OrderPlacedV1 {
    OrderPlacedV1 {
        order_id: format!("o-{id}"),
        quantity: 3,
    }
}

/// An event with id `id` at `sequence` that decodes as [`order`]`(id)`.
fn good_at(id: u128, sequence: i64) -> EventV1 {
    order_event(
        id,
        sequence,
        serde_json::json!({ "order_id": format!("o-{id}"), "quantity": 3 }),
    )
}

/// An event with id `id` at `sequence` that misses the required `quantity`.
fn bad_at(id: u128, sequence: i64) -> EventV1 {
    order_event(
        id,
        sequence,
        serde_json::json!({ "order_id": format!("o-{id}") }),
    )
}

/// `[ok, ok, bad, ok]` at sequences 41 to 44: `k = 2`, `last = 42`, with a tail after
/// the bad event.
fn mixed_batch() -> Vec<EventV1> {
    vec![
        good_at(1, 41),
        good_at(2, 42),
        bad_at(3, 43),
        good_at(4, 44),
    ]
}

/// `[ok, ok, bad]` at sequences 41 to 43: `k = 2`, `last = 42`.
fn ok_ok_bad_batch() -> Vec<EventV1> {
    vec![good_at(1, 41), good_at(2, 42), bad_at(3, 43)]
}

/// `[bad, ok]` at sequences 41 and 42: `k = 0`.
fn bad_first_batch() -> Vec<EventV1> {
    vec![bad_at(1, 41), good_at(2, 42)]
}

/// `[ok, ok, ok]` at sequences 41 to 43: `k = n`.
fn good_batch() -> Vec<EventV1> {
    vec![good_at(1, 41), good_at(2, 42), good_at(3, 43)]
}

/// Topic, partition and attempts of the delivery the adapter is called with.
fn delivery() -> Delivery {
    Delivery::new(ginst(TOPIC), 7, 2)
}

fn not_yet() -> HandlerOutcome {
    HandlerOutcome::Retry {
        reason: "not yet".to_owned(),
    }
}

/// The decode error's display for an `ORDER_PLACED` event whose data does not decode.
const DECODE_REASON: &str = "event data of gts.cf.core.events.event.v1~example.orders.order_placed.x.v1~ \
                             does not decode as the payload type (data)";

/// One offset write: group, topic, partition and offset.
type RecordedCommit = (ConsumerGroupId, TopicId, u32, i64);

/// Records every offset written into a transaction.
#[derive(Default)]
struct RecordingTxOffsetManager {
    calls: Mutex<Vec<RecordedCommit>>,
}

#[async_trait::async_trait]
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

#[async_trait::async_trait]
impl CommitOffsetInTx for RecordingTxOffsetManager {
    async fn commit_in_tx<TX>(
        &self,
        _txn: &TX,
        group: &ConsumerGroupId,
        topic: &TopicId,
        partition: u32,
        offset: Sequence,
    ) -> Result<(), OffsetManagerError>
    where
        TX: toolkit_db::secure::DBRunner + Sync,
    {
        self.calls
            .lock()
            .unwrap()
            .push((*group, *topic, partition, offset.as_i64()));
        Ok(())
    }
}

impl ConsumerOffsetManager for RecordingTxOffsetManager {
    type BuilderState = WithTx<Self>;

    fn into_builder_state(self) -> Self::BuilderState {
        WithTx(self)
    }
}

type Manager = RecordingTxOffsetManager;

async fn sqlite() -> toolkit_db::Db {
    toolkit_db::connect_db(
        "sqlite::memory:",
        toolkit_db::ConnectOpts {
            max_conns: Some(1),
            ..toolkit_db::ConnectOpts::default()
        },
    )
    .await
    .expect("sqlite db")
}

/// What a handler commits through its handle.
#[derive(Clone, Copy)]
enum Commit {
    Nothing,
    Offset(i64),
}

/// What every recording handler does on a call: commits as told in its own
/// transaction, keeps the text of a refused commit, and answers `outcome`.
struct TxStep {
    db: toolkit_db::Db,
    commit: Commit,
    outcome: Result<HandlerOutcome, ConsumerError>,
    commit_errors: Arc<Mutex<Vec<String>>>,
}

impl TxStep {
    fn new(
        db: &toolkit_db::Db,
        commit: Commit,
        outcome: Result<HandlerOutcome, ConsumerError>,
    ) -> Self {
        Self {
            db: db.clone(),
            commit,
            outcome,
            commit_errors: Arc::new(Mutex::new(Vec::new())),
        }
    }

    async fn run(&self, handle: TxCommitHandle<Manager>) -> Result<HandlerOutcome, ConsumerError> {
        if let Commit::Offset(offset) = self.commit {
            let committed = self
                .db
                .transaction_ref(move |tx| {
                    Box::pin(async move {
                        Ok(handle
                            .commit_offset_in_tx(tx, Sequence::assigned(offset))
                            .await
                            .map_err(|err| err.to_string()))
                    })
                })
                .await
                .expect("transaction");
            if let Err(err) = committed {
                self.commit_errors.lock().unwrap().push(err);
            }
        }
        self.outcome.clone()
    }
}

/// The offsets a handle lets its holder commit.
fn scope(handle: &TxCommitHandle<Manager>) -> Vec<i64> {
    handle.batch_offsets.iter().map(|at| at.as_i64()).collect()
}

/// What the typed single-event handler saw of one call: the event id and data, the
/// delivery's topic, partition and attempts, and its handle's offsets.
type TypedCall = (Uuid, Option<OrderPlacedV1>, String, u32, u16, Vec<i64>);
/// What the typed batch handler saw of one call: each event's id and data, the
/// delivery's topic, partition and attempts, and its handle's offsets.
type BatchCall = (
    Vec<(Uuid, Option<OrderPlacedV1>)>,
    String,
    u32,
    u16,
    Vec<i64>,
);
/// What the raw fallback handler saw of one call: the event id, type and data, the
/// delivery's topic, partition and attempts, and its handle's offsets.
type RawCall = (
    Uuid,
    GtsTypeId,
    Option<serde_json::Value>,
    String,
    u32,
    u16,
    Vec<i64>,
);

struct TypedTxRecorder {
    step: TxStep,
    calls: Arc<Mutex<Vec<TypedCall>>>,
}

impl TypedTxRecorder {
    fn new(step: TxStep) -> Self {
        Self {
            step,
            calls: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

#[async_trait::async_trait]
impl TxEventHandler<OrderPlacedV1, Manager> for TypedTxRecorder {
    async fn handle(
        &self,
        event: EventV1<OrderPlacedV1>,
        delivery: &Delivery,
        commit: TxCommitHandle<Manager>,
    ) -> Result<HandlerOutcome, ConsumerError> {
        self.calls.lock().unwrap().push((
            event.id(),
            event.data().cloned(),
            delivery.topic().to_string(),
            delivery.partition(),
            delivery.attempts(),
            scope(&commit),
        ));
        self.step.run(commit).await
    }
}

struct BatchTxRecorder {
    step: TxStep,
    calls: Arc<Mutex<Vec<BatchCall>>>,
}

impl BatchTxRecorder {
    fn new(step: TxStep) -> Self {
        Self {
            step,
            calls: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

#[async_trait::async_trait]
impl TxBatchHandler<OrderPlacedV1, Manager> for BatchTxRecorder {
    async fn handle_batch(
        &self,
        batch: &EventBatch<'_, OrderPlacedV1>,
        delivery: &Delivery,
        commit: TxCommitHandle<Manager>,
    ) -> Result<HandlerOutcome, ConsumerError> {
        self.calls.lock().unwrap().push((
            batch
                .iter()
                .map(|event| (event.id(), event.data().cloned()))
                .collect(),
            delivery.topic().to_string(),
            delivery.partition(),
            delivery.attempts(),
            scope(&commit),
        ));
        self.step.run(commit).await
    }
}

struct RawTxRecorder {
    step: TxStep,
    calls: Arc<Mutex<Vec<RawCall>>>,
}

impl RawTxRecorder {
    fn new(step: TxStep) -> Self {
        Self {
            step,
            calls: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

#[async_trait::async_trait]
impl TxEventHandler<serde_json::Value, Manager> for RawTxRecorder {
    async fn handle(
        &self,
        event: EventV1,
        delivery: &Delivery,
        commit: TxCommitHandle<Manager>,
    ) -> Result<HandlerOutcome, ConsumerError> {
        self.calls.lock().unwrap().push((
            event.id(),
            event.event_type().clone(),
            event.data().cloned(),
            delivery.topic().to_string(),
            delivery.partition(),
            delivery.attempts(),
            scope(&commit),
        ));
        self.step.run(commit).await
    }
}

/// What one adapter call left behind: its result, the handle's commit state after it,
/// and every offset written into a transaction.
struct Delivered {
    result: Result<HandlerOutcome, ConsumerError>,
    committed: Option<Sequence>,
    writes: Vec<RecordedCommit>,
}

/// Calls `adapter` with `events`, each at its own position, and a handle over all of
/// them, as the transactional dispatcher does.
async fn deliver_through(
    adapter: &dyn DeliverTxBatch<Manager>,
    manager: Arc<Manager>,
    events: &[EventV1],
) -> Delivered {
    let positions: Vec<StoredPosition> = events
        .iter()
        .map(|event| StoredPosition {
            partition: 7,
            sequence: event.sequence().unwrap(),
            sequence_time: "2026-01-02T03:04:06Z".parse().unwrap(),
        })
        .collect();
    let handle = TxCommitHandle::new(TxCommitHandleParts {
        partition: 7,
        offsets: positions.iter().map(|at| at.sequence).collect(),
        offset_manager: manager.clone(),
        group: group(),
        topic: topic_id(),
    });
    let committed = handle.committed_offset.clone();
    let result = adapter
        .deliver(&EventBatch::new(events), &positions, &delivery(), handle)
        .await;
    // The poisoning test leaves the state poisoned; its value is read all the same.
    let committed = *committed
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let writes = manager.calls.lock().unwrap().clone();
    Delivered {
        result,
        committed,
        writes,
    }
}

/// Calls the typed single-event adapter for `policy` with `events`.
async fn deliver_one(
    handler: TypedTxRecorder,
    policy: TxUndecodable<Manager>,
    events: &[EventV1],
) -> Delivered {
    let adapter = TxTypedEventHandlerAdapter::<_, OrderPlacedV1, Manager>::new(handler, policy);
    deliver_through(&adapter, Arc::new(Manager::default()), events).await
}

/// Calls the typed batch adapter for `policy` with `events`.
async fn deliver_batch(
    handler: BatchTxRecorder,
    policy: TxUndecodable<Manager>,
    events: &[EventV1],
) -> Delivered {
    let adapter = TxTypedBatchHandlerAdapter::<_, OrderPlacedV1, Manager>::new(handler, policy);
    deliver_through(&adapter, Arc::new(Manager::default()), events).await
}

fn write(offset: i64) -> RecordedCommit {
    (group(), topic_id(), 7, offset)
}

/// The typed single-event call for [`good_at`]`(id, ..)`, with a handle over `scope`.
fn typed_call(id: u128, scope: Vec<i64>) -> TypedCall {
    (
        Uuid::from_u128(id),
        Some(order(id)),
        TOPIC.to_owned(),
        7,
        2,
        scope,
    )
}

/// The raw call for [`bad_at`]`(id, ..)`, with a handle over `scope`.
fn raw_bad_call(id: u128, scope: Vec<i64>) -> RawCall {
    (
        Uuid::from_u128(id),
        GtsTypeId::new(ORDER_PLACED),
        Some(serde_json::json!({ "order_id": format!("o-{id}") })),
        TOPIC.to_owned(),
        7,
        2,
        scope,
    )
}

/// The typed batch call for the [`good_at`] events `ids`, with a handle over `scope`.
fn batch_call(ids: &[u128], scope: Vec<i64>) -> BatchCall {
    (
        ids.iter()
            .map(|&id| (Uuid::from_u128(id), Some(order(id))))
            .collect(),
        TOPIC.to_owned(),
        7,
        2,
        scope,
    )
}

fn fallback(raw: RawTxRecorder) -> TxUndecodable<Manager> {
    TxUndecodable::Fallback(Arc::new(raw))
}

fn internal(text: &str) -> ConsumerError {
    EventBrokerError::Internal(text.to_owned())
}

// ---------------------------------------------------------------------------
// `TxTypedEventHandlerAdapter`: one event, a `TxEventHandler<P, OM>`.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_decoded_event_goes_to_the_typed_tx_handler_with_the_delivered_handle() {
    let db = sqlite().await;
    let handler = TypedTxRecorder::new(TxStep::new(
        &db,
        Commit::Offset(41),
        Ok(HandlerOutcome::Success),
    ));
    let calls = handler.calls.clone();

    let delivered = deliver_one(handler, TxUndecodable::Stall, &[good_at(1, 41)]).await;

    let Ok(HandlerOutcome::Success) = &delivered.result else {
        panic!("unexpected result: {:?}", delivered.result);
    };
    assert_eq!(delivered.committed, Some(Sequence::assigned(41)));
    assert_eq!(delivered.writes, vec![write(41)]);
    assert_eq!(*calls.lock().unwrap(), vec![typed_call(1, vec![41])]);
}

#[tokio::test]
async fn a_decoded_event_whose_typed_tx_handler_retries_is_retried() {
    let db = sqlite().await;
    let handler = TypedTxRecorder::new(TxStep::new(&db, Commit::Nothing, Ok(not_yet())));
    let calls = handler.calls.clone();

    let delivered = deliver_one(handler, TxUndecodable::Stall, &[good_at(1, 41)]).await;

    let Ok(HandlerOutcome::Retry { reason }) = &delivered.result else {
        panic!("unexpected result: {:?}", delivered.result);
    };
    assert_eq!(reason, "not yet");
    assert_eq!(delivered.committed, None);
    assert_eq!(delivered.writes, vec![]);
    assert_eq!(*calls.lock().unwrap(), vec![typed_call(1, vec![41])]);
}

#[tokio::test]
async fn an_undecodable_event_on_a_tx_stall_route_is_retried_and_nothing_is_committed() {
    let db = sqlite().await;
    let handler = TypedTxRecorder::new(TxStep::new(
        &db,
        Commit::Offset(41),
        Ok(HandlerOutcome::Success),
    ));
    let calls = handler.calls.clone();

    let delivered = deliver_one(handler, TxUndecodable::Stall, &[bad_at(1, 41)]).await;

    let Ok(HandlerOutcome::Retry { reason }) = &delivered.result else {
        panic!("unexpected result: {:?}", delivered.result);
    };
    assert_eq!(reason, DECODE_REASON);
    assert_eq!(delivered.committed, None);
    assert_eq!(delivered.writes, vec![]);
    assert_eq!(*calls.lock().unwrap(), vec![]);
}

#[tokio::test]
async fn an_undecodable_event_on_a_tx_fallback_route_goes_to_the_raw_handler_with_the_delivered_handle()
 {
    let db = sqlite().await;
    let handler = TypedTxRecorder::new(TxStep::new(
        &db,
        Commit::Offset(41),
        Ok(HandlerOutcome::Success),
    ));
    let calls = handler.calls.clone();
    let raw = RawTxRecorder::new(TxStep::new(
        &db,
        Commit::Offset(41),
        Ok(HandlerOutcome::Success),
    ));
    let raw_calls = raw.calls.clone();

    let delivered = deliver_one(handler, fallback(raw), &[bad_at(1, 41)]).await;

    let Ok(HandlerOutcome::Success) = &delivered.result else {
        panic!("unexpected result: {:?}", delivered.result);
    };
    assert_eq!(delivered.committed, Some(Sequence::assigned(41)));
    assert_eq!(delivered.writes, vec![write(41)]);
    assert_eq!(*calls.lock().unwrap(), vec![]);
    assert_eq!(*raw_calls.lock().unwrap(), vec![raw_bad_call(1, vec![41])]);
}

#[tokio::test]
async fn a_tx_fallback_that_retries_a_single_event_is_retried() {
    let db = sqlite().await;
    let handler = TypedTxRecorder::new(TxStep::new(
        &db,
        Commit::Nothing,
        Ok(HandlerOutcome::Success),
    ));
    let raw = RawTxRecorder::new(TxStep::new(&db, Commit::Nothing, Ok(not_yet())));
    let raw_calls = raw.calls.clone();

    let delivered = deliver_one(handler, fallback(raw), &[bad_at(1, 41)]).await;

    let Ok(HandlerOutcome::Retry { reason }) = &delivered.result else {
        panic!("unexpected result: {:?}", delivered.result);
    };
    assert_eq!(reason, "not yet");
    assert_eq!(delivered.committed, None);
    assert_eq!(delivered.writes, vec![]);
    assert_eq!(*raw_calls.lock().unwrap(), vec![raw_bad_call(1, vec![41])]);
}

#[tokio::test]
async fn an_empty_tx_delivery_succeeds_without_calling_the_typed_handler() {
    let db = sqlite().await;
    let handler = TypedTxRecorder::new(TxStep::new(&db, Commit::Nothing, Ok(not_yet())));
    let calls = handler.calls.clone();

    let delivered = deliver_one(handler, TxUndecodable::Stall, &[]).await;

    let Ok(HandlerOutcome::Success) = &delivered.result else {
        panic!("unexpected result: {:?}", delivered.result);
    };
    assert_eq!(delivered.committed, None);
    assert_eq!(delivered.writes, vec![]);
    assert_eq!(*calls.lock().unwrap(), vec![]);
}

#[tokio::test]
async fn a_typed_tx_handler_error_passes_through() {
    let db = sqlite().await;
    let handler = TypedTxRecorder::new(TxStep::new(
        &db,
        Commit::Nothing,
        Err(internal("typed handler failed")),
    ));
    let calls = handler.calls.clone();

    let delivered = deliver_one(handler, TxUndecodable::Stall, &[good_at(1, 41)]).await;

    let Err(err) = &delivered.result else {
        panic!("unexpected result: {:?}", delivered.result);
    };
    assert_eq!(err.to_string(), "internal: typed handler failed");
    assert_eq!(delivered.committed, None);
    assert_eq!(delivered.writes, vec![]);
    assert_eq!(*calls.lock().unwrap(), vec![typed_call(1, vec![41])]);
}

#[tokio::test]
async fn a_tx_fallback_error_on_a_single_event_passes_through() {
    let db = sqlite().await;
    let handler = TypedTxRecorder::new(TxStep::new(
        &db,
        Commit::Nothing,
        Ok(HandlerOutcome::Success),
    ));
    let calls = handler.calls.clone();
    let raw = RawTxRecorder::new(TxStep::new(
        &db,
        Commit::Nothing,
        Err(internal("fallback handler failed")),
    ));
    let raw_calls = raw.calls.clone();

    let delivered = deliver_one(handler, fallback(raw), &[bad_at(1, 41)]).await;

    let Err(err) = &delivered.result else {
        panic!("unexpected result: {:?}", delivered.result);
    };
    assert_eq!(err.to_string(), "internal: fallback handler failed");
    assert_eq!(delivered.committed, None);
    assert_eq!(delivered.writes, vec![]);
    assert_eq!(*calls.lock().unwrap(), vec![]);
    assert_eq!(*raw_calls.lock().unwrap(), vec![raw_bad_call(1, vec![41])]);
}

/// The stall retry reason names the type and the decode category, never the data.
#[tokio::test]
async fn the_tx_stall_retry_reason_carries_no_payload_value() {
    let planted = serde_json::json!({ "order_id": "o-1", "quantity": "PLANTED-quantity-7" });
    let message = serde_json::from_value::<OrderPlacedV1>(planted.clone())
        .unwrap_err()
        .to_string();
    assert!(
        message.contains("PLANTED-quantity-7"),
        "serde's message quotes the planted value: {message}"
    );
    let db = sqlite().await;
    let handler = TypedTxRecorder::new(TxStep::new(
        &db,
        Commit::Nothing,
        Ok(HandlerOutcome::Success),
    ));
    let calls = handler.calls.clone();

    let delivered = deliver_one(
        handler,
        TxUndecodable::Stall,
        &[order_event(1, 41, planted)],
    )
    .await;

    let Ok(HandlerOutcome::Retry { reason }) = &delivered.result else {
        panic!("unexpected result: {:?}", delivered.result);
    };
    assert_eq!(reason, DECODE_REASON);
    assert!(
        !reason.contains("PLANTED"),
        "a retry reason quotes the payload: {reason}"
    );
    assert_eq!(delivered.committed, None);
    assert_eq!(delivered.writes, vec![]);
    assert_eq!(*calls.lock().unwrap(), vec![]);
}

// ---------------------------------------------------------------------------
// `TxTypedBatchHandlerAdapter`: a batch, a `TxBatchHandler<P, OM>`.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_empty_tx_batch_succeeds_without_calling_the_batch_handler() {
    let db = sqlite().await;
    let handler = BatchTxRecorder::new(TxStep::new(&db, Commit::Nothing, Ok(not_yet())));
    let calls = handler.calls.clone();

    let delivered = deliver_batch(handler, TxUndecodable::Stall, &[]).await;

    let Ok(HandlerOutcome::Success) = &delivered.result else {
        panic!("unexpected result: {:?}", delivered.result);
    };
    assert_eq!(delivered.committed, None);
    assert_eq!(delivered.writes, vec![]);
    assert_eq!(*calls.lock().unwrap(), vec![]);
}

#[tokio::test]
async fn a_fully_decoded_tx_batch_goes_whole_to_the_handler_with_the_delivered_handle() {
    let db = sqlite().await;
    let handler = BatchTxRecorder::new(TxStep::new(
        &db,
        Commit::Offset(43),
        Ok(HandlerOutcome::Success),
    ));
    let calls = handler.calls.clone();

    let delivered = deliver_batch(handler, TxUndecodable::Stall, &good_batch()).await;

    let Ok(HandlerOutcome::Success) = &delivered.result else {
        panic!("unexpected result: {:?}", delivered.result);
    };
    assert_eq!(delivered.committed, Some(Sequence::assigned(43)));
    assert_eq!(delivered.writes, vec![write(43)]);
    assert_eq!(
        *calls.lock().unwrap(),
        vec![batch_call(&[1, 2, 3], vec![41, 42, 43])]
    );
}

#[tokio::test]
async fn a_fully_decoded_tx_batch_passes_the_handlers_retry_through() {
    let db = sqlite().await;
    let handler = BatchTxRecorder::new(TxStep::new(&db, Commit::Nothing, Ok(not_yet())));
    let calls = handler.calls.clone();

    let delivered = deliver_batch(handler, TxUndecodable::Stall, &good_batch()).await;

    let Ok(HandlerOutcome::Retry { reason }) = &delivered.result else {
        panic!("unexpected result: {:?}", delivered.result);
    };
    assert_eq!(reason, "not yet");
    assert_eq!(delivered.committed, None);
    assert_eq!(
        *calls.lock().unwrap(),
        vec![batch_call(&[1, 2, 3], vec![41, 42, 43])]
    );
}

#[tokio::test]
async fn a_tx_batch_handler_error_on_a_fully_decoded_batch_passes_through() {
    let db = sqlite().await;
    let handler = BatchTxRecorder::new(TxStep::new(
        &db,
        Commit::Nothing,
        Err(internal("batch handler failed")),
    ));
    let calls = handler.calls.clone();

    let delivered = deliver_batch(handler, TxUndecodable::Stall, &good_batch()).await;

    let Err(err) = &delivered.result else {
        panic!("unexpected result: {:?}", delivered.result);
    };
    assert_eq!(err.to_string(), "internal: batch handler failed");
    assert_eq!(delivered.committed, None);
    assert_eq!(delivered.writes, vec![]);
    assert_eq!(
        *calls.lock().unwrap(),
        vec![batch_call(&[1, 2, 3], vec![41, 42, 43])]
    );
}

#[tokio::test]
async fn a_typed_tx_prefix_whose_handler_retries_is_retried_without_the_policy() {
    let db = sqlite().await;
    let handler = BatchTxRecorder::new(TxStep::new(&db, Commit::Nothing, Ok(not_yet())));
    let calls = handler.calls.clone();
    let raw = RawTxRecorder::new(TxStep::new(
        &db,
        Commit::Offset(43),
        Ok(HandlerOutcome::Success),
    ));
    let raw_calls = raw.calls.clone();

    let delivered = deliver_batch(handler, fallback(raw), &mixed_batch()).await;

    let Ok(HandlerOutcome::Retry { reason }) = &delivered.result else {
        panic!("unexpected result: {:?}", delivered.result);
    };
    assert_eq!(reason, "not yet");
    assert_eq!(delivered.committed, None);
    assert_eq!(delivered.writes, vec![]);
    assert_eq!(
        *calls.lock().unwrap(),
        vec![batch_call(&[1, 2], vec![41, 42])]
    );
    assert_eq!(*raw_calls.lock().unwrap(), vec![]);
}

/// The typed handler commits `last` and still answers `Retry`: the `Retry` is returned
/// as it is, and the fallback does not get `bad`.
#[tokio::test]
async fn a_typed_tx_prefix_that_commits_and_retries_is_retried_without_the_policy() {
    let db = sqlite().await;
    let handler = BatchTxRecorder::new(TxStep::new(&db, Commit::Offset(42), Ok(not_yet())));
    let calls = handler.calls.clone();
    let raw = RawTxRecorder::new(TxStep::new(
        &db,
        Commit::Offset(43),
        Ok(HandlerOutcome::Success),
    ));
    let raw_calls = raw.calls.clone();

    let delivered = deliver_batch(handler, fallback(raw), &mixed_batch()).await;

    let Ok(HandlerOutcome::Retry { reason }) = &delivered.result else {
        panic!("unexpected result: {:?}", delivered.result);
    };
    assert_eq!(reason, "not yet");
    assert_eq!(delivered.committed, Some(Sequence::assigned(42)));
    assert_eq!(delivered.writes, vec![write(42)]);
    assert_eq!(
        *calls.lock().unwrap(),
        vec![batch_call(&[1, 2], vec![41, 42])]
    );
    assert_eq!(*raw_calls.lock().unwrap(), vec![]);
}

/// The typed handler committed `e[0]` of `[ok, ok, bad]` only, so the dispatcher
/// advances through `e[0]` only (DC1 partial); the fallback does not get `bad`.
#[tokio::test]
async fn a_typed_tx_prefix_committed_short_of_its_last_event_succeeds_without_the_policy() {
    let db = sqlite().await;
    let handler = BatchTxRecorder::new(TxStep::new(
        &db,
        Commit::Offset(41),
        Ok(HandlerOutcome::Success),
    ));
    let calls = handler.calls.clone();
    let raw = RawTxRecorder::new(TxStep::new(
        &db,
        Commit::Offset(43),
        Ok(HandlerOutcome::Success),
    ));
    let raw_calls = raw.calls.clone();

    let delivered = deliver_batch(handler, fallback(raw), &ok_ok_bad_batch()).await;

    let Ok(HandlerOutcome::Success) = &delivered.result else {
        panic!("unexpected result: {:?}", delivered.result);
    };
    assert_eq!(delivered.committed, Some(Sequence::assigned(41)));
    assert_eq!(delivered.writes, vec![write(41)]);
    assert_eq!(
        *calls.lock().unwrap(),
        vec![batch_call(&[1, 2], vec![41, 42])]
    );
    assert_eq!(*raw_calls.lock().unwrap(), vec![]);
}

/// With no commit, the typed handler's `Success` passes through without the policy,
/// and the dispatcher reports the missing commit as it does for any transactional
/// handler.
#[tokio::test]
async fn a_typed_tx_prefix_that_commits_nothing_succeeds_without_the_policy() {
    let db = sqlite().await;
    let handler = BatchTxRecorder::new(TxStep::new(
        &db,
        Commit::Nothing,
        Ok(HandlerOutcome::Success),
    ));
    let calls = handler.calls.clone();
    let raw = RawTxRecorder::new(TxStep::new(
        &db,
        Commit::Offset(43),
        Ok(HandlerOutcome::Success),
    ));
    let raw_calls = raw.calls.clone();

    let delivered = deliver_batch(handler, fallback(raw), &ok_ok_bad_batch()).await;

    let Ok(HandlerOutcome::Success) = &delivered.result else {
        panic!("unexpected result: {:?}", delivered.result);
    };
    assert_eq!(delivered.committed, None);
    assert_eq!(delivered.writes, vec![]);
    assert_eq!(
        *calls.lock().unwrap(),
        vec![batch_call(&[1, 2], vec![41, 42])]
    );
    assert_eq!(*raw_calls.lock().unwrap(), vec![]);
}

#[tokio::test]
async fn a_tx_stall_route_succeeds_after_a_committed_typed_prefix() {
    let db = sqlite().await;
    let handler = BatchTxRecorder::new(TxStep::new(
        &db,
        Commit::Offset(42),
        Ok(HandlerOutcome::Success),
    ));
    let calls = handler.calls.clone();

    let delivered = deliver_batch(handler, TxUndecodable::Stall, &mixed_batch()).await;

    let Ok(HandlerOutcome::Success) = &delivered.result else {
        panic!("unexpected result: {:?}", delivered.result);
    };
    assert_eq!(delivered.committed, Some(Sequence::assigned(42)));
    assert_eq!(delivered.writes, vec![write(42)]);
    assert_eq!(
        *calls.lock().unwrap(),
        vec![batch_call(&[1, 2], vec![41, 42])]
    );
}

#[tokio::test]
async fn a_tx_stall_route_retries_an_undecodable_first_event_without_the_batch_handler() {
    let db = sqlite().await;
    let handler = BatchTxRecorder::new(TxStep::new(
        &db,
        Commit::Offset(41),
        Ok(HandlerOutcome::Success),
    ));
    let calls = handler.calls.clone();

    let delivered = deliver_batch(handler, TxUndecodable::Stall, &bad_first_batch()).await;

    let Ok(HandlerOutcome::Retry { reason }) = &delivered.result else {
        panic!("unexpected result: {:?}", delivered.result);
    };
    assert_eq!(reason, DECODE_REASON);
    assert_eq!(delivered.committed, None);
    assert_eq!(delivered.writes, vec![]);
    assert_eq!(*calls.lock().unwrap(), vec![]);
}

#[tokio::test]
async fn a_tx_fallback_after_a_committed_typed_prefix_gets_the_undecodable_event_with_its_own_handle()
 {
    let db = sqlite().await;
    let handler = BatchTxRecorder::new(TxStep::new(
        &db,
        Commit::Offset(42),
        Ok(HandlerOutcome::Success),
    ));
    let calls = handler.calls.clone();
    let raw = RawTxRecorder::new(TxStep::new(
        &db,
        Commit::Offset(43),
        Ok(HandlerOutcome::Success),
    ));
    let raw_calls = raw.calls.clone();

    let delivered = deliver_batch(handler, fallback(raw), &mixed_batch()).await;

    let Ok(HandlerOutcome::Success) = &delivered.result else {
        panic!("unexpected result: {:?}", delivered.result);
    };
    assert_eq!(delivered.committed, Some(Sequence::assigned(43)));
    assert_eq!(delivered.writes, vec![write(42), write(43)]);
    assert_eq!(
        *calls.lock().unwrap(),
        vec![batch_call(&[1, 2], vec![41, 42])]
    );
    assert_eq!(*raw_calls.lock().unwrap(), vec![raw_bad_call(3, vec![43])]);
}

#[tokio::test]
async fn a_tx_fallback_that_retries_after_a_committed_typed_prefix_succeeds() {
    let db = sqlite().await;
    let handler = BatchTxRecorder::new(TxStep::new(
        &db,
        Commit::Offset(42),
        Ok(HandlerOutcome::Success),
    ));
    let raw = RawTxRecorder::new(TxStep::new(&db, Commit::Nothing, Ok(not_yet())));
    let raw_calls = raw.calls.clone();

    let delivered = deliver_batch(handler, fallback(raw), &mixed_batch()).await;

    let Ok(HandlerOutcome::Success) = &delivered.result else {
        panic!("unexpected result: {:?}", delivered.result);
    };
    assert_eq!(delivered.committed, Some(Sequence::assigned(42)));
    assert_eq!(delivered.writes, vec![write(42)]);
    assert_eq!(*raw_calls.lock().unwrap(), vec![raw_bad_call(3, vec![43])]);
}

#[tokio::test]
async fn a_tx_fallback_gets_an_undecodable_first_event_with_its_own_handle() {
    let db = sqlite().await;
    let handler = BatchTxRecorder::new(TxStep::new(
        &db,
        Commit::Nothing,
        Ok(HandlerOutcome::Success),
    ));
    let calls = handler.calls.clone();
    let raw = RawTxRecorder::new(TxStep::new(
        &db,
        Commit::Offset(41),
        Ok(HandlerOutcome::Success),
    ));
    let raw_calls = raw.calls.clone();

    let delivered = deliver_batch(handler, fallback(raw), &bad_first_batch()).await;

    let Ok(HandlerOutcome::Success) = &delivered.result else {
        panic!("unexpected result: {:?}", delivered.result);
    };
    assert_eq!(delivered.committed, Some(Sequence::assigned(41)));
    assert_eq!(delivered.writes, vec![write(41)]);
    assert_eq!(*calls.lock().unwrap(), vec![]);
    assert_eq!(*raw_calls.lock().unwrap(), vec![raw_bad_call(1, vec![41])]);
}

#[tokio::test]
async fn a_tx_fallback_that_retries_an_undecodable_first_event_is_retried() {
    let db = sqlite().await;
    let handler = BatchTxRecorder::new(TxStep::new(
        &db,
        Commit::Nothing,
        Ok(HandlerOutcome::Success),
    ));
    let raw = RawTxRecorder::new(TxStep::new(&db, Commit::Nothing, Ok(not_yet())));
    let raw_calls = raw.calls.clone();

    let delivered = deliver_batch(handler, fallback(raw), &bad_first_batch()).await;

    let Ok(HandlerOutcome::Retry { reason }) = &delivered.result else {
        panic!("unexpected result: {:?}", delivered.result);
    };
    assert_eq!(reason, "not yet");
    assert_eq!(delivered.committed, None);
    assert_eq!(delivered.writes, vec![]);
    assert_eq!(*raw_calls.lock().unwrap(), vec![raw_bad_call(1, vec![41])]);
}

#[tokio::test]
async fn a_typed_tx_prefix_handler_error_passes_through_without_the_policy() {
    let db = sqlite().await;
    let handler = BatchTxRecorder::new(TxStep::new(
        &db,
        Commit::Nothing,
        Err(internal("batch handler failed")),
    ));
    let calls = handler.calls.clone();
    let raw = RawTxRecorder::new(TxStep::new(
        &db,
        Commit::Offset(43),
        Ok(HandlerOutcome::Success),
    ));
    let raw_calls = raw.calls.clone();

    let delivered = deliver_batch(handler, fallback(raw), &mixed_batch()).await;

    let Err(err) = &delivered.result else {
        panic!("unexpected result: {:?}", delivered.result);
    };
    assert_eq!(err.to_string(), "internal: batch handler failed");
    assert_eq!(delivered.committed, None);
    assert_eq!(delivered.writes, vec![]);
    assert_eq!(
        *calls.lock().unwrap(),
        vec![batch_call(&[1, 2], vec![41, 42])]
    );
    assert_eq!(*raw_calls.lock().unwrap(), vec![]);
}

/// The prefix is committed in its own transaction and, while the consumer owns the
/// partition, is not handed to the handler again, so a fallback error after it is
/// logged and succeeds; the dispatcher advances through the prefix.
#[tokio::test]
async fn a_tx_fallback_error_after_a_committed_typed_prefix_is_logged_and_succeeds() {
    let capture = WarnCapture::install();
    let db = sqlite().await;
    let handler = BatchTxRecorder::new(TxStep::new(
        &db,
        Commit::Offset(42),
        Ok(HandlerOutcome::Success),
    ));
    let raw = RawTxRecorder::new(TxStep::new(
        &db,
        Commit::Nothing,
        Err(internal("fallback handler failed")),
    ));
    let raw_calls = raw.calls.clone();

    let delivered = deliver_batch(handler, fallback(raw), &mixed_batch()).await;

    let Ok(HandlerOutcome::Success) = &delivered.result else {
        panic!("unexpected result: {:?}", delivered.result);
    };
    assert_eq!(delivered.committed, Some(Sequence::assigned(42)));
    assert_eq!(delivered.writes, vec![write(42)]);
    assert_eq!(*raw_calls.lock().unwrap(), vec![raw_bad_call(3, vec![43])]);
    assert_eq!(
        capture.lines(),
        vec![
            "WARN transactional fallback handler returned Err after a committed typed prefix; \
             treating as Success error=internal: fallback handler failed"
                .to_owned()
        ]
    );
}

#[tokio::test]
async fn a_tx_fallback_error_on_an_undecodable_first_event_passes_through() {
    let capture = WarnCapture::install();
    let db = sqlite().await;
    let handler = BatchTxRecorder::new(TxStep::new(
        &db,
        Commit::Nothing,
        Ok(HandlerOutcome::Success),
    ));
    let calls = handler.calls.clone();
    let raw = RawTxRecorder::new(TxStep::new(
        &db,
        Commit::Nothing,
        Err(internal("fallback handler failed")),
    ));
    let raw_calls = raw.calls.clone();

    let delivered = deliver_batch(handler, fallback(raw), &bad_first_batch()).await;

    let Err(err) = &delivered.result else {
        panic!("unexpected result: {:?}", delivered.result);
    };
    assert_eq!(err.to_string(), "internal: fallback handler failed");
    assert_eq!(delivered.committed, None);
    assert_eq!(delivered.writes, vec![]);
    assert_eq!(*calls.lock().unwrap(), vec![]);
    assert_eq!(*raw_calls.lock().unwrap(), vec![raw_bad_call(1, vec![41])]);
    assert_eq!(capture.lines(), Vec::<String>::new());
}

/// The typed prefix's handle covers the prefix only, so the undecodable event's
/// offset is refused there and nothing is written.
#[tokio::test]
async fn the_typed_tx_prefix_handle_refuses_the_undecodable_events_offset() {
    let db = sqlite().await;
    let step = TxStep::new(&db, Commit::Offset(43), Ok(HandlerOutcome::Success));
    let commit_errors = step.commit_errors.clone();
    let handler = BatchTxRecorder::new(step);

    let delivered = deliver_batch(handler, TxUndecodable::Stall, &mixed_batch()).await;

    let Ok(HandlerOutcome::Success) = &delivered.result else {
        panic!("unexpected result: {:?}", delivered.result);
    };
    assert_eq!(
        *commit_errors.lock().unwrap(),
        vec![
            "invalid consumer options: committed offset 43 is not present in delivered batch \
             offsets [Sequence(41), Sequence(42)]"
                .to_owned()
        ]
    );
    assert_eq!(delivered.committed, None);
    assert_eq!(delivered.writes, vec![]);
}

/// The fallback's handle covers the undecodable event only, so a prefix offset is
/// refused there.
#[tokio::test]
async fn the_tx_fallback_handle_refuses_a_typed_prefix_offset() {
    let db = sqlite().await;
    let handler = BatchTxRecorder::new(TxStep::new(
        &db,
        Commit::Offset(42),
        Ok(HandlerOutcome::Success),
    ));
    let step = TxStep::new(&db, Commit::Offset(41), Ok(HandlerOutcome::Success));
    let commit_errors = step.commit_errors.clone();
    let raw = RawTxRecorder::new(step);

    let delivered = deliver_batch(handler, fallback(raw), &mixed_batch()).await;

    let Ok(HandlerOutcome::Success) = &delivered.result else {
        panic!("unexpected result: {:?}", delivered.result);
    };
    assert_eq!(
        *commit_errors.lock().unwrap(),
        vec![
            "invalid consumer options: committed offset 41 is not present in delivered batch \
             offsets [Sequence(43)]"
                .to_owned()
        ]
    );
    assert_eq!(delivered.committed, Some(Sequence::assigned(42)));
    assert_eq!(delivered.writes, vec![write(42)]);
}

/// `[ok, bad, ok]`: decoding stops at the bad event, so the good event after it joins
/// neither the typed batch nor its handle.
#[tokio::test]
async fn the_tx_batch_handler_gets_only_the_prefix_before_the_first_undecodable_event() {
    let db = sqlite().await;
    let handler = BatchTxRecorder::new(TxStep::new(
        &db,
        Commit::Offset(41),
        Ok(HandlerOutcome::Success),
    ));
    let calls = handler.calls.clone();

    let delivered = deliver_batch(
        handler,
        TxUndecodable::Stall,
        &[good_at(1, 41), bad_at(2, 42), good_at(3, 43)],
    )
    .await;

    let Ok(HandlerOutcome::Success) = &delivered.result else {
        panic!("unexpected result: {:?}", delivered.result);
    };
    assert_eq!(delivered.committed, Some(Sequence::assigned(41)));
    assert_eq!(delivered.writes, vec![write(41)]);
    assert_eq!(*calls.lock().unwrap(), vec![batch_call(&[1], vec![41])]);
}

/// Poisons the commit state its handle shares with the adapter, then succeeds.
struct PoisoningTxBatchHandler;

#[async_trait::async_trait]
impl TxBatchHandler<OrderPlacedV1, Manager> for PoisoningTxBatchHandler {
    async fn handle_batch(
        &self,
        _batch: &EventBatch<'_, OrderPlacedV1>,
        _delivery: &Delivery,
        commit: TxCommitHandle<Manager>,
    ) -> Result<HandlerOutcome, ConsumerError> {
        let state = commit.committed_offset.clone();
        let _ = std::thread::spawn(move || {
            let _guard = state.lock().unwrap();
            panic!("poisons the commit state");
        })
        .join();
        Ok(HandlerOutcome::Success)
    }
}

#[tokio::test]
async fn a_poisoned_commit_state_after_a_typed_tx_prefix_is_an_internal_error() {
    let adapter = TxTypedBatchHandlerAdapter::<_, OrderPlacedV1, Manager>::new(
        PoisoningTxBatchHandler,
        TxUndecodable::Stall,
    );

    let delivered = deliver_through(&adapter, Arc::new(Manager::default()), &mixed_batch()).await;

    let Err(err) = &delivered.result else {
        panic!("unexpected result: {:?}", delivered.result);
    };
    assert_eq!(err.to_string(), "internal: tx commit state mutex poisoned");
}

/// The stall retry reason of an undecodable first event names the type and the decode
/// category, never the data.
#[tokio::test]
async fn the_tx_batch_stall_retry_reason_carries_no_payload_value() {
    let planted = serde_json::json!({ "order_id": "o-1", "quantity": "PLANTED-quantity-7" });
    let message = serde_json::from_value::<OrderPlacedV1>(planted.clone())
        .unwrap_err()
        .to_string();
    assert!(
        message.contains("PLANTED-quantity-7"),
        "serde's message quotes the planted value: {message}"
    );
    let db = sqlite().await;
    let handler = BatchTxRecorder::new(TxStep::new(
        &db,
        Commit::Nothing,
        Ok(HandlerOutcome::Success),
    ));
    let calls = handler.calls.clone();

    let delivered = deliver_batch(
        handler,
        TxUndecodable::Stall,
        &[order_event(1, 41, planted), good_at(2, 42)],
    )
    .await;

    let Ok(HandlerOutcome::Retry { reason }) = &delivered.result else {
        panic!("unexpected result: {:?}", delivered.result);
    };
    assert_eq!(reason, DECODE_REASON);
    assert!(
        !reason.contains("PLANTED"),
        "a retry reason quotes the payload: {reason}"
    );
    assert_eq!(delivered.committed, None);
    assert_eq!(delivered.writes, vec![]);
    assert_eq!(*calls.lock().unwrap(), vec![]);
}

// ---------------------------------------------------------------------------
// The route builder.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn tx_undecodable_prints_its_variant_name_only() {
    let db = sqlite().await;
    let fallback = fallback(RawTxRecorder::new(TxStep::new(
        &db,
        Commit::Nothing,
        Ok(HandlerOutcome::Success),
    )));

    assert_eq!(format!("{:?}", TxUndecodable::<Manager>::Stall), "Stall");
    assert_eq!(format!("{fallback:?}"), "Fallback");
}

#[test]
fn a_tx_typed_route_builder_prints_its_topic_type_and_policy() {
    let route = ConsumerBuilder::new_unbound()
        .group(ConsumerGroupRef::auto_anonymous("tx-typed-route-debug"))
        .topics([ginst(TOPIC)])
        .offset_manager(Manager::default())
        .route()
        .topic(ginst(TOPIC))
        .event::<OrderPlacedV1>(TxUndecodable::Stall);

    assert_eq!(
        format!("{route:?}"),
        "TxTypedRouteBuilder { \
         topic: \"gts.cf.core.events.topic.v1~example.orders.x.x.v1\", \
         event_type: \"gts.cf.core.events.event.v1~example.orders.order_placed.x.v1~\", \
         undecodable: Stall }"
    );
}

/// A typed handler that is never called: the route tests only register it.
struct IdleTypedTxHandler;

#[async_trait::async_trait]
impl TxEventHandler<OrderPlacedV1, Manager> for IdleTypedTxHandler {
    async fn handle(
        &self,
        _event: EventV1<OrderPlacedV1>,
        _delivery: &Delivery,
        _commit: TxCommitHandle<Manager>,
    ) -> Result<HandlerOutcome, ConsumerError> {
        Ok(HandlerOutcome::Success)
    }
}

#[async_trait::async_trait]
impl TxBatchHandler<OrderPlacedV1, Manager> for IdleTypedTxHandler {
    async fn handle_batch(
        &self,
        _batch: &EventBatch<'_, OrderPlacedV1>,
        _delivery: &Delivery,
        _commit: TxCommitHandle<Manager>,
    ) -> Result<HandlerOutcome, ConsumerError> {
        Ok(HandlerOutcome::Success)
    }
}

#[test]
fn a_tx_typed_route_replaces_an_earlier_event_type_with_its_payload_type() {
    let ready = ConsumerBuilder::new_unbound()
        .group(ConsumerGroupRef::auto_anonymous("tx-typed-route-pattern"))
        .topics([ginst(TOPIC)])
        .offset_manager(Manager::default())
        .route()
        .topic(ginst(TOPIC))
        .event_type(gpat(ORDER_CREATED))
        .event::<OrderPlacedV1>(TxUndecodable::Stall)
        .handler(IdleTypedTxHandler);

    assert_eq!(
        ready.routes,
        vec![ConsumerRoute {
            topic: ginst(TOPIC),
            event_type: Some(gpat(ORDER_PLACED)),
            handler_kind: ConsumerRouteHandlerKind::Single,
        }]
    );
    assert_eq!(ready.invalid_routes, Vec::<String>::new());
}

#[test]
fn a_tx_typed_batch_route_registers_a_batch_route_for_its_payload_type() {
    let ready = ConsumerBuilder::new_unbound()
        .group(ConsumerGroupRef::auto_anonymous(
            "tx-typed-batch-route-pattern",
        ))
        .topics([ginst(TOPIC)])
        .offset_manager(Manager::default())
        .route()
        .topic(ginst(TOPIC))
        .event_type(gpat(ORDER_CREATED))
        .event::<OrderPlacedV1>(TxUndecodable::Stall)
        .batch_handler(IdleTypedTxHandler);

    assert_eq!(
        ready.routes,
        vec![ConsumerRoute {
            topic: ginst(TOPIC),
            event_type: Some(gpat(ORDER_PLACED)),
            handler_kind: ConsumerRouteHandlerKind::Batch,
        }]
    );
    assert_eq!(ready.invalid_routes, Vec::<String>::new());
}

/// An untyped transactional handler on `serde_json::Value`, whose
/// `GtsSchema::TYPE_ID` is empty.
struct ValueTxHandler;

#[async_trait::async_trait]
impl TxEventHandler<serde_json::Value, Manager> for ValueTxHandler {
    async fn handle(
        &self,
        _event: EventV1,
        _delivery: &Delivery,
        _commit: TxCommitHandle<Manager>,
    ) -> Result<HandlerOutcome, ConsumerError> {
        Ok(HandlerOutcome::Success)
    }
}

#[async_trait::async_trait]
impl TxBatchHandler<serde_json::Value, Manager> for ValueTxHandler {
    async fn handle_batch(
        &self,
        _batch: &EventBatch<'_, serde_json::Value>,
        _delivery: &Delivery,
        _commit: TxCommitHandle<Manager>,
    ) -> Result<HandlerOutcome, ConsumerError> {
        Ok(HandlerOutcome::Success)
    }
}

/// A payload type whose id is no GTS type id cannot set a transactional route's
/// type, so `start()` refuses the consumer before it calls the broker.
#[tokio::test]
async fn a_tx_typed_route_whose_payload_type_id_is_not_a_gts_id_fails_start() {
    let ready = ConsumerBuilder::new_unbound()
        .group(ConsumerGroupRef::auto_anonymous("tx-typed-route-invalid"))
        .topics([ginst(TOPIC)])
        .offset_manager(Manager::default())
        .default_handler(ValueTxHandler)
        .route()
        .topic(ginst(TOPIC))
        .event::<serde_json::Value>(TxUndecodable::Stall)
        .handler(ValueTxHandler);

    let err = match ready.start().await {
        Ok(_) => panic!("a typed route without a GTS type id must fail start"),
        Err(err) => err,
    };

    assert!(
        matches!(err, EventBrokerError::InvalidConsumerOptions { .. }),
        "unexpected error: {err}"
    );
    assert_eq!(
        err.to_string(),
        "invalid consumer options: typed route payload type id \"\" is not a GTS type id: \
         Invalid GTS identifier: : must start with 'gts.'"
    );
}

/// As for a typed single-event route, a batch route of such a payload type is refused
/// at `start()`.
#[tokio::test]
async fn a_tx_typed_batch_route_whose_payload_type_id_is_not_a_gts_id_fails_start() {
    let ready = ConsumerBuilder::new_unbound()
        .group(ConsumerGroupRef::auto_anonymous(
            "tx-typed-batch-route-invalid",
        ))
        .topics([ginst(TOPIC)])
        .offset_manager(Manager::default())
        .default_handler(ValueTxHandler)
        .route()
        .topic(ginst(TOPIC))
        .event::<serde_json::Value>(TxUndecodable::Stall)
        .batch_handler(ValueTxHandler);

    let err = match ready.start().await {
        Ok(_) => panic!("a typed batch route without a GTS type id must fail start"),
        Err(err) => err,
    };

    assert!(
        matches!(err, EventBrokerError::InvalidConsumerOptions { .. }),
        "unexpected error: {err}"
    );
    assert_eq!(
        err.to_string(),
        "invalid consumer options: typed route payload type id \"\" is not a GTS type id: \
         Invalid GTS identifier: : must start with 'gts.'"
    );
}
