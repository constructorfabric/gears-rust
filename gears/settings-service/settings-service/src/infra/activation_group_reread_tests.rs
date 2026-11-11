//! The group's re-reads of a notification: bounded at once, off the stream,
//! in order per tenant, and dropped by the stop.

use super::*;
use crate::infra::activation_group::{LANE_DEPTH, REREAD_CONCURRENCY};

/// A reader whose reads of a `slow…` key wait, while armed, until the gate
/// opens; it counts the reads in flight, the most at once, and the slow reads
/// that finished.
struct SlowReader {
    armed: AtomicBool,
    open: tokio::sync::watch::Sender<bool>,
    in_flight: AtomicUsize,
    most: AtomicUsize,
    finished_slow: AtomicUsize,
}

/// Counts a read out of flight however it ends: answered or dropped.
struct InFlight<'a>(&'a AtomicUsize);

impl Drop for InFlight<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl SlowReader {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            armed: AtomicBool::new(false),
            open: tokio::sync::watch::channel(false).0,
            in_flight: AtomicUsize::new(0),
            most: AtomicUsize::new(0),
            finished_slow: AtomicUsize::new(0),
        })
    }

    fn arm(&self) {
        self.armed.store(true, Ordering::SeqCst);
    }

    fn open_gate(&self) {
        self.open.send_replace(true);
    }

    fn in_flight(&self) -> usize {
        self.in_flight.load(Ordering::SeqCst)
    }

    fn most(&self) -> usize {
        self.most.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl SettingsReaderClient for SlowReader {
    async fn get_effective(
        &self,
        _: &SecurityContext,
        req: GetEffectiveRequest,
    ) -> Result<EffectiveValueResponse, CanonicalError> {
        let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        let _out = InFlight(&self.in_flight);
        self.most.fetch_max(now, Ordering::SeqCst);
        let slow = req.key.as_str().contains(".slow");
        if slow && self.armed.load(Ordering::SeqCst) {
            let mut open = self.open.subscribe();
            open.wait_for(|o| *o).await.expect("the gate stays");
            self.finished_slow.fetch_add(1, Ordering::SeqCst);
        }
        Ok(EffectiveValueResponse {
            key: req.key,
            scope: req.scope,
            value: json!("fresh"),
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

struct Slow {
    broker: Arc<ScriptedBroker>,
    backend: Arc<FakeBackend>,
    reader: Arc<SlowReader>,
    client: ActivationClient,
}

fn slow_fixture() -> Slow {
    slow_fixture_with(timing())
}

/// The fresh-read timeout of the tests that run into it.
const READ_TIMEOUT: Duration = Duration::from_secs(1);

fn read_timeout_timing() -> ClientTiming {
    ClientTiming {
        fresh_read: READ_TIMEOUT,
        ..timing()
    }
}

fn slow_fixture_with(timing: ClientTiming) -> Slow {
    let hub = Arc::new(ClientHub::new());
    let broker = Arc::new(ScriptedBroker::on_topic(ACTIVATION_TOPIC_ID));
    hub.register::<dyn EventBrokerApi>(Arc::clone(&broker) as Arc<dyn EventBrokerApi>);
    let backend = Arc::new(FakeBackend::default());
    let reader = SlowReader::new();
    let client = ActivationClient::new(ConsumerDeps {
        backend: Arc::clone(&backend) as Arc<dyn ActivationBackend>,
        reader: Arc::clone(&reader) as Arc<dyn SettingsReaderClient>,
        metrics: Arc::new(RecordingNotifyMetrics::default()) as Arc<dyn NotifyMetrics>,
        hub,
        timing,
        identity: instance(),
    });
    Slow {
        broker,
        backend,
        reader,
        client,
    }
}

impl Slow {
    async fn subscribe(
        &self,
        keys: &[String],
        at: Uuid,
        handler: &Arc<Recorder>,
    ) -> SubscriptionHandle {
        let handle = self
            .client
            .subscribe(
                &ctx_of(GEAR),
                keys.iter().map(|k| key(k)).collect(),
                at.to_string(),
                Arc::clone(handler) as Arc<dyn SettingChangeHandler>,
            )
            .await
            .expect("subscribe answers a handle");
        until("the keys to register", || {
            keys.iter().all(|k| is_registered(&handle, k))
        })
        .await;
        handle
    }

    fn deliver(&self, at: Uuid, change_ref: Uuid, keys: &[String]) {
        let changes: Vec<(&str, Uuid)> = keys.iter().map(|k| (k.as_str(), change_ref)).collect();
        let event = wire(
            CHANGE_NOTIFICATION_EVENT_TYPE,
            at,
            Utc::now(),
            notification(at, GEAR, &changes),
        );
        self.broker.send(Ok(WireFrame::Event(event)));
    }

    fn reports_of(&self, change_ref: Uuid) -> usize {
        self.backend
            .reports()
            .iter()
            .filter(|(_, r)| r.change_ref == change_ref.to_string())
            .count()
    }
}

fn slow_keys(n: usize) -> Vec<String> {
    (0..n).map(|i| format!("slow{i}")).collect()
}

fn names(names: &[&str]) -> Vec<String> {
    names.iter().map(|n| (*n).to_owned()).collect()
}

#[tokio::test]
async fn a_large_notification_delays_neither_another_tenants_nor_the_loss_of_the_broker() {
    let f = slow_fixture();
    let large = slow_keys(200);
    let big = Recorder::applying();
    let other = Recorder::applying();
    let a = f.subscribe(&large, tenant(1), &big).await;
    let b = f.subscribe(&names(&["fast"]), tenant(2), &other).await;
    until("the join for both tenants", || {
        f.broker
            .joins()
            .last()
            .is_some_and(|j| j.interests.len() == 2)
            && f.broker.opened_streams() >= 2
    })
    .await;
    f.reader.arm();

    // 200 keys, each read held, then another tenant's notification and the
    // stream failing behind it.
    f.deliver(tenant(1), tenant(0xc1), &large);
    f.deliver(tenant(2), tenant(0xc2), &names(&["fast"]));
    f.broker.send(Err(EventBrokerError::Internal(
        "the broker went away".to_owned(),
    )));

    until("the other tenant's report", || {
        f.reports_of(tenant(0xc2)) == 1
    })
    .await;
    until("the loss to release the group", || {
        !f.broker.deletes().is_empty()
    })
    .await;
    assert_eq!(
        f.reports_of(tenant(0xc1)),
        0,
        "the large one is still being read"
    );
    until("the large one's reads to fill its bound", || {
        f.reader.in_flight() == REREAD_CONCURRENCY
    })
    .await;

    // The reaction outlives the loss of the stream it came on.
    f.reader.open_gate();
    until("the large notification's reports", || {
        f.reports_of(tenant(0xc1)) == 200
    })
    .await;
    let bundles = big.bundles();
    assert_eq!(bundles.len(), 1);
    assert_eq!(bundles[0].changes.len(), 200);
    // Each tenant's lane has its own bound: the other tenant's one read on top.
    assert!(f.reader.most() <= REREAD_CONCURRENCY + 1);
    drop((a, b));
}

/// Tenant 1 holds `slow0` and tenant 2 `fast`, both joined; the reads of
/// `slow0` are held from here on.
async fn two_tenants_one_hung(f: &Slow) -> (SubscriptionHandle, SubscriptionHandle) {
    let a = f
        .subscribe(&names(&["slow0"]), tenant(1), &Recorder::applying())
        .await;
    let b = f
        .subscribe(&names(&["fast"]), tenant(2), &Recorder::applying())
        .await;
    until("the join for both tenants", || {
        f.broker
            .joins()
            .last()
            .is_some_and(|j| j.interests.len() == 2)
            && f.broker.opened_streams() >= 2
    })
    .await;
    f.reader.arm();
    (a, b)
}

#[tokio::test(start_paused = true)]
async fn a_tenant_whose_reads_hang_delays_neither_another_tenant_nor_the_loss_of_the_broker() {
    let f = slow_fixture();
    let handles = two_tenants_one_hung(&f).await;

    // A write burst at the hung tenant — more notifications than the lane depth
    // of 32 that once parked the stream — then another tenant's notification
    // and the stream failing.
    let started = tokio::time::Instant::now();
    for n in 0..40 {
        f.deliver(tenant(1), Uuid::from_u128(0xa000 + n), &names(&["slow0"]));
    }
    f.deliver(tenant(2), tenant(0xc2), &names(&["fast"]));
    f.broker.send(Err(EventBrokerError::Internal(
        "the broker went away".to_owned(),
    )));

    until("the other tenant's report", || {
        f.reports_of(tenant(0xc2)) == 1
    })
    .await;
    until("the loss to release the group", || {
        !f.broker.deletes().is_empty()
    })
    .await;
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "neither waited for the hung tenant's reads"
    );
    assert_eq!(
        f.backend.reports().len(),
        1,
        "the hung tenant reported nothing"
    );
    drop(handles);
}

#[tokio::test(start_paused = true)]
async fn with_a_hung_tenants_lane_full_the_stream_waits_at_most_one_fresh_read_timeout() {
    let f = slow_fixture_with(read_timeout_timing());
    let handles = two_tenants_one_hung(&f).await;

    // One notification in flight, the lane full behind it and one more that
    // has to wait for room; then another tenant's and the stream failing.
    let started = tokio::time::Instant::now();
    for n in 0..LANE_DEPTH + 2 {
        let n = u128::try_from(n).expect("fits");
        f.deliver(tenant(1), Uuid::from_u128(0xa000 + n), &names(&["slow0"]));
    }
    f.deliver(tenant(2), tenant(0xc2), &names(&["fast"]));
    f.broker.send(Err(EventBrokerError::Internal(
        "the broker went away".to_owned(),
    )));

    until("the other tenant's report", || {
        f.reports_of(tenant(0xc2)) == 1
    })
    .await;
    until("the loss to release the group", || {
        !f.broker.deletes().is_empty()
    })
    .await;
    let waited = started.elapsed();
    assert!(
        waited >= READ_TIMEOUT,
        "the stream waited for room: {waited:?}"
    );
    assert!(
        waited < READ_TIMEOUT * 2,
        "for one fresh-read timeout, not more: {waited:?}"
    );
    assert_eq!(
        f.backend.reports().len(),
        1,
        "the hung tenant reported nothing"
    );
    drop(handles);
}

#[tokio::test(start_paused = true)]
async fn a_re_read_past_the_fresh_read_timeout_is_left_out_unreported_and_delivered_again() {
    let f = slow_fixture_with(read_timeout_timing());
    let handler = Recorder::applying();
    let handle = f
        .subscribe(&names(&["slow0", "fast"]), tenant(1), &handler)
        .await;
    until("the stream to open", || f.broker.opened_streams() >= 1).await;
    f.reader.arm();

    let change = tenant(0xc1);
    f.deliver(tenant(1), change, &names(&["slow0", "fast"]));
    until("the bundle of the key that answered", || {
        handler.bundles().len() == 1
    })
    .await;
    let got: Vec<(SettingKey, Option<Uuid>)> = handler.bundles()[0]
        .changes
        .iter()
        .map(|c| (c.key.clone(), c.change_ref))
        .collect();
    assert_eq!(got, vec![(key("fast"), Some(change))]);
    until("the report of the key that answered", || {
        f.reports_of(change) == 1
    })
    .await;
    let reported = |name: &str| {
        f.backend
            .reports()
            .iter()
            .filter(|(_, r)| r.change_ref == change.to_string() && r.key == key(name).as_str())
            .count()
    };
    assert_eq!(reported("slow0"), 0, "a timed-out re-read reports nothing");
    assert_eq!(f.reader.in_flight(), 0, "the timed-out read was dropped");
    assert!(
        is_registered(&handle, "slow0"),
        "the key's state is left as it was: the service delivers it again"
    );

    // The delivery loop's redelivery, with the store answering again.
    f.reader.open_gate();
    f.deliver(tenant(1), change, &names(&["slow0"]));
    until("the redelivered key's report", || reported("slow0") == 1).await;
    assert_eq!(reported("fast"), 1);
    drop(handle);
}

#[tokio::test(start_paused = true)]
async fn the_re_reads_of_one_notification_end_within_one_fresh_read_timeout_in_all() {
    let f = slow_fixture_with(read_timeout_timing());
    // Three times the reads one notification has in flight at once: one at a
    // time per read, they would hold the lane for three timeouts.
    let mut keys = slow_keys(3 * REREAD_CONCURRENCY);
    keys.push("fast".to_owned());
    let handler = Recorder::applying();
    let handle = f.subscribe(&keys, tenant(1), &handler).await;
    until("the stream to open", || f.broker.opened_streams() >= 1).await;
    f.reader.arm();

    let started = tokio::time::Instant::now();
    f.deliver(tenant(1), tenant(0xc1), &slow_keys(3 * REREAD_CONCURRENCY));
    f.deliver(tenant(1), tenant(0xc2), &names(&["fast"]));
    until("the next notification's bundle", || {
        handler.bundles().len() == 1
    })
    .await;
    let waited = started.elapsed();
    assert!(
        waited < READ_TIMEOUT * 2,
        "the lane held the first one for one timeout: {waited:?}"
    );
    assert_eq!(f.reports_of(tenant(0xc1)), 0, "no slow key was read");
    assert_eq!(f.reader.in_flight(), 0, "the unfinished reads were dropped");
    drop(handle);
}

#[tokio::test(start_paused = true)]
async fn a_subscribe_read_past_the_fresh_read_timeout_leaves_the_key_pending_and_is_retried() {
    let f = slow_fixture_with(read_timeout_timing());
    f.reader.arm();
    let handler = Recorder::applying();
    let handle = f
        .client
        .subscribe(
            &ctx_of(GEAR),
            vec![key("slow0")],
            tenant(1).to_string(),
            Arc::clone(&handler) as Arc<dyn SettingChangeHandler>,
        )
        .await
        .expect("subscribe answers a handle");

    until("the timed-out read to leave the key pending", || {
        is_pending(&handle, "slow0", PendingReason::FreshReadFailed)
    })
    .await;
    f.reader.open_gate();
    until("the retry's read to register the key", || {
        is_registered(&handle, "slow0")
    })
    .await;
}

#[tokio::test]
async fn the_re_reads_of_one_notification_run_at_most_the_bound_at_once_in_the_handles_order() {
    let f = slow_fixture();
    let large = slow_keys(100);
    let handler = Recorder::applying();
    let handle = f.subscribe(&large, tenant(1), &handler).await;
    until("the stream to open", || f.broker.opened_streams() >= 1).await;
    f.reader.arm();

    f.deliver(tenant(1), tenant(0xc1), &large);
    until("the bound to fill", || {
        f.reader.in_flight() == REREAD_CONCURRENCY
    })
    .await;
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(
        f.reader.in_flight(),
        REREAD_CONCURRENCY,
        "never past the bound"
    );

    f.reader.open_gate();
    until("the bundle", || handler.bundles().len() == 1).await;
    assert_eq!(f.reader.most(), REREAD_CONCURRENCY);
    let got: Vec<SettingKey> = handler.bundles()[0]
        .changes
        .iter()
        .map(|c| c.key.clone())
        .collect();
    let held: Vec<SettingKey> = handle.states().keys().cloned().collect();
    assert_eq!(got, held, "the bundle keeps the handle's key order");
}

#[tokio::test]
async fn a_tenants_notifications_reach_its_handle_in_the_order_they_came() {
    let f = slow_fixture();
    let handler = Recorder::applying();
    let handle = f
        .subscribe(&names(&["slow0", "fast"]), tenant(1), &handler)
        .await;
    until("the stream to open", || f.broker.opened_streams() >= 1).await;
    f.reader.arm();

    f.deliver(tenant(1), tenant(0xc1), &names(&["slow0"]));
    f.deliver(tenant(1), tenant(0xc2), &names(&["fast"]));
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        handler.bundles().is_empty(),
        "the later notification does not overtake the earlier one"
    );

    f.reader.open_gate();
    until("both bundles", || handler.bundles().len() == 2).await;
    let refs: Vec<Vec<Option<Uuid>>> = handler
        .bundles()
        .iter()
        .map(|b| b.changes.iter().map(|c| c.change_ref).collect())
        .collect();
    assert_eq!(
        refs,
        vec![vec![Some(tenant(0xc1))], vec![Some(tenant(0xc2))]]
    );
    drop(handle);
}

#[tokio::test]
async fn a_stop_during_a_slow_re_read_ends_promptly_and_drops_the_reads() {
    let f = slow_fixture();
    let large = slow_keys(50);
    let handler = Recorder::applying();
    let handle = f.subscribe(&large, tenant(1), &handler).await;
    until("the stream to open", || f.broker.opened_streams() >= 1).await;
    f.reader.arm();

    f.deliver(tenant(1), tenant(0xc1), &large);
    until("reads in flight", || f.reader.in_flight() > 0).await;

    let started = Instant::now();
    f.client.stop().await;
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "the stop is prompt"
    );
    until("the reads to be dropped", || f.reader.in_flight() == 0).await;

    f.reader.open_gate();
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(f.reader.finished_slow.load(Ordering::SeqCst), 0);
    assert!(handler.bundles().is_empty());
    assert_eq!(f.reports_of(tenant(0xc1)), 0);
    drop(handle);
}
