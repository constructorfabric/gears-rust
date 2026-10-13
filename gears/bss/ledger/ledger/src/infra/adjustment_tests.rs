//! The adjustment-flow repository error mapping.

use super::{map_adjustment_repo_err, map_note_record_err};
use crate::domain::error::DomainError;
use crate::domain::model::RepoError;
use bss_ledger_sdk::MoneyError;

#[test]
fn conflicts_retry_caller_mismatches_are_client_errors_the_rest_is_internal() {
    assert!(matches!(
        map_adjustment_repo_err("ctx", RepoError::Conflict("stale version".into())),
        DomainError::ConcurrentModification(_)
    ));
    assert!(matches!(
        map_adjustment_repo_err("ctx", RepoError::Money(MoneyError::CurrencyMismatch)),
        DomainError::CurrencyMismatch(_)
    ));
    assert!(matches!(
        map_adjustment_repo_err("ctx", RepoError::Money(MoneyError::ScaleMismatch)),
        DomainError::InconsistentScale(_)
    ));
    assert!(matches!(
        map_adjustment_repo_err("ctx", RepoError::InvalidStoredMoney("bad".into())),
        DomainError::Internal(_)
    ));
}

/// The flow mapper delegates to the one `RepoError` table, keeping the context
/// on internal faults.
#[test]
fn the_flow_mapper_delegates_to_the_shared_table_with_context() {
    assert!(matches!(
        map_adjustment_repo_err("ctx", RepoError::InvalidRequest("bad".into())),
        DomainError::InvalidRequest(_)
    ));
    assert!(matches!(
        map_adjustment_repo_err("ctx", RepoError::CurrencyScaleLocked("EUR".into())),
        DomainError::CurrencyScaleLocked(_)
    ));
    match map_adjustment_repo_err("seed exposure", RepoError::Db("down".into())) {
        DomainError::Internal(detail) => assert!(detail.starts_with("seed exposure: "), "{detail}"),
        other => panic!("expected Internal, got {other:?}"),
    }
}

/// A note row's amounts are ledger-computed: a money mismatch persisting it is
/// an internal invariant breach, never the client's `CURRENCY_MISMATCH`; only a
/// lost race retries.
#[test]
fn a_note_record_money_mismatch_is_internal_and_only_a_conflict_retries() {
    assert!(matches!(
        map_note_record_err("insert credit_note", RepoError::Conflict("dup".into())),
        DomainError::ConcurrentModification(_)
    ));
    for money in [MoneyError::CurrencyMismatch, MoneyError::ScaleMismatch] {
        assert!(matches!(
            map_note_record_err("insert credit_note", RepoError::Money(money)),
            DomainError::Internal(_)
        ));
    }
    assert!(matches!(
        map_note_record_err("insert debit_note", RepoError::InvalidRequest("bad".into())),
        DomainError::Internal(_)
    ));
}
