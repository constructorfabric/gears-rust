//! Pure: no runtime, no clock, no storage, and not even an event - the whole
//! derivation reads sequences. Keep it that way.

use crate::domain::model::Sequence;

use super::accounting::{AbsentRun, absent_runs, account_for_fetch};

fn absent(from: Sequence, through: Sequence) -> AbsentRun {
    AbsentRun { from, through }
}

#[test]
fn a_dense_fetch_proves_nothing_absent() {
    let returned = [100, 101, 102];

    let accounting = account_for_fetch(99, &returned, 10);

    assert_eq!(accounting.accounted_through(), 102);
    assert!(!accounting.saturated());
    assert_eq!(absent_runs(99, &returned), vec![]);
}

#[test]
fn a_hole_between_returned_events_is_proven_absent() {
    // 150..=159 were deleted; the fetch covered them and found nothing.
    let returned: Vec<Sequence> = (100..=149).chain(160..=200).collect();

    let accounting = account_for_fetch(99, &returned, 1000);

    assert_eq!(accounting.accounted_through(), 200);
    assert_eq!(absent_runs(99, &returned), vec![absent(150, 159)]);
}

#[test]
fn several_holes_are_each_proven() {
    let returned = [100, 105, 106, 110];

    assert_eq!(
        absent_runs(99, &returned),
        vec![absent(101, 104), absent(107, 109)]
    );
}

#[test]
fn a_hole_immediately_after_the_requested_offset_is_proven() {
    // Nothing at 100..=104; the first surviving event is 105.
    let returned = [105, 106];

    assert_eq!(absent_runs(99, &returned), vec![absent(100, 104)]);
}

#[test]
fn a_single_missing_sequence_is_a_run_of_one() {
    let returned = [100, 102];

    assert_eq!(absent_runs(99, &returned), vec![absent(101, 101)]);
}

#[test]
fn nothing_is_concluded_above_the_highest_returned_sequence() {
    // A saturated fetch stopped at its bound, so 211 and beyond are unknown -
    // not absent. Stepping over an unknown would skip events that exist.
    let returned: Vec<Sequence> = (111..=210).collect();

    let accounting = account_for_fetch(110, &returned, 100);

    assert!(accounting.saturated());
    assert_eq!(accounting.accounted_through(), 210);
    assert_eq!(absent_runs(110, &returned), vec![]);
}

#[test]
fn a_short_fetch_still_concludes_nothing_beyond_what_it_returned() {
    // Under its bound, so nothing exists above 150 *yet* - but "yet" is the
    // tail, not a gap, and a later append will land there.
    let returned: Vec<Sequence> = (100..=150).collect();

    let accounting = account_for_fetch(99, &returned, 1000);

    assert!(!accounting.saturated());
    assert_eq!(accounting.accounted_through(), 150);
    assert_eq!(absent_runs(99, &returned), vec![]);
}

#[test]
fn an_empty_fetch_proves_nothing_and_widens_nothing() {
    let accounting = account_for_fetch(99, &[], 100);

    assert!(accounting.proved_nothing());
    assert_eq!(
        accounting.accounted_through(),
        99,
        "an empty result must not widen the accounted span"
    );
    assert_eq!(absent_runs(99, &[]), vec![]);
}

#[test]
fn an_empty_but_saturated_fetch_is_not_treated_as_having_proved_nothing() {
    // Degenerate: a zero bound. It cannot have examined anything, but it also
    // did not establish emptiness, so it must not be mistaken for proof.
    let accounting = account_for_fetch(99, &[], 0);

    assert_eq!(accounting.accounted_through(), 99);
    assert!(accounting.proved_nothing());
}

#[test]
fn sequences_at_or_below_the_requested_offset_are_ignored() {
    // A backend returning these violates the exclusive-offset contract; trust
    // the contract, not the response.
    let returned = [98, 99, 100];

    let accounting = account_for_fetch(99, &returned, 10);

    assert_eq!(accounting.accounted_through(), 100);
    assert_eq!(absent_runs(99, &returned), vec![]);
}

#[test]
fn the_accounted_span_starts_where_the_fetch_was_requested() {
    let accounting = account_for_fetch(500, &[600], 10);

    assert_eq!(accounting.requested_from(), 500);
    assert_eq!(accounting.accounted_through(), 600);
    assert_eq!(absent_runs(500, &[600]), vec![absent(501, 599)]);
}
