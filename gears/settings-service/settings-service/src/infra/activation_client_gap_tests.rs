//! The consumer group's lost-broker recovery, the end of a parked handler
//! with its handle or the gear, and the two `pending` states of a key
//! (nothing registered, registration kept), each pinned on its own.

use super::*;

// ---------------------------------------------------------------------------
// The lost broker: stream end, stream error, the broker gone from the hub
// ---------------------------------------------------------------------------

/// A backoff whose every wait is 5 to 10 seconds, so a wait is told apart from
/// the test's own polling on the paused clock.
fn long_backoff_timing() -> ClientTiming {
    ClientTiming {
        backoff: Backoff::with_steps(Duration::from_secs(10), Duration::from_secs(10)),
        ..timing()
    }
}

#[derive(Clone, Copy)]
enum Loss {
    StreamEnds,
    StreamFails,
    BrokerRemoved,
}

impl Loss {
    fn happens(self, f: &Fixture) {
        match self {
            Self::StreamEnds => f.broker.streams.lock().unwrap().clear(),
            Self::StreamFails => f.broker.send(Err(EventBrokerError::Internal(
                "the stream broke".to_owned(),
            ))),
            Self::BrokerRemoved => {
                f.hub
                    .remove::<dyn EventBrokerApi>()
                    .expect("the broker was plugged");
            }
        }
    }
}

/// The group was left and deleted on the loss; returns the deleted group.
async fn assert_released(f: &Fixture, first_group: ConsumerGroupId) {
    until("the group to be left and deleted", || {
        f.broker.leaves() == 1 && !f.broker.deletes().is_empty()
    })
    .await;
    assert_eq!(f.broker.deletes(), vec![first_group]);
    assert!(
        f.logs
            .text()
            .contains("the activation consumer group is paused"),
        "the loss is logged as a pause"
    );
}

/// A new group joined on the broker once the backoff ended, not before.
async fn assert_recreated_after_the_backoff(f: &Fixture, first_group: ConsumerGroupId) {
    // The shortest wait of the schedule is 5 seconds.
    tokio::time::advance(Duration::from_secs(4)).await;
    tokio::task::yield_now().await;
    assert_eq!(
        f.broker.creates(),
        1,
        "no new group before the backoff ends"
    );
    assert_eq!(f.broker.joins().len(), 1);

    // The longest is 10 seconds.
    tokio::time::advance(Duration::from_secs(7)).await;
    until("a new group to be created and joined", || {
        f.broker.creates() == 2 && f.broker.joins().len() == 2 && f.broker.opened_streams() >= 1
    })
    .await;
    let second_group = f.broker.creates.lock().unwrap()[1];
    assert_ne!(
        second_group, first_group,
        "a new group, not the deleted one"
    );
    assert_eq!(f.broker.joins()[1].group, second_group);
    assert_eq!(f.broker.joins()[1].interests.len(), 1);
    assert_eq!(f.broker.deletes(), vec![first_group], "the new group stays");
    assert_eq!(f.broker.leaves(), 1);
}

