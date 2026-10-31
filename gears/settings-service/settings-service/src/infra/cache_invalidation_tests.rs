#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use event_broker_sdk::models::EventType;
use event_broker_sdk::{
    AssignedPartition, ConsumerGroup, ConsumerGroupId, ConsumerGroupKind, ConsumerGroupQuery,
    ControlCode, CreateConsumerGroupRequest, Event, EventBrokerApi, EventBrokerError, FrameStream,
    GtsInstanceId, GtsTypeId, IngestOutcome, JoinRequest, Page, PartitionRange, Position,
    ProducerCursors, ProducerId, ProducerMode, ResetScope, SeekPosition, SeekResult, Sequence,
    Subscription, SubscriptionAssignment, SubscriptionId, Topic, TopicSegment, WireEvent,
    WireFrame,
};
use serde_json::{Value, json};
use settings_service_sdk::gts::{
    ACTIVATION_SUBJECT_TYPE, CACHE_INVALIDATE_EVENT_TYPE, CACHE_TOPIC_ID,
};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio_util::sync::CancellationToken;
use toolkit::ClientHub;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;
use types_registry_sdk::RegisterResult;
use uuid::Uuid;

use super::CacheInvalidationConsumer;
use crate::domain::activation::NotifyMetrics;
use crate::domain::activation::testing::RecordingNotifyMetrics;
use crate::domain::platform_scope::PlatformScope;
use crate::domain::resolution::EffectiveCache;
use crate::domain::resolution::cache::tests_entry;
use crate::infra::event_broker::{Backoff, CacheSchemaRegistry, EventBrokerClient};

const ROOT: Uuid = Uuid::from_u128(0xa0);

fn tenant(n: u128) -> Uuid {
    Uuid::from_u128(n)
}

// ---------------------------------------------------------------------------
// Doubles
// ---------------------------------------------------------------------------

type FrameResult = Result<WireFrame, EventBrokerError>;

/// A broker that records what the consumer does to its groups and hands out one
/// scripted stream per join.
#[derive(Default)]
struct ScriptedBroker {
    create_refusals: AtomicUsize,
    join_refusals: AtomicUsize,
    delete_fails: Mutex<bool>,
    /// `create_consumer_group` and `join` never answer.
    create_hangs: AtomicBool,
    join_hangs: AtomicBool,
    /// `leave` never answers.
    leave_hangs: AtomicBool,
    /// Every stream ends as soon as it is opened.
    streams_end_at_once: AtomicBool,
    create_calls: Mutex<Vec<std::time::Instant>>,
    creates: Mutex<Vec<ConsumerGroupId>>,
    joins: Mutex<Vec<JoinRequest>>,
    seeks: Mutex<Vec<Vec<SeekPosition>>>,
    leaves: Mutex<Vec<SubscriptionId>>,
    deletes: Mutex<Vec<ConsumerGroupId>>,
    streams: Mutex<Vec<UnboundedSender<FrameResult>>>,
    next_stream: Mutex<Option<UnboundedReceiver<FrameResult>>>,
}

fn refused() -> EventBrokerError {
    EventBrokerError::Internal("refused by the scripted broker".to_owned())
}

fn unsupported() -> EventBrokerError {
    EventBrokerError::Internal("not supported by the scripted broker".to_owned())
}

fn spend(counter: &AtomicUsize) -> bool {
    counter
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
        .is_ok()
}

impl ScriptedBroker {
    fn creates(&self) -> usize {
        self.creates.lock().unwrap().len()
    }
    fn deletes(&self) -> Vec<ConsumerGroupId> {
        self.deletes.lock().unwrap().clone()
    }
    fn opened_streams(&self) -> usize {
        self.streams.lock().unwrap().len()
    }
    /// Deliver a frame on the most recent stream.
    fn send(&self, frame: FrameResult) {
        let streams = self.streams.lock().unwrap();
        streams.last().expect("a stream is open").send(frame).ok();
    }
    /// End the most recent stream.
    fn end_stream(&self) {
        self.streams.lock().unwrap().pop();
    }
}

