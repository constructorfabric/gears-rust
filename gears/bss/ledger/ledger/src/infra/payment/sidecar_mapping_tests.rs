//! Repository error mapping of the payment sidecars: a counter CAS race must
//! stay the one retryable variant, a cap must keep its named wire rejection.

use super::{map_clawback_repo_err, map_repo_err, map_return_repo_err};
use crate::domain::error::DomainError;
use crate::domain::model::RepoError;

fn conflict() -> RepoError {
    RepoError::Conflict("stale payment counter version".into())
}

fn cap() -> RepoError {
    RepoError::MoneyOutCapExceeded("payment_settlement counter cap".into())
}

fn db() -> RepoError {
    RepoError::Db("storage failure".into())
}

#[test]
fn return_sidecar_mapping() {
    assert!(matches!(
        map_return_repo_err(conflict()),
        DomainError::ConcurrentModification(_)
    ));
    assert!(matches!(
        map_return_repo_err(cap()),
        DomainError::SettlementReturnOverAllocated(_)
    ));
    assert!(matches!(
        map_return_repo_err(db()),
        DomainError::Internal(_)
    ));
}

#[test]
fn payment_sidecar_mapping() {
    assert!(matches!(
        map_repo_err(conflict()),
        DomainError::ConcurrentModification(_)
    ));
    assert!(matches!(
        map_repo_err(cap()),
        DomainError::MoneyOutCapExceeded(_)
    ));
    assert!(matches!(
        map_repo_err(RepoError::DisputeNotOpen("dispute d-1 is missing".into())),
        DomainError::InvalidDisputeTransition(_)
    ));
    assert!(matches!(map_repo_err(db()), DomainError::Internal(_)));
}

#[test]
fn clawback_sidecar_mapping() {
    assert!(matches!(
        map_clawback_repo_err(conflict()),
        DomainError::ConcurrentModification(_)
    ));
    assert!(matches!(
        map_clawback_repo_err(cap()),
        DomainError::ChargebackExceedsSettled(_)
    ));
    assert!(matches!(
        map_clawback_repo_err(db()),
        DomainError::Internal(_)
    ));
}
