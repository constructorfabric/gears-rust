//! The dispute state machine: openings, reopenings and outcomes, cycle by cycle.
#![allow(clippy::unwrap_used)]

use super::*;

fn row(phase: DisputePhase, cycle: i32) -> ObservedDispute<'static> {
    ObservedDispute {
        payment_id: "pay-1",
        last_phase: phase,
        cycle,
    }
}

#[test]
fn a_new_dispute_opens_at_cycle_one_only() {
    assert!(check_transition("d", None, "pay-1", DisputePhase::Opened, 1).is_ok());
    let err = check_transition("d", None, "pay-1", DisputePhase::Opened, 2).unwrap_err();
    assert!(matches!(
        err,
        DisputeTransitionError::FirstCycle { cycle: 2, .. }
    ));
    assert!(err.to_string().contains("not 2"), "{err}");
}

#[test]
fn a_reopen_follows_a_terminal_cycle_at_the_next_cycle() {
    for terminal in [DisputePhase::Won, DisputePhase::Lost] {
        assert!(
            check_transition(
                "d",
                Some(row(terminal, 1)),
                "pay-1",
                DisputePhase::Opened,
                2
            )
            .is_ok()
        );
        for cycle in [1, 3] {
            assert!(matches!(
                check_transition(
                    "d",
                    Some(row(terminal, 1)),
                    "pay-1",
                    DisputePhase::Opened,
                    cycle
                ),
                Err(DisputeTransitionError::Reopen { .. })
            ));
        }
    }
    for open in [DisputePhase::Opened, DisputePhase::Partial] {
        let err = check_transition("d", Some(row(open, 1)), "pay-1", DisputePhase::Opened, 2)
            .unwrap_err();
        assert!(matches!(err, DisputeTransitionError::Reopen { .. }));
    }
    let err = check_transition(
        "d",
        Some(row(DisputePhase::Won, 1)),
        "pay-1",
        DisputePhase::Opened,
        3,
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("cycle 3") && err.to_string().contains("observed WON at cycle 1"),
        "{err}"
    );
    // A cycle at the i32 ceiling cannot be reopened.
    assert!(
        check_transition(
            "d",
            Some(row(DisputePhase::Lost, i32::MAX)),
            "pay-1",
            DisputePhase::Opened,
            i32::MIN
        )
        .is_err()
    );
}

#[test]
fn a_reopen_cannot_change_the_payment() {
    let err = check_transition(
        "d",
        Some(row(DisputePhase::Won, 1)),
        "pay-2",
        DisputePhase::Opened,
        2,
    )
    .unwrap_err();
    assert!(matches!(err, DisputeTransitionError::ForeignPayment { .. }));
    assert!(err.to_string().contains("pay-1") && err.to_string().contains("pay-2"));
}

#[test]
fn an_outcome_resolves_only_the_opened_cycle_it_names() {
    for outcome in [DisputePhase::Won, DisputePhase::Lost, DisputePhase::Partial] {
        assert!(
            check_transition("d", Some(row(DisputePhase::Opened, 2)), "pay-1", outcome, 2).is_ok()
        );
        // A stale cycle, an already-resolved cycle and a missing dispute are refused.
        assert!(matches!(
            check_transition("d", Some(row(DisputePhase::Opened, 2)), "pay-1", outcome, 1),
            Err(DisputeTransitionError::OutcomeNotOpen { .. })
        ));
        assert!(matches!(
            check_transition("d", Some(row(DisputePhase::Won, 1)), "pay-1", outcome, 1),
            Err(DisputeTransitionError::OutcomeNotOpen { .. })
        ));
        assert!(matches!(
            check_transition("d", None, "pay-1", outcome, 1),
            Err(DisputeTransitionError::NoOpenedCycle { .. })
        ));
    }
    let err = check_transition(
        "d",
        Some(row(DisputePhase::Won, 1)),
        "pay-1",
        DisputePhase::Lost,
        1,
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("LOST") && err.to_string().contains("observed WON at cycle 1"),
        "{err}"
    );
}

#[test]
fn opened_is_not_an_outcome() {
    assert!(matches!(
        check_outcome(
            "d",
            Some(row(DisputePhase::Won, 1)),
            DisputePhase::Opened,
            2
        ),
        Err(DisputeTransitionError::NotAnOutcome { .. })
    ));
}
