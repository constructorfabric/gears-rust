#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use event_broker_sdk::models::EventType;
use event_broker_sdk::{
    ConsumerGroup, ConsumerGroupId, ConsumerGroupQuery, CreateConsumerGroupRequest, Event,
    EventBrokerApi, EventBrokerError, FrameStream, IngestOutcome, JoinRequest, Page,
    PartitionRange, ProducerCursors, ProducerId, ProducerMode, ResetScope, SeekPosition,
    SeekResult, Subscription, SubscriptionAssignment, SubscriptionId, Topic, TopicSegment,
};
use serde_json::Value;
use settings_service_sdk::gts::{CACHE_INVALIDATE_EVENT_TYPE, CACHE_TOPIC_ID};
use tokio_util::sync::CancellationToken;
use toolkit::ClientHub;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;
use types_registry_sdk::RegisterResult;
use uuid::Uuid;

use super::{Backoff, CacheSchemaRegistry, EventBrokerClient};
use crate::domain::coherence::testing::CountingMetrics;
use crate::domain::coherence::{
    BroadcastContext, BroadcastGateway, BroadcastMetrics, BroadcastSink, ChangeOutcome,
    CoherencePublisher, PublishError, TenantKeys,
};
use crate::domain::error::DomainError;
use crate::domain::platform_scope::PlatformScope;
use crate::domain::resolution::scope_class;
use crate::infra::broadcast::build_broadcasts;
use crate::infra::event_schemas::CacheInvalidation;
use crate::infra::service_identity::service_principal;

// ---------------------------------------------------------------------------
// Doubles
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum Mode {
    Accept,
    Refuse,
    Stall,
}

/// A broker that accepts, refuses or stalls every publish; the rest is refused.
struct ScriptedBroker {
    mode: Mutex<Mode>,
    published: Mutex<Vec<(SecurityContext, Event)>>,
}

impl ScriptedBroker {
    fn new(mode: Mode) -> Arc<Self> {
        Arc::new(Self {
            mode: Mutex::new(mode),
            published: Mutex::new(Vec::new()),
        })
    }
}

