//! Tests for the approval state machine.

use super::{APPROVAL_NOT_PENDING, ApprovalDecision, ApprovalState, TransitionRefusal};

fn refused(from: ApprovalState, to: ApprovalState) -> TransitionRefusal {
    from.transition(to)
        .expect_err(&format!("expected {from} -> {to} to be refused"))
}

#[test]
fn the_three_legal_edges_are_the_only_legal_edges() {
    // The whole 4x4 product, enumerated. §4's machine has exactly three edges
    // and every one of them leaves `submitted`; a fourth arriving here has to be
    // argued for rather than noticed.
    let legal: Vec<(ApprovalState, ApprovalState)> = ApprovalState::ALL
        .iter()
        .flat_map(|from| {
            ApprovalState::ALL
                .iter()
                .filter(move |to| from.can_transition(**to))
                .map(move |to| (*from, *to))
        })
        .collect();

    assert_eq!(
        legal,
        vec![
            (ApprovalState::Submitted, ApprovalState::Approved),
            (ApprovalState::Submitted, ApprovalState::Rejected),
            (ApprovalState::Submitted, ApprovalState::Voided),
        ]
    );
}

#[test]
fn a_submitted_record_is_approved_rejected_or_voided() {
    // `inst-as-approve`, `inst-as-reject`, `inst-as-void`. Asserted before the
    // refusals below, because without it every refusal test would pass against a
    // machine that refuses everything.
    assert!(
        ApprovalState::Submitted
            .transition(ApprovalState::Approved)
            .is_ok()
    );
    assert!(
        ApprovalState::Submitted
            .transition(ApprovalState::Rejected)
            .is_ok()
    );
    assert!(
        ApprovalState::Submitted
            .transition(ApprovalState::Voided)
            .is_ok()
    );
}

#[test]
fn a_decided_record_is_immutable_and_the_refusal_is_the_pendingness_one() {
    // `inst-as-immutable`, and the arm that carries `APPROVAL_NOT_PENDING`: a
    // decision on an already-decided record is refused, and refused **for that
    // reason** rather than for the shape of the move.
    //
    // `to` ranges over every state, so the three decided self-edges are in here
    // too; the fourth, `submitted -> submitted`, is
    // `the_self_edge_of_a_pending_record_is_refused_without_minting_a_code`'s,
    // which asserts the refusal it carries rather than only that there is one.
    // Between them the self-edges are covered with the stronger assertion.
    for from in [
        ApprovalState::Approved,
        ApprovalState::Rejected,
        ApprovalState::Voided,
    ] {
        for to in ApprovalState::ALL {
            let refusal = refused(from, *to);
            assert_eq!(
                refusal,
                TransitionRefusal::NotPending { from },
                "{from} -> {to} must answer that the record is no longer pending"
            );
            assert_eq!(refusal.code(), Some(APPROVAL_NOT_PENDING));
        }
    }
}

#[test]
fn the_self_edge_of_a_pending_record_is_refused_without_minting_a_code() {
    // The one refusal in the machine no surface can provoke: approve, reject and
    // withdraw each name a fixed outcome, so nothing on a wire path can ask a
    // `submitted` record to stay `submitted`. A code here would document an API
    // a client cannot reach, which is the reason D-146 gives for leaving the
    // frontier regression uncoded.
    let refusal = refused(ApprovalState::Submitted, ApprovalState::Submitted);
    assert_eq!(
        refusal,
        TransitionRefusal::NotAnOutcome {
            to: ApprovalState::Submitted
        }
    );
    assert_eq!(refusal.code(), None);
}

#[test]
fn every_decision_a_surface_can_ask_for_lands_on_a_decided_state() {
    // `ApprovalDecision` is what the three POST routes carry, and it makes the
    // self-edge unrepresentable rather than merely refused — the machine's only
    // uncoded refusal is then unreachable by construction and not by convention.
    for decision in ApprovalDecision::ALL {
        let outcome = decision.outcome();
        assert!(
            !outcome.is_pending(),
            "{decision:?} must leave the record decided"
        );
        assert!(ApprovalState::Submitted.can_transition(outcome));
    }
    assert_eq!(
        ApprovalDecision::ALL
            .iter()
            .map(|d| d.outcome())
            .collect::<Vec<_>>(),
        vec![
            ApprovalState::Approved,
            ApprovalState::Rejected,
            ApprovalState::Voided,
        ]
    );
}