async fn broker_lost_then_back(loss: Loss) {
    let f = fixture(true, long_backoff_timing());
    let handler = Recorder::applying();
    let handle = joined(&f, &["a", "b"], tenant(1), &handler).await;
    let before = format!("{:?}", handle.states());
    let reads_before = f.reader.requests().len();
    assert_eq!(f.broker.creates(), 1);
    assert_eq!(f.broker.joins().len(), 1);
    assert_eq!(f.broker.leaves(), 0);
    let first_group = f.broker.creates.lock().unwrap()[0];

    loss.happens(&f);
    assert_released(&f, first_group).await;
    // Where the broker was removed it is back at once; the group still waits.
    if matches!(loss, Loss::BrokerRemoved) {
        f.plug();
    }
    assert_recreated_after_the_backoff(&f, first_group).await;

    // The registrations were the handle's, not the group's: none was redone.
    assert_eq!(f.backend.register_calls("a"), 1);
    assert_eq!(f.backend.register_calls("b"), 1);
    assert_eq!(f.reader.requests().len(), reads_before);
    assert_eq!(format!("{:?}", handle.states()), before);
    assert!(is_registered(&handle, "a") && is_registered(&handle, "b"));
    assert!(handler.bundles().is_empty());

    // And the new group delivers.
    deliver(
        &f,
        tenant(1),
        notification(tenant(1), GEAR, &[("a", tenant(0xc1))]),
    );
    until("the report through the new group", || {
        f.backend
            .reports()
            .iter()
            .any(|(_, r)| r.change_ref == tenant(0xc1).to_string())
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn the_end_of_the_stream_releases_the_group_and_a_new_one_is_made_after_the_backoff() {
    broker_lost_then_back(Loss::StreamEnds).await;
}

#[tokio::test(start_paused = true)]
async fn an_error_frame_releases_the_group_and_a_new_one_is_made_after_the_backoff() {
    broker_lost_then_back(Loss::StreamFails).await;
}

#[tokio::test(start_paused = true)]
async fn a_broker_removed_from_the_hub_releases_the_group_and_a_new_one_is_made_when_it_is_back() {
    broker_lost_then_back(Loss::BrokerRemoved).await;
}

#[tokio::test(start_paused = true)]
async fn a_broker_that_stays_away_is_waited_for_on_the_backoff_and_the_keys_stay_registered() {
    let f = fixture(true, long_backoff_timing());
    let handler = Recorder::applying();
    let handle = joined(&f, &["a"], tenant(1), &handler).await;

    Loss::BrokerRemoved.happens(&f);
    until("the release", || {
        f.broker.leaves() == 1 && f.broker.deletes().len() == 1
    })
    .await;
    for _ in 0..4 {
        tokio::time::advance(Duration::from_secs(11)).await;
        tokio::task::yield_now().await;
    }
    assert_eq!(f.broker.creates(), 1, "nothing to create a group on");
    assert!(is_registered(&handle, "a"));
    assert_eq!(f.backend.register_calls("a"), 1);

    f.plug();
    tokio::time::advance(Duration::from_secs(11)).await;
    until("the group again", || f.broker.creates() == 2).await;
    assert_eq!(f.backend.register_calls("a"), 1);
}

// ---------------------------------------------------------------------------
// A parked handler ends with its handle or the gear
// ---------------------------------------------------------------------------

/// Counts a handler future out of existence however it ends.
struct Dropped<'a>(&'a AtomicUsize);

impl Drop for Dropped<'_> {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

/// A handler parked inside `on_change` until opened; once opened it would
/// apply the bundle, so a delivery that outlived its handle shows as a
/// completed call and a report.
struct Parked {
    open: tokio::sync::watch::Sender<bool>,
    entered: AtomicUsize,
    completed: AtomicUsize,
    dropped: AtomicUsize,
}

impl Parked {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            open: tokio::sync::watch::channel(false).0,
            entered: AtomicUsize::new(0),
            completed: AtomicUsize::new(0),
            dropped: AtomicUsize::new(0),
        })
    }

    fn count(counter: &AtomicUsize) -> usize {
        counter.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl SettingChangeHandler for Parked {
    async fn on_change(&self, bundle: ChangeBundle) -> Vec<(SettingKey, ActivationOutcome)> {
        self.entered.fetch_add(1, Ordering::SeqCst);
        let _guard = Dropped(&self.dropped);
        let mut open = self.open.subscribe();
        open.wait_for(|o| *o).await.expect("the gate stays");
        self.completed.fetch_add(1, Ordering::SeqCst);
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

/// A handle with `on_change` parked on a notification of `a`.
async fn parked_on_change() -> (Fixture, SubscriptionHandle, Arc<Parked>) {
    let f = fixture(true, timing());
    let parked = Parked::new();
    let handle = subscribe_with(&f, &["a"], tenant(1), Arc::clone(&parked) as Arc<_>).await;
    until("registered", || is_registered(&handle, "a")).await;
    until("the stream to open", || f.broker.opened_streams() >= 1).await;
    deliver(
        &f,
        tenant(1),
        notification(tenant(1), GEAR, &[("a", tenant(0xc1))]),
    );
    until("the handler to be parked", || {
        Parked::count(&parked.entered) == 1
    })
    .await;
    assert_eq!(Parked::count(&parked.dropped), 0);
    (f, handle, parked)
}

/// The handler future was dropped, and what it would have done never happens.
async fn assert_dropped_and_silent(f: &Fixture, parked: &Parked) {
    until("the handler future to be dropped", || {
        Parked::count(&parked.dropped) == 1
    })
    .await;
    parked.open.send_replace(true);
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(Parked::count(&parked.entered), 1);
    assert_eq!(Parked::count(&parked.completed), 0, "it never resumed");
    assert_eq!(Parked::count(&parked.dropped), 1);
    assert!(f.backend.reports().is_empty(), "nothing was reported");
}

async fn changed_ends(handle: &mut SubscriptionHandle) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while handle.changed().await {}
    })
    .await
    .expect("`changed` returns false once the handle can change no more");
}

#[tokio::test]
async fn dropping_the_handle_drops_the_parked_handler_future() {
    let (f, handle, parked) = parked_on_change().await;

    drop(handle);

    assert_dropped_and_silent(&f, &parked).await;
}

#[tokio::test]
async fn unsubscribing_the_handle_drops_the_parked_handler_future_and_ends_changed() {
    let (f, mut handle, parked) = parked_on_change().await;

    let answers = handle
        .unsubscribe(&ctx_of(GEAR))
        .await
        .expect("unsubscribed");

    assert_eq!(answers[&key("a")], UnsubscribeResult::Unsubscribed);
    assert_dropped_and_silent(&f, &parked).await;
    changed_ends(&mut handle).await;
}

