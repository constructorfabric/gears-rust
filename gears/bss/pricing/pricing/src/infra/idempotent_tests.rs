//! What the seam decides without a database.
//!
//! Everything else about this module is a property of one transaction over a
//! real store — the claim, the rollback, the replay — and lives in
//! `tests/sqlite_idempotent_seam.rs`, where the assertions can look at rows.
//! What is left here is the boundary the rollback crosses: a typed refusal
//! raised **inside** the transaction has to reach the caller as itself, while a
//! failure to begin or commit is infrastructure and has no domain meaning. Get
//! that wrong in the flattening direction and every `DUPLICATE_SCOPE_KEY` a
//! guarded create meets arrives as an internal fault.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use toolkit_db::secure::{InfraError, TxError};

use super::tx_failure;
use crate::domain::error::DomainError;

#[test]
fn a_typed_refusal_survives_the_rollback_as_itself() {
    let refusal = DomainError::DuplicateScopeKey("plan/USD/EU/base".to_owned());

    assert_eq!(tx_failure(TxError::Domain(refusal.clone())), refusal);
}

#[test]
fn a_failure_to_begin_or_commit_has_no_domain_meaning() {
    // The only thing outside the body is the BEGIN and the COMMIT, and neither
    // is a request the caller could reshape - so it is an internal fault and
    // never a refusal a client is invited to act on.
    let flattened = tx_failure(TxError::Infra(InfraError::new("database is locked")));

    match flattened {
        DomainError::Internal(detail) => assert!(detail.contains("database is locked"), "{detail}"),
        other => panic!("an infra failure is internal, got {other:?}"),
    }
}
