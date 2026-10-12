use std::time::Duration;

/// A tick that overruns its second delays the next by a full period: the missed ticks are not
/// fired back to back, which would repeat the due scans and the Products calls while Products is
/// slow (PS-12).
#[tokio::test(start_paused = true)]
async fn a_slow_tick_delays_the_next_instead_of_bursting() {
    let mut interval = super::tick_interval();
    interval.tick().await;
    // The tick's work took five seconds.
    tokio::time::advance(Duration::from_secs(5)).await;
    interval.tick().await;
    let overdue = tokio::time::Instant::now();
    interval.tick().await;
    assert!(
        overdue.elapsed() >= Duration::from_secs(1),
        "the tick after an overdue one waits a period, not {:?}",
        overdue.elapsed()
    );
}
