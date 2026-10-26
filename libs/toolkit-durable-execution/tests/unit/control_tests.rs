use super::*;
use crate::AuthorizationError;
use std::sync::atomic::{AtomicUsize, Ordering};

fn database_fault() -> StoreError {
    StoreError::Database(toolkit_db::DbError::from(sea_orm::DbErr::Custom(
        "fixture database unavailable".into(),
    )))
}

fn config(limit: u32) -> Config {
    Config {
        dispatch_interval_secs: 1,
        control_plane_failure_limit: limit,
        ..Config::default()
    }
}

#[test]
fn errors_are_triaged_into_pause_skip_and_retry() {
    for error in [
        StoreError::AuthorizationUnavailable,
        StoreError::Authorization(AuthorizationError::denied()),
        StoreError::Forbidden,
    ] {
        assert_eq!(triage(&error), Triage::Pause, "{error}");
    }
    for error in [
        StoreError::Conflict,
        StoreError::Invariant("fixture"),
        StoreError::Domain(crate::error::domain::DomainError::Internal("fixture")),
    ] {
        assert_eq!(triage(&error), Triage::Skip, "{error}");
    }
    for error in [database_fault(), StoreError::DeliveryUnavailable] {
        assert_eq!(triage(&error), Triage::Retry, "{error}");
    }
}

#[tokio::test(start_paused = true)]
async fn authorization_failures_pause_without_counting_towards_the_limit() {
    let calls = AtomicUsize::new(0);
    let stop = CancellationToken::new();
    let config = config(1);
    let looping = control_loop(&config, MissedTickBehavior::Delay, &stop, || async {
        if calls.fetch_add(1, Ordering::SeqCst) == 9 {
            stop.cancel();
        }
        Err(StoreError::Authorization(AuthorizationError::unavailable()))
    });
    tokio::time::timeout(Duration::from_secs(60), looping)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 10);
}

#[tokio::test(start_paused = true)]
async fn storage_failures_stop_after_exactly_the_limit() {
    let calls = AtomicUsize::new(0);
    let stop = CancellationToken::new();
    let config = config(3);
    let error = tokio::time::timeout(
        Duration::from_secs(600),
        control_loop(&config, MissedTickBehavior::Delay, &stop, || async {
            calls.fetch_add(1, Ordering::SeqCst);
            Err(database_fault())
        }),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert!(matches!(error, StoreError::Database(_)));
    assert_eq!(calls.load(Ordering::SeqCst), 3);
}

#[tokio::test(start_paused = true)]
async fn success_resets_the_failure_count() {
    let calls = AtomicUsize::new(0);
    let stop = CancellationToken::new();
    let config = config(2);
    let looping = control_loop(&config, MissedTickBehavior::Delay, &stop, || async {
        let call = calls.fetch_add(1, Ordering::SeqCst);
        if call == 7 {
            stop.cancel();
        }
        // Fail, succeed, fail, succeed...: never two consecutive failures.
        if call.is_multiple_of(2) {
            Err(database_fault())
        } else {
            Ok(())
        }
    });
    tokio::time::timeout(Duration::from_secs(600), looping)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 8);
}
