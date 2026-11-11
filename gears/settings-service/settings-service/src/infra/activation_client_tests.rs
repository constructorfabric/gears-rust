//! The in-process activation client over a scripted service, a scripted reader
//! and a scripted broker: the handle and its states, the process's one consumer
//! group, the fresh re-read, the handler and the report.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use chrono::Utc;
use event_broker_sdk::{
    ConsumerGroupId, EventBrokerApi, EventBrokerError, EventV1, GtsTypeId, Sequence,
    StoredPosition, WireFrame,
};
use secrecy::SecretString;
use serde_json::{Value, json};
use settings_service_sdk::api::{BulkOutcome, BulkSelector, SettingsReaderClient};
use settings_service_sdk::gts::{
    ACTIVATION_SUBJECT_TYPE, ACTIVATION_TOPIC_ID, CACHE_INVALIDATE_EVENT_TYPE,
    CHANGE_NOTIFICATION_EVENT_TYPE,
};
use settings_service_sdk::models::{
    EffectiveSource, EffectiveValueResponse, Freshness, GetEffectiveRequest,
};
use settings_service_sdk::{
    ActivationOutcome, ChangeBundle, InstanceIdentity, KeyState, PendingReason, RefusedReason,
    ReportResult, SecretHandle, SettingChangeHandler, SettingKey, SettingsActivationClient,
    SubscriptionHandle, UnsubscribeResult,
};
use toolkit::ClientHub;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::{
    ActivationBackend, ActivationClient, ClientTiming, ConsumerDeps, RegisterError,
    UnsubscribeFailure, failure_code,
};
use crate::domain::activation::testing::{LogBuf, RecordingNotifyMetrics};
use crate::domain::activation::{BackResponse, NotifyMetrics};
use crate::domain::error::DomainError;
use crate::infra::activation_group::MAX_TENANT_INTERESTS;
use crate::infra::broker_group::lifecycle_tests as lifecycle;
use crate::infra::event_broker::Backoff;
use crate::infra::mutation_lock::LOCK_EXHAUSTED_DETAIL;
use crate::test_support::scripted_broker::{ScriptedBroker, spend};

fn tenant(n: u128) -> Uuid {
    Uuid::from_u128(n)
}

fn key(name: &str) -> SettingKey {
    SettingKey::parse(&format!(
        "gts.cf.core.settings.setting_type.v1~acme.settings.network.{name}.v1~"
    ))
    .expect("fixture key parses")
}

/// A gear's context: a subscriber is a service principal.
fn ctx_of(subject: u128) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::from_u128(subject))
        .subject_tenant_id(tenant(0xa0))
        .subject_type(crate::infra::service_identity::SERVICE_SUBJECT_TYPE)
        .build()
        .expect("a context")
}

/// The gear that subscribes in most tests.
const GEAR: u128 = 0x6ea1;

// ---------------------------------------------------------------------------
// The scripted service
// ---------------------------------------------------------------------------

/// `(subscriber, key, tenant, when)` of one `register`.
type Registration = (String, String, Uuid, Instant);

#[derive(Default)]
struct FakeBackend {
    /// Failures `register` answers a key with, once each, in order.
    register_script: Mutex<HashMap<String, VecDeque<RegisterError>>>,
    /// A failure `register` answers a key with until it is cleared.
    register_always: Mutex<HashMap<String, RegisterError>>,
    /// `(subscriber, key, tenant, when)` of every `register`.
    registered: Mutex<Vec<Registration>>,
    /// While set, `register` waits for `release`.
    held: AtomicBool,
    release: tokio::sync::Notify,
    unsubscribe_script: Mutex<HashMap<String, VecDeque<UnsubscribeFailure>>>,
    /// `(subscriber, key, tenant)` of every `unsubscribe`.
    unsubscribed: Mutex<Vec<(String, String, Uuid)>>,
    /// The instance id of every `unsubscribe`, in order.
    unsubscribed_as: Mutex<Vec<Uuid>>,
    /// The identity every `register` carried, in order.
    identities: Mutex<Vec<InstanceIdentity>>,
    /// While set, `unsubscribe` never answers.
    stall_unsubscribe: AtomicBool,
    /// `(subscriber, response)` of every `report`.
    reports: Mutex<Vec<(String, BackResponse)>>,
    report_answers: Mutex<VecDeque<Result<crate::domain::activation::ReportResult, DomainError>>>,
}

impl FakeBackend {
    fn register_fails(&self, name: &str, failure: RegisterError, times: usize) {
        self.register_script
            .lock()
            .unwrap()
            .entry(key(name).as_str().to_owned())
            .or_default()
            .extend(std::iter::repeat_n(failure, times));
    }

    fn register_always_fails(&self, name: &str, failure: RegisterError) {
        self.register_always
            .lock()
            .unwrap()
            .insert(key(name).as_str().to_owned(), failure);
    }

    fn declared(&self, name: &str) {
        self.register_always
            .lock()
            .unwrap()
            .remove(key(name).as_str());
    }

    fn register_calls(&self, name: &str) -> usize {
        self.registered
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, k, _, _)| k == key(name).as_str())
            .count()
    }

    fn registered_at(&self, name: &str) -> Vec<Instant> {
        self.registered
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, k, _, _)| k == key(name).as_str())
            .map(|(_, _, _, at)| *at)
            .collect()
    }

    fn unsubscribe_fails(&self, name: &str, failure: UnsubscribeFailure, times: usize) {
        self.unsubscribe_script
            .lock()
            .unwrap()
            .entry(key(name).as_str().to_owned())
            .or_default()
            .extend(std::iter::repeat_n(failure, times));
    }

    fn reports(&self) -> Vec<(String, BackResponse)> {
        self.reports.lock().unwrap().clone()
    }

    fn hold_registrations(&self) {
        self.held.store(true, Ordering::SeqCst);
    }

    fn release_registrations(&self) {
        self.held.store(false, Ordering::SeqCst);
        self.release.notify_waiters();
    }
}

#[async_trait]
impl ActivationBackend for FakeBackend {
    async fn register(
        &self,
        ctx: &SecurityContext,
        key: &SettingKey,
        tenant: Uuid,
        identity: &InstanceIdentity,
    ) -> Result<(), RegisterError> {
        self.identities.lock().unwrap().push(identity.clone());
        while self.held.load(Ordering::SeqCst) {
            let waiting = self.release.notified();
            tokio::pin!(waiting);
            waiting.as_mut().enable();
            if !self.held.load(Ordering::SeqCst) {
                break;
            }
            waiting.await;
        }
        self.registered.lock().unwrap().push((
            ctx.subject_id().to_string(),
            key.as_str().to_owned(),
            tenant,
            Instant::now(),
        ));
        if let Some(failure) = self.register_always.lock().unwrap().get(key.as_str()) {
            return Err(failure.clone());
        }
        match self
            .register_script
            .lock()
            .unwrap()
            .get_mut(key.as_str())
            .and_then(VecDeque::pop_front)
        {
            Some(failure) => Err(failure),
            None => Ok(()),
        }
    }

    async fn unsubscribe(
        &self,
        ctx: &SecurityContext,
        key: &SettingKey,
        tenant: Uuid,
        instance_id: Uuid,
    ) -> Result<(), UnsubscribeFailure> {
        if self.stall_unsubscribe.load(Ordering::SeqCst) {
            std::future::pending::<()>().await;
        }
        self.unsubscribed_as.lock().unwrap().push(instance_id);
        self.unsubscribed.lock().unwrap().push((
            ctx.subject_id().to_string(),
            key.as_str().to_owned(),
            tenant,
        ));
        match self
            .unsubscribe_script
            .lock()
            .unwrap()
            .get_mut(key.as_str())
            .and_then(VecDeque::pop_front)
        {
            Some(failure) => Err(failure),
            None => Ok(()),
        }
    }

    async fn report(
        &self,
        ctx: &SecurityContext,
        response: BackResponse,
    ) -> Result<crate::domain::activation::ReportResult, DomainError> {
        self.reports
            .lock()
            .unwrap()
            .push((ctx.subject_id().to_string(), response));
        self.report_answers
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Ok(crate::domain::activation::ReportResult::Applied))
    }
}

#[derive(Clone, Copy)]
enum ReadFail {
    Unavailable,
    Retired,
    NotFound,
}

impl ReadFail {
    fn error(self, key: &SettingKey) -> CanonicalError {
        CanonicalError::from(match self {
            Self::Unavailable => DomainError::Unavailable {
                detail: "the store could not answer".to_owned(),
            },
            Self::Retired => DomainError::Retired {
                key: key.as_str().to_owned(),
            },
            Self::NotFound => DomainError::NotFound {
                resource: "setting",
            },
        })
    }
}

#[derive(Default)]
struct FakeReader {
    values: Mutex<HashMap<String, Value>>,
    failures: Mutex<HashMap<String, VecDeque<ReadFail>>>,
    broken: Mutex<HashMap<String, ReadFail>>,
    requests: Mutex<Vec<GetEffectiveRequest>>,
    /// The key whose next fresh read waits for `open_gate`, and how many reads
    /// wait now. The value is taken before the wait, so a read released late
    /// answers what the store held when it began.
    gate: Mutex<Option<(String, Arc<tokio::sync::Semaphore>)>>,
    waiting: AtomicUsize,
    gate_spent: AtomicBool,
}

impl FakeReader {
    /// Block the next fresh read of `name` until `open_gate`.
    fn block_next_read(&self, name: &str) {
        self.gate_spent.store(false, Ordering::SeqCst);
        *self.gate.lock().unwrap() = Some((
            key(name).as_str().to_owned(),
            Arc::new(tokio::sync::Semaphore::new(0)),
        ));
    }

    fn reads_waiting(&self) -> usize {
        self.waiting.load(Ordering::SeqCst)
    }

    fn open_gate(&self, name: &str) {
        let gate = self.gate.lock().unwrap();
        let (gated, semaphore) = gate.as_ref().expect("a gate was armed");
        assert_eq!(gated, key(name).as_str());
        semaphore.add_permits(1);
    }

    fn set(&self, name: &str, value: Value) {
        self.values
            .lock()
            .unwrap()
            .insert(key(name).as_str().to_owned(), value);
    }

    fn fails(&self, name: &str, failure: ReadFail, times: usize) {
        self.failures
            .lock()
            .unwrap()
            .entry(key(name).as_str().to_owned())
            .or_default()
            .extend(std::iter::repeat_n(failure, times));
    }

    fn breaks(&self, name: &str, failure: ReadFail) {
        self.broken
            .lock()
            .unwrap()
            .insert(key(name).as_str().to_owned(), failure);
    }

    fn requests(&self) -> Vec<GetEffectiveRequest> {
        self.requests.lock().unwrap().clone()
    }

    fn reads_of(&self, name: &str) -> usize {
        self.requests()
            .iter()
            .filter(|r| r.key.as_str() == key(name).as_str())
            .count()
    }
}

#[async_trait]
impl SettingsReaderClient for FakeReader {
    async fn get_effective(
        &self,
        _: &SecurityContext,
        req: GetEffectiveRequest,
    ) -> Result<EffectiveValueResponse, CanonicalError> {
        self.requests.lock().unwrap().push(req.clone());
        if let Some(failure) = self.broken.lock().unwrap().get(req.key.as_str()) {
            return Err(failure.error(&req.key));
        }
        if let Some(failure) = self
            .failures
            .lock()
            .unwrap()
            .get_mut(req.key.as_str())
            .and_then(VecDeque::pop_front)
        {
            return Err(failure.error(&req.key));
        }
        let value = self
            .values
            .lock()
            .unwrap()
            .get(req.key.as_str())
            .cloned()
            .unwrap_or_else(|| json!("value-1"));
        let gate = {
            let gate = self.gate.lock().unwrap();
            if gate.as_ref().is_some_and(|(k, _)| k == req.key.as_str()) {
                gate.as_ref().map(|(_, s)| Arc::clone(s))
            } else {
                None
            }
        };
        // Only the first read of the key after the gate was armed waits.
        if let Some(semaphore) = gate
            && !self.gate_spent.swap(true, Ordering::SeqCst)
        {
            self.waiting.fetch_add(1, Ordering::SeqCst);
            semaphore.acquire().await.expect("the gate stays").forget();
        }
        Ok(EffectiveValueResponse {
            key: req.key,
            scope: req.scope,
            value,
            source: EffectiveSource::OwnOverride,
            source_scope: None,
            traits: json!({}),
            inheritance_trail: Vec::new(),
        })
    }

    async fn get_effective_bulk(
        &self,
        _: &SecurityContext,
        _: BulkSelector,
        _: String,
    ) -> Result<Vec<BulkOutcome>, CanonicalError> {
        panic!("a fresh read never goes through the bulk read");
    }

    async fn resolve_secret(
        &self,
        _: &SecurityContext,
        _: SecretHandle,
    ) -> Result<SecretString, CanonicalError> {
        panic!("the client never resolves a secret");
    }
}

// ---------------------------------------------------------------------------
// The handler and the log
// ---------------------------------------------------------------------------

type Respond = dyn Fn(&ChangeBundle) -> Vec<(SettingKey, ActivationOutcome)> + Send + Sync;

/// A handler that records its bundles and answers as scripted: by default it
/// applies every bundled value.
struct Recorder {
    bundles: Mutex<Vec<ChangeBundle>>,
    respond: Box<Respond>,
    panics: AtomicUsize,
}

impl Recorder {
    fn applying() -> Arc<Self> {
        Self::answering(|bundle| {
            bundle
                .changes
                .iter()
                .map(|c| {
                    (
                        c.key.clone(),
                        ActivationOutcome::Success {
                            applied_value: c.value.value.clone(),
                        },
                    )
                })
                .collect()
        })
    }

    fn answering(
        respond: impl Fn(&ChangeBundle) -> Vec<(SettingKey, ActivationOutcome)> + Send + Sync + 'static,
    ) -> Arc<Self> {
        Arc::new(Self {
            bundles: Mutex::new(Vec::new()),
            respond: Box::new(respond),
            panics: AtomicUsize::new(0),
        })
    }