#[async_trait]
impl EventBrokerApi for ScriptedBroker {
    async fn register_producer(
        &self,
        _: &SecurityContext,
        _: ProducerMode,
        _: &str,
    ) -> Result<ProducerId, EventBrokerError> {
        Err(unsupported())
    }
    async fn publish(
        &self,
        _: &SecurityContext,
        _: &Event,
    ) -> Result<IngestOutcome, EventBrokerError> {
        Err(unsupported())
    }
    async fn publish_batch(
        &self,
        _: &SecurityContext,
        _: &[Event],
    ) -> Result<IngestOutcome, EventBrokerError> {
        Err(unsupported())
    }
    async fn get_producer_cursors(
        &self,
        _: &SecurityContext,
        _: ProducerId,
    ) -> Result<ProducerCursors, EventBrokerError> {
        Err(unsupported())
    }
    async fn reset_producer_chain(
        &self,
        _: &SecurityContext,
        _: ProducerId,
        _: ResetScope<'_>,
    ) -> Result<(), EventBrokerError> {
        Err(unsupported())
    }
    async fn create_consumer_group(
        &self,
        _: &SecurityContext,
        req: CreateConsumerGroupRequest,
    ) -> Result<ConsumerGroup, EventBrokerError> {
        assert!(req.client_agent.starts_with("settings-service:"));
        self.create_calls
            .lock()
            .unwrap()
            .push(std::time::Instant::now());
        if self.create_hangs.load(Ordering::SeqCst) {
            std::future::pending::<()>().await;
        }
        if spend(&self.create_refusals) {
            return Err(refused());
        }
        let id = ConsumerGroupId::new(Uuid::new_v4());
        self.creates.lock().unwrap().push(id);
        Ok(ConsumerGroup {
            id,
            tenant_id: ROOT,
            owner_principal_id: Uuid::nil(),
            kind: ConsumerGroupKind::Anonymous,
            description: None,
            created_at: Utc::now(),
        })
    }
    async fn get_consumer_group(
        &self,
        _: &SecurityContext,
        _: &ConsumerGroupId,
    ) -> Result<ConsumerGroup, EventBrokerError> {
        Err(unsupported())
    }
    async fn list_consumer_groups(
        &self,
        _: &SecurityContext,
        _: ConsumerGroupQuery,
    ) -> Result<Page<ConsumerGroup>, EventBrokerError> {
        Err(unsupported())
    }
    async fn delete_consumer_group(
        &self,
        _: &SecurityContext,
        id: &ConsumerGroupId,
    ) -> Result<(), EventBrokerError> {
        self.deletes.lock().unwrap().push(*id);
        if *self.delete_fails.lock().unwrap() {
            return Err(refused());
        }
        Ok(())
    }
    async fn join(
        &self,
        _: &SecurityContext,
        req: JoinRequest,
    ) -> Result<SubscriptionAssignment, EventBrokerError> {
        self.joins.lock().unwrap().push(req);
        if self.join_hangs.load(Ordering::SeqCst) {
            std::future::pending::<()>().await;
        }
        if spend(&self.join_refusals) {
            return Err(refused());
        }
        let (tx, rx) = unbounded_channel();
        self.streams.lock().unwrap().push(tx);
        *self.next_stream.lock().unwrap() = Some(rx);
        Ok(SubscriptionAssignment {
            subscription_id: SubscriptionId(Uuid::new_v4()),
            topology_version: 7,
            assigned: vec![AssignedPartition {
                topic: GtsInstanceId::try_new(CACHE_TOPIC_ID).unwrap(),
                partition: 0,
            }],
        })
    }
    async fn get_subscription(
        &self,
        _: &SecurityContext,
        _: SubscriptionId,
    ) -> Result<Subscription, EventBrokerError> {
        Err(unsupported())
    }
    async fn list_subscriptions(
        &self,
        _: &SecurityContext,
    ) -> Result<Vec<Subscription>, EventBrokerError> {
        Err(unsupported())
    }
    async fn leave(&self, _: &SecurityContext, id: SubscriptionId) -> Result<(), EventBrokerError> {
        self.leaves.lock().unwrap().push(id);
        if self.leave_hangs.load(Ordering::SeqCst) {
            std::future::pending::<()>().await;
        }
        Ok(())
    }
    async fn stream(
        &self,
        _: &SecurityContext,
        _: SubscriptionId,
    ) -> Result<FrameStream, EventBrokerError> {
        if self.streams_end_at_once.load(Ordering::SeqCst) {
            return Ok(Box::pin(futures_util::stream::empty()));
        }
        let rx = self
            .next_stream
            .lock()
            .unwrap()
            .take()
            .ok_or_else(unsupported)?;
        Ok(Box::pin(futures_util::stream::unfold(
            rx,
            |mut rx| async move { rx.recv().await.map(|frame| (frame, rx)) },
        )))
    }
    async fn seek(
        &self,
        _: &SecurityContext,
        _: SubscriptionId,
        topology_version: i64,
        positions: &[SeekPosition],
    ) -> Result<Vec<SeekResult>, EventBrokerError> {
        assert_eq!(topology_version, 7);
        self.seeks.lock().unwrap().push(positions.to_vec());
        Ok(Vec::new())
    }
    async fn list_topics(&self, _: &SecurityContext) -> Result<Vec<Topic>, EventBrokerError> {
        Err(unsupported())
    }
    async fn list_topic_segments(
        &self,
        _: &SecurityContext,
        _: &str,
        _: u32,
        _: PartitionRange,
    ) -> Result<TopicSegment, EventBrokerError> {
        Err(unsupported())
    }
    async fn list_event_types(
        &self,
        _: &SecurityContext,
    ) -> Result<Vec<EventType>, EventBrokerError> {
        Err(unsupported())
    }
    async fn get_event_type(
        &self,
        _: &SecurityContext,
        _: &str,
    ) -> Result<EventType, EventBrokerError> {
        Err(unsupported())
    }
}

