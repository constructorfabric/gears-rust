use super::*;

#[tokio::test(start_paused = true)]
async fn reserves_and_lease_caps_cover_configuration_boundaries() {
    for (total, runtime, cleanup) in [
        (1, 833_333_334, 666_666_668),
        (30, 25_000_000_000, 20_000_000_000),
        (86_400, 86_395_000_000_000, 86_390_000_000_000),
    ] {
        let budget = ShutdownBudget::new(Duration::from_secs(total));
        let start = budget.begin();
        assert_eq!(
            budget.runtime_deadline() - start,
            Duration::from_nanos(runtime)
        );
        assert_eq!(
            budget.cleanup_deadline(start + Duration::from_secs(100_000)) - start,
            Duration::from_nanos(cleanup)
        );
        let lease = start + Duration::from_millis(100);
        assert_eq!(budget.cleanup_deadline(lease), lease);
    }
}

#[tokio::test(start_paused = true)]
async fn late_workers_and_io_share_the_original_deadline() {
    let budget = ShutdownBudget::new(Duration::from_secs(30));
    let start = budget.begin();
    tokio::time::advance(Duration::from_secs(19)).await;
    assert_eq!(budget.begin(), start);
    assert_eq!(
        budget.cleanup_deadline(start + Duration::from_secs(120)),
        start + Duration::from_secs(20)
    );
    let stop = CancellationToken::new();
    stop.cancel();
    assert!(
        budget
            .bound(&stop, std::future::pending::<()>())
            .await
            .is_err()
    );
    assert_eq!(start.elapsed(), Duration::from_secs(25));
    assert!(
        budget
            .bound(&stop, std::future::pending::<()>())
            .await
            .is_err()
    );
    assert_eq!(start.elapsed(), Duration::from_secs(25));
}

#[tokio::test(start_paused = true)]
async fn normal_io_does_not_start_the_shutdown_clock() {
    let budget = ShutdownBudget::new(Duration::from_secs(30));
    let stop = CancellationToken::new();
    budget
        .bound(&stop, tokio::time::sleep(Duration::from_secs(120)))
        .await
        .unwrap();
    assert!(budget.started.get().is_none());
    let start = Instant::now();
    stop.cancel();
    assert!(
        budget
            .bound(&stop, std::future::pending::<()>())
            .await
            .is_err()
    );
    assert_eq!(start.elapsed(), Duration::from_secs(25));
}
