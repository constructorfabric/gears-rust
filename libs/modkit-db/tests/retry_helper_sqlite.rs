#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg(feature = "sqlite")]

//! Tests for [`Db::transaction_serializable_with_retry`] (and `_max`).
//!
//! These exercise the retry policy itself (predicate, attempt counting,
//! exhaustion, log on retry) using `sqlite::memory:` and a domain error
//! type — they don't depend on `PostgreSQL`/`InnoDB` SQLSTATE detection.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use modkit_db::{ConnectOpts, DEFAULT_SERIALIZATION_RETRIES, DbError, connect_db};

#[derive(Debug)]
enum TestError {
    Retryable,
    Permanent,
    #[allow(dead_code)]
    Db(DbError),
}

impl From<DbError> for TestError {
    fn from(e: DbError) -> Self {
        TestError::Db(e)
    }
}

fn is_retryable(e: &TestError) -> bool {
    matches!(e, TestError::Retryable)
}

#[tokio::test]
async fn retry_default_succeeds_after_transient_failures() {
    // The default budget is `DEFAULT_SERIALIZATION_RETRIES` (= 3), so a body
    // that fails twice and succeeds on the third attempt must succeed without
    // the caller specifying a max.
    let db = connect_db("sqlite::memory:", ConnectOpts::default())
        .await
        .expect("connect sqlite memory");
    let counter = Arc::new(AtomicU32::new(0));

    let counter_for_body = Arc::clone(&counter);
    let result: Result<u32, TestError> = db
        .transaction_serializable_with_retry(is_retryable, move |_tx| {
            let counter = Arc::clone(&counter_for_body);
            Box::pin(async move {
                let n = counter.fetch_add(1, Ordering::SeqCst) + 1;
                if n < DEFAULT_SERIALIZATION_RETRIES {
                    Err(TestError::Retryable)
                } else {
                    Ok(n)
                }
            })
        })
        .await;

    assert!(
        matches!(result, Ok(n) if n == DEFAULT_SERIALIZATION_RETRIES),
        "got {result:?}"
    );
    assert_eq!(
        counter.load(Ordering::SeqCst),
        DEFAULT_SERIALIZATION_RETRIES
    );
}

#[tokio::test]
async fn retry_returns_last_error_on_exhaustion() {
    let db = connect_db("sqlite::memory:", ConnectOpts::default())
        .await
        .expect("connect sqlite memory");
    let counter = Arc::new(AtomicU32::new(0));

    let counter_for_body = Arc::clone(&counter);
    let result: Result<(), TestError> = db
        .transaction_serializable_with_retry_max(3, is_retryable, move |_tx| {
            let counter = Arc::clone(&counter_for_body);
            Box::pin(async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Err(TestError::Retryable)
            })
        })
        .await;

    assert!(
        matches!(result, Err(TestError::Retryable)),
        "got {result:?}"
    );
    assert_eq!(counter.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn non_retryable_error_returns_immediately() {
    let db = connect_db("sqlite::memory:", ConnectOpts::default())
        .await
        .expect("connect sqlite memory");
    let counter = Arc::new(AtomicU32::new(0));

    let counter_for_body = Arc::clone(&counter);
    let result: Result<(), TestError> = db
        .transaction_serializable_with_retry_max(3, is_retryable, move |_tx| {
            let counter = Arc::clone(&counter_for_body);
            Box::pin(async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Err(TestError::Permanent)
            })
        })
        .await;

    assert!(
        matches!(result, Err(TestError::Permanent)),
        "got {result:?}"
    );
    assert_eq!(counter.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn zero_max_attempts_treated_as_one() {
    let db = connect_db("sqlite::memory:", ConnectOpts::default())
        .await
        .expect("connect sqlite memory");
    let counter = Arc::new(AtomicU32::new(0));

    let counter_for_body = Arc::clone(&counter);
    let result: Result<(), TestError> = db
        .transaction_serializable_with_retry_max(0, is_retryable, move |_tx| {
            let counter = Arc::clone(&counter_for_body);
            Box::pin(async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Err(TestError::Retryable)
            })
        })
        .await;

    assert!(
        matches!(result, Err(TestError::Retryable)),
        "got {result:?}"
    );
    assert_eq!(counter.load(Ordering::SeqCst), 1);
}