/// A registry that accepts, or refuses every document.
struct Registry {
    refuse: bool,
}

impl Registry {
    fn answer(&self) -> Vec<RegisterResult> {
        if self.refuse {
            vec![RegisterResult::Err {
                gts_id: None,
                error: CanonicalError::from(crate::domain::error::DomainError::Internal {
                    diagnostic: "registry says no".to_owned(),
                }),
            }]
        } else {
            vec![RegisterResult::Ok {
                gts_id: "gts.x".to_owned(),
            }]
        }
    }
}

#[async_trait]
impl CacheSchemaRegistry for Registry {
    async fn register_topic(&self, _: Value) -> Result<Vec<RegisterResult>, CanonicalError> {
        Ok(self.answer())
    }
    async fn register_event_type(&self, _: Value) -> Result<Vec<RegisterResult>, CanonicalError> {
        Ok(self.answer())
    }
}

struct Scope;

#[async_trait]
impl PlatformScope for Scope {
    async fn root_tenant(&self) -> Result<Uuid, crate::domain::error::DomainError> {
        Ok(ROOT)
    }
}

#[derive(Clone, Default)]
struct LogBuf(Arc<Mutex<Vec<u8>>>);

impl LogBuf {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

impl std::io::Write for LogBuf {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogBuf {
    type Writer = Self;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

// ---------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------

struct Fixture {
    hub: Arc<ClientHub>,
    broker: Arc<ScriptedBroker>,
    client: Arc<EventBrokerClient>,
    cache: Arc<EffectiveCache>,
    cancel: CancellationToken,
    logs: LogBuf,
    _guard: tracing::subscriber::DefaultGuard,
    task: Option<tokio::task::JoinHandle<()>>,
}

fn fixture(registry_refuses: bool, plugged: bool) -> Fixture {
    let logs = LogBuf::default();
    let guard = tracing::subscriber::set_default(
        tracing_subscriber::fmt()
            .with_writer(logs.clone())
            .with_ansi(false)
            .finish(),
    );
    let hub = Arc::new(ClientHub::new());
    let broker = Arc::new(ScriptedBroker::default());
    if plugged {
        hub.register::<dyn EventBrokerApi>(Arc::clone(&broker) as Arc<dyn EventBrokerApi>);
    }
    let client = Arc::new(
        EventBrokerClient::new(
            Arc::clone(&hub),
            Arc::new(Registry {
                refuse: registry_refuses,
            }),
            Arc::new(Scope),
            Duration::from_secs(2),
            Uuid::from_u128(0x1d),
        )
        .with_backoff(Backoff::with_steps(
            Duration::from_millis(1),
            Duration::from_millis(4),
        )),
    );
    Fixture {
        hub,
        broker,
        client,
        cache: Arc::new(EffectiveCache::new(Duration::from_secs(30))),
        cancel: CancellationToken::new(),
        logs,
        _guard: guard,
        task: None,
    }
}

impl Fixture {
    fn start(&mut self) {
        let consumer =
            CacheInvalidationConsumer::new(Arc::clone(&self.client), Arc::clone(&self.cache))
                .with_timing(
                    Duration::from_secs(2),
                    Duration::from_millis(5),
                    Duration::from_millis(40),
                );
        let cancel = self.cancel.clone();
        self.task = Some(tokio::spawn(async move { consumer.run(cancel).await }));
    }

    async fn stop(&mut self) {
        self.cancel.cancel();
        tokio::time::timeout(Duration::from_secs(5), self.task.take().expect("started"))
            .await
            .expect("the task ends within the deadline")
            .expect("and does not panic");
    }

