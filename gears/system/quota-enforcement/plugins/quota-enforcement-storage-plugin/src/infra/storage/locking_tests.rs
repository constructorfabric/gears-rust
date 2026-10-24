use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use quota_enforcement_sdk::StorageError;
use sea_orm::DbErr;
use toolkit_db::secure::ScopeError;

use super::{ContentionBudget, is_lock_not_available, with_budget};
use crate::infra::storage::consumption_store::TxError;

/// What a `NOWAIT` read on a held row comes back as once it is re-wrapped.
fn refused() -> TxError {
    TxError::Scope(ScopeError::Db(DbErr::Custom(
        "error returned from database: could not obtain lock on row in relation \"qe_quotas\""
            .to_owned(),
    )))
}

#[test]
fn a_refused_lock_is_recognised_and_other_failures_are_not() {
    assert!(is_lock_not_available(&refused()));
    assert!(!is_lock_not_available(&TxError::Scope(ScopeError::Db(
        DbErr::Custom("connection reset by peer".to_owned())
    ))));
    assert!(!is_lock_not_available(&TxError::Storage(
        StorageError::IdempotencyPayloadMismatch
    )));
}

#[tokio::test]
async fn a_zero_budget_answers_the_first_refusal_without_retrying() {
    let attempts = AtomicU32::new(0);
    let outcome: Result<(), TxError> =
        with_budget(ContentionBudget::starting_now(Duration::ZERO), || {
            attempts.fetch_add(1, Ordering::SeqCst);
            async { Err(refused()) }
        })
        .await;
    assert!(matches!(
        outcome,
        Err(TxError::Storage(StorageError::LeaseContentionTimeout))
    ));
    assert_eq!(
        attempts.load(Ordering::SeqCst),
        1,
        "fail fast means one try"
    );
}

#[tokio::test]
async fn a_row_released_within_the_budget_is_taken_on_a_retry() {
    let attempts = AtomicU32::new(0);
    let outcome = with_budget(
        ContentionBudget::starting_now(Duration::from_secs(5)),
        || {
            let this = attempts.fetch_add(1, Ordering::SeqCst);
            async move { if this < 3 { Err(refused()) } else { Ok(this) } }
        },
    )
    .await;
    assert!(
        matches!(outcome, Ok(3)),
        "the fourth try found the row free"
    );
}

#[tokio::test]
async fn no_attempt_starts_once_the_pause_has_spent_the_budget() {
    // Shorter than the first backoff: the pause runs to the deadline, and a
    // second attempt would start after it.
    let attempts = AtomicU32::new(0);
    let outcome: Result<(), TxError> = with_budget(
        ContentionBudget::starting_now(Duration::from_millis(1)),
        || {
            attempts.fetch_add(1, Ordering::SeqCst);
            async { Err(refused()) }
        },
    )
    .await;
    assert!(matches!(
        outcome,
        Err(TxError::Storage(StorageError::LeaseContentionTimeout))
    ));
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn the_budget_bounds_the_total_wait_across_every_retry() {
    let started = Instant::now();
    let outcome: Result<(), TxError> = with_budget(
        ContentionBudget::starting_now(Duration::from_millis(120)),
        || async { Err(refused()) },
    )
    .await;
    let waited = started.elapsed();
    assert!(matches!(
        outcome,
        Err(TxError::Storage(StorageError::LeaseContentionTimeout))
    ));
    assert!(
        waited >= Duration::from_millis(100),
        "gave up early: {waited:?}"
    );
    assert!(
        waited < Duration::from_millis(600),
        "a deadline, not a per-retry bound: {waited:?}"
    );
}

#[tokio::test]
async fn only_a_refused_lock_is_retried() {
    let attempts = AtomicU32::new(0);
    let outcome: Result<(), TxError> = with_budget(
        ContentionBudget::starting_now(Duration::from_secs(5)),
        || {
            attempts.fetch_add(1, Ordering::SeqCst);
            async { Err(TxError::Storage(StorageError::IdempotencyPayloadMismatch)) }
        },
    )
    .await;
    assert!(matches!(
        outcome,
        Err(TxError::Storage(StorageError::IdempotencyPayloadMismatch))
    ));
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
}

#[test]
fn a_scope_maps_to_the_same_stripe_everywhere() {
    use crate::infra::storage::repo::idempotency_repo::{ScopeKey, stripe_of};
    let key = ScopeKey {
        tenant_id: uuid::Uuid::nil(),
        subject_key: &[1; 32],
        operation_type: "debit",
        idem_key: "k1",
    };
    // Pinned from an independent FNV-1a computation: a change here remaps
    // every scope, which two versions running side by side must never do.
    assert_eq!(stripe_of(&key), 47_844);
    assert_eq!(
        stripe_of(&ScopeKey {
            operation_type: "credit",
            ..key
        }),
        32_968
    );
    assert_eq!(
        stripe_of(&ScopeKey {
            idem_key: "k2",
            ..key
        }),
        49_149
    );
}
