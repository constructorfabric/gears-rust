//! Unit tests for the pure schedule-change vocabulary (Group H4): the treatment
//! gate (the §3.6 invariant — `catch_up`/unknown never auto-proceeds) and the
//! action parser.
#![allow(clippy::unwrap_used, clippy::panic)]

use super::{ChangeAction, ProceedTreatment, gate_treatment};
use crate::domain::error::DomainError;

#[test]
fn prospective_proceeds() {
    assert_eq!(
        gate_treatment("prospective").unwrap(),
        ProceedTreatment::Prospective
    );
}

#[test]
fn separate_contract_proceeds() {
    assert_eq!(
        gate_treatment("separate_contract").unwrap(),
        ProceedTreatment::SeparateContract
    );
}

#[test]
fn catch_up_is_review() {
    let err = gate_treatment("catch_up").expect_err("catch_up must not proceed");
    assert!(
        matches!(err, DomainError::ModificationTreatmentReview(_)),
        "catch_up must surface as ModificationTreatmentReview, got {err:?}"
    );
}

#[test]
fn unknown_treatment_is_review() {
    for literal in ["", "PROSPECTIVE", "retrospective", "nonsense"] {
        let err = gate_treatment(literal).expect_err("unknown treatment must not proceed");
        assert!(
            matches!(err, DomainError::ModificationTreatmentReview(_)),
            "treatment {literal:?} must be a review, got {err:?}"
        );
    }
}

#[test]
fn action_parses_cancel_and_replace() {
    assert_eq!(ChangeAction::parse("cancel").unwrap(), ChangeAction::Cancel);
    assert_eq!(
        ChangeAction::parse("replace").unwrap(),
        ChangeAction::Replace
    );
}

#[test]
fn action_rejects_unknown() {
    let err = ChangeAction::parse("delete").expect_err("unknown action rejected");
    assert!(
        matches!(err, DomainError::InvalidRequest(_)),
        "unknown action must be InvalidRequest, got {err:?}"
    );
}