    fn plug(&self) {
        self.hub
            .register::<dyn EventBrokerApi>(Arc::clone(&self.broker) as Arc<dyn EventBrokerApi>);
    }
}

async fn until(what: &str, mut done: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !done() {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
}

fn wire(type_id: &str, data: Value) -> WireFrame {
    WireFrame::Event(WireEvent {
        id: Uuid::new_v4(),
        type_id: GtsTypeId::new(type_id),
        tenant_id: ROOT,
        subject: tenant(1).to_string(),
        subject_type: GtsTypeId::new(ACTIVATION_SUBJECT_TYPE),
        partition: 0,
        sequence: Sequence::assigned(1),
        occurred_at: Utc::now(),
        sequence_time: Utc::now(),
        trace_parent: None,
        data,
    })
}

fn invalidation(keys: &[&str]) -> Value {
    json!({
        "change_set_id": Uuid::from_u128(0xc5),
        "tenant": tenant(1),
        "changed_keys": keys,
    })
}

// ---------------------------------------------------------------------------
// Rows
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_refused_join_is_retried_on_the_one_group_and_leaks_none() {
    let mut f = fixture(false, true);
    f.broker.join_refusals.store(3, Ordering::SeqCst);
    f.start();
    until("the stream to open", || f.broker.opened_streams() == 1).await;

    assert_eq!(f.broker.creates(), 1, "one group however many joins failed");
    let joins = f.broker.joins.lock().unwrap().clone();
    assert_eq!(joins.len(), 4, "three refusals and the join that held");
    let group = f.broker.creates.lock().unwrap()[0];
    assert!(
        joins.iter().all(|j| j.group == group),
        "always the same group"
    );
    assert!(f.logs.text().contains("join refused"), "{}", f.logs.text());

    f.stop().await;
    assert_eq!(
        f.broker.deletes(),
        vec![group],
        "and that group is deleted at the end"
    );
}

#[tokio::test]
async fn a_refused_group_creation_is_retried_until_one_group_is_held() {
    let mut f = fixture(false, true);
    f.broker.create_refusals.store(2, Ordering::SeqCst);
    f.start();
    until("the stream to open", || f.broker.opened_streams() == 1).await;
    assert_eq!(f.broker.creates(), 1);
    assert!(f.logs.text().contains("consumer group refused"));
    f.stop().await;
}

#[tokio::test]
async fn the_join_carries_one_interest_for_the_type_under_the_root_and_seeks_the_latest() {
    let mut f = fixture(false, true);
    f.start();
    until("the stream to open", || f.broker.opened_streams() == 1).await;

    let joins = f.broker.joins.lock().unwrap().clone();
    let [interest] = joins[0].interests.as_slice() else {
        panic!("exactly one interest");
    };
    assert_eq!(interest.topic().as_ref(), CACHE_TOPIC_ID);
    assert_eq!(interest.tenant_id(), ROOT);
    let types: Vec<String> = interest.types().iter().map(ToString::to_string).collect();
    assert_eq!(types, vec![CACHE_INVALIDATE_EVENT_TYPE.to_owned()]);
    let seeks = f.broker.seeks.lock().unwrap().clone();
    assert_eq!(seeks.len(), 1);
    assert!(
        seeks[0].iter().all(|s| matches!(s.value, Position::Latest)),
        "offsets start from the latest position"
    );
    f.stop().await;
}

#[tokio::test]
async fn a_cache_invalidate_delivery_evicts_the_named_keys_key_wide() {
    let mut f = fixture(false, true);
    for (key, t) in [("k.one", 2), ("k.one", 3), ("k.two", 2), ("k.keep", 2)] {
        f.cache.seed(Arc::new(tests_entry(key, tenant(t))));
    }
    f.start();
    until("the stream to open", || f.broker.opened_streams() == 1).await;

    f.broker.send(Ok(wire(
        CACHE_INVALIDATE_EVENT_TYPE,
        invalidation(&["k.one", "k.two"]),
    )));
    until("the eviction", || f.cache.get("k.two", tenant(2)).is_none()).await;

    assert!(f.cache.get("k.one", tenant(2)).is_none());
    assert!(
        f.cache.get("k.one", tenant(3)).is_none(),
        "every scope of the key"
    );
    assert!(
        f.cache.get("k.keep", tenant(2)).is_some(),
        "an unnamed key stays"
    );
    f.stop().await;
}

#[tokio::test]
async fn another_type_and_a_malformed_body_evict_nothing_and_the_consumer_keeps_running() {
    let mut f = fixture(false, true);
    f.cache.seed(Arc::new(tests_entry("k.one", tenant(2))));
    f.cache.seed(Arc::new(tests_entry("k.last", tenant(2))));
    f.start();
    until("the stream to open", || f.broker.opened_streams() == 1).await;

    f.broker.send(Ok(wire(
        "gts.cf.core.events.event.v1~cf.core.other.thing.v1~",
        invalidation(&["k.one"]),
    )));
    f.broker.send(Ok(wire(
        CACHE_INVALIDATE_EVENT_TYPE,
        json!({ "tenant": "not-a-uuid", "changed_keys": ["k.one"] }),
    )));
    f.broker.send(Ok(WireFrame::Heartbeat {
        at: "now".to_owned(),
    }));
    // Frames are taken in order: once this one has landed, the others were seen.
    f.broker.send(Ok(wire(
        CACHE_INVALIDATE_EVENT_TYPE,
        invalidation(&["k.last"]),
    )));
    until("the last eviction", || {
        f.cache.get("k.last", tenant(2)).is_none()
    })
    .await;

    assert!(
        f.cache.get("k.one", tenant(2)).is_some(),
        "nothing else was evicted"
    );
    assert!(f.logs.text().contains("malformed"), "{}", f.logs.text());
    f.stop().await;
}

#[tokio::test]
async fn stop_leaves_and_deletes_the_group_once_and_logs_start_and_stop() {
    let mut f = fixture(false, true);
    f.start();
    until("the stream to open", || f.broker.opened_streams() == 1).await;
    let group = f.broker.creates.lock().unwrap()[0];

    f.stop().await;

    assert_eq!(f.broker.leaves.lock().unwrap().len(), 1);
    assert_eq!(f.broker.deletes(), vec![group]);
    let logs = f.logs.text();
    assert!(
        logs.contains("cache invalidation consumer started"),
        "{logs}"
    );
    assert!(
        logs.contains("cache invalidation consumer stopped"),
        "{logs}"
    );
}

#[tokio::test]
async fn a_failed_delete_on_stop_warns_and_the_task_still_ends() {
    let mut f = fixture(false, true);
    *f.broker.delete_fails.lock().unwrap() = true;
    f.start();
    until("the stream to open", || f.broker.opened_streams() == 1).await;

    f.stop().await;

    assert_eq!(f.broker.deletes().len(), 1);
    assert!(f.logs.text().contains("not deleted"), "{}", f.logs.text());
}

#[tokio::test]
async fn a_stream_that_ends_pauses_deletes_the_group_and_resumes_on_a_new_one() {
    let mut f = fixture(false, true);
    f.start();
    until("the first stream", || f.broker.opened_streams() == 1).await;
    let first = f.broker.creates.lock().unwrap()[0];

    f.broker.end_stream();
    until("a second group and stream", || f.broker.creates() == 2).await;

    assert_eq!(
        f.broker.deletes(),
        vec![first],
        "the lost group was deleted best-effort"
    );
    let second = f.broker.creates.lock().unwrap()[1];
    assert_ne!(first, second);
    assert!(
        f.logs.text().contains("event broker lost"),
        "{}",
        f.logs.text()
    );
    let joins = f.broker.joins.lock().unwrap().clone();
    assert_eq!(
        joins.last().unwrap().group,
        second,
        "the resume joins the new group"
    );
    f.stop().await;
}

#[tokio::test]
async fn a_stream_error_is_a_lost_broker_and_a_failed_delete_still_drops_the_group() {
    let mut f = fixture(false, true);
    *f.broker.delete_fails.lock().unwrap() = true;
    f.start();
    until("the first stream", || f.broker.opened_streams() == 1).await;

    f.broker.send(Err(refused()));
    until("a second group", || f.broker.creates() == 2).await;

    assert_eq!(f.broker.deletes().len(), 1, "one best-effort attempt");
    assert!(f.logs.text().contains("not deleted"), "{}", f.logs.text());
    f.stop().await;
}

#[tokio::test]
async fn a_broker_that_leaves_the_hub_pauses_the_consumer_and_its_return_resumes_it() {
    let mut f = fixture(false, true);
    f.start();
    until("the first stream", || f.broker.opened_streams() == 1).await;
    let first = f.broker.creates.lock().unwrap()[0];

    f.hub.remove::<dyn EventBrokerApi>();
    until("the group to be dropped", || !f.broker.deletes().is_empty()).await;
    assert_eq!(f.broker.deletes(), vec![first]);

    // While the broker is away, no group is created.
    tokio::time::sleep(Duration::from_millis(60)).await;
    assert_eq!(f.broker.creates(), 1, "paused, not creating");

    f.plug();
    until("a new group", || f.broker.creates() == 2).await;
    until("the new stream", || f.broker.opened_streams() == 2).await;
    f.stop().await;
}

#[tokio::test]
async fn a_terminal_control_frame_joins_again_through_the_same_group() {
    let mut f = fixture(false, true);
    f.start();
    until("the first stream", || f.broker.opened_streams() == 1).await;

    f.broker.send(Ok(WireFrame::Control {
        code: ControlCode::Terminal,
        positions: Vec::new(),
        reason: None,
    }));
    until("a second join", || {
        f.broker.joins.lock().unwrap().len() == 2
    })
    .await;

    assert_eq!(f.broker.creates(), 1, "never a second group");
    f.stop().await;
}

#[tokio::test]
async fn a_refused_registration_leaves_the_consumer_idle_without_a_group() {
    let mut f = fixture(true, true);
    f.start();
    until("the refusal to be logged", || {
        f.logs.text().contains("not registered")
    })
    .await;
    tokio::time::sleep(Duration::from_millis(30)).await;

    assert_eq!(
        f.broker.creates(),
        0,
        "no group while registration is refused"
    );
    assert!(
        !f.client.state().types_registered,
        "and the publish side reads skipped"
    );
    f.stop().await;
    assert!(f.broker.deletes().is_empty());
}

#[tokio::test]
async fn a_cancel_while_no_broker_is_resolvable_ends_the_task_cleanly() {
    let mut f = fixture(false, false);
    f.start();
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(f.broker.creates(), 0);
    f.stop().await;
    assert!(f.logs.text().contains("consumer stopped"));
}

// ---------------------------------------------------------------------------
// Trace continuation
// ---------------------------------------------------------------------------

const CARRIED_TRACE: &str = "4bf92f3577b34da6a3ce929d0e0e4736";

/// Deliver `event` under a subscriber that exports every closed span, and
/// return the exported `cache_invalidate.consume` spans.
fn consumed_spans(
    f: &Fixture,
    trace_parent: Option<&str>,
) -> Vec<opentelemetry_sdk::trace::SpanData> {
    use opentelemetry::trace::TracerProvider as _;
    use tracing_subscriber::layer::SubscriberExt as _;

    opentelemetry::global::set_text_map_propagator(
        opentelemetry_sdk::propagation::TraceContextPropagator::new(),
    );
    let exporter = opentelemetry_sdk::trace::InMemorySpanExporter::default();
    let provider = opentelemetry_sdk::trace::SdkTracerProvider::builder()
        .with_simple_exporter(exporter.clone())
        .build();
    let subscriber = tracing_subscriber::registry()
        .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("consume-test")));
    let consumer = CacheInvalidationConsumer::new(f.client.clone(), f.cache.clone());
    let WireFrame::Event(mut event) = wire(CACHE_INVALIDATE_EVENT_TYPE, invalidation(&["k.one"]))
    else {
        unreachable!("wire builds an event frame");
    };
    event.trace_parent = trace_parent.map(str::to_owned);
    tracing::subscriber::with_default(subscriber, || consumer.deliver(&event));
    exporter
        .get_finished_spans()
        .expect("finished spans")
        .into_iter()
        .filter(|s| s.name == "cache_invalidate.consume")
        .collect()
}

#[test]
fn a_delivery_with_a_traceparent_continues_the_carried_trace_and_evicts() {
    let f = fixture(false, true);
    f.cache.seed(Arc::new(tests_entry("k.one", tenant(2))));
    let carried = format!("00-{CARRIED_TRACE}-00f067aa0ba902b7-01");
    let spans = consumed_spans(&f, Some(&carried));
    assert_eq!(spans.len(), 1);
    assert_eq!(
        spans[0].span_context.trace_id().to_string(),
        CARRIED_TRACE,
        "the consume span continues the publisher's trace"
    );
    assert_eq!(
        spans[0].parent_span_id.to_string(),
        "00f067aa0ba902b7",
        "and hangs under the publisher's span"
    );
    assert!(f.cache.get("k.one", tenant(2)).is_none());
}

#[test]
fn a_delivery_without_a_traceparent_opens_a_fresh_root_and_still_evicts() {
    let f = fixture(false, true);
    f.cache.seed(Arc::new(tests_entry("k.one", tenant(2))));
    let spans = consumed_spans(&f, None);
    assert_eq!(spans.len(), 1);
    assert_ne!(spans[0].span_context.trace_id().to_string(), CARRIED_TRACE);
    assert_eq!(
        spans[0].parent_span_id,
        opentelemetry::trace::SpanId::INVALID,
        "a root span"
    );
    assert!(f.cache.get("k.one", tenant(2)).is_none());
}

// ---------------------------------------------------------------------------
// publish-to-deliver
// ---------------------------------------------------------------------------

#[test]
fn every_cache_invalidate_delivery_observes_publish_to_deliver_even_a_malformed_one() {
    let f = fixture(false, true);
    let metrics = Arc::new(RecordingNotifyMetrics::default());
    let consumer = CacheInvalidationConsumer::new(f.client.clone(), f.cache)
        .with_metrics(Arc::clone(&metrics) as Arc<dyn NotifyMetrics>);
    for data in [invalidation(&["k.one"]), json!({ "tenant": "not-a-uuid" })] {
        let WireFrame::Event(mut event) = wire(CACHE_INVALIDATE_EVENT_TYPE, data) else {
            unreachable!("wire builds an event frame");
        };
        event.occurred_at = Utc::now() - chrono::Duration::seconds(3);
        consumer.deliver(&event);
    }
    // Another type is not a cache_invalidate delivery and is not observed.
    let WireFrame::Event(other) = wire(
        "gts.cf.core.events.event.v1~cf.core.other.thing.v1~",
        invalidation(&["k.one"]),
    ) else {
        unreachable!("wire builds an event frame");
    };
    consumer.deliver(&other);

    let observed = metrics.delivered.lock().unwrap().clone();
    assert_eq!(observed.len(), 2, "the malformed delivery is observed too");
    for (event, elapsed) in observed {
        assert_eq!(event, "cache_invalidate");
        assert!(
            elapsed >= Duration::from_secs(3) && elapsed < Duration::from_secs(60),
            "{elapsed:?} runs from occurred_at to receipt"
        );
    }
}

// ---------------------------------------------------------------------------
// A broker call that never answers does not hold the stop
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_join_that_never_returns_does_not_hold_the_stop_and_the_group_is_released() {
    let mut f = fixture(false, true);
    f.broker.join_hangs.store(true, Ordering::SeqCst);
    f.start();
    until("the join to be entered", || {
        !f.broker.joins.lock().unwrap().is_empty()
    })
    .await;
    let group = f.broker.creates.lock().unwrap()[0];

    // `stop` itself fails the test when the task outlives its 5 s bound.
    let started = std::time::Instant::now();
    f.stop().await;
    assert!(started.elapsed() < Duration::from_secs(1));

    assert_eq!(
        f.broker.deletes(),
        vec![group],
        "the held group is deleted best-effort"
    );
    assert_eq!(f.broker.creates(), 1, "and no second one was created");
    assert!(
        f.logs.text().contains("stop interrupted"),
        "{}",
        f.logs.text()
    );
}

#[tokio::test]
async fn a_leave_that_never_returns_does_not_starve_the_group_deletion() {
    let f = fixture(false, true);
    f.broker.leave_hangs.store(true, Ordering::SeqCst);
    let consumer = CacheInvalidationConsumer::new(Arc::clone(&f.client), Arc::clone(&f.cache))
        .with_timing(
            Duration::from_millis(300),
            Duration::from_millis(5),
            Duration::from_millis(40),
        );
    let held = super::Held {
        broker: Arc::clone(&f.broker) as Arc<dyn EventBrokerApi>,
        ctx: SecurityContext::anonymous(),
        group: ConsumerGroupId::new(Uuid::new_v4()),
        subscription: Some(SubscriptionId(Uuid::new_v4())),
    };

    let started = std::time::Instant::now();
    consumer.release(&held, "test").await;
    assert!(started.elapsed() < Duration::from_millis(300) + Duration::from_millis(150));

    assert_eq!(f.broker.leaves.lock().unwrap().len(), 1, "leave was tried");
    assert_eq!(
        f.broker.deletes(),
        vec![held.group],
        "and the group deleted"
    );
    assert!(
        f.logs.text().contains("passed its share"),
        "{}",
        f.logs.text()
    );
}

#[tokio::test]
async fn a_group_creation_that_never_returns_does_not_hold_the_stop() {
    let mut f = fixture(false, true);
    f.broker.create_hangs.store(true, Ordering::SeqCst);
    f.start();
    until("the creation to be entered", || {
        !f.broker.create_calls.lock().unwrap().is_empty()
    })
    .await;

    let started = std::time::Instant::now();
    f.stop().await;
    assert!(started.elapsed() < Duration::from_secs(1));

    assert_eq!(f.broker.creates(), 0, "nothing was held");
    assert!(f.broker.deletes().is_empty(), "so nothing is deleted");
    assert!(
        f.logs.text().contains("stop interrupted"),
        "{}",
        f.logs.text()
    );
}

// ---------------------------------------------------------------------------
// The backoff resets on a proven stream, not on a join
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_broker_that_ends_every_stream_at_once_is_not_retried_in_a_hot_loop() {
    let mut f = fixture(false, true);
    f.broker.streams_end_at_once.store(true, Ordering::SeqCst);
    f.start();
    until("six groups to have been created", || {
        f.broker.create_calls.lock().unwrap().len() >= 6
    })
    .await;
    f.stop().await;

    // The fixture's schedule is 1 ms doubling to 4 ms; a join that succeeds no
    // longer resets it, so the pauses between creations rise with the ceiling
    // and hold at its floor (half the cap) instead of falling back to the base.
    let times = f.broker.create_calls.lock().unwrap().clone();
    let gaps: Vec<Duration> = times.windows(2).map(|w| w[1] - w[0]).collect();
    // Cycle k waits at least half of ceiling k: 0.5, 1, 2, 2, 2 ms.
    for (gap, floor) in gaps.iter().zip([500, 1000, 2000, 2000, 2000]) {
        assert!(*gap >= Duration::from_micros(floor), "{gaps:?}");
    }
}

#[tokio::test]
async fn the_first_frame_resets_the_backoff() {
    let f = fixture(false, true);
    let consumer = CacheInvalidationConsumer::new(Arc::clone(&f.client), Arc::clone(&f.cache));
    let mut pause = f.client.backoff();
    pause.next();
    pause.next();
    assert!(pause.ceiling() > Duration::from_millis(1));

    // A stream that opens and ends at once proves nothing.
    let nothing: super::FrameStream = Box::pin(futures_util::stream::empty());
    let taken = consumer
        .take(nothing, &mut pause, &CancellationToken::new())
        .await;
    assert!(matches!(taken, super::Taken::Ended(super::Ended::Lost)));
    assert!(pause.ceiling() > Duration::from_millis(1), "not reset");

    // One frame does.
    let one: super::FrameStream =
        Box::pin(futures_util::stream::iter([Ok(WireFrame::Heartbeat {
            at: "now".to_owned(),
        })]));
    let taken = consumer
        .take(one, &mut pause, &CancellationToken::new())
        .await;
    assert!(matches!(taken, super::Taken::Ended(super::Ended::Lost)));
    assert_eq!(pause.ceiling(), Duration::from_millis(1), "reset");
}

#[tokio::test]
async fn a_terminal_control_frame_does_not_reset_the_backoff() {
    let f = fixture(false, true);
    let consumer = CacheInvalidationConsumer::new(Arc::clone(&f.client), Arc::clone(&f.cache));
    let mut pause = f.client.backoff();
    pause.next();
    pause.next();
    let before = pause.ceiling();
    assert!(before > Duration::from_millis(1));

    let terminal: super::FrameStream =
        Box::pin(futures_util::stream::iter([Ok(WireFrame::Control {
            code: ControlCode::Terminal,
            positions: Vec::new(),
            reason: None,
        })]));
    let taken = consumer
        .take(terminal, &mut pause, &CancellationToken::new())
        .await;
    assert!(matches!(taken, super::Taken::Rejoin));
    assert_eq!(pause.ceiling(), before, "a terminal frame proves nothing");
}

#[tokio::test]
async fn a_quiet_stream_that_stays_open_long_enough_resets_the_backoff() {
    let f = fixture(false, true);
    let consumer = CacheInvalidationConsumer::new(Arc::clone(&f.client), Arc::clone(&f.cache))
        .with_timing(
            Duration::from_secs(2),
            Duration::from_millis(5),
            Duration::from_millis(30),
        );
    let mut pause = f.client.backoff();
    pause.next();
    pause.next();
    let quiet: super::FrameStream = Box::pin(futures_util::stream::pending());
    let cancel = CancellationToken::new();
    let stopper = {
        let cancel = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(120)).await;
            cancel.cancel();
        })
    };
    let taken = consumer.take(quiet, &mut pause, &cancel).await;
    stopper.await.unwrap();
    assert!(matches!(
        taken,
        super::Taken::Ended(super::Ended::Cancelled)
    ));
    assert_eq!(pause.ceiling(), Duration::from_millis(1), "reset");
}
