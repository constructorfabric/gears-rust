//! The bulk vocabulary's round trip, executed.
//!
//! What these pin is the **agreement with the store**: every token here is a
//! value the `CHECK` admits, and every value the `CHECK` admits is a token here.
//! The list is transcribed from `pricing_bulk_operation`'s current form — not from
//! `pricing_bulk_operation`, which created the table with six states and is four
//! migrations out of date (D-283's lesson, applied before writing rather than
//! after).

use super::{BulkKind, BulkState};

/// Exactly the tokens `chk_pricing_bulk_operation_state` admits, in the
/// migration's own order.
const STORED_STATES: &[&str] = &[
    "validating",
    "validation_failed",
    "awaiting_approval",
    "committing",
    "completed",
    "completed_with_conflicts",
    "rejected",
];

/// Exactly the tokens `chk_pricing_bulk_operation_kind` admits.
const STORED_KINDS: &[&str] = &["import", "repricing"];

#[test]
fn every_state_the_check_admits_round_trips() {
    for token in STORED_STATES {
        let parsed = BulkState::parse(token).expect("the CHECK admits it, so this must read it");
        assert_eq!(
            parsed.as_str(),
            *token,
            "the token has to survive the round trip: a state that reads as one thing and \
             writes as another would move a run by writing it back"
        );
    }
}

#[test]
fn every_kind_the_check_admits_round_trips() {
    for token in STORED_KINDS {
        assert_eq!(
            BulkKind::parse(token)
                .expect("the CHECK admits it")
                .as_str(),
            *token
        );
    }
}

#[test]
fn a_token_outside_the_enumeration_is_refused_rather_than_defaulted() {
    // The boundary reading `RepoError::CorruptRow` exists for: a value the store
    // holds and this crate does not must stop here, not travel into a rule as
    // some nearest neighbour.
    assert!(BulkState::parse("cancelled").is_err());
    assert!(BulkState::parse("").is_err());
    assert!(
        BulkState::parse("Validating").is_err(),
        "case is not a token"
    );
    assert!(BulkKind::parse("clone").is_err());
}

#[test]
fn the_terminal_states_are_the_four_the_check_pairs_with_completed_at() {
    // `chk_pricing_bulk_operation_completed_at` says `completed_at IS NOT NULL`
    // exactly when the state is one of these four. A fifth state added here
    // without the CHECK, or the reverse, is a run the store thinks is over and
    // this crate does not.
    let terminal: Vec<&str> = STORED_STATES
        .iter()
        .filter(|token| {
            BulkState::parse(token)
                .expect("a stored token")
                .is_terminal()
        })
        .copied()
        .collect();
    assert_eq!(
        terminal,
        vec![
            "validation_failed",
            "completed",
            "completed_with_conflicts",
            "rejected"
        ]
    );
}

#[test]
fn awaiting_approval_and_committing_are_not_terminal() {
    // Stated separately because `is_terminal` returning `true` for everything
    // would satisfy the list above only by accident of ordering, and a run that
    // reads as over while it still holds its row locks is the leak
    // `inst-bk-lock` exists to prevent.
    assert!(!BulkState::AwaitingApproval.is_terminal());
    assert!(!BulkState::Committing.is_terminal());
    assert!(!BulkState::Validating.is_terminal());
}
