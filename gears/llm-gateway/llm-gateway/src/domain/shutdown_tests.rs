//! `ShutdownSignal`: begin, the cut deadline and idempotence (paused clock).

use std::time::Duration;

use tokio_util::sync::CancellationToken;

use super::ShutdownSignal;

const CUT_AFTER: Duration = Duration::from_secs(20);

#[tokio::test(start_paused = true)]
async fn a_fresh_signal_has_neither_begun_nor_cut() {
    let signal = ShutdownSignal::new(CUT_AFTER);
    assert!(!signal.is_begun());
    assert!(!signal.cut().is_cancelled());
}

#[tokio::test(start_paused = true)]
async fn begin_is_visible_at_once_and_the_cut_fires_exactly_after_the_delay() {
    let signal = ShutdownSignal::new(CUT_AFTER);
    let started = tokio::time::Instant::now();

    signal.begin();
    assert!(signal.is_begun());
    assert!(!signal.cut().is_cancelled());

    signal.cut().cancelled().await;
    assert_eq!(started.elapsed(), CUT_AFTER);
}

#[tokio::test(start_paused = true)]
async fn a_second_begin_does_not_restart_the_cut_timer() {
    let signal = ShutdownSignal::new(CUT_AFTER);
    let started = tokio::time::Instant::now();
    signal.begin();
    tokio::time::sleep(Duration::from_secs(10)).await;
    signal.begin();

    signal.cut().cancelled().await;
    assert_eq!(started.elapsed(), CUT_AFTER);
}

#[tokio::test(start_paused = true)]
async fn clones_share_the_state() {
    let signal = ShutdownSignal::new(CUT_AFTER);
    let clone = signal.clone();
    clone.begin();
    assert!(signal.is_begun());
}

#[tokio::test(start_paused = true)]
async fn begin_on_waits_for_the_stop_token() {
    let signal = ShutdownSignal::new(CUT_AFTER);
    let stop = CancellationToken::new();
    let waiter = {
        let signal = signal.clone();
        let stop = stop.clone();
        tokio::spawn(async move { signal.begin_on(&stop).await })
    };
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(!signal.is_begun(), "not before the stop token fires");

    stop.cancel();
    waiter.await.expect("waiter");
    assert!(signal.is_begun());
}

#[test]
fn the_default_signal_is_not_begun() {
    assert!(!ShutdownSignal::default().is_begun());
}