    fn bundles(&self) -> Vec<ChangeBundle> {
        self.bundles.lock().unwrap().clone()
    }
}

#[async_trait]
impl SettingChangeHandler for Recorder {
    async fn on_change(&self, bundle: ChangeBundle) -> Vec<(SettingKey, ActivationOutcome)> {
        assert!(
            !spend(&self.panics),
            "the handler was told to panic (a scripted failure)"
        );
        let outcomes = (self.respond)(&bundle);
        self.bundles.lock().unwrap().push(bundle);
        outcomes
    }
}

// ---------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------

struct Fixture {
    hub: Arc<ClientHub>,
    broker: Arc<ScriptedBroker>,
    backend: Arc<FakeBackend>,
    reader: Arc<FakeReader>,
    metrics: Arc<RecordingNotifyMetrics>,
    client: ActivationClient,
    logs: LogBuf,
    _guard: tracing::subscriber::DefaultGuard,
}

fn timing() -> ClientTiming {
    ClientTiming {
        backoff: Backoff::with_steps(Duration::from_millis(1), Duration::from_millis(4)),
        warn_every: Duration::from_secs(60),
        cleanup_deadline: Duration::from_secs(2),
        liveness: Duration::from_millis(5),
        healthy: Duration::from_millis(40),
        // Longer than any test waits: a read a test holds is never cut short
        // unless the test says so.
        fresh_read: Duration::from_secs(3600),
    }
}

/// The identity of the process in most tests.
fn instance() -> InstanceIdentity {
    InstanceIdentity::new(Uuid::from_u128(0x1d), Some("test-host"))
}

fn fixture(plugged: bool, timing: ClientTiming) -> Fixture {
    fixture_as(plugged, timing, instance())
}

/// A client of the process `identity`: tests that emulate two instances of one
/// gear build two, each over its own scripted service.
fn fixture_as(plugged: bool, timing: ClientTiming, identity: InstanceIdentity) -> Fixture {
    let logs = LogBuf::default();
    let guard = tracing::subscriber::set_default(
        tracing_subscriber::fmt()
            .with_writer(logs.clone())
            .with_ansi(false)
            .with_max_level(tracing::Level::TRACE)
            .finish(),
    );
    let hub = Arc::new(ClientHub::new());
    let broker = Arc::new(ScriptedBroker::on_topic(ACTIVATION_TOPIC_ID));
    if plugged {
        hub.register::<dyn EventBrokerApi>(Arc::clone(&broker) as Arc<dyn EventBrokerApi>);
    }
    let backend = Arc::new(FakeBackend::default());
    let reader = Arc::new(FakeReader::default());
    let metrics = Arc::new(RecordingNotifyMetrics::default());
    let client = ActivationClient::new(ConsumerDeps {
        backend: Arc::clone(&backend) as Arc<dyn ActivationBackend>,
        reader: Arc::clone(&reader) as Arc<dyn SettingsReaderClient>,
        metrics: Arc::clone(&metrics) as Arc<dyn NotifyMetrics>,
        hub: Arc::clone(&hub),
        timing,
        identity,
    });
    Fixture {
        hub,
        broker,
        backend,
        reader,
        metrics,
        client,
        logs,
        _guard: guard,
    }
}

impl Fixture {
    fn plug(&self) {
        self.hub
            .register::<dyn EventBrokerApi>(Arc::clone(&self.broker) as Arc<dyn EventBrokerApi>);
    }

    async fn subscribe(
        &self,
        keys: &[&str],
        at: Uuid,
        handler: &Arc<Recorder>,
    ) -> SubscriptionHandle {
        self.subscribe_as(GEAR, keys, at, handler).await
    }

    async fn subscribe_as(
        &self,
        subject: u128,
        keys: &[&str],
        at: Uuid,
        handler: &Arc<Recorder>,
    ) -> SubscriptionHandle {
        self.client
            .subscribe(
                &ctx_of(subject),
                keys.iter().map(|k| key(k)).collect(),
                at.to_string(),
                Arc::clone(handler) as Arc<dyn SettingChangeHandler>,
            )
            .await
            .expect("subscribe answers a handle")
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

fn state_of(handle: &SubscriptionHandle, name: &str) -> KeyState {
    handle
        .states()
        .get(&key(name))
        .cloned()
        .unwrap_or_else(|| panic!("`{name}` is not on the handle"))
}

fn is_registered(handle: &SubscriptionHandle, name: &str) -> bool {
    matches!(state_of(handle, name), KeyState::Registered { .. })
}

fn is_pending(handle: &SubscriptionHandle, name: &str, expected: PendingReason) -> bool {
    matches!(state_of(handle, name), KeyState::Pending { reason } if reason == expected)
}

fn wire(type_id: &str, at: Uuid, occurred: chrono::DateTime<Utc>, data: Value) -> EventV1 {
    traced_wire(type_id, at, occurred, data, None)
}

/// A delivered event as the broker streams it: stored, so its position is
/// stamped, and carrying `trace_parent` when given one.
fn traced_wire(
    type_id: &str,
    at: Uuid,
    occurred: chrono::DateTime<Utc>,
    data: Value,
    trace_parent: Option<String>,
) -> EventV1 {
    let builder = EventV1::builder_for(GtsTypeId::new(type_id), data)
        .tenant_id(at)
        .source("settings-service")
        .subject(at.to_string())
        .subject_type_id(GtsTypeId::new(ACTIVATION_SUBJECT_TYPE))
        .occurred_at(occurred);
    match trace_parent {
        Some(trace_parent) => builder.trace_parent(trace_parent),
        None => builder,
    }
    .build()
    .expect("a well-formed envelope")
    .stamp_position(StoredPosition {
        partition: 0,
        sequence: Sequence::assigned(1),
        sequence_time: Utc::now(),
    })
}

fn notification(at: Uuid, subscriber: u128, changes: &[(&str, Uuid)]) -> Value {
    json!({
        "tenant": at.to_string(),
        "subscriber": Uuid::from_u128(subscriber).to_string(),
        "changes": changes
            .iter()
            .map(|(k, r)| json!({ "key": key(k).as_str(), "change_ref": r }))
            .collect::<Vec<_>>(),
    })
}

fn deliver(f: &Fixture, at: Uuid, data: Value) -> EventV1 {
    let event = wire(CHANGE_NOTIFICATION_EVENT_TYPE, at, Utc::now(), data);
    f.broker.send(Ok(WireFrame::Event(event.clone())));
    event
}

/// A gear with a joined group and the keys registered at `at`.
async fn joined(
    f: &Fixture,
    keys: &[&str],
    at: Uuid,
    handler: &Arc<Recorder>,
) -> SubscriptionHandle {
    let handle = f.subscribe(keys, at, handler).await;
    until("the keys to register", || {
        keys.iter().all(|k| is_registered(&handle, k))
    })
    .await;
    until("the stream to open", || f.broker.opened_streams() >= 1).await;
    handle
}

// ---------------------------------------------------------------------------
// subscribe: input, states
// ---------------------------------------------------------------------------

#[tokio::test]
async fn subscribe_refuses_invalid_input_with_nothing_registered_and_no_group() {
    let f = fixture(true, timing());
    let handler = Recorder::applying();
    let ctx = ctx_of(GEAR);
    let as_handler = || Arc::clone(&handler) as Arc<dyn SettingChangeHandler>;

    let empty = f
        .client
        .subscribe(&ctx, vec![], tenant(1).to_string(), as_handler())
        .await
        .expect_err("empty keys");
    let malformed = f
        .client
        .subscribe(&ctx, vec![key("a")], "not-a-uuid".to_owned(), as_handler())
        .await
        .expect_err("a malformed tenant");

    for error in [empty, malformed] {
        assert!(
            matches!(error, CanonicalError::InvalidArgument { .. }),
            "{error:?}"
        );
    }
    assert!(f.backend.registered.lock().unwrap().is_empty());
    assert_eq!(f.client.groups().group_count(), 0);
    assert_eq!(f.broker.creates(), 0);
}

#[tokio::test]
async fn subscribe_returns_the_handle_at_once_every_key_pending_then_registers_each_with_its_fresh_value()
 {
    let f = fixture(true, timing());
    f.backend.hold_registrations();
    f.reader.set("a", json!("fresh-a"));
    let handler = Recorder::applying();

    let handle = f.subscribe(&["a", "b"], tenant(1), &handler).await;

    for name in ["a", "b"] {
        assert!(
            is_pending(&handle, name, PendingReason::Registering),
            "{name}: {:?}",
            state_of(&handle, name)
        );
    }
    f.backend.release_registrations();
    until("both keys to register", || {
        is_registered(&handle, "a") && is_registered(&handle, "b")
    })
    .await;
    let KeyState::Registered { value: Some(value) } = state_of(&handle, "a") else {
        panic!("registered with a value");
    };
    assert_eq!(value.value, json!("fresh-a"));
    // The read after the commit is the fresh read, once per key, as the caller's.
    let reads = f.reader.requests();
    assert_eq!(reads.len(), 2);
    assert!(reads.iter().all(|r| r.freshness == Freshness::Fresh));
    assert!(
        reads
            .iter()
            .all(|r| r.scope == format!("/tenants/{}", tenant(1)))
    );
    assert!(
        handler.bundles().is_empty(),
        "a first-pass registration hands the handler nothing: the consumer materializes from the states"
    );
}

#[tokio::test]
async fn the_subscriber_is_the_caller_and_each_key_registers_at_the_tenant() {
    let f = fixture(true, timing());
    let handler = Recorder::applying();
    let handle = f.subscribe(&["a", "b"], tenant(7), &handler).await;
    until("registration", || {
        is_registered(&handle, "a") && is_registered(&handle, "b")
    })
    .await;

    let calls = f.backend.registered.lock().unwrap().clone();
    assert_eq!(calls.len(), 2);
    for (subscriber, _, at, _) in calls {
        assert_eq!(subscriber, Uuid::from_u128(GEAR).to_string());
        assert_eq!(at, tenant(7));
    }
}

#[tokio::test]
async fn a_key_refused_a_403_is_refused_terminally_and_never_retried() {
    let f = fixture(true, timing());
    f.backend.register_always_fails("a", RegisterError::Denied);
    let handler = Recorder::applying();

    let handle = f.subscribe(&["a", "b"], tenant(1), &handler).await;
    until("b to register", || is_registered(&handle, "b")).await;

    assert!(matches!(
        state_of(&handle, "a"),
        KeyState::Refused {
            reason: RefusedReason::NotReadable
        }
    ));
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(f.backend.register_calls("a"), 1, "terminal: no retry");
    assert!(f.logs.text().contains("subscription refused"));
}

#[tokio::test]
async fn a_key_not_found_is_pending_retried_and_on_registering_hands_the_handler_a_bundle_without_change_ref()
 {
    let f = fixture(true, timing());
    f.backend
        .register_always_fails("late", RegisterError::NotFound);
    f.reader.set("late", json!("declared-later"));
    let handler = Recorder::applying();

    let handle = f.subscribe(&["late", "early"], tenant(1), &handler).await;
    until("early to register", || is_registered(&handle, "early")).await;
    until("late to be retried", || {
        f.backend.register_calls("late") >= 3
    })
    .await;
    assert!(is_pending(&handle, "late", PendingReason::NotFound));
    assert!(handler.bundles().is_empty());

    f.backend.declared("late");
    until("late to register", || is_registered(&handle, "late")).await;
    until("the bundle", || !handler.bundles().is_empty()).await;

    let bundles = handler.bundles();
    assert_eq!(bundles.len(), 1);
    assert_eq!(bundles[0].tenant, tenant(1).to_string());
    assert_eq!(bundles[0].changes.len(), 1);
    assert_eq!(bundles[0].changes[0].key, key("late"));
    assert_eq!(bundles[0].changes[0].change_ref, None, "it owes no report");
    assert_eq!(bundles[0].changes[0].value.value, json!("declared-later"));
    assert!(
        f.backend.reports().is_empty(),
        "the outcomes of a bundle without change_ref are not reported"
    );
}

#[tokio::test]
async fn a_retry_that_registers_delivers_its_read_when_no_notification_overlapped() {
    let f = fixture(true, timing());
    f.backend
        .register_fails("k", RegisterError::Unavailable("internal".to_owned()), 1);
    f.reader.set("k", json!("read"));
    f.reader.block_next_read("k");
    let handler = Recorder::applying();
    let handle = f.subscribe(&["k"], tenant(1), &handler).await;
    until("the read to wait", || f.reader.reads_waiting() == 1).await;

    f.reader.open_gate("k");
    until("the key to register", || is_registered(&handle, "k")).await;
    until("the bundle", || !handler.bundles().is_empty()).await;

    let bundles = handler.bundles();
    assert_eq!(bundles.len(), 1);
    assert_eq!(bundles[0].changes[0].change_ref, None);
    assert_eq!(bundles[0].changes[0].value.value, json!("read"));
}

#[tokio::test]
async fn a_registration_read_overtaken_by_a_notification_delivers_nothing_after_it() {
    let f = fixture(true, timing());
    f.backend
        .register_fails("k", RegisterError::Unavailable("internal".to_owned()), 1);
    f.reader.set("k", json!("old"));
    f.reader.block_next_read("k");
    let handler = Recorder::applying();
    let handle = f.subscribe(&["k"], tenant(1), &handler).await;
    until("the stream to open", || f.broker.opened_streams() >= 1).await;
    until("the read to wait", || f.reader.reads_waiting() == 1).await;

    // The store changes and the group hands the handler the newer value while
    // the registration's read, which began before, is still out.
    f.reader.set("k", json!("new"));
    let change_ref = tenant(0xca);
    deliver(
        &f,
        tenant(1),
        notification(tenant(1), GEAR, &[("k", change_ref)]),
    );
    until("the notification bundle", || !handler.bundles().is_empty()).await;
    until("its report", || !f.backend.reports().is_empty()).await;

    f.reader.open_gate("k");
    until("the key to register", || is_registered(&handle, "k")).await;
    tokio::time::sleep(Duration::from_millis(40)).await;

    let bundles = handler.bundles();
    assert_eq!(bundles.len(), 1, "no registration bundle after it");
    assert_eq!(bundles[0].changes[0].change_ref, Some(change_ref));
    assert_eq!(bundles[0].changes[0].value.value, json!("new"));
    let reports = f.backend.reports();
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].1.change_ref, change_ref.to_string());
}

