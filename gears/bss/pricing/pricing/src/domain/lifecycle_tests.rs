//! Tests for the row lifecycle state machine.

use super::LifecycleState;
use crate::domain::error::DomainError;

fn refused(from: LifecycleState, to: LifecycleState) {
    assert!(
        matches!(from.transition(to), Err(DomainError::LifecycleForbidden(_))),
        "expected {from} -> {to} to be refused"
    );
}

#[test]
fn the_four_legal_edges_are_the_only_legal_edges() {
    // Enumerating the whole 5x5 product is the point: a new variant, or a
    // widened `matches!` arm, changes this count and has to be argued for.
    let legal: Vec<(LifecycleState, LifecycleState)> = LifecycleState::ALL
        .iter()
        .flat_map(|from| {
            LifecycleState::ALL
                .iter()
                .filter(move |to| from.can_transition(**to))
                .map(move |to| (*from, *to))
        })
        .collect();

    assert_eq!(
        legal,
        vec![
            (LifecycleState::Draft, LifecycleState::Abandoned),
            (LifecycleState::Draft, LifecycleState::Published),
            (LifecycleState::Published, LifecycleState::Superseded),
            (LifecycleState::Published, LifecycleState::Retired),
        ]
    );
}

#[test]
fn a_draft_publishes() {
    assert!(
        LifecycleState::Draft
            .transition(LifecycleState::Published)
            .is_ok()
    );
}

#[test]
fn a_published_row_supersedes_or_retires() {
    // The two sanctioned producers of the supersession flip are the
    // supersession unit and the grandfathering cutover commit; retirement is
    // its own publish unit (D-128).
    assert!(
        LifecycleState::Published
            .transition(LifecycleState::Superseded)
            .is_ok()
    );
    assert!(
        LifecycleState::Published
            .transition(LifecycleState::Retired)
            .is_ok()
    );
}

#[test]
fn retirement_is_terminal() {
    // Nothing leaves `retired`. This is why retirement had to become a publish
    // unit: no later publish could ever re-project the plan to correct a read
    // model that still advertised it as sellable.
    refused(LifecycleState::Retired, LifecycleState::Draft);
    refused(LifecycleState::Retired, LifecycleState::Superseded);
    // The one edge out of `retired` an operator can actually attempt answers in
    // its own words; see the test below.
    assert!(
        LifecycleState::Retired
            .transition(LifecycleState::Published)
            .is_err()
    );
}

#[test]
fn re_publishing_a_retired_subject_is_a_stop_and_not_the_generic_refusal() {
    // The narrowing D-146 made (the test that would have passed before it and
    // must fail after): every illegal edge used to answer `LIFECYCLE_FORBIDDEN`,
    // so this one — the only refusal in the machine with **no** alternative
    // action, because a retired plan can never publish again — was
    // indistinguishable from a caller bug the operator can fix.
    let err = LifecycleState::Retired
        .transition(LifecycleState::Published)
        .expect_err("retired is terminal");

    assert!(
        matches!(err, DomainError::PlanRetiredNoSuccessor(_)),
        "got: {err:?}"
    );
    // And the refusals that keep the generic code are the ones that describe no
    // operator action either way.
    refused(LifecycleState::Superseded, LifecycleState::Published);
    refused(LifecycleState::Published, LifecycleState::Draft);
}

#[test]
fn a_superseded_row_never_returns_to_published() {
    // Its key belongs to the successor; re-publishing it would put two current
    // rows on one canonical scope key.
    refused(LifecycleState::Superseded, LifecycleState::Published);
    refused(LifecycleState::Superseded, LifecycleState::Draft);
    refused(LifecycleState::Superseded, LifecycleState::Retired);
}

#[test]
fn a_published_row_never_returns_to_draft() {
    // The append-only discipline is the whole immutability guarantee: an edit
    // of a published row is a new row, never a walk backwards.
    refused(LifecycleState::Published, LifecycleState::Draft);
}

#[test]
fn no_state_transitions_to_itself() {
    // A re-publish is not a no-op; treating it as one would let a retry pass
    // the same guard twice.
    for state in LifecycleState::ALL {
        refused(*state, *state);
    }
}

#[test]
fn a_draft_publishes_or_is_abandoned_and_does_nothing_else() {
    assert!(
        LifecycleState::Draft
            .transition(LifecycleState::Abandoned)
            .is_ok()
    );
    refused(LifecycleState::Draft, LifecycleState::Superseded);
    refused(LifecycleState::Draft, LifecycleState::Retired);
}

#[test]
fn abandonment_is_terminal() {
    // The tombstone's whole job is to hold the `revision` number the discarded
    // draft consumed. An edge out of it would put a live row back at a number
    // that has already been referenced as a durable name, which is the state
    // D-145 removed by refusing to delete the row in the first place.
    for state in LifecycleState::ALL {
        refused(LifecycleState::Abandoned, *state);
    }
}

#[test]
fn only_a_draft_has_mutable_content() {
    assert!(LifecycleState::Draft.is_content_mutable());
    assert!(!LifecycleState::Abandoned.is_content_mutable());
    assert!(!LifecycleState::Published.is_content_mutable());
    assert!(!LifecycleState::Superseded.is_content_mutable());
    assert!(!LifecycleState::Retired.is_content_mutable());
}

#[test]
fn a_retired_revision_is_still_the_current_one() {
    // D-128 widened the predicate for exactly this: the projector needs a
    // referent after retirement, so a retired plan keeps a resolvable delta
    // for its in-flight subscribers.
    assert!(LifecycleState::Retired.is_current_revision());
    assert!(LifecycleState::Published.is_current_revision());
    assert!(!LifecycleState::Draft.is_current_revision());
    assert!(!LifecycleState::Superseded.is_current_revision());
    // A tombstone is not a revision anyone may resolve: it never published, so
    // it was never projected, and the partial index that decides "current"
    // restates this predicate — counting it in would collide the plan's real
    // current revision with every draft it ever discarded.
    assert!(!LifecycleState::Abandoned.is_current_revision());
}

#[test]
fn the_refusal_names_the_edge_it_refused() {
    // An operator reading the log needs to know which move was attempted, not
    // just that one was.
    let err = LifecycleState::Retired
        .transition(LifecycleState::Published)
        .expect_err("retired is terminal");

    assert!(err.to_string().contains("retired -> published"));
}