#[tokio::test]
async fn stopping_the_gear_drops_the_parked_handler_future() {
    let (f, handle, parked) = parked_on_change().await;

    f.client.stop().await;

    assert_dropped_and_silent(&f, &parked).await;
    drop(handle);
}

#[tokio::test]
async fn stopping_the_client_ends_changed_of_a_live_handle() {
    let f = fixture(true, timing());
    let mut handle = subscribe_with(&f, &["a"], tenant(1), Parked::new() as Arc<_>).await;
    until("registered", || is_registered(&handle, "a")).await;

    f.client.stop().await;

    changed_ends(&mut handle).await;
    assert!(!handle.changed().await);
}

#[tokio::test]
async fn a_consumer_loop_on_changed_finishes_after_the_client_stops() {
    let f = fixture(true, timing());
    let mut handle = subscribe_with(&f, &["a"], tenant(1), Parked::new() as Arc<_>).await;
    until("registered", || is_registered(&handle, "a")).await;
    let consumer = tokio::spawn(async move { while handle.changed().await {} });
    tokio::task::yield_now().await;
    assert!(!consumer.is_finished());

    f.client.stop().await;

    tokio::time::timeout(Duration::from_secs(5), consumer)
        .await
        .expect("the loop ends by itself")
        .expect("no panic");
}

#[tokio::test]
async fn a_subscribe_after_the_client_stopped_is_refused() {
    let f = fixture(true, timing());
    f.client.stop().await;

    let refused = f
        .client
        .subscribe(
            &ctx_of(GEAR),
            vec![key("a")],
            tenant(1).to_string(),
            Parked::new() as Arc<_>,
        )
        .await;

    assert!(refused.is_err(), "a stopped client takes no subscription");
}

#[tokio::test]
async fn stopping_twice_or_with_an_unsubscribed_handle_does_not_panic() {
    let f = fixture(true, timing());
    let mut gone = subscribe_with(&f, &["a"], tenant(1), Parked::new() as Arc<_>).await;
    let mut live = subscribe_with(&f, &["b"], tenant(1), Parked::new() as Arc<_>).await;
    until("registered", || is_registered(&live, "b")).await;
    gone.unsubscribe(&ctx_of(GEAR)).await.expect("unsubscribed");

    f.client.stop().await;
    f.client.stop().await;

    changed_ends(&mut gone).await;
    changed_ends(&mut live).await;
}

// ---------------------------------------------------------------------------
// The two `pending` states of a key, each held and not passed through
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_key_whose_fresh_read_keeps_failing_is_pending_with_its_registration_kept() {
    let f = fixture(true, timing());
    f.reader.breaks("a", ReadFail::Unavailable);
    let handler = Recorder::applying();

    let handle = f.subscribe(&["a"], tenant(1), &handler).await;
    until("the key to wait on its read", || {
        is_pending(&handle, "a", PendingReason::FreshReadFailed)
    })
    .await;

    // Retried from the read, again and again, never from the registration.
    let reads = f.reader.reads_of("a");
    until("the read to be retried", || {
        f.reader.reads_of("a") >= reads + 3
    })
    .await;
    assert!(is_pending(&handle, "a", PendingReason::FreshReadFailed));
    assert_eq!(f.backend.register_calls("a"), 1, "the registration is kept");
    assert!(handler.bundles().is_empty());

    // The read recovers: the same registration settles, with its value.
    f.reader.broken.lock().unwrap().clear();
    until("registered", || is_registered(&handle, "a")).await;
    assert_eq!(f.backend.register_calls("a"), 1);
}

#[tokio::test]
async fn a_registration_the_service_cannot_take_is_pending_unavailable_with_nothing_registered() {
    let f = fixture(true, timing());
    f.backend
        .register_always_fails("a", RegisterError::Unavailable("lock_exhausted".to_owned()));
    let handler = Recorder::applying();

    let handle = f.subscribe(&["a"], tenant(1), &handler).await;
    until("the key to be pending for the service", || {
        is_pending(&handle, "a", PendingReason::Unavailable)
    })
    .await;

    // It stays so while the registration is retried: never registered, never
    // read — there is nothing to read until the registration commits.
    let calls = f.backend.register_calls("a");
    until("the registration to be retried", || {
        f.backend.register_calls("a") >= calls + 3
    })
    .await;
    assert!(is_pending(&handle, "a", PendingReason::Unavailable));
    assert_eq!(f.reader.reads_of("a"), 0);
    assert!(handler.bundles().is_empty());

    f.backend.declared("a");
    until("registered", || is_registered(&handle, "a")).await;
}