#[tokio::test]
async fn an_unavailable_registration_is_pending_with_nothing_registered_and_registers_when_it_recovers()
 {
    // A lock wait that ran out, an authorization service that cannot decide and
    // a database error all arrive as the same refusal.
    let f = fixture(true, timing());
    f.backend.register_fails(
        "a",
        RegisterError::Unavailable("lock_exhausted".to_owned()),
        3,
    );
    let handler = Recorder::applying();

    let handle = f.subscribe(&["a"], tenant(1), &handler).await;
    until("the retries", || f.backend.register_calls("a") >= 2).await;
    assert!(
        is_pending(&handle, "a", PendingReason::Unavailable) || is_registered(&handle, "a"),
        "pending while it fails"
    );
    until("registered", || is_registered(&handle, "a")).await;
    assert_eq!(
        f.backend.register_calls("a"),
        4,
        "three failures and the success"
    );
}

#[tokio::test]
async fn a_fresh_read_failing_after_the_commit_keeps_the_registration_and_retries_from_the_read() {
    let f = fixture(true, timing());
    f.reader.fails("a", ReadFail::Unavailable, 2);
    let handler = Recorder::applying();

    let handle = f.subscribe(&["a"], tenant(1), &handler).await;
    until("the first read to fail", || f.reader.reads_of("a") >= 1).await;
    until("registered", || is_registered(&handle, "a")).await;

    assert_eq!(
        f.backend.register_calls("a"),
        1,
        "the registration was kept: only the read is retried"
    );
    assert_eq!(f.reader.reads_of("a"), 3);
    // The retry that settled it hands the handler the fresh value.
    until("the bundle", || !handler.bundles().is_empty()).await;
    assert_eq!(handler.bundles()[0].changes[0].change_ref, None);
}

#[tokio::test]
async fn a_retired_key_registers_with_its_value_absent() {
    let f = fixture(true, timing());
    f.reader.breaks("old", ReadFail::Retired);
    let handler = Recorder::applying();

    let handle = f.subscribe(&["old"], tenant(1), &handler).await;
    until("registered", || is_registered(&handle, "old")).await;

    assert!(matches!(
        state_of(&handle, "old"),
        KeyState::Registered { value: None }
    ));
    assert_eq!(f.backend.register_calls("old"), 1);
    assert_eq!(f.reader.reads_of("old"), 1, "no retry of a settled key");
}

#[tokio::test]
async fn the_handle_wakes_the_caller_on_each_change_of_state() {
    let f = fixture(true, timing());
    f.backend.hold_registrations();
    let handler = Recorder::applying();
    let mut handle = f.subscribe(&["a"], tenant(1), &handler).await;
    f.backend.release_registrations();

    tokio::time::timeout(Duration::from_secs(5), async {
        while !is_registered(&handle, "a") {
            assert!(handle.changed().await);
        }
    })
    .await
    .expect("the handle announced the registration");
}

#[tokio::test]
async fn a_pending_keys_retries_wait_exponentially_with_jitter() {
    let mut t = timing();
    t.backoff = Backoff::with_steps(Duration::from_millis(20), Duration::from_millis(80));
    let f = fixture(true, t);
    f.backend
        .register_always_fails("a", RegisterError::NotFound);
    let handler = Recorder::applying();
    let _handle = f.subscribe(&["a"], tenant(1), &handler).await;
    until("six attempts", || f.backend.register_calls("a") >= 6).await;

    let at = f.backend.registered_at("a");
    // The wait before attempt k + 1 is drawn from `[ceiling / 2, ceiling]`, the
    // ceiling doubling from 20 ms to its 80 ms cap: 10, 20, 40, 40, 40 at least.
    for (index, floor_ms) in [10_u64, 20, 40, 40, 40].into_iter().enumerate() {
        let gap = at[index + 1].duration_since(at[index]);
        assert!(
            gap >= Duration::from_millis(floor_ms),
            "gap {index}: {gap:?} below its floor of {floor_ms} ms"
        );
    }
}

#[tokio::test]
async fn a_pending_key_is_logged_once_each_interval_not_on_every_retry() {
    let mut t = timing();
    t.warn_every = Duration::from_millis(120);
    let f = fixture(true, t);
    f.backend
        .register_always_fails("ghost", RegisterError::NotFound);
    let handler = Recorder::applying();
    let started = Instant::now();
    let _handle = f.subscribe(&["ghost"], tenant(1), &handler).await;

    tokio::time::sleep(Duration::from_millis(500)).await;

    // The bound follows the measured time, not the nominal 500 ms: a warning
    // needs a full interval since the last, so at most one per interval elapsed.
    let intervals = intervals_in(started.elapsed(), Duration::from_millis(120));
    let warnings = f.logs.count("subscription still pending");
    let retries = f.backend.register_calls("ghost");
    assert!(
        (1..=intervals.max(1)).contains(&warnings),
        "one warning per interval over {intervals} intervals, got {warnings}\n{}",
        f.logs.text()
    );
    assert!(
        retries > warnings * 3,
        "{retries} retries, {warnings} warnings"
    );
    assert!(f.logs.text().contains("NotFound"), "the reason is named");
    assert!(
        f.logs.text().contains(key("ghost").as_str()),
        "the key is named"
    );
}

#[tokio::test]
async fn a_multi_key_subscribe_registers_each_key_on_its_own() {
    let f = fixture(true, timing());
    f.backend
        .register_always_fails("b", RegisterError::NotFound);
    let handler = Recorder::applying();

    let handle = f.subscribe(&["a", "b", "c"], tenant(1), &handler).await;
    until("a and c", || {
        is_registered(&handle, "a") && is_registered(&handle, "c")
    })
    .await;

    assert!(
        is_pending(&handle, "b", PendingReason::NotFound),
        "the call is not atomic across keys: one key's refusal leaves the others registered"
    );
}

// ---------------------------------------------------------------------------
// The interest limit and the group
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_65th_tenant_interest_is_refused_before_registering_anything() {
    let f = fixture(true, timing());
    let applier = Recorder::applying();
    let mut handles = Vec::new();
    for n in 0..MAX_TENANT_INTERESTS as u128 {
        handles.push(f.subscribe(&["a"], tenant(100 + n), &applier).await);
    }
    for handle in &handles {
        until("the 64 tenants to register", || is_registered(handle, "a")).await;
    }
    let before = f.backend.registered.lock().unwrap().len();

    let refused = f.subscribe(&["a", "b"], tenant(999), &applier).await;

    for name in ["a", "b"] {
        assert!(matches!(
            state_of(&refused, name),
            KeyState::Refused {
                reason: RefusedReason::InterestLimit
            }
        ));
    }
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(
        f.backend.registered.lock().unwrap().len(),
        before,
        "nothing was registered for the refused tenant"
    );
    assert!(f.logs.text().contains("subscription refused"));
    // A tenant the gear already holds is not a new interest: still accepted.
    let again = f.subscribe(&["b"], tenant(100), &applier).await;
    until("the repeat to register", || is_registered(&again, "b")).await;
    assert_eq!(
        f.client
            .groups()
            .tenants_of(&Uuid::from_u128(GEAR).to_string())
            .unwrap()
            .len(),
        MAX_TENANT_INTERESTS
    );
}

#[tokio::test]
async fn one_group_serves_every_handle_of_a_gear_and_one_interest_per_tenant_joins_it() {
    let f = fixture(true, timing());
    let handler = Recorder::applying();
    let first = joined(&f, &["a"], tenant(1), &handler).await;
    let second = f.subscribe(&["b"], tenant(2), &handler).await;
    let third = f.subscribe(&["c"], tenant(1), &handler).await;
    until("registration", || {
        is_registered(&second, "b") && is_registered(&third, "c")
    })
    .await;
    until("the second join", || f.broker.joins().len() >= 2).await;

    assert_eq!(f.broker.creates(), 1, "one group per gear per process");
    assert_eq!(f.client.groups().group_count(), 1);
    let joins = f.broker.joins();
    let group = f.broker.creates.lock().unwrap()[0];
    assert!(joins.iter().all(|j| j.group == group));
    let last = joins.last().unwrap();
    let mut tenants: Vec<Uuid> = last
        .interests
        .iter()
        .map(event_broker_sdk::api::SubscriptionInterest::tenant_id)
        .collect();
    tenants.sort();
    assert_eq!(
        tenants,
        vec![tenant(1), tenant(2)],
        "one interest per subscription tenant"
    );
    for interest in &last.interests {
        assert_eq!(interest.topic().as_ref(), ACTIVATION_TOPIC_ID);
        let types: Vec<String> = interest
            .types()
            .iter()
            .map(|p| p.as_ref().to_owned())
            .collect();
        assert_eq!(types, vec![CHANGE_NOTIFICATION_EVENT_TYPE.to_owned()]);
    }
    assert!(
        f.broker.leaves() >= 1,
        "the first subscription was left for the wider one"
    );
    let seeks = f.broker.seeks.lock().unwrap().clone();
    assert!(
        seeks
            .iter()
            .flatten()
            .all(|s| format!("{:?}", s.value).contains("Latest")),
        "offsets start from the latest position"
    );
    drop((first, second, third));
}

#[tokio::test]
async fn two_gears_hold_two_groups_under_their_own_identities() {
    let f = fixture(true, timing());
    let handler = Recorder::applying();
    let one = f.subscribe_as(1, &["a"], tenant(1), &handler).await;
    let two = f.subscribe_as(2, &["a"], tenant(1), &handler).await;
    until("registration", || {
        is_registered(&one, "a") && is_registered(&two, "a")
    })
    .await;
    until("two groups", || f.broker.creates() == 2).await;

    assert_eq!(f.client.groups().group_count(), 2);
    let mut agents = f.broker.create_agents.lock().unwrap().clone();
    agents.sort();
    assert_eq!(
        agents,
        vec![
            format!("settings-consumer:{}", Uuid::from_u128(1)),
            format!("settings-consumer:{}", Uuid::from_u128(2)),
        ]
    );
}

#[tokio::test]
async fn the_group_is_deleted_with_the_last_handle_and_only_then() {
    let f = fixture(true, timing());
    let handler = Recorder::applying();
    let first = joined(&f, &["a"], tenant(1), &handler).await;
    let second = f.subscribe(&["b"], tenant(1), &handler).await;
    until("b", || is_registered(&second, "b")).await;
    let group = f.broker.creates.lock().unwrap()[0];

    drop(first);
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert!(f.broker.deletes().is_empty(), "a handle is still live");

    drop(second);
    until("the group to be deleted", || !f.broker.deletes().is_empty()).await;
    assert_eq!(f.broker.deletes(), vec![group]);
    assert!(f.broker.leaves() >= 1, "left before it was deleted");
    assert_eq!(f.client.groups().group_count(), 0);
    assert!(
        f.backend.unsubscribed.lock().unwrap().is_empty(),
        "dropping a handle unsubscribes nothing"
    );
}

#[tokio::test]
async fn a_subscribe_after_the_last_handle_went_creates_a_new_group() {
    let f = fixture(true, timing());
    let handler = Recorder::applying();
    let first = joined(&f, &["a"], tenant(1), &handler).await;
    drop(first);
    until("the first group to go", || !f.broker.deletes().is_empty()).await;

    let again = f.subscribe(&["a"], tenant(1), &handler).await;
    until("registered", || is_registered(&again, "a")).await;
    until("a second group", || f.broker.creates() == 2).await;
    assert_eq!(f.client.groups().group_count(), 1);
    assert_eq!(
        f.backend.register_calls("a"),
        2,
        "a repeated subscribe returns a new handle over the same subscription"
    );
}

