use super::*;

fn database() -> StoreError {
    StoreError::Database(toolkit_db::DbError::Sea(sea_orm::DbErr::Custom(
        "fixture database unavailable".into(),
    )))
}

fn scoped_database() -> StoreError {
    StoreError::Scope(toolkit_db::secure::ScopeError::Db(sea_orm::DbErr::Custom(
        "fixture scoped database unavailable".into(),
    )))
}

#[test]
fn database_errors_share_a_bounded_exponential_budget_and_success_resets_it() {
    let mut retry = RetryPolicy::new(&Config {
        dispatch_interval_secs: 5,
        control_plane_failure_limit: 8,
        ..Config::default()
    });
    for (index, seconds) in [5, 10, 20, 40, 60, 60, 60].into_iter().enumerate() {
        let error = if index % 2 == 0 {
            database()
        } else {
            scoped_database()
        };
        assert_eq!(
            retry.retry_delay(&error),
            Some(Duration::from_secs(seconds))
        );
    }
    assert_eq!(retry.retry_delay(&database()), None);
    retry.reset();
    assert_eq!(
        retry.retry_delay(&scoped_database()),
        Some(Duration::from_secs(5))
    );
}

#[test]
fn permanent_scope_and_delivery_errors_are_never_retried() {
    let mut retry = RetryPolicy::new(&Config::default());
    for error in [
        StoreError::Scope(toolkit_db::secure::ScopeError::Denied("fixture")),
        StoreError::Scope(toolkit_db::secure::ScopeError::Invalid("fixture")),
        StoreError::DeliveryUnavailable,
        StoreError::Invariant("fixture"),
        StoreError::AuthorizationUnavailable,
        StoreError::Forbidden,
        StoreError::Conflict,
    ] {
        assert_eq!(retry.retry_delay(&error), None);
    }
    // Non-database errors do not consume the database budget.
    assert_eq!(retry.retry_delay(&database()), Some(Duration::from_secs(5)));
}

#[test]
fn single_failure_budget_stops_immediately_and_large_polling_interval_stays_bounded() {
    let mut retry = RetryPolicy::new(&Config {
        control_plane_failure_limit: 1,
        ..Config::default()
    });
    assert_eq!(retry.retry_delay(&database()), None);
    let mut retry = RetryPolicy::new(&Config {
        dispatch_interval_secs: 86_400,
        ..Config::default()
    });
    for _ in 0..4 {
        assert_eq!(
            retry.retry_delay(&database()),
            Some(Duration::from_hours(24))
        );
    }
    assert_eq!(retry.retry_delay(&database()), None);
}

#[tokio::test(start_paused = true)]
async fn backoff_wait_completes_at_deadline_and_shutdown_interrupts_it() {
    let stop = CancellationToken::new();
    let began = tokio::time::Instant::now();
    assert!(wait_retry(Duration::from_secs(20), &stop).await);
    assert_eq!(began.elapsed(), Duration::from_secs(20));

    let task_stop = stop.clone();
    let task = tokio::spawn(async move { wait_retry(Duration::from_secs(60), &task_stop).await });
    tokio::task::yield_now().await;
    let began = tokio::time::Instant::now();
    stop.cancel();
    assert!(!task.await.unwrap());
    assert_eq!(began.elapsed(), Duration::ZERO);
    assert!(!wait_retry(Duration::ZERO, &stop).await);
}