#[test]
fn deciding_a_pending_record_yields_its_outcome_and_deciding_a_decided_one_refuses() {
    assert_eq!(
        ApprovalState::Submitted.decide(ApprovalDecision::Approve),
        Ok(ApprovalState::Approved)
    );
    assert_eq!(
        ApprovalState::Approved.decide(ApprovalDecision::Reject),
        Err(TransitionRefusal::NotPending {
            from: ApprovalState::Approved
        })
    );
}

#[test]
fn no_decision_is_ever_refused_as_not_an_outcome() {
    // The whole state x decision product, and the property
    // `approval_repo::decide` rests on: every refusal a *decision* can provoke is
    // `NotPending`, whose `from` is the record's own state. That is what lets the
    // repository fold the refusal into `APPROVAL_NOT_PENDING` and still tell the
    // truth — while it took a bare `ApprovalState`, the same fold answered a
    // pending record with "approval X is submitted; only a submitted record is
    // decidable", a sentence contradicting itself.
    //
    // The uncoded refusal is a real arm of the machine and stays reachable
    // through `transition`; what this pins is that nothing a surface can *ask
    // for* reaches it.
    for state in ApprovalState::ALL {
        for decision in ApprovalDecision::ALL {
            match state.decide(*decision) {
                Ok(outcome) => assert_eq!(outcome, decision.outcome()),
                Err(refusal) => assert_eq!(
                    refusal,
                    TransitionRefusal::NotPending { from: *state },
                    "{state} + {decision:?} must refuse for pendingness or not at all"
                ),
            }
        }
    }
}

#[test]
fn only_a_submitted_record_is_pending() {
    assert!(ApprovalState::Submitted.is_pending());
    assert!(!ApprovalState::Approved.is_pending());
    assert!(!ApprovalState::Rejected.is_pending());
    assert!(!ApprovalState::Voided.is_pending());
}

#[test]
fn a_reason_is_mandatory_on_a_reject_and_on_nothing_else() {
    // `inst-as-reject`, and `chk_pricing_approval_reason` is the same rule in
    // the store. Stated here too because the column cannot tell a caller which
    // field to supply.
    assert!(ApprovalState::Rejected.requires_reason());
    assert!(!ApprovalState::Approved.requires_reason());
    assert!(!ApprovalState::Voided.requires_reason());
    assert!(!ApprovalState::Submitted.requires_reason());
}

#[test]
fn an_approver_is_recorded_on_a_decision_and_never_on_a_void() {
    // The mirror of `chk_pricing_approval_approver`. A void has no human
    // decider: a TOCTOU void is the system's, and a withdraw's decider is the
    // submitter — whom the distinctness rule forbids in that column.
    assert!(ApprovalState::Approved.requires_approver());
    assert!(ApprovalState::Rejected.requires_approver());
    assert!(!ApprovalState::Voided.requires_approver());
    assert!(!ApprovalState::Submitted.requires_approver());
}

#[test]
fn the_persisted_tokens_are_the_ones_the_check_constraint_admits() {
    // `chk_pricing_approval_state` lists these four literals. A token renamed on
    // one side only is a row the other side can neither write nor read.
    assert_eq!(
        ApprovalState::ALL
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>(),
        vec!["submitted", "approved", "rejected", "voided"]
    );
}

#[test]
fn every_state_round_trips_through_its_token() {
    for state in ApprovalState::ALL {
        assert_eq!(ApprovalState::from_token(state.as_str()), Some(*state));
        assert_eq!(state.to_string(), state.as_str());
    }
    assert_eq!(ApprovalState::from_token("withdrawn"), None);
}

#[test]
fn the_refusal_names_the_state_it_refused_from() {
    // An operator reading the log needs to know which record answered, not only
    // that one did.
    let refusal = refused(ApprovalState::Rejected, ApprovalState::Approved);
    assert!(refusal.to_string().contains("rejected"), "got: {refusal}");
}