#[tokio::test]
async fn dropping_a_handle_stops_its_retries_and_unsubscribes_nothing() {
    let f = fixture(true, timing());
    f.backend
        .register_always_fails("ghost", RegisterError::NotFound);
    let handler = Recorder::applying();
    let handle = f.subscribe(&["ghost"], tenant(1), &handler).await;
    until("a few retries", || f.backend.register_calls("ghost") >= 3).await;

    drop(handle);
    tokio::time::sleep(Duration::from_millis(30)).await;
    let after_drop = f.backend.register_calls("ghost");
    tokio::time::sleep(Duration::from_millis(60)).await;

    assert_eq!(
        f.backend.register_calls("ghost"),
        after_drop,
        "no retry after the drop"
    );
    assert!(f.backend.unsubscribed.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_refused_join_is_retried_on_the_one_group_and_leaks_none() {
    let f = fixture(true, timing());
    f.broker.join_refusals.store(3, Ordering::SeqCst);
    let handler = Recorder::applying();
    let handle = f.subscribe(&["a"], tenant(1), &handler).await;
    until("the stream to open", || f.broker.opened_streams() == 1).await;

    assert_eq!(f.broker.creates(), 1, "one group however many joins failed");
    let joins = f.broker.joins();
    assert_eq!(joins.len(), 4, "three refusals and the join that held");
    let group = f.broker.creates.lock().unwrap()[0];
    assert!(joins.iter().all(|j| j.group == group));
    assert!(f.logs.text().contains("join refused"));

    drop(handle);
    until("the group to be deleted", || !f.broker.deletes().is_empty()).await;
    assert_eq!(f.broker.deletes(), vec![group]);
}

#[tokio::test]
async fn a_refused_group_creation_is_retried_until_one_group_is_held() {
    let f = fixture(true, timing());
    f.broker.create_refusals.store(2, Ordering::SeqCst);
    let handler = Recorder::applying();
    let _handle = f.subscribe(&["a"], tenant(1), &handler).await;
    until("the stream to open", || f.broker.opened_streams() == 1).await;
    assert_eq!(f.broker.creates(), 1);
    assert!(f.logs.text().contains("consumer group refused"));
}

#[tokio::test]
async fn stop_leaves_and_deletes_the_group_and_refuses_new_handles() {
    let f = fixture(true, timing());
    let handler = Recorder::applying();
    let handle = joined(&f, &["a"], tenant(1), &handler).await;
    let group = f.broker.creates.lock().unwrap()[0];

    f.client.stop().await;

    assert_eq!(f.broker.deletes(), vec![group]);
    assert!(f.broker.leaves() >= 1);
    let error = f
        .client
        .subscribe(
            &ctx_of(GEAR),
            vec![key("b")],
            tenant(1).to_string(),
            Arc::clone(&handler) as Arc<dyn SettingChangeHandler>,
        )
        .await
        .expect_err("a stopped client takes no handle");
    assert!(
        matches!(error, CanonicalError::ServiceUnavailable { .. }),
        "{error:?}"
    );
    drop(handle);
    assert_eq!(f.broker.deletes().len(), 1, "never deleted twice");
}

#[tokio::test]
async fn a_group_that_cannot_be_deleted_or_left_is_a_warning_and_nothing_else() {
    let f = fixture(true, timing());
    f.broker.delete_fails.store(true, Ordering::SeqCst);
    let handler = Recorder::applying();
    let handle = joined(&f, &["a"], tenant(1), &handler).await;

    drop(handle);
    until("the deletion to be attempted", || {
        !f.broker.deletes().is_empty()
    })
    .await;
    until("the warning", || {
        f.logs.text().contains("consumer group not deleted")
    })
    .await;
    assert!(
        f.backend.unsubscribed.lock().unwrap().is_empty(),
        "no subscription is touched"
    );

    // A leave that never answers costs its share of the deadline, not the stop.
    let mut t = timing();
    t.cleanup_deadline = Duration::from_millis(300);
    let g = fixture(true, t);
    g.broker.leave_hangs.store(true, Ordering::SeqCst);
    let _held = joined(&g, &["a"], tenant(1), &handler).await;
    let started = Instant::now();
    g.client.stop().await;
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "bounded by the deadline"
    );
    assert_eq!(
        g.broker.deletes().len(),
        1,
        "the deletion still got what remained"
    );
    assert!(
        g.logs
            .text()
            .contains("leaving the activation topic failed")
    );
}

#[tokio::test]
async fn with_no_broker_the_keys_stay_registered_and_the_group_joins_once_one_resolves() {
    let f = fixture(false, timing());
    let handler = Recorder::applying();
    let handle = f.subscribe(&["a"], tenant(1), &handler).await;
    until("registered", || is_registered(&handle, "a")).await;
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert!(
        is_registered(&handle, "a"),
        "no broker is not a pending key"
    );
    assert_eq!(f.broker.creates(), 0);

    f.plug();
    until("the stream to open", || f.broker.opened_streams() == 1).await;
    assert_eq!(f.broker.creates(), 1);
    assert!(is_registered(&handle, "a"));
}

#[tokio::test]
async fn a_missing_join_grant_moves_registered_keys_to_pending_and_back_once_it_is_granted() {
    let f = fixture(true, timing());
    f.broker.join_refusals.store(4, Ordering::SeqCst);
    f.broker.refuse_as_grant.store(true, Ordering::SeqCst);
    f.reader.set("a", json!("kept-value"));
    let handler = Recorder::applying();

    let mut handle = f.subscribe(&["a"], tenant(1), &handler).await;
    until("the key to be held pending", || {
        is_pending(&handle, "a", PendingReason::BrokerGrantMissing)
    })
    .await;
    assert_eq!(
        f.backend.register_calls("a"),
        1,
        "the subscription is kept: it is not registered again"
    );
    assert!(f.logs.text().contains("join refused"));

    tokio::time::timeout(Duration::from_secs(5), async {
        while !is_registered(&handle, "a") {
            handle.changed().await;
        }
    })
    .await
    .expect("registered again once the join succeeds");
    let KeyState::Registered { value: Some(value) } = state_of(&handle, "a") else {
        panic!("the value it was registered with comes back");
    };
    assert_eq!(value.value, json!("kept-value"));
    assert_eq!(f.backend.register_calls("a"), 1);
    assert_eq!(
        f.reader.reads_of("a"),
        1,
        "no fresh read for a restored key"
    );
}

#[tokio::test]
async fn a_missing_define_grant_moves_registered_keys_to_pending_too() {
    let f = fixture(true, timing());
    f.broker.create_refusals.store(3, Ordering::SeqCst);
    f.broker.refuse_as_grant.store(true, Ordering::SeqCst);
    let handler = Recorder::applying();

    let handle = f.subscribe(&["a"], tenant(1), &handler).await;
    until("pending for the grant", || {
        is_pending(&handle, "a", PendingReason::BrokerGrantMissing)
    })
    .await;
    until("registered again", || is_registered(&handle, "a")).await;
    assert_eq!(f.broker.creates(), 1);
}

// ---------------------------------------------------------------------------
// unsubscribe
// ---------------------------------------------------------------------------

#[tokio::test]
async fn unsubscribe_deletes_each_key_under_the_callers_identity_and_closes_the_handle() {
    let f = fixture(true, timing());
    let handler = Recorder::applying();
    let handle = joined(&f, &["a", "b"], tenant(1), &handler).await;

    let answers = handle.unsubscribe(&ctx_of(GEAR)).await.expect("answers");

    assert_eq!(answers[&key("a")], UnsubscribeResult::Unsubscribed);
    assert_eq!(answers[&key("b")], UnsubscribeResult::Unsubscribed);
    assert!(
        handle.states().is_empty(),
        "unsubscribed keys leave the handle"
    );
    let calls = f.backend.unsubscribed.lock().unwrap().clone();
    assert_eq!(calls.len(), 2);
    assert!(
        calls
            .iter()
            .all(|(s, _, at)| *s == Uuid::from_u128(GEAR).to_string() && *at == tenant(1))
    );
    until("the group to go with its last handle", || {
        !f.broker.deletes().is_empty()
    })
    .await;
}

#[tokio::test]
async fn unsubscribe_answers_404_for_a_key_that_is_not_the_callers_and_unavailable_without_deleting()
 {
    let f = fixture(true, timing());
    f.backend
        .unsubscribe_fails("foreign", UnsubscribeFailure::NotFound, 1);
    f.backend
        .unsubscribe_fails("busy", UnsubscribeFailure::Unavailable, 1);
    let handler = Recorder::applying();
    let handle = joined(&f, &["foreign", "busy", "mine"], tenant(1), &handler).await;

    let first = handle.unsubscribe(&ctx_of(GEAR)).await.expect("answers");

    assert_eq!(first[&key("foreign")], UnsubscribeResult::NotFound);
    assert_eq!(first[&key("busy")], UnsubscribeResult::Unavailable);
    assert_eq!(first[&key("mine")], UnsubscribeResult::Unsubscribed);
    let left: Vec<_> = handle.states().keys().cloned().collect();
    assert_eq!(
        left,
        vec![key("busy")],
        "only the key answered Unavailable stays"
    );
    assert!(is_registered(&handle, "busy"), "with its state");
    assert!(f.broker.deletes().is_empty(), "the handle is still live");

    // The SDK retried nothing; the caller calls again, and the lock is free.
    assert_eq!(f.backend.unsubscribed.lock().unwrap().len(), 3);
    let second = handle.unsubscribe(&ctx_of(GEAR)).await.expect("answers");
    assert_eq!(second[&key("busy")], UnsubscribeResult::Unsubscribed);
    until("the group to be deleted", || !f.broker.deletes().is_empty()).await;
}

#[tokio::test]
async fn unsubscribe_stops_the_pending_retries_of_the_keys_it_answered() {
    let f = fixture(true, timing());
    f.backend
        .register_always_fails("ghost", RegisterError::NotFound);
    f.backend
        .unsubscribe_fails("held", UnsubscribeFailure::Unavailable, 1);
    f.backend
        .register_always_fails("held", RegisterError::NotFound);
    let handler = Recorder::applying();
    let handle = f.subscribe(&["ghost", "held"], tenant(1), &handler).await;
    until("retries of both", || {
        f.backend.register_calls("ghost") >= 2 && f.backend.register_calls("held") >= 2
    })
    .await;

    let answers = handle.unsubscribe(&ctx_of(GEAR)).await.expect("answers");
    assert_eq!(answers[&key("ghost")], UnsubscribeResult::Unsubscribed);
    assert_eq!(answers[&key("held")], UnsubscribeResult::Unavailable);
    tokio::time::sleep(Duration::from_millis(20)).await;
    let (ghost, held) = (
        f.backend.register_calls("ghost"),
        f.backend.register_calls("held"),
    );
    tokio::time::sleep(Duration::from_millis(60)).await;

    assert_eq!(
        f.backend.register_calls("ghost"),
        ghost,
        "its retries stopped"
    );
    assert!(
        f.backend.register_calls("held") > held,
        "a key answered Unavailable keeps its pending retries"
    );
}

/// Two handles of one gear over `a` at `tenant(1)`, both registered.
async fn two_handles(f: &Fixture) -> (SubscriptionHandle, SubscriptionHandle) {
    let handler = Recorder::applying();
    let first = joined(f, &["a"], tenant(1), &handler).await;
    let second = joined(f, &["a"], tenant(1), &handler).await;
    (first, second)
}

#[tokio::test]
async fn unsubscribe_on_one_handle_keeps_the_subscription_another_live_handle_still_holds() {
    let f = fixture(true, timing());
    let (first, second) = two_handles(&f).await;

    let answers = first.unsubscribe(&ctx_of(GEAR)).await.expect("answers");

    assert_eq!(answers[&key("a")], UnsubscribeResult::Unsubscribed);
    assert!(
        f.backend.unsubscribed.lock().unwrap().is_empty(),
        "the row is still the second handle's: nothing is deleted"
    );
    assert!(first.states().is_empty(), "the key left the first handle");
    assert!(is_registered(&second, "a"), "the second still holds it");

    let answers = second.unsubscribe(&ctx_of(GEAR)).await.expect("answers");
    assert_eq!(answers[&key("a")], UnsubscribeResult::Unsubscribed);
    assert_eq!(
        f.backend.unsubscribed.lock().unwrap().len(),
        1,
        "the last holder deletes"
    );
}

#[tokio::test]
async fn a_handle_over_the_key_at_another_tenant_is_not_a_holder() {
    let f = fixture(true, timing());
    let handler = Recorder::applying();
    let first = joined(&f, &["a"], tenant(1), &handler).await;
    let _other = joined(&f, &["a"], tenant(2), &handler).await;

    first.unsubscribe(&ctx_of(GEAR)).await.expect("answers");

    let calls = f.backend.unsubscribed.lock().unwrap().clone();
    assert_eq!(calls.len(), 1, "the tenant's row is deleted");
    assert_eq!(calls[0].2, tenant(1));
}

#[tokio::test]
async fn a_dropped_handle_is_not_a_holder() {
    let f = fixture(true, timing());
    let (first, second) = two_handles(&f).await;
    drop(second);

    first.unsubscribe(&ctx_of(GEAR)).await.expect("answers");

    assert_eq!(f.backend.unsubscribed.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn two_concurrent_unsubscribes_of_one_key_delete_it_exactly_once() {
    let f = fixture(true, timing());
    let (first, second) = two_handles(&f).await;
    let ctx = ctx_of(GEAR);

    let (one, two) = tokio::join!(first.unsubscribe(&ctx), second.unsubscribe(&ctx));

    assert_eq!(
        one.expect("answers")[&key("a")],
        UnsubscribeResult::Unsubscribed
    );
    assert_eq!(
        two.expect("answers")[&key("a")],
        UnsubscribeResult::Unsubscribed
    );
    assert_eq!(
        f.backend.unsubscribed.lock().unwrap().len(),
        1,
        "exactly one backend call"
    );
}

// ---------------------------------------------------------------------------
// Receiving
// ---------------------------------------------------------------------------

fn at_receipt(f: &Fixture) -> Vec<(&'static str, Duration)> {
    f.metrics.delivered.lock().unwrap().clone()
}

#[tokio::test]
async fn a_notification_is_re_read_fresh_per_key_handed_to_the_handler_and_its_outcomes_reported() {
    let f = fixture(true, timing());
    let handler = Recorder::applying();
    let _handle = joined(&f, &["a", "b"], tenant(1), &handler).await;
    f.reader.set("a", json!({ "z": 1, "a": [true] }));
    f.reader.set("b", json!("two"));
    let before = f.reader.requests().len();
    let (ref_a, ref_b) = (tenant(0xca), tenant(0xcb));

    deliver(
        &f,
        tenant(1),
        notification(tenant(1), GEAR, &[("a", ref_a), ("b", ref_b)]),
    );
    until("the handler", || !handler.bundles().is_empty()).await;
    until("both reports", || f.backend.reports().len() == 2).await;

    let reads: Vec<_> = f.reader.requests()[before..].to_vec();
    assert_eq!(reads.len(), 2, "one store read per notified key");
    assert!(reads.iter().all(|r| r.freshness == Freshness::Fresh));
    let bundle = &handler.bundles()[0];
    assert_eq!(bundle.tenant, tenant(1).to_string());
    let by_key: BTreeMap<_, _> = bundle
        .changes
        .iter()
        .map(|c| (c.key.clone(), (c.change_ref, c.value.value.clone())))
        .collect();
    assert_eq!(
        by_key[&key("a")],
        (Some(ref_a), json!({ "z": 1, "a": [true] }))
    );
    assert_eq!(by_key[&key("b")], (Some(ref_b), json!("two")));
    let reports = f.backend.reports();
    let mut seen: Vec<_> = reports
        .iter()
        .map(|(subscriber, r)| {
            (
                subscriber.clone(),
                r.key.clone(),
                r.change_ref.clone(),
                r.tenant.clone(),
                r.status.clone(),
                r.applied_value.clone(),
            )
        })
        .collect();
    seen.sort();
    let me = Uuid::from_u128(GEAR).to_string();
    assert_eq!(
        seen,
        vec![
            (
                me.clone(),
                key("a").as_str().to_owned(),
                ref_a.to_string(),
                tenant(1).to_string(),
                "success".to_owned(),
                Some(r#"{"a":[true],"z":1}"#.to_owned())
            ),
            (
                me,
                key("b").as_str().to_owned(),
                ref_b.to_string(),
                tenant(1).to_string(),
                "success".to_owned(),
                Some(r#""two""#.to_owned())
            ),
        ]
    );
}

#[tokio::test]
async fn a_failure_and_a_secret_are_reported_as_a_detail_and_a_hash() {
    let f = fixture(true, timing());
    let handler = Recorder::answering(|bundle| {
        bundle
            .changes
            .iter()
            .map(|c| {
                let outcome = if c.key == key("secret") {
                    ActivationOutcome::SecretSuccess {
                        plaintext: SecretString::from("hunter2"),
                    }
                } else {
                    ActivationOutcome::Failed {
                        detail: "pool rebuild failed".to_owned(),
                    }
                };
                (c.key.clone(), outcome)
            })
            .collect()
    });
    let _handle = joined(&f, &["secret", "pool"], tenant(1), &handler).await;

    deliver(
        &f,
        tenant(1),
        notification(
            tenant(1),
            GEAR,
            &[("secret", tenant(0xc1)), ("pool", tenant(0xc2))],
        ),
    );
    until("both reports", || f.backend.reports().len() == 2).await;

    let reports: BTreeMap<String, BackResponse> = f
        .backend
        .reports()
        .into_iter()
        .map(|(_, r)| (r.key.clone(), r))
        .collect();
    let secret = &reports[key("secret").as_str()];
    assert_eq!(secret.status, "success");
    assert_eq!(
        secret.applied_value.as_deref(),
        Some(
            settings_service_sdk::canonical::secret_hash(
                key("secret").as_str(),
                &tenant(1).to_string(),
                &SecretString::from("hunter2")
            )
            .unwrap()
            .as_str()
        ),
        "the hash is salted by the key and the tenant, computed in the consumer's process"
    );
    let pool = &reports[key("pool").as_str()];
    assert_eq!(pool.status, "failed");
    assert_eq!(pool.detail.as_deref(), Some("pool rebuild failed"));
    assert_eq!(pool.applied_value, None);
}

#[tokio::test]
async fn a_notification_for_another_subscriber_is_dropped_silently_but_observed() {
    let f = fixture(true, timing());
    let handler = Recorder::applying();
    let _handle = joined(&f, &["a"], tenant(1), &handler).await;
    let reads = f.reader.requests().len();
    let logged = f.logs.text().len();

    deliver(
        &f,
        tenant(1),
        notification(tenant(1), 0xf0e, &[("a", tenant(0xca))]),
    );
    until("the delivery to be observed", || !at_receipt(&f).is_empty()).await;
    tokio::time::sleep(Duration::from_millis(30)).await;

    assert!(handler.bundles().is_empty());
    assert_eq!(f.reader.requests().len(), reads, "nothing was re-read");
    assert!(f.backend.reports().is_empty());
    assert_eq!(
        at_receipt(&f).len(),
        1,
        "a dropped delivery is observed too"
    );
    assert_eq!(
        f.logs.text().len(),
        logged,
        "a foreign subscriber's notification leaves no log line"
    );
}

#[tokio::test]
async fn a_malformed_delivery_is_dropped_with_a_warning_naming_its_id_and_still_observed() {
    let f = fixture(true, timing());
    let handler = Recorder::applying();
    let _handle = joined(&f, &["a"], tenant(1), &handler).await;
    let good = notification(tenant(1), GEAR, &[("a", tenant(0xca))]);
    let mut cases: Vec<(EventV1, &str)> = Vec::new();
    cases.push((
        wire(
            CACHE_INVALIDATE_EVENT_TYPE,
            tenant(1),
            Utc::now(),
            good.clone(),
        ),
        "another type",
    ));
    let mut bad_tenant = good.clone();
    bad_tenant["tenant"] = json!("not-a-uuid");
    cases.push((
        wire(
            CHANGE_NOTIFICATION_EVENT_TYPE,
            tenant(1),
            Utc::now(),
            bad_tenant,
        ),
        "tenant",
    ));
    let mut no_subscriber = good.clone();
    no_subscriber.as_object_mut().unwrap().remove("subscriber");
    cases.push((
        wire(
            CHANGE_NOTIFICATION_EVENT_TYPE,
            tenant(1),
            Utc::now(),
            no_subscriber,
        ),
        "subscriber",
    ));
    let mut bad_changes = good;
    bad_changes["changes"] = json!([{ "key": "k" }]);
    cases.push((
        wire(
            CHANGE_NOTIFICATION_EVENT_TYPE,
            tenant(1),
            Utc::now(),
            bad_changes,
        ),
        "changes",
    ));

    for (event, _) in &cases {
        f.broker.send(Ok(WireFrame::Event(event.clone())));
    }
    until("every delivery to be observed", || {
        at_receipt(&f).len() == cases.len()
    })
    .await;
    until("the warnings", || {
        f.logs.count("malformed change_notification") == cases.len()
    })
    .await;

    for (event, what) in &cases {
        assert!(
            f.logs.text().contains(&event.id().to_string()),
            "{what}: the event's id is named"
        );
    }
    assert!(handler.bundles().is_empty(), "the handler is never invoked");
}

#[tokio::test]
async fn publish_to_deliver_is_observed_at_receipt_from_occurred_at() {
    let f = fixture(true, timing());
    let handler = Recorder::applying();
    let _handle = joined(&f, &["a"], tenant(1), &handler).await;

    let published = Utc::now() - chrono::Duration::seconds(5);
    f.broker.send(Ok(WireFrame::Event(wire(
        CHANGE_NOTIFICATION_EVENT_TYPE,
        tenant(1),
        published,
        notification(tenant(1), GEAR, &[("a", tenant(0xca))]),
    ))));
    until("the observation", || !at_receipt(&f).is_empty()).await;

    let (event, elapsed) = at_receipt(&f)[0];
    assert_eq!(event, "change_notification");
    assert!(elapsed >= Duration::from_secs(5), "{elapsed:?}");
    assert!(elapsed < Duration::from_secs(30), "{elapsed:?}");
}

#[tokio::test]
async fn a_failed_re_read_leaves_the_key_out_and_reports_nothing_for_it() {
    for failure in [ReadFail::Unavailable, ReadFail::Retired, ReadFail::NotFound] {
        let f = fixture(true, timing());
        let handler = Recorder::applying();
        let _handle = joined(&f, &["ok", "down"], tenant(1), &handler).await;
        f.reader.breaks("down", failure);

        deliver(
            &f,
            tenant(1),
            notification(
                tenant(1),
                GEAR,
                &[("ok", tenant(0xc1)), ("down", tenant(0xc2))],
            ),
        );
        until("the handler", || !handler.bundles().is_empty()).await;
        until("the report", || !f.backend.reports().is_empty()).await;
        tokio::time::sleep(Duration::from_millis(20)).await;

        let bundle = &handler.bundles()[0];
        let keys: Vec<_> = bundle.changes.iter().map(|c| c.key.clone()).collect();
        assert_eq!(keys, vec![key("ok")], "the unreadable key is left out");
        let reports = f.backend.reports();
        assert_eq!(reports.len(), 1, "and reports nothing");
        assert_eq!(reports[0].1.key, key("ok").as_str());
    }
}

#[tokio::test]
async fn a_notified_key_no_live_handle_holds_is_not_re_read() {
    let f = fixture(true, timing());
    let handler = Recorder::applying();
    let _handle = joined(&f, &["held"], tenant(1), &handler).await;
    let before = f.reader.requests().len();

    deliver(
        &f,
        tenant(1),
        notification(
            tenant(1),
            GEAR,
            &[("held", tenant(0xc1)), ("other", tenant(0xc2))],
        ),
    );
    until("the handler", || !handler.bundles().is_empty()).await;
    until("the report", || !f.backend.reports().is_empty()).await;

    assert_eq!(
        f.reader.requests().len() - before,
        1,
        "only the held key was read"
    );
    assert_eq!(f.reader.reads_of("other"), 0);
    assert_eq!(handler.bundles()[0].changes.len(), 1);
}

#[tokio::test]
async fn a_handle_only_hears_the_keys_it_holds_at_the_tenant_the_notification_names() {
    let f = fixture(true, timing());
    let one = Recorder::applying();
    let two = Recorder::applying();
    let _a = joined(&f, &["a"], tenant(1), &one).await;
    let b = f.subscribe(&["b"], tenant(2), &two).await;
    until("b", || is_registered(&b, "b")).await;
    until("the wider join", || f.broker.joins().len() >= 2).await;
    until("its stream", || f.broker.opened_streams() >= 2).await;

    deliver(
        &f,
        tenant(2),
        notification(tenant(2), GEAR, &[("a", tenant(0xc1)), ("b", tenant(0xc2))]),
    );
    until("the second handler", || !two.bundles().is_empty()).await;
    tokio::time::sleep(Duration::from_millis(20)).await;

    assert!(
        one.bundles().is_empty(),
        "a handle at another tenant is not told"
    );
    assert_eq!(two.bundles()[0].changes.len(), 1);
    assert_eq!(two.bundles()[0].changes[0].key, key("b"));
}

#[tokio::test]
async fn a_repeated_notification_is_re_read_re_applied_and_re_reported_never_suppressed() {
    let f = fixture(true, timing());
    let handler = Recorder::applying();
    let _handle = joined(&f, &["a"], tenant(1), &handler).await;
    let before = f.reader.requests().len();
    let data = notification(tenant(1), GEAR, &[("a", tenant(0xca))]);

    deliver(&f, tenant(1), data.clone());
    deliver(&f, tenant(1), data);
    until("two reports", || f.backend.reports().len() == 2).await;

    assert_eq!(handler.bundles().len(), 2);
    assert_eq!(f.reader.requests().len() - before, 2);
    let reports = f.backend.reports();
    assert_eq!(
        reports[0].1, reports[1].1,
        "the repeat reports as the first did"
    );
}

#[tokio::test]
async fn an_outcome_for_a_key_outside_the_bundle_is_not_reported() {
    let f = fixture(true, timing());
    let handler = Recorder::answering(|_| {
        vec![(
            key("stranger"),
            ActivationOutcome::Success {
                applied_value: json!(1),
            },
        )]
    });
    let _handle = joined(&f, &["a"], tenant(1), &handler).await;

    deliver(
        &f,
        tenant(1),
        notification(tenant(1), GEAR, &[("a", tenant(0xca))]),
    );
    until("the handler", || !handler.bundles().is_empty()).await;
    tokio::time::sleep(Duration::from_millis(30)).await;

    assert!(f.backend.reports().is_empty());
}

#[tokio::test]
async fn a_handler_that_panics_costs_its_delivery_not_the_group() {
    let f = fixture(true, timing());
    let handler = Recorder::applying();
    handler.panics.store(1, Ordering::SeqCst);
    let _handle = joined(&f, &["a"], tenant(1), &handler).await;

    deliver(
        &f,
        tenant(1),
        notification(tenant(1), GEAR, &[("a", tenant(0xc1))]),
    );
    until("the panic to be logged", || {
        f.logs.text().contains("a change handler panicked")
    })
    .await;
    deliver(
        &f,
        tenant(1),
        notification(tenant(1), GEAR, &[("a", tenant(0xc2))]),
    );
    until("the next delivery to be handled", || {
        f.backend.reports().len() == 1
    })
    .await;
    assert_eq!(
        f.backend.reports()[0].1.change_ref,
        tenant(0xc2).to_string()
    );
}

#[tokio::test]
async fn a_delivery_continues_the_publishers_trace_and_a_terminal_frame_rejoins_the_same_group() {
    let f = fixture(true, timing());
    let handler = Recorder::applying();
    let _handle = joined(&f, &["a"], tenant(1), &handler).await;
    let event = traced_wire(
        CHANGE_NOTIFICATION_EVENT_TYPE,
        tenant(1),
        Utc::now(),
        notification(tenant(1), GEAR, &[("a", tenant(0xca))]),
        Some("00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01".to_owned()),
    );
    f.broker.send(Ok(WireFrame::Event(event)));
    until("the report", || f.backend.reports().len() == 1).await;

    f.broker.send(Ok(WireFrame::Control {
        code: event_broker_sdk::ControlCode::Terminal,
        positions: Vec::new(),
        reason: None,
    }));
    until("a second join", || f.broker.joins().len() == 2).await;
    assert_eq!(f.broker.creates(), 1, "through the same group");
}

// ---------------------------------------------------------------------------
// report_outcome
// ---------------------------------------------------------------------------

#[tokio::test]
async fn report_outcome_answers_applied_or_ignored_and_encodes_the_value_first() {
    let f = fixture(true, timing());
    let ctx = ctx_of(GEAR);
    f.backend.report_answers.lock().unwrap().extend([
        Ok(crate::domain::activation::ReportResult::Applied),
        Ok(crate::domain::activation::ReportResult::Ignored),
    ]);

    let applied = f
        .client
        .report_outcome(
            &ctx,
            tenant(0xca),
            tenant(1).to_string(),
            key("a"),
            ActivationOutcome::Success {
                applied_value: json!({ "b": 1, "a": 2 }),
            },
        )
        .await
        .expect("answers");
    let ignored = f
        .client
        .report_outcome(
            &ctx,
            tenant(0xcb),
            tenant(1).to_string(),
            key("a"),
            ActivationOutcome::Failed {
                detail: "nope".to_owned(),
            },
        )
        .await
        .expect("answers");

    assert_eq!(applied, ReportResult::Applied);
    assert_eq!(ignored, ReportResult::Ignored);
    let reports = f.backend.reports();
    assert_eq!(
        reports[0].1.applied_value.as_deref(),
        Some(r#"{"a":2,"b":1}"#)
    );
    assert_eq!(
        reports[0].0,
        Uuid::from_u128(GEAR).to_string(),
        "the subscriber is the caller"
    );
}

#[tokio::test]
async fn report_outcome_refuses_malformed_input_with_nothing_reported() {
    let f = fixture(true, timing());
    let ctx = ctx_of(GEAR);
    // A value outside the number domain never reaches the service.
    let beyond: Value = serde_json::from_str("9007199254740993").unwrap();
    let not_canonical = f
        .client
        .report_outcome(
            &ctx,
            tenant(0xca),
            tenant(1).to_string(),
            key("a"),
            ActivationOutcome::Success {
                applied_value: beyond,
            },
        )
        .await
        .expect_err("not canonical");
    assert!(matches!(
        not_canonical,
        CanonicalError::InvalidArgument { .. }
    ));
    assert!(f.backend.reports().is_empty());

    // A tenant that is not a UUID is the service's refusal, mapped as one.
    f.backend
        .report_answers
        .lock()
        .unwrap()
        .push_back(Err(DomainError::Validation {
            field: "tenant".to_owned(),
            code: crate::field::VALIDATION,
            message: "`tenant` is not a UUID".to_owned(),
        }));
    let malformed = f
        .client
        .report_outcome(
            &ctx,
            tenant(0xca),
            "not-a-uuid".to_owned(),
            key("a"),
            ActivationOutcome::Failed {
                detail: "x".to_owned(),
            },
        )
        .await
        .expect_err("malformed tenant");
    assert!(matches!(malformed, CanonicalError::InvalidArgument { .. }));
    // A secret cannot be salted by a tenant that is not one: refused before the call.
    let before = f.backend.reports().len();
    let secret = f
        .client
        .report_outcome(
            &ctx,
            tenant(0xca),
            "not-a-uuid".to_owned(),
            key("a"),
            ActivationOutcome::SecretSuccess {
                plaintext: SecretString::from("x"),
            },
        )
        .await
        .expect_err("a malformed tenant");
    // It names the tenant, not the applied value: the value is fine.
    let CanonicalError::InvalidArgument {
        ctx: toolkit_canonical_errors::InvalidArgument::FieldViolations { field_violations },
        ..
    } = secret
    else {
        panic!("a field violation");
    };
    assert_eq!(field_violations.len(), 1);
    assert_eq!(field_violations[0].field, "tenant");
    assert_eq!(f.backend.reports().len(), before);
}

#[tokio::test]
async fn a_report_that_errors_is_answered_once_and_never_retried_by_the_sdk() {
    let f = fixture(true, timing());
    f.backend
        .report_answers
        .lock()
        .unwrap()
        .push_back(Err(DomainError::Unavailable {
            detail: "the audit append failed".to_owned(),
        }));
    let outcome = || ActivationOutcome::Success {
        applied_value: json!("v"),
    };

    let failed = f
        .client
        .report_outcome(
            &ctx_of(GEAR),
            tenant(0xca),
            tenant(1).to_string(),
            key("a"),
            outcome(),
        )
        .await
        .expect_err("the service could not record it");
    assert!(
        matches!(failed, CanonicalError::ServiceUnavailable { .. }),
        "{failed:?}"
    );
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(f.backend.reports().len(), 1, "one attempt, no retry");

    // The consumer's repeat of the same report applies once: it is a new call.
    let again = f
        .client
        .report_outcome(
            &ctx_of(GEAR),
            tenant(0xca),
            tenant(1).to_string(),
            key("a"),
            outcome(),
        )
        .await
        .expect("the repeat is answered");
    assert_eq!(again, ReportResult::Applied);
    assert_eq!(f.backend.reports().len(), 2);
}

#[tokio::test]
async fn a_report_refused_to_a_person_is_permission_denied_once_and_never_retried() {
    let f = fixture(true, timing());
    f.backend
        .report_answers
        .lock()
        .unwrap()
        .push_back(Err(DomainError::Unauthorized {
            resource: settings_service_sdk::gts::VALUE_SCHEMA,
        }));
    let person = SecurityContext::builder()
        .subject_id(Uuid::from_u128(GEAR))
        .subject_tenant_id(tenant(0xa0))
        .subject_type(crate::domain::stepup::USER_SUBJECT_TYPE)
        .build()
        .expect("a context");

    let refused = f
        .client
        .report_outcome(
            &person,
            tenant(0xca),
            tenant(1).to_string(),
            key("a"),
            ActivationOutcome::Success {
                applied_value: json!("v"),
            },
        )
        .await
        .expect_err("a person is no subscriber");
    assert!(
        matches!(refused, CanonicalError::PermissionDenied { .. }),
        "{refused:?}"
    );
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(f.backend.reports().len(), 1, "one attempt, no retry");
}

/// The in-process backend over the real Subscription Manager and Recorder: a
/// caller whose subject is a person gets the terminal answers the client never
/// retries — a denied registration, nothing of its own to unsubscribe, a
/// refused report — and nothing is read, locked or written.
#[tokio::test]
async fn the_service_backend_refuses_a_person_terminally_and_writes_nothing() {
    use crate::domain::activation::testing::InProcessLocks;
    use crate::domain::activation::{ActivationRecorder, KeyLocks, ReadGate, SubscriptionManager};
    use crate::domain::resolution::scope_class;
    use crate::infra::storage::activation_repo::ActivationRepo;
    use crate::infra::storage::audit_store::AuditStore;
    use crate::infra::subscriptions::SubscriptionService;
    use crate::test_support::{FakeReadGate, ResolutionHarness};

    let base = ResolutionHarness::new().await;
    base.declare("flag", scope_class::CASCADING, json!(false))
        .await;
    let gate = FakeReadGate::allowing();
    let locks = InProcessLocks::with_budget(Duration::from_millis(60));
    let metrics = Arc::new(RecordingNotifyMetrics::default()) as Arc<dyn NotifyMetrics>;
    let manager = Arc::new(SubscriptionManager::new(
        Arc::clone(&base.resolver),
        ActivationRepo,
        Arc::clone(&gate) as Arc<dyn ReadGate>,
        Arc::clone(&metrics),
    ));
    let subscriptions = Arc::new(SubscriptionService::new(
        Arc::clone(&base.db),
        manager,
        Arc::clone(&locks) as Arc<dyn KeyLocks>,
    ));
    let recorder = Arc::new(ActivationRecorder::new(
        Arc::clone(&base.db),
        ActivationRepo,
        AuditStore,
        metrics,
    ));
    let backend = super::ServiceBackend::new(subscriptions, recorder);

    let root = base.tree.root;
    let key = base.key("flag");
    let identity = InstanceIdentity::new(Uuid::from_u128(0x1d), Some("host"));
    let person = crate::test_support::context_for(root);

    assert_eq!(
        backend.register(&person, &key, root, &identity).await,
        Err(RegisterError::Denied),
        "terminal: the key is `refused`, never retried"
    );
    assert_eq!(
        backend
            .unsubscribe(&person, &key, root, identity.instance_id())
            .await,
        Err(UnsubscribeFailure::NotFound)
    );
    let report = backend
        .report(
            &person,
            BackResponse {
                instance_id: Some(identity.instance_id()),
                change_ref: Uuid::from_u128(0xc0de).to_string(),
                tenant: root.to_string(),
                key: key.as_str().to_owned(),
                status: "success".to_owned(),
                applied_value: Some("false".to_owned()),
                detail: None,
            },
        )
        .await;
    assert!(
        matches!(report, Err(DomainError::Unauthorized { .. })),
        "{report:?}"
    );

    assert_eq!(gate.asked.load(Ordering::SeqCst), 0, "no decision asked");
    assert_eq!(locks.asks_for(key.as_str()), 0, "no lock taken");
    let conn = base.db.conn().expect("connection");
    assert!(
        crate::domain::activation::ActivationRepository::of_key(
            &ActivationRepo,
            &conn,
            key.as_str()
        )
        .await
        .expect("rows")
        .is_empty(),
        "nothing registered"
    );

    // The same backend takes a gear's registration.
    let gear = crate::test_support::service_context_for(root);
    backend
        .register(&gear, &key, root, &identity)
        .await
        .expect("a service principal registers");
}

#[tokio::test]
async fn a_failed_report_in_a_handler_path_is_logged_and_not_retried() {
    let f = fixture(true, timing());
    f.backend
        .report_answers
        .lock()
        .unwrap()
        .push_back(Err(DomainError::Unavailable {
            detail: "the audit append failed".to_owned(),
        }));
    let handler = Recorder::applying();
    let _handle = joined(&f, &["a"], tenant(1), &handler).await;

    deliver(
        &f,
        tenant(1),
        notification(tenant(1), GEAR, &[("a", tenant(0xca))]),
    );
    until("the report", || f.backend.reports().len() == 1).await;
    until("the warning", || {
        f.logs.text().contains("could not be reported")
    })
    .await;
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(f.backend.reports().len(), 1);
}

// ---------------------------------------------------------------------------
// What never reaches a log
// ---------------------------------------------------------------------------

#[tokio::test]
async fn no_value_hash_or_secret_reaches_a_log_line() {
    let f = fixture(true, timing());
    f.reader.set("a", json!("value-marker-7731"));
    f.reader.set("secret", json!("handle-marker-5519"));
    let handler = Recorder::answering(|bundle| {
        bundle
            .changes
            .iter()
            .map(|c| {
                let outcome = if c.key == key("secret") {
                    ActivationOutcome::SecretSuccess {
                        plaintext: SecretString::from("plaintext-marker-9921"),
                    }
                } else {
                    ActivationOutcome::Failed {
                        detail: "detail-is-contractually-free-of-values".to_owned(),
                    }
                };
                (c.key.clone(), outcome)
            })
            .collect()
    });
    f.backend.register_fails("a", RegisterError::NotFound, 2);
    f.reader.fails("a", ReadFail::Unavailable, 1);
    let _handle = joined(&f, &["a", "secret"], tenant(1), &handler).await;
    deliver(
        &f,
        tenant(1),
        notification(
            tenant(1),
            GEAR,
            &[("a", tenant(0xc1)), ("secret", tenant(0xc2))],
        ),
    );
    until("both reports", || f.backend.reports().len() == 2).await;
    f.reader.breaks("a", ReadFail::Unavailable);
    deliver(
        &f,
        tenant(1),
        notification(tenant(1), GEAR, &[("a", tenant(0xc3))]),
    );
    tokio::time::sleep(Duration::from_millis(40)).await;

    let text = f.logs.text();
    assert!(!text.is_empty());
    let hash = settings_service_sdk::canonical::secret_hash(
        key("secret").as_str(),
        &tenant(1).to_string(),
        &SecretString::from("plaintext-marker-9921"),
    )
    .unwrap();
    for forbidden in [
        "value-marker-7731",
        "handle-marker-5519",
        "plaintext-marker-9921",
        hash.as_str(),
        "contractually-free",
    ] {
        assert!(
            !text.contains(forbidden),
            "`{forbidden}` reached a log line:\n{text}"
        );
    }
}

// ---------------------------------------------------------------------------
// The handle's own task, the handle's end, the group's identity and its warnings
// ---------------------------------------------------------------------------

/// A handler that blocks inside `on_change` until opened, recording the
/// `change_ref`s of each bundle it was handed at the start.
struct Gated {
    open: AtomicBool,
    started: Mutex<Vec<Vec<Option<Uuid>>>>,
}

impl Gated {
    fn closed() -> Arc<Self> {
        Arc::new(Self {
            open: AtomicBool::new(false),
            started: Mutex::new(Vec::new()),
        })
    }

    fn open(&self) {
        self.open.store(true, Ordering::SeqCst);
    }

    fn started(&self) -> Vec<Vec<Option<Uuid>>> {
        self.started.lock().unwrap().clone()
    }
}

#[async_trait]
impl SettingChangeHandler for Gated {
    async fn on_change(&self, bundle: ChangeBundle) -> Vec<(SettingKey, ActivationOutcome)> {
        self.started
            .lock()
            .unwrap()
            .push(bundle.changes.iter().map(|c| c.change_ref).collect());
        while !self.open.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        bundle
            .changes
            .iter()
            .map(|c| {
                (
                    c.key.clone(),
                    ActivationOutcome::Success {
                        applied_value: c.value.value.clone(),
                    },
                )
            })
            .collect()
    }
}

async fn subscribe_with(
    f: &Fixture,
    keys: &[&str],
    at: Uuid,
    handler: Arc<dyn SettingChangeHandler>,
) -> SubscriptionHandle {
    f.client
        .subscribe(
            &ctx_of(GEAR),
            keys.iter().map(|k| key(k)).collect(),
            at.to_string(),
            handler,
        )
        .await
        .expect("subscribe answers a handle")
}

fn terminal() -> WireFrame {
    WireFrame::Control {
        code: event_broker_sdk::ControlCode::Terminal,
        positions: Vec::new(),
        reason: None,
    }
}

/// How many whole `every` fit in `elapsed`: the most warnings a throttle at that
/// cadence can have logged.
fn intervals_in(elapsed: Duration, every: Duration) -> usize {
    let whole = elapsed.as_millis().checked_div(every.as_millis());
    usize::try_from(whole.expect("a non-zero interval")).expect("a small count")
}

#[tokio::test]
async fn a_handler_that_panics_on_a_pending_registrations_bundle_does_not_stop_the_walk() {
    let f = fixture(true, timing());
    f.backend.register_fails("a", RegisterError::NotFound, 1);
    f.backend.register_fails("b", RegisterError::NotFound, 6);
    let handler = Recorder::applying();
    handler.panics.store(1, Ordering::SeqCst);

    let handle = f.subscribe(&["a", "b"], tenant(1), &handler).await;

    until("both keys to register", || {
        is_registered(&handle, "a") && is_registered(&handle, "b")
    })
    .await;
    assert!(
        f.logs.text().contains("a change handler panicked"),
        "the panic is logged once, without a payload"
    );
    assert!(!f.logs.text().contains("scripted failure"), "no payload");
    until("the later key's bundle", || {
        handler
            .bundles()
            .iter()
            .any(|b| b.changes.iter().any(|c| c.key == key("b")))
    })
    .await;
    assert!(
        f.backend.reports().is_empty(),
        "a bundle without a change_ref owes no report"
    );
}

#[tokio::test]
async fn unsubscribing_the_handle_ends_a_changed_loop() {
    let f = fixture(true, timing());
    let handler = Recorder::applying();
    let mut handle = f.subscribe(&["a"], tenant(1), &handler).await;
    until("registered", || is_registered(&handle, "a")).await;

    handle
        .unsubscribe(&ctx_of(GEAR))
        .await
        .expect("unsubscribed");

    tokio::time::timeout(Duration::from_secs(5), async {
        while handle.changed().await {}
    })
    .await
    .expect("`changed` returns false once the handle is unsubscribed");
}

#[tokio::test]
async fn a_slow_handler_delays_neither_another_handle_nor_a_rejoin() {
    let f = fixture(true, timing());
    let slow = Gated::closed();
    let quick = Recorder::applying();
    let a = subscribe_with(&f, &["a"], tenant(1), Arc::clone(&slow) as Arc<_>).await;
    let b = f.subscribe(&["b"], tenant(1), &quick).await;
    until("both registered", || {
        is_registered(&a, "a") && is_registered(&b, "b")
    })
    .await;
    until("the stream to open", || f.broker.opened_streams() >= 1).await;

    deliver(
        &f,
        tenant(1),
        notification(tenant(1), GEAR, &[("a", tenant(0xc1)), ("b", tenant(0xc2))]),
    );
    until("the slow handler to be inside", || {
        !slow.started().is_empty()
    })
    .await;

    // The other handle is handed its bundle and reported while `a` is held.
    until("the other handle's report", || {
        f.backend
            .reports()
            .iter()
            .any(|(_, r)| r.change_ref == tenant(0xc2).to_string())
    })
    .await;
    // And the group joins again for a new tenant's interest meanwhile.
    let c = f.subscribe(&["c"], tenant(2), &quick).await;
    until("the rejoin", || f.broker.joins().len() >= 2).await;
    assert!(
        f.backend
            .reports()
            .iter()
            .all(|(_, r)| r.change_ref != tenant(0xc1).to_string()),
        "the slow handler has not answered yet"
    );

    slow.open();
    until("the slow handle's report", || {
        f.backend
            .reports()
            .iter()
            .any(|(_, r)| r.change_ref == tenant(0xc1).to_string())
    })
    .await;
    drop((a, b, c));
}

#[tokio::test]
async fn one_handles_bundles_reach_its_handler_one_at_a_time_in_order() {
    let f = fixture(true, timing());
    let gated = Gated::closed();
    let handle = subscribe_with(&f, &["a"], tenant(1), Arc::clone(&gated) as Arc<_>).await;
    until("registered", || is_registered(&handle, "a")).await;
    until("the stream to open", || f.broker.opened_streams() >= 1).await;

    deliver(
        &f,
        tenant(1),
        notification(tenant(1), GEAR, &[("a", tenant(0xc1))]),
    );
    until("the first bundle", || gated.started().len() == 1).await;
    deliver(
        &f,
        tenant(1),
        notification(tenant(1), GEAR, &[("a", tenant(0xc2))]),
    );
    until("the second to be read", || f.reader.reads_of("a") >= 3).await;
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(
        gated.started().len(),
        1,
        "the second waits for the first to finish"
    );

    gated.open();
    until("the second bundle", || gated.started().len() == 2).await;
    assert_eq!(
        gated.started(),
        vec![vec![Some(tenant(0xc1))], vec![Some(tenant(0xc2))]]
    );
    until("both reports", || f.backend.reports().len() == 2).await;
}

#[tokio::test]
async fn the_group_joins_under_the_newest_live_handles_context() {
    let f = fixture(true, timing());
    let handler = Recorder::applying();
    let first = joined(&f, &["a"], tenant(1), &handler).await;
    let fresh = SecurityContext::builder()
        .subject_id(Uuid::from_u128(GEAR))
        .subject_tenant_id(tenant(0xb0))
        .subject_type(crate::infra::service_identity::SERVICE_SUBJECT_TYPE)
        .build()
        .expect("a context");
    let second = f
        .client
        .subscribe(
            &fresh,
            vec![key("b")],
            tenant(2).to_string(),
            Arc::clone(&handler) as Arc<dyn SettingChangeHandler>,
        )
        .await
        .expect("a handle");
    until("the join for the new interest", || {
        f.broker.joins().len() >= 2
    })
    .await;
    assert_eq!(
        f.broker.join_homes.lock().unwrap().last().copied(),
        Some(tenant(0xb0)),
        "the join carries the newest handle's identity"
    );

    // The first handle goes; the group keeps the second's identity.
    drop(first);
    f.broker.send(Ok(terminal()));
    until("a rejoin", || f.broker.joins().len() >= 3).await;
    assert_eq!(
        f.broker.join_homes.lock().unwrap().last().copied(),
        Some(tenant(0xb0))
    );

    // The second goes while another handle lives: the group falls back to it.
    let third = f.subscribe(&["c"], tenant(1), &handler).await;
    until("the third to register", || is_registered(&third, "c")).await;
    let joined_so_far = f.broker.joins().len();
    drop(second);
    f.broker.send(Ok(terminal()));
    until("a rejoin", || f.broker.joins().len() > joined_so_far).await;
    assert_eq!(
        f.broker.join_homes.lock().unwrap().last().copied(),
        Some(tenant(0xa0)),
        "the remaining handle's identity, not the dropped one's"
    );
    drop(third);
}

#[tokio::test]
async fn a_key_stuck_on_a_missing_grant_is_warned_once_each_interval_and_so_is_the_refusal() {
    let mut t = timing();
    t.warn_every = Duration::from_millis(120);
    let f = fixture(true, t);
    f.broker.join_refusals.store(1_000_000, Ordering::SeqCst);
    f.broker.refuse_as_grant.store(true, Ordering::SeqCst);
    let handler = Recorder::applying();
    let started = Instant::now();
    let handle = f.subscribe(&["stuck"], tenant(1), &handler).await;
    until("pending for the grant", || {
        is_pending(&handle, "stuck", PendingReason::BrokerGrantMissing)
    })
    .await;

    tokio::time::sleep(Duration::from_millis(500)).await;

    let intervals = intervals_in(started.elapsed(), Duration::from_millis(120));
    let warned = f.logs.count("subscription still pending");
    assert!(
        (1..=intervals.max(1)).contains(&warned),
        "once each interval over {intervals}, got {warned}\n{}",
        f.logs.text()
    );
    assert!(
        f.logs.text().contains(key("stuck").as_str()),
        "names the key"
    );
    assert!(
        f.logs.text().contains("BrokerGrantMissing"),
        "names the reason"
    );
    let refusals = f.logs.count("activation topic join refused");
    assert!(
        (1..=intervals + 1).contains(&refusals),
        "the refusal is throttled to the same interval, got {refusals}"
    );
    assert!(
        f.broker.joins().len() > refusals * 3,
        "the join is retried far more often than it is logged"
    );
}

#[tokio::test]
async fn a_grant_lost_while_a_registration_is_in_flight_leaves_the_key_pending_and_restores_its_value()
 {
    let f = fixture(true, timing());
    f.broker.join_refusals.store(1_000_000, Ordering::SeqCst);
    f.broker.refuse_as_grant.store(true, Ordering::SeqCst);
    f.backend.hold_registrations();
    f.reader.set("a", json!("in-flight"));
    let handler = Recorder::applying();
    let handle = f.subscribe(&["a"], tenant(1), &handler).await;
    until("the grant to be refused", || {
        f.logs.text().contains("join refused")
    })
    .await;

    // The registration commits after the group already reported the missing
    // grant: the key goes straight to `pending`, its value held.
    f.backend.release_registrations();
    until("pending for the grant", || {
        is_pending(&handle, "a", PendingReason::BrokerGrantMissing)
    })
    .await;
    f.broker.join_refusals.store(0, Ordering::SeqCst);
    until("registered again", || is_registered(&handle, "a")).await;
    let KeyState::Registered { value: Some(value) } = state_of(&handle, "a") else {
        panic!("the held value comes back");
    };
    assert_eq!(value.value, json!("in-flight"));
}

#[tokio::test]
async fn a_stop_ends_the_pending_retries_of_every_handle() {
    let f = fixture(true, timing());
    f.backend
        .register_always_fails("ghost", RegisterError::NotFound);
    let handler = Recorder::applying();
    let _handle = f.subscribe(&["ghost"], tenant(1), &handler).await;
    until("a few retries", || f.backend.register_calls("ghost") >= 3).await;

    f.client.stop().await;
    tokio::time::sleep(Duration::from_millis(30)).await;
    let after_stop = f.backend.register_calls("ghost");
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert_eq!(
        f.backend.register_calls("ghost"),
        after_stop,
        "the walker ended with the stop"
    );
}

#[tokio::test]
async fn a_stop_during_a_rejoins_leave_still_leaves_the_subscription_before_the_delete() {
    let mut t = timing();
    t.cleanup_deadline = Duration::from_millis(300);
    let f = fixture(true, t);
    let handler = Recorder::applying();
    let _handle = joined(&f, &["a"], tenant(1), &handler).await;
    f.broker.leave_hangs.store(true, Ordering::SeqCst);

    f.broker.send(Ok(terminal()));
    until("the rejoin's leave to start", || f.broker.leaves() == 1).await;
    f.client.stop().await;

    let leaves = f.broker.leaves.lock().unwrap().clone();
    assert_eq!(
        leaves.len(),
        2,
        "the cancelled leave is made again on release"
    );
    assert_eq!(leaves[0], leaves[1], "of the same subscription");
    assert_eq!(
        f.broker.deletes().len(),
        1,
        "and the group is deleted after"
    );
}

#[tokio::test]
async fn the_pending_warning_names_the_failure_code_and_the_code_names_no_error_text() {
    let mut t = timing();
    t.warn_every = Duration::from_millis(60);
    let f = fixture(true, t);
    f.backend.register_always_fails(
        "a",
        RegisterError::Unavailable("unavailable:authorization_service".to_owned()),
    );
    let handler = Recorder::applying();
    let _handle = f.subscribe(&["a"], tenant(1), &handler).await;
    until("the warning", || {
        f.logs.text().contains("unavailable:authorization_service")
    })
    .await;

    assert_eq!(
        failure_code(&DomainError::Unavailable {
            detail: LOCK_EXHAUSTED_DETAIL.to_owned()
        }),
        "lock_exhausted"
    );
    assert_eq!(
        failure_code(&DomainError::dependency_unavailable(
            "authorization service",
            "decide",
            "dsn=postgres://user:secret@host"
        )),
        "unavailable:authorization_service"
    );
    assert_eq!(
        failure_code(&DomainError::Internal {
            diagnostic: "secret diagnostic".to_owned()
        }),
        "internal"
    );
}

// ---------------------------------------------------------------------------
// The per-instance identity, re-registration and the process's graceful stop
// ---------------------------------------------------------------------------

/// A second gear in the process.
const OTHER_GEAR: u128 = 0x6ea2;

#[tokio::test]
async fn a_registration_a_report_and_an_unsubscribe_carry_the_process_identity() {
    let identity = InstanceIdentity::new(Uuid::from_u128(0xabc), Some("host-a"));
    let f = fixture_as(true, timing(), identity.clone());
    let handler = Recorder::applying();
    let handle = joined(&f, &["a"], tenant(1), &handler).await;

    // The registration: the id and the label.
    assert_eq!(f.backend.identities.lock().unwrap().clone(), vec![identity]);

    // A report through the handler's returned outcome, and a direct one.
    deliver(
        &f,
        tenant(1),
        notification(tenant(1), GEAR, &[("a", tenant(0xca))]),
    );
    until("the handler's report", || f.backend.reports().len() == 1).await;
    f.client
        .report_outcome(
            &ctx_of(GEAR),
            tenant(0xcb),
            tenant(1).to_string(),
            key("a"),
            ActivationOutcome::Failed {
                detail: "late".to_owned(),
            },
        )
        .await
        .expect("answers");
    let reports = f.backend.reports();
    assert_eq!(reports.len(), 2);
    for (_, report) in &reports {
        assert_eq!(report.instance_id, Some(Uuid::from_u128(0xabc)));
    }

    // The unsubscribe.
    handle.unsubscribe(&ctx_of(GEAR)).await.expect("answers");
    assert_eq!(
        f.backend.unsubscribed_as.lock().unwrap().clone(),
        vec![Uuid::from_u128(0xabc)]
    );
}

#[tokio::test]
async fn two_instances_of_one_gear_register_under_their_own_ids() {
    let one = fixture_as(
        true,
        timing(),
        InstanceIdentity::new(Uuid::from_u128(1), Some("one")),
    );
    let two = fixture_as(
        true,
        timing(),
        InstanceIdentity::new(Uuid::from_u128(2), None),
    );
    let handler = Recorder::applying();
    let _a = joined(&one, &["a"], tenant(1), &handler).await;
    let _b = joined(&two, &["a"], tenant(1), &handler).await;
    let first = one.backend.identities.lock().unwrap()[0].clone();
    let second = two.backend.identities.lock().unwrap()[0].clone();
    assert_ne!(first.instance_id(), second.instance_id());
    assert_eq!(first.instance_label(), Some("one"));
    assert_eq!(second.instance_label(), None);
}

#[tokio::test]
async fn the_production_client_runs_under_the_one_process_identity() {
    let process = InstanceIdentity::process();
    assert_eq!(process.instance_id().get_version_num(), 7);
    assert!(std::ptr::eq(process, InstanceIdentity::process()));
}

fn not_subscribed() -> crate::domain::activation::ReportResult {
    crate::domain::activation::ReportResult::NotSubscribed
}

#[tokio::test]
async fn not_subscribed_re_registers_a_held_key_and_hands_the_handler_a_bundle_without_change_ref()
{
    let f = fixture(true, timing());
    let handler = Recorder::applying();
    let handle = joined(&f, &["a"], tenant(1), &handler).await;
    assert_eq!(f.backend.register_calls("a"), 1);
    f.backend.hold_registrations();
    f.backend
        .report_answers
        .lock()
        .unwrap()
        .push_back(Ok(not_subscribed()));

    let answer = f
        .client
        .report_outcome(
            &ctx_of(GEAR),
            tenant(0xca),
            tenant(1).to_string(),
            key("a"),
            ActivationOutcome::Success {
                applied_value: json!("value-1"),
            },
        )
        .await
        .expect("answers");

    assert_eq!(answer, ReportResult::NotSubscribed);
    until("the key to wait for its registration", || {
        is_pending(&handle, "a", PendingReason::NotRegistered)
    })
    .await;
    f.reader.set("a", json!("fresh-after-forgetting"));
    f.backend.release_registrations();
    until("the key to register again", || is_registered(&handle, "a")).await;
    until("the bundle", || !handler.bundles().is_empty()).await;

    assert_eq!(f.backend.register_calls("a"), 2);
    let bundles = handler.bundles();
    assert_eq!(bundles.len(), 1);
    assert_eq!(bundles[0].changes[0].change_ref, None);
    assert_eq!(
        bundles[0].changes[0].value.value,
        json!("fresh-after-forgetting")
    );
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(
        f.backend.reports().len(),
        1,
        "the report was never repeated and the bundle owes none"
    );
}

#[tokio::test]
async fn the_handlers_own_report_answered_not_subscribed_re_registers_the_key_too() {
    let f = fixture(true, timing());
    let handler = Recorder::applying();
    let handle = joined(&f, &["a"], tenant(1), &handler).await;
    f.backend
        .report_answers
        .lock()
        .unwrap()
        .push_back(Ok(not_subscribed()));

    deliver(
        &f,
        tenant(1),
        notification(tenant(1), GEAR, &[("a", tenant(0xca))]),
    );
    until("the key to be registered again", || {
        f.backend.register_calls("a") == 2
    })
    .await;
    until("registered", || is_registered(&handle, "a")).await;
    until("the registration's bundle", || handler.bundles().len() == 2).await;
    assert_eq!(handler.bundles()[1].changes[0].change_ref, None);
    assert_eq!(f.backend.reports().len(), 1, "one report, never retried");
}

#[tokio::test]
async fn a_key_released_by_unsubscribe_or_by_stop_is_never_registered_again() {
    let f = fixture(true, timing());
    let handler = Recorder::applying();
    let released = joined(&f, &["a"], tenant(1), &handler).await;
    released.unsubscribe(&ctx_of(GEAR)).await.expect("answers");

    let other = joined(&f, &["b"], tenant(1), &handler).await;
    for name in ["a", "b"] {
        f.backend
            .report_answers
            .lock()
            .unwrap()
            .push_back(Ok(not_subscribed()));
        f.client
            .report_outcome(
                &ctx_of(GEAR),
                tenant(0xca),
                tenant(1).to_string(),
                key(name),
                ActivationOutcome::Success {
                    applied_value: json!("v"),
                },
            )
            .await
            .expect("answers");
    }
    // `b`, which a live handle holds, goes back to registration; `a`, released
    // by `unsubscribe()`, does not.
    until("b registered again", || f.backend.register_calls("b") == 2).await;
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(f.backend.register_calls("a"), 1);

    // After the stop nothing is registered again either.
    f.client.stop().await;
    f.backend
        .report_answers
        .lock()
        .unwrap()
        .push_back(Ok(not_subscribed()));
    f.client
        .report_outcome(
            &ctx_of(GEAR),
            tenant(0xcb),
            tenant(1).to_string(),
            key("b"),
            ActivationOutcome::Success {
                applied_value: json!("v"),
            },
        )
        .await
        .expect("answers");
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(f.backend.register_calls("b"), 2);
    drop(other);
}

#[tokio::test]
async fn a_notification_for_a_key_waiting_in_not_registered_is_applied_without_change_ref_and_owes_no_report()
 {
    let f = fixture(true, timing());
    let handler = Recorder::applying();
    let handle = joined(&f, &["a", "b"], tenant(1), &handler).await;
    f.backend.hold_registrations();
    f.backend
        .report_answers
        .lock()
        .unwrap()
        .push_back(Ok(not_subscribed()));
    f.client
        .report_outcome(
            &ctx_of(GEAR),
            tenant(0xca),
            tenant(1).to_string(),
            key("a"),
            ActivationOutcome::Success {
                applied_value: json!("v"),
            },
        )
        .await
        .expect("answers");
    until("a waits", || {
        is_pending(&handle, "a", PendingReason::NotRegistered)
    })
    .await;

    let (ref_a, ref_b) = (tenant(0xda), tenant(0xdb));
    deliver(
        &f,
        tenant(1),
        notification(tenant(1), GEAR, &[("a", ref_a), ("b", ref_b)]),
    );
    until("the bundle", || !handler.bundles().is_empty()).await;
    until("b's report", || f.backend.reports().len() == 2).await;

    let bundle = &handler.bundles()[0];
    let by_key: BTreeMap<_, _> = bundle
        .changes
        .iter()
        .map(|c| (c.key.clone(), c.change_ref))
        .collect();
    assert_eq!(by_key[&key("a")], None, "applied, owing no report");
    assert_eq!(by_key[&key("b")], Some(ref_b), "a registered key keeps it");
    let reports = f.backend.reports();
    assert_eq!(reports.len(), 2, "the direct report and b's, none for a");
    assert_eq!(reports[1].1.key, key("b").as_str());
    // The registration retry went on unchanged.
    f.backend.release_registrations();
    until("a registered again", || is_registered(&handle, "a")).await;
}

#[tokio::test]
async fn the_stop_unsubscribes_every_gears_subscriptions_of_the_process_dropped_handles_included() {
    let identity = InstanceIdentity::new(Uuid::from_u128(0x57), None);
    let f = fixture_as(true, timing(), identity);
    let handler = Recorder::applying();
    let live = joined(&f, &["a"], tenant(1), &handler).await;
    let dropped = joined(&f, &["b"], tenant(2), &handler).await;
    drop(dropped);
    let releasing = joined(&f, &["c"], tenant(1), &handler).await;
    releasing.unsubscribe(&ctx_of(GEAR)).await.expect("answers");
    let other = f
        .subscribe_as(OTHER_GEAR, &["a"], tenant(1), &handler)
        .await;
    until("the other gear's key", || is_registered(&other, "a")).await;
    assert_eq!(
        f.backend.unsubscribed.lock().unwrap().len(),
        1,
        "dropping deleted nothing; the explicit unsubscribe one"
    );

    f.client.stop().await;

    let mine = Uuid::from_u128(GEAR).to_string();
    let theirs = Uuid::from_u128(OTHER_GEAR).to_string();
    let mut deleted: Vec<_> = f
        .backend
        .unsubscribed
        .lock()
        .unwrap()
        .iter()
        .skip(1)
        .cloned()
        .collect();
    deleted.sort();
    let mut expected = vec![
        (mine.clone(), key("a").as_str().to_owned(), tenant(1)),
        (mine, key("b").as_str().to_owned(), tenant(2)),
        (theirs, key("a").as_str().to_owned(), tenant(1)),
    ];
    expected.sort();
    assert_eq!(
        deleted, expected,
        "one per key and tenant of every gear of the process: the dropped handle's key included, the released one not repeated"
    );
    assert!(
        f.backend
            .unsubscribed_as
            .lock()
            .unwrap()
            .iter()
            .all(|id| *id == Uuid::from_u128(0x57)),
        "under the process's instance id"
    );
    assert!(!live.states().is_empty());
}

/// *Consumer Activation SDK*, graceful stop: a walker already inside the
/// registration when the stop drains the process's subscriptions is not cut
/// short by it; the registration commits after the drain, and the client
/// unsubscribes it under the process's `instance_id` rather than leaving it
/// registered unannounced.
#[tokio::test]
async fn a_registration_committing_after_the_stop_drained_is_unsubscribed() {
    let identity = InstanceIdentity::new(Uuid::from_u128(0x58), None);
    let f = fixture_as(true, timing(), identity);
    f.backend.hold_registrations();
    let handler = Recorder::applying();
    let handle = f.subscribe(&["a"], tenant(1), &handler).await;
    until("the registration in flight", || {
        !f.backend.identities.lock().unwrap().is_empty()
    })
    .await;

    f.client.stop().await;
    assert!(
        f.backend.unsubscribed.lock().unwrap().is_empty(),
        "nothing was registered when the stop drained"
    );
    f.backend.release_registrations();

    until("the late registration unsubscribed", || {
        !f.backend.unsubscribed.lock().unwrap().is_empty()
    })
    .await;
    assert_eq!(
        *f.backend.unsubscribed.lock().unwrap(),
        [(
            Uuid::from_u128(GEAR).to_string(),
            key("a").as_str().to_owned(),
            tenant(1)
        )]
    );
    assert_eq!(
        *f.backend.unsubscribed_as.lock().unwrap(),
        [Uuid::from_u128(0x58)],
        "under the process's instance id"
    );
    drop(handle);
}

/// The same late registration whose unsubscribe fails stays registered, with a
/// warning.
#[tokio::test]
async fn a_late_registration_that_cannot_be_unsubscribed_is_a_warning() {
    let f = fixture(true, timing());
    f.backend.hold_registrations();
    f.backend
        .unsubscribe_fails("a", UnsubscribeFailure::Unavailable, 1);
    let handler = Recorder::applying();
    let handle = f.subscribe(&["a"], tenant(1), &handler).await;
    until("the registration in flight", || {
        !f.backend.identities.lock().unwrap().is_empty()
    })
    .await;

    f.client.stop().await;
    f.backend.release_registrations();

    until("the warning", || {
        f.logs
            .text()
            .contains("registered while the client stopped was not deregistered")
    })
    .await;
    drop(handle);
}

#[tokio::test]
async fn an_unsubscribe_of_the_stop_that_does_not_finish_is_abandoned_at_the_deadline() {
    let mut t = timing();
    t.cleanup_deadline = Duration::from_millis(150);
    let f = fixture(true, t);
    let handler = Recorder::applying();
    let handle = joined(&f, &["a"], tenant(1), &handler).await;
    f.backend.stall_unsubscribe.store(true, Ordering::SeqCst);

    let started = Instant::now();
    f.client.stop().await;

    assert!(
        started.elapsed() < Duration::from_secs(2),
        "bounded by the deadline"
    );
    assert!(f.backend.unsubscribed.lock().unwrap().is_empty());
    assert!(
        f.logs
            .text()
            .contains("not deregistered within the cleanup deadline")
    );
    drop(handle);
}

#[tokio::test]
async fn a_key_refused_for_the_registration_cap_stays_pending_and_is_retried() {
    // The service answers `Unavailable` when 64 instances hold the key and none
    // is evictable.
    let f = fixture(true, timing());
    f.backend.register_fails(
        "a",
        RegisterError::Unavailable("unavailable:registration_cap".to_owned()),
        3,
    );
    let handler = Recorder::applying();
    let handle = f.subscribe(&["a"], tenant(1), &handler).await;
    until("pending for the cap", || {
        is_pending(&handle, "a", PendingReason::Unavailable)
    })
    .await;
    until("registered when a slot frees", || {
        is_registered(&handle, "a")
    })
    .await;
    assert_eq!(f.backend.register_calls("a"), 4);
}

// ---------------------------------------------------------------------------
// The shared group-session lifecycle table
// ---------------------------------------------------------------------------

/// The activation group under the lifecycle table both consumers of the shared
/// group session run: one handle holds the group.
struct ActivationHarness {
    f: Fixture,
    handler: Arc<Recorder>,
    handle: Option<SubscriptionHandle>,
}

impl ActivationHarness {
    fn new() -> Self {
        Self {
            f: fixture(false, timing()),
            handler: Recorder::applying(),
            handle: None,
        }
    }
}

impl lifecycle::Harness for ActivationHarness {
    fn scripted(&self) -> Arc<dyn EventBrokerApi> {
        Arc::clone(&self.f.broker) as Arc<dyn EventBrokerApi>
    }
    fn plug(&self, broker: Arc<dyn EventBrokerApi>) {
        self.f.hub.register::<dyn EventBrokerApi>(broker);
    }
    fn refuse_creates(&self, n: usize) {
        self.f.broker.create_refusals.store(n, Ordering::SeqCst);
    }
    fn refuse_joins(&self, n: usize) {
        self.f.broker.join_refusals.store(n, Ordering::SeqCst);
    }
    fn hang_leaves(&self) {
        self.f.broker.leave_hangs.store(true, Ordering::SeqCst);
    }
    fn send_terminal(&self) {
        self.f.broker.send(Ok(terminal()));
    }
    fn end_stream(&self) {
        self.f.broker.streams.lock().unwrap().pop();
    }
    async fn start(&mut self) {
        self.handle = Some(self.f.subscribe(&["a"], tenant(1), &self.handler).await);
    }
    async fn stop(&mut self) {
        tokio::time::timeout(Duration::from_secs(5), self.f.client.stop())
            .await
            .expect("the stop ends within the deadline");
    }
    fn logs(&self) -> String {
        self.f.logs.text()
    }
}

#[tokio::test]
async fn lifecycle_stop_leaves_the_subscription_then_deletes_the_group() {
    lifecycle::stop_leaves_the_subscription_then_deletes_the_group(ActivationHarness::new()).await;
}

#[tokio::test]
async fn lifecycle_refusals_are_retried_on_one_group_and_leak_none() {
    lifecycle::refusals_are_retried_on_one_group_and_leak_none(ActivationHarness::new()).await;
}

#[tokio::test]
async fn lifecycle_a_terminal_frame_rejoins_through_the_same_group() {
    lifecycle::a_terminal_frame_rejoins_through_the_same_group(ActivationHarness::new()).await;
}

#[tokio::test]
async fn lifecycle_a_lost_stream_releases_the_group_and_a_new_one_is_created() {
    lifecycle::a_lost_stream_releases_the_group_and_a_new_one_is_created(ActivationHarness::new())
        .await;
}

#[tokio::test]
async fn lifecycle_a_stop_during_a_rejoins_leave_still_leaves_before_the_delete() {
    lifecycle::a_stop_during_a_rejoins_leave_still_leaves_before_the_delete(
        ActivationHarness::new(),
    )
    .await;
}

#[path = "activation_group_reread_tests.rs"]
mod activation_group_reread_tests;

#[path = "activation_client_gap_tests.rs"]
mod activation_client_gap_tests;

#[path = "activation_client_trace_tests.rs"]
mod activation_client_trace_tests;
