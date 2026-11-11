//! The delivery loop's own mechanics: the tick owns the timing, the gauges
//! ride on it, a broker that appears later is picked up, and the stop ends it
//! with every state kept. What a tick does is `domain::activation::notifier`'s.
//!
//! Log-capture assertions assume one process per test (nextest, as the gear's
//! `make` targets run) and may flake under plain `cargo test`.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::ActivationDelivery;
use crate::domain::activation::testing::{
    FakeNotifications, FakeScanLock, InMemoryNotificationRepo, PublishScript,
    RecordingNotifyMetrics, capture, logged,
};
use crate::domain::activation::{
    ActivationNotifier, NotificationGateway, NotificationStore, NotifyMetrics, ScanLock,
};

const INTERVAL: Duration = Duration::from_millis(60);
const T1: &str = "00000000-0000-0000-0000-0000000000c1";

struct Rig {
    repo: Arc<InMemoryNotificationRepo>,
    gateway: Arc<FakeNotifications>,
    scan: Arc<FakeScanLock>,
    metrics: Arc<RecordingNotifyMetrics>,
    delivery: Arc<ActivationDelivery>,
}

fn rig() -> Rig {
    rig_with(INTERVAL)
}

fn rig_with(interval: Duration) -> Rig {
    let repo = InMemoryNotificationRepo::new();
    let gateway = FakeNotifications::ready();
    let scan = Arc::new(FakeScanLock::default());
    let metrics = Arc::new(RecordingNotifyMetrics::default());
    let notifier = Arc::new(ActivationNotifier::new(
        Arc::clone(&repo) as Arc<dyn NotificationStore>,
        Arc::clone(&gateway) as Arc<dyn NotificationGateway>,
        Arc::clone(&scan) as Arc<dyn ScanLock>,
        Arc::clone(&metrics) as Arc<dyn NotifyMetrics>,
        interval,
    ));
    Rig {
        repo,
        gateway,
        scan,
        metrics,
        delivery: Arc::new(ActivationDelivery::new(notifier)),
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

fn start(r: &Rig, cancel: &CancellationToken) -> tokio::task::JoinHandle<()> {
    let delivery = Arc::clone(&r.delivery);
    let cancel = cancel.clone();
    tokio::spawn(async move { delivery.run(cancel).await })
}

#[tokio::test]
async fn the_first_pass_runs_at_the_start_not_one_interval_later() {
    // A long interval, so the only thing that can delay the first pass is the
    // start jitter, which is capped at one second: a pass well before the
    // interval elapses proves it ran at the start.
    let r = rig_with(Duration::from_secs(30));
    r.repo.add("gear-x", T1, "k.a", Uuid::from_u128(1));
    let cancel = CancellationToken::new();
    let task = start(&r, &cancel);
    tokio::time::timeout(Duration::from_secs(3), async {
        while r.gateway.events().is_empty() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the first pass ran at the start");
    cancel.cancel();
    task.await.unwrap();
}

#[test]
fn the_start_jitter_stays_within_a_second_and_the_interval() {
    for _ in 0..200 {
        assert!(super::start_jitter(Duration::from_secs(30)) <= Duration::from_secs(1));
        assert!(super::start_jitter(INTERVAL) <= INTERVAL);
        assert_eq!(super::start_jitter(Duration::ZERO), Duration::ZERO);
    }
}

#[tokio::test]
async fn the_gauges_ride_every_tick_and_a_broker_that_appears_later_is_picked_up() {
    let r = rig();
    r.repo.add("gear-x", T1, "k.a", Uuid::from_u128(1));
    r.gateway.broker.store(false, Ordering::SeqCst);
    let cancel = CancellationToken::new();
    let task = start(&r, &cancel);
    until("two gauge refreshes", || {
        r.metrics.awaiting.lock().unwrap().len() >= 2
    })
    .await;
    assert!(r.gateway.events().is_empty(), "no broker, no publish");
    assert!(
        r.repo.sweeps.load(Ordering::SeqCst) >= 1,
        "the forgetting and the sweep run without a broker"
    );
    assert_eq!(r.repo.scans.load(Ordering::SeqCst), 0, "no due scan");

    r.gateway.broker.store(true, Ordering::SeqCst);
    until("the catch-up publish", || r.gateway.events().len() == 1).await;
    assert!(
        r.repo
            .row("gear-x", T1, "k.a")
            .unwrap()
            .last_notified
            .is_some()
    );
    cancel.cancel();
    task.await.unwrap();
}

#[tokio::test]
async fn the_loop_logs_its_start_and_stop_at_info() {
    let r = rig();
    let (buf, _guard) = capture();
    let cancel = CancellationToken::new();
    let task = start(&r, &cancel);
    until("the start line", || logged(&buf).contains("started")).await;
    cancel.cancel();
    task.await.unwrap();
    let log = logged(&buf);
    assert!(
        log.contains("INFO") && log.contains("activation delivery loop started"),
        "{log}"
    );
    assert!(log.contains("activation delivery loop stopped"), "{log}");
}

#[tokio::test]
async fn a_stop_during_a_pass_ends_the_loop_with_the_states_and_the_lock_kept_sound() {
    let r = rig();
    r.repo.add("gear-x", T1, "k.a", Uuid::from_u128(1));
    r.gateway.script.lock().unwrap().push(PublishScript::Hold);
    let cancel = CancellationToken::new();
    let task = start(&r, &cancel);
    r.gateway.gate.arrived.notified().await;
    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("the loop ends within the deadline")
        .unwrap();
    assert_eq!(
        r.scan.held.load(Ordering::SeqCst),
        0,
        "the scan lock is released"
    );
    let row = r.repo.row("gear-x", T1, "k.a").expect("the state is kept");
    assert_eq!(row.last_notified, None);
}

#[tokio::test]
async fn a_stop_before_the_first_tick_ends_the_loop_without_a_pass() {
    let r = rig();
    r.repo.add("gear-x", T1, "k.a", Uuid::from_u128(1));
    let cancel = CancellationToken::new();
    cancel.cancel();
    start(&r, &cancel).await.unwrap();
    assert_eq!(r.scan.attempts.load(Ordering::SeqCst), 0);
    assert!(r.repo.row("gear-x", T1, "k.a").is_some());
}