fn unsupported() -> EventBrokerError {
    EventBrokerError::Internal("not supported by the scripted broker".to_owned())
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
        ctx: &SecurityContext,
        event: &Event,
    ) -> Result<IngestOutcome, EventBrokerError> {
        let mode = *self.mode.lock().unwrap();
        match mode {
            Mode::Refuse => return Err(unsupported()),
            Mode::Stall => std::future::pending::<()>().await,
            Mode::Accept => {}
        }
        self.published
            .lock()
            .unwrap()
            .push((ctx.clone(), event.clone()));
        Ok(IngestOutcome::Accepted)
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
        _: CreateConsumerGroupRequest,
    ) -> Result<ConsumerGroup, EventBrokerError> {
        Err(unsupported())
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
        _: &ConsumerGroupId,
    ) -> Result<(), EventBrokerError> {
        Err(unsupported())
    }
    async fn join(
        &self,
        _: &SecurityContext,
        _: JoinRequest,
    ) -> Result<SubscriptionAssignment, EventBrokerError> {
        Err(unsupported())
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
    async fn leave(&self, _: &SecurityContext, _: SubscriptionId) -> Result<(), EventBrokerError> {
        Err(unsupported())
    }
    async fn stream(
        &self,
        _: &SecurityContext,
        _: SubscriptionId,
    ) -> Result<FrameStream, EventBrokerError> {
        Err(unsupported())
    }
    async fn seek(
        &self,
        _: &SecurityContext,
        _: SubscriptionId,
        _: i64,
        _: &[SeekPosition],
    ) -> Result<Vec<SeekResult>, EventBrokerError> {
        Err(unsupported())
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

/// A registry that refuses a scripted number of calls per document, can answer
/// "already registered", and records what it was asked to register.
#[derive(Default)]
struct ScriptedRegistry {
    topic_refusals: AtomicUsize,
    type_refusals: AtomicUsize,
    already_registered: bool,
    topics: Mutex<Vec<Value>>,
    types: Mutex<Vec<Value>>,
}

impl ScriptedRegistry {
    fn answer(&self, refusals: &AtomicUsize) -> Vec<RegisterResult> {
        let refused = refusals
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok();
        let id = Some("gts.x".to_owned());
        if refused {
            vec![RegisterResult::Err {
                gts_id: id,
                error: CanonicalError::from(DomainError::Internal {
                    diagnostic: "registry says no".to_owned(),
                }),
            }]
        } else if self.already_registered {
            vec![RegisterResult::Err {
                gts_id: id,
                error: CanonicalError::from(DomainError::Conflict {
                    detail: "already registered".to_owned(),
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
impl CacheSchemaRegistry for ScriptedRegistry {
    async fn register_topic(&self, topic: Value) -> Result<Vec<RegisterResult>, CanonicalError> {
        self.topics.lock().unwrap().push(topic);
        Ok(self.answer(&self.topic_refusals))
    }

    async fn register_event_type(
        &self,
        schema: Value,
    ) -> Result<Vec<RegisterResult>, CanonicalError> {
        self.types.lock().unwrap().push(schema);
        Ok(self.answer(&self.type_refusals))
    }
}

/// A scope that fails a scripted number of times, then answers.
struct ScriptedScope {
    failures: AtomicUsize,
    asked: AtomicUsize,
    root: Uuid,
}

#[async_trait]
impl PlatformScope for ScriptedScope {
    async fn root_tenant(&self) -> Result<Uuid, DomainError> {
        self.asked.fetch_add(1, Ordering::SeqCst);
        let failed = self
            .failures
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok();
        if failed {
            Err(DomainError::Unavailable {
                detail: "tenant resolver down".to_owned(),
            })
        } else {
            Ok(self.root)
        }
    }
}

// ---------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------

const ROOT: Uuid = Uuid::from_u128(0xa0);

struct Fixture {
    hub: Arc<ClientHub>,
    registry: Arc<ScriptedRegistry>,
    scope: Arc<ScriptedScope>,
    client: Arc<EventBrokerClient>,
}

fn fixture(registry: ScriptedRegistry, scope_failures: usize, timeout: Duration) -> Fixture {
    let hub = Arc::new(ClientHub::new());
    let registry = Arc::new(registry);
    let scope = Arc::new(ScriptedScope {
        failures: AtomicUsize::new(scope_failures),
        asked: AtomicUsize::new(0),
        root: ROOT,
    });
    let client = Arc::new(
        EventBrokerClient::new(
            Arc::clone(&hub),
            Arc::clone(&registry) as Arc<dyn CacheSchemaRegistry>,
            Arc::clone(&scope) as Arc<dyn PlatformScope>,
            timeout,
            Uuid::from_u128(0x1d),
        )
        .with_backoff(Backoff::with_steps(
            Duration::from_millis(1),
            Duration::from_millis(8),
        )),
    );
    Fixture {
        hub,
        registry,
        scope,
        client,
    }
}

fn plain() -> Fixture {
    fixture(ScriptedRegistry::default(), 0, Duration::from_secs(2))
}

fn plug(f: &Fixture, mode: Mode) -> Arc<ScriptedBroker> {
    let broker = ScriptedBroker::new(mode);
    f.hub
        .register::<dyn EventBrokerApi>(Arc::clone(&broker) as Arc<dyn EventBrokerApi>);
    broker
}

fn an_event() -> Event {
    let ctx = BroadcastContext {
        root_tenant: ROOT,
        change_set_id: Uuid::from_u128(0xc5),
        trace_parent: None,
    };
    let touched = [TenantKeys {
        tenant: Uuid::from_u128(0x7e).to_string(),
        keys: vec!["acme.net.proxy".to_owned()],
    }];
    build_broadcasts(&touched, &ctx).remove(0)
}

#[derive(Clone, Default)]
struct LogBuf(Arc<Mutex<Vec<u8>>>);

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

/// Captures this thread's log lines until the guard drops.
fn capture() -> (LogBuf, tracing::subscriber::DefaultGuard) {
    let buf = LogBuf::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(buf.clone())
        .with_ansi(false)
        .finish();
    (buf, tracing::subscriber::set_default(subscriber))
}

fn logged(buf: &LogBuf) -> String {
    String::from_utf8(buf.0.lock().unwrap().clone()).unwrap()
}

// ---------------------------------------------------------------------------
// Resolution
// ---------------------------------------------------------------------------

#[tokio::test]
async fn init_state_is_nothing_resolved_and_nothing_asked() {
    let f = plain();
    assert!(f.client.broker().is_none());
    assert_eq!(f.client.root_tenant(), None);
    let state = f.client.state();
    assert!(!state.types_registered && !state.root_tenant_known);
    assert_eq!(f.scope.asked.load(Ordering::SeqCst), 0);
    assert!(f.registry.topics.lock().unwrap().is_empty());
}

#[tokio::test]
async fn no_broker_means_not_ready_and_nothing_is_registered_or_asked() {
    let f = plain();
    assert!(!f.client.resolve_once(&CancellationToken::new()).await);
    assert!(f.registry.topics.lock().unwrap().is_empty());
    assert!(f.registry.types.lock().unwrap().is_empty());
    assert_eq!(f.scope.asked.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn the_first_resolution_registers_both_schemas_and_learns_the_root_tenant() {
    let f = plain();
    plug(&f, Mode::Accept);
    assert!(f.client.resolve_once(&CancellationToken::new()).await);

    let topics = f.registry.topics.lock().unwrap();
    assert_eq!(topics.len(), 1);
    assert_eq!(topics[0]["id"], CACHE_TOPIC_ID);
    let types = f.registry.types.lock().unwrap();
    assert_eq!(types.len(), 1);
    assert!(types[0].to_string().contains(CACHE_INVALIDATE_EVENT_TYPE));

    let state = f.client.state();
    assert!(state.types_registered && state.root_tenant_known);
    assert_eq!(f.client.root_tenant(), Some(ROOT));
    assert!(f.client.broker().is_some());
}

#[tokio::test]
async fn a_second_resolution_registers_and_asks_nothing_again() {
    let f = plain();
    plug(&f, Mode::Accept);
    assert!(f.client.resolve_once(&CancellationToken::new()).await);
    assert!(f.client.resolve_once(&CancellationToken::new()).await);
    assert_eq!(f.registry.topics.lock().unwrap().len(), 1);
    assert_eq!(f.registry.types.lock().unwrap().len(), 1);
    assert_eq!(f.scope.asked.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn already_registered_is_success() {
    let f = fixture(
        ScriptedRegistry {
            already_registered: true,
            ..Default::default()
        },
        0,
        Duration::from_secs(2),
    );
    plug(&f, Mode::Accept);
    assert!(f.client.resolve_once(&CancellationToken::new()).await);
    assert!(f.client.state().types_registered);
}

#[tokio::test]
async fn a_refused_registration_warns_leaves_types_unregistered_and_is_retried() {
    let f = fixture(
        ScriptedRegistry {
            type_refusals: AtomicUsize::new(1),
            ..Default::default()
        },
        0,
        Duration::from_secs(2),
    );
    plug(&f, Mode::Accept);
    let (buf, _guard) = capture();

    assert!(!f.client.resolve_once(&CancellationToken::new()).await);
    let state = f.client.state();
    assert!(!state.types_registered, "the type half is still missing");
    assert!(state.root_tenant_known, "the root tenant is independent");
    let log = logged(&buf);
    assert!(
        log.contains("WARN") && log.contains("refused the cache_invalidate type"),
        "{log}"
    );

    assert!(f.client.resolve_once(&CancellationToken::new()).await);
    assert!(f.client.state().types_registered);
    // The topic registered the first time is not registered again.
    assert_eq!(f.registry.topics.lock().unwrap().len(), 1);
    assert_eq!(f.registry.types.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn an_unavailable_root_tenant_is_not_known_and_is_retried() {
    let f = fixture(ScriptedRegistry::default(), 1, Duration::from_secs(2));
    plug(&f, Mode::Accept);

    assert!(!f.client.resolve_once(&CancellationToken::new()).await);
    let state = f.client.state();
    assert!(!state.root_tenant_known);
    assert!(state.types_registered);
    assert!(f.client.publishing_context().is_none());

    assert!(f.client.resolve_once(&CancellationToken::new()).await);
    assert!(f.client.state().root_tenant_known);
    assert_eq!(f.scope.asked.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn resolution_waits_for_the_broker_to_appear_and_stops_on_cancel() {
    let f = plain();
    let cancel = CancellationToken::new();
    let waiting = {
        let client = Arc::clone(&f.client);
        let cancel = cancel.clone();
        tokio::spawn(async move { client.resolve_until_ready(&cancel).await })
    };
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(!waiting.is_finished(), "idle while no broker is present");
    plug(&f, Mode::Accept);
    assert!(waiting.await.unwrap());

    let g = plain();
    let cancel = CancellationToken::new();
    let waiting = {
        let client = Arc::clone(&g.client);
        let cancel = cancel.clone();
        tokio::spawn(async move { client.resolve_until_ready(&cancel).await })
    };
    tokio::time::sleep(Duration::from_millis(10)).await;
    cancel.cancel();
    assert!(!waiting.await.unwrap());
}

// ---------------------------------------------------------------------------
// Backoff
// ---------------------------------------------------------------------------

#[test]
fn the_production_schedule_doubles_from_one_second_to_a_thirty_second_cap() {
    let mut backoff = Backoff::standard();
    let mut ceilings = Vec::new();
    for _ in 0..8 {
        let ceiling = backoff.ceiling();
        let wait = backoff.next();
        assert!(wait <= ceiling, "{wait:?} within {ceiling:?}");
        assert!(
            wait >= ceiling / 2,
            "{wait:?} not below half of {ceiling:?}"
        );
        ceilings.push(ceiling.as_secs());
    }
    assert_eq!(ceilings, [1, 2, 4, 8, 16, 30, 30, 30]);
}

#[test]
fn the_jitter_has_a_floor_at_half_the_ceiling_and_still_spreads() {
    let mut waits = Vec::new();
    for _ in 0..64 {
        let mut backoff = Backoff::standard();
        waits.push(backoff.next());
    }
    assert!(
        waits.iter().all(|w| *w >= Duration::from_millis(500)),
        "no wait is drawn below the floor"
    );
    assert!(waits.iter().all(|w| *w <= Duration::from_secs(1)));
    assert!(waits.iter().any(|w| *w < Duration::from_millis(750)));
    assert!(waits.iter().any(|w| *w > Duration::from_millis(750)));
}

#[test]
fn a_broker_that_ends_every_stream_at_once_is_retried_ever_more_slowly_up_to_the_cap() {
    // Nothing resets the schedule between cycles (no stream proves healthy),
    // so the successive waits' floors rise to half the cap and stay there: the
    // creation rate decays toward one per ~30 s, never a hot loop.
    let mut backoff = Backoff::standard();
    let mut floors = Vec::new();
    let mut total = Duration::ZERO;
    for _ in 0..10 {
        let ceiling = backoff.ceiling();
        let wait = backoff.next();
        assert!(wait >= ceiling / 2 && wait <= ceiling);
        floors.push((ceiling / 2).as_millis());
        total += wait;
    }
    assert_eq!(
        floors,
        [
            500, 1000, 2000, 4000, 8000, 15000, 15000, 15000, 15000, 15000
        ]
    );
    // Ten cycles take at least the sum of the floors: ~91 s, not ~5 s.
    assert!(total >= Duration::from_secs(90), "{total:?}");
}

#[test]
fn a_proven_stream_resets_the_schedule() {
    let mut backoff = Backoff::standard();
    backoff.next();
    backoff.next();
    backoff.reset();
    assert_eq!(backoff.ceiling(), Duration::from_secs(1));
}

#[tokio::test]
async fn a_wait_cycle_ends_early_when_cancelled() {
    let f = plain();
    let cancel = CancellationToken::new();
    cancel.cancel();
    let mut slow = Backoff::standard();
    assert!(!EventBrokerClient::wait_cycle(&mut slow, &cancel).await);
    let mut fast = f.client.backoff();
    assert!(EventBrokerClient::wait_cycle(&mut fast, &CancellationToken::new()).await);
}

// ---------------------------------------------------------------------------
// Publishing
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_accepted_publish_is_ok_and_carries_the_publishing_identity() {
    let f = plain();
    let broker = plug(&f, Mode::Accept);
    assert!(f.client.resolve_once(&CancellationToken::new()).await);

    let session = f.client.session().expect("a session");
    session.publish(an_event()).await.expect("accepted");

    let published = broker.published.lock().unwrap();
    assert_eq!(published.len(), 1);
    let ctx = &published[0].0;
    assert_eq!(ctx.subject_tenant_id(), ROOT);
    assert_eq!(ctx.subject_id(), service_principal());
    assert_eq!(ctx.token_scopes(), ["*".to_owned()]);
    assert!(ctx.subject_type().is_some());
    let body: CacheInvalidation =
        serde_json::from_value(published[0].1.data.clone().unwrap()).unwrap();
    assert_eq!(body.changed_keys, ["acme.net.proxy"]);
}

#[tokio::test]
async fn the_publishing_context_names_the_root_tenant_once_known() {
    let f = plain();
    assert!(f.client.publishing_context().is_none());
    plug(&f, Mode::Accept);
    assert!(f.client.resolve_once(&CancellationToken::new()).await);
    let ctx = f.client.publishing_context().expect("context");
    assert_eq!(ctx.subject_tenant_id(), ROOT);
    assert_eq!(ctx.token_scopes(), ["*".to_owned()]);
}

#[tokio::test]
async fn a_stalled_broker_times_out() {
    let f = fixture(ScriptedRegistry::default(), 0, Duration::from_millis(30));
    plug(&f, Mode::Stall);
    assert!(f.client.resolve_once(&CancellationToken::new()).await);
    let session = f.client.session().expect("a session");
    let err = session.publish(an_event()).await.unwrap_err();
    assert_eq!(err, PublishError::TimedOut);
}

#[tokio::test]
async fn a_refusing_broker_yields_refused_and_the_client_logs_no_value() {
    let f = plain();
    plug(&f, Mode::Refuse);
    assert!(f.client.resolve_once(&CancellationToken::new()).await);
    let (buf, _guard) = capture();
    let session = f.client.session().expect("a session");
    let err = session.publish(an_event()).await.unwrap_err();
    assert!(matches!(err, PublishError::Refused(ref r) if r.contains("not supported")));
    let log = logged(&buf);
    assert!(
        !log.contains("WARN"),
        "the client itself is quiet; the outcome warns once: {log}"
    );
    assert!(
        !log.contains("acme.net.proxy"),
        "no key or value in the log"
    );
}

#[tokio::test]
async fn without_a_broker_there_is_no_session() {
    let f = plain();
    assert!(f.client.session().is_none());
    assert!(BroadcastGateway::session(f.client.as_ref()).is_none());
}

#[tokio::test]
async fn a_session_has_no_publishing_context_before_the_root_tenant_is_known() {
    let f = fixture(ScriptedRegistry::default(), 1, Duration::from_secs(2));
    plug(&f, Mode::Accept);
    assert!(!f.client.resolve_once(&CancellationToken::new()).await);
    assert!(f.client.session().is_none());
}

#[tokio::test]
async fn a_broker_that_leaves_the_hub_after_the_session_is_a_refusal_on_the_held_handle() {
    let f = plain();
    let broker = plug(&f, Mode::Accept);
    assert!(f.client.resolve_once(&CancellationToken::new()).await);

    let session = f.client.session().expect("a session");
    f.hub.remove::<dyn EventBrokerApi>();
    assert!(f.client.broker().is_none(), "the hub no longer holds it");

    // The request keeps publishing through the handle it took: the broker
    // answers, and no false "no broker" is reported.
    session.publish(an_event()).await.expect("accepted");
    assert_eq!(broker.published.lock().unwrap().len(), 1);
    *broker.mode.lock().unwrap() = Mode::Refuse;
    let err = session.publish(an_event()).await.unwrap_err();
    assert!(
        matches!(err, PublishError::Refused(ref r) if r.contains("not supported")),
        "{err:?}"
    );
}

#[tokio::test]
async fn one_refused_publish_through_the_client_is_exactly_one_warn() {
    let f = plain();
    plug(&f, Mode::Refuse);
    assert!(f.client.resolve_once(&CancellationToken::new()).await);
    let metrics = Arc::new(CountingMetrics::default());
    let hook = CoherencePublisher::new(
        Arc::clone(&f.client) as Arc<dyn BroadcastGateway>,
        Arc::clone(&metrics) as Arc<dyn BroadcastMetrics>,
    );
    let (buf, _guard) = capture();
    // Two tenants: the first publish is refused, the second skipped.
    hook.publish_value_writes(
        [
            ChangeOutcome::value_write(
                "acme.net.proxy",
                Uuid::from_u128(1),
                scope_class::LOCAL,
                true,
            ),
            ChangeOutcome::value_write(
                "acme.net.proxy",
                Uuid::from_u128(2),
                scope_class::LOCAL,
                true,
            ),
        ],
        Uuid::from_u128(0xc5),
    )
    .await;
    assert_eq!(metrics.all(), ["failed", "skipped"]);
    let log = logged(&buf);
    assert_eq!(log.matches("WARN").count(), 1, "{log}");
    assert!(
        log.contains("not supported"),
        "the warn names the reason: {log}"
    );
}

// ---------------------------------------------------------------------------
// Cancellation of the awaited calls
// ---------------------------------------------------------------------------

/// A resolver that never answers.
struct HungScope;

#[async_trait]
impl PlatformScope for HungScope {
    async fn root_tenant(&self) -> Result<Uuid, DomainError> {
        std::future::pending().await
    }
}

/// A registry that never answers.
struct HungRegistry;

#[async_trait]
impl CacheSchemaRegistry for HungRegistry {
    async fn register_topic(&self, _: Value) -> Result<Vec<RegisterResult>, CanonicalError> {
        std::future::pending().await
    }
    async fn register_event_type(&self, _: Value) -> Result<Vec<RegisterResult>, CanonicalError> {
        std::future::pending().await
    }
}

fn hung_client(
    registry: Arc<dyn CacheSchemaRegistry>,
    scope: Arc<dyn PlatformScope>,
) -> (Arc<ClientHub>, Arc<EventBrokerClient>) {
    let hub = Arc::new(ClientHub::new());
    hub.register::<dyn EventBrokerApi>(ScriptedBroker::new(Mode::Accept) as Arc<dyn EventBrokerApi>);
    let client = Arc::new(
        EventBrokerClient::new(
            Arc::clone(&hub),
            registry,
            scope,
            Duration::from_secs(2),
            Uuid::from_u128(0x1d),
        )
        .with_backoff(Backoff::with_steps(
            Duration::from_millis(1),
            Duration::from_millis(8),
        )),
    );
    (hub, client)
}

async fn ends_within_when_cancelled(client: Arc<EventBrokerClient>) {
    let cancel = CancellationToken::new();
    let waiting = {
        let cancel = cancel.clone();
        tokio::spawn(async move { client.resolve_until_ready(&cancel).await })
    };
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(
        !waiting.is_finished(),
        "parked on the call that never answers"
    );
    cancel.cancel();
    let ready = tokio::time::timeout(Duration::from_secs(2), waiting)
        .await
        .expect("the stop is not held by the call")
        .unwrap();
    assert!(!ready);
}

#[tokio::test]
async fn a_tenant_resolver_that_never_answers_does_not_hold_the_stop() {
    let (_hub, client) = hung_client(Arc::new(ScriptedRegistry::default()), Arc::new(HungScope));
    ends_within_when_cancelled(client).await;
}

#[tokio::test]
async fn a_types_registry_that_never_answers_does_not_hold_the_stop() {
    let scope = Arc::new(ScriptedScope {
        failures: AtomicUsize::new(0),
        asked: AtomicUsize::new(0),
        root: ROOT,
    });
    let (_hub, client) = hung_client(Arc::new(HungRegistry), scope);
    ends_within_when_cancelled(client).await;
}

#[test]
fn the_group_alias_names_the_instance() {
    let f = plain();
    assert_eq!(
        f.client.group_alias(),
        format!("settings-service:{}", Uuid::from_u128(0x1d))
    );
}
