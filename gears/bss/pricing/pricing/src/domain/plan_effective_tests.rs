#![allow(clippy::expect_used, clippy::unwrap_used)]
//! D-447: the effective state of a plan's revisions on a day. A scheduled revision whose date has
//! come reads as published from 00:00 UTC of that date, its plan's published revision reads as
//! superseded, and the plan's `published_rev` is the due revision's number; nothing else moves.
use super::*;
use crate::domain::test_support::date;

const PLAN: Uuid = Uuid::from_u128(0x0a);
const OTHER_PLAN: Uuid = Uuid::from_u128(0x0b);

fn stored(
    n: u128,
    plan_id: Uuid,
    rev_no: i32,
    state: RevisionState,
    available_from: Option<&str>,
) -> StoredRevision {
    let published_at = matches!(state, RevisionState::Published | RevisionState::Superseded)
        .then(|| date("2026-09-01").with_hms(10, 30, 0).unwrap().assume_utc());
    StoredRevision {
        id: Uuid::from_u128(n),
        plan_id,
        rev_no,
        state,
        available_from: available_from.map(date),
        published_at,
    }
}
/// Each revision's (id, effective state, effective `published_at`), in the order given.
fn states(revisions: &[StoredRevision], today: &str) -> Vec<(u128, RevisionState, Option<String>)> {
    effective(revisions, date(today))
        .into_iter()
        .map(|r| {
            (
                r.id.as_u128(),
                r.state,
                r.published_at.map(|at| {
                    at.format(&time::format_description::well_known::Rfc3339)
                        .unwrap()
                }),
            )
        })
        .collect()
}
/// A plan's history: rev 1 superseded, rev 2 published, rev 3 scheduled for 2026-10-01.
fn history() -> Vec<StoredRevision> {
    vec![
        stored(1, PLAN, 1, RevisionState::Superseded, None),
        stored(2, PLAN, 2, RevisionState::Published, None),
        stored(3, PLAN, 3, RevisionState::Scheduled, Some("2026-10-01")),
    ]
}
const EARLIER: &str = "2026-09-01T10:30:00Z";

#[test]
fn without_a_scheduled_revision_every_revision_reads_as_stored() {
    let revisions = vec![
        stored(1, PLAN, 1, RevisionState::Superseded, None),
        stored(2, PLAN, 2, RevisionState::Published, Some("2026-09-01")),
        stored(3, PLAN, 3, RevisionState::Draft, Some("2026-09-02")),
    ];
    assert_eq!(
        states(&revisions, "2026-10-05"),
        [
            (1, RevisionState::Superseded, Some(EARLIER.to_owned())),
            (2, RevisionState::Published, Some(EARLIER.to_owned())),
            (3, RevisionState::Draft, None),
        ]
    );
    assert_eq!(
        published_rev(Some(2), &revisions, PLAN, date("2026-10-05")),
        Some(2)
    );
    let pending = vec![stored(
        4,
        PLAN,
        1,
        RevisionState::Pending,
        Some("2026-09-02"),
    )];
    assert_eq!(
        states(&pending, "2026-10-05"),
        [(4, RevisionState::Pending, None)]
    );
    assert_eq!(
        published_rev(None, &pending, PLAN, date("2026-10-05")),
        None
    );
}

#[test]
fn a_scheduled_revision_before_its_date_reads_as_stored_and_so_does_the_published_one() {
    let revisions = history();
    assert_eq!(
        states(&revisions, "2026-09-30"),
        [
            (1, RevisionState::Superseded, Some(EARLIER.to_owned())),
            (2, RevisionState::Published, Some(EARLIER.to_owned())),
            (3, RevisionState::Scheduled, None),
        ]
    );
    assert_eq!(
        published_rev(Some(2), &revisions, PLAN, date("2026-09-30")),
        Some(2)
    );
}

#[test]
fn a_due_scheduled_revision_reads_published_from_midnight_utc_and_its_predecessor_superseded() {
    let revisions = history();
    for today in ["2026-10-01", "2026-10-02", "2027-01-01"] {
        assert_eq!(
            states(&revisions, today),
            [
                (1, RevisionState::Superseded, Some(EARLIER.to_owned())),
                // The predecessor keeps the instant it was published at.
                (2, RevisionState::Superseded, Some(EARLIER.to_owned())),
                (
                    3,
                    RevisionState::Published,
                    Some("2026-10-01T00:00:00Z".to_owned())
                ),
            ],
            "{today}"
        );
        assert_eq!(
            published_rev(Some(2), &revisions, PLAN, date(today)),
            Some(3),
            "{today}"
        );
    }
}

#[test]
fn a_due_first_revision_has_no_predecessor_to_supersede() {
    let revisions = vec![stored(
        5,
        PLAN,
        1,
        RevisionState::Scheduled,
        Some("2026-10-01"),
    )];
    assert_eq!(
        states(&revisions, "2026-09-30"),
        [(5, RevisionState::Scheduled, None)]
    );
    assert_eq!(
        published_rev(None, &revisions, PLAN, date("2026-09-30")),
        None
    );
    assert_eq!(
        states(&revisions, "2026-10-01"),
        [(
            5,
            RevisionState::Published,
            Some("2026-10-01T00:00:00Z".to_owned())
        )]
    );
    assert_eq!(
        published_rev(None, &revisions, PLAN, date("2026-10-01")),
        Some(1)
    );
}

#[test]
fn a_due_switch_leaves_superseded_history_drafts_and_other_plans_untouched() {
    let mut revisions = history();
    revisions.insert(0, stored(9, OTHER_PLAN, 4, RevisionState::Published, None));
    revisions.push(stored(
        10,
        OTHER_PLAN,
        5,
        RevisionState::Draft,
        Some("2026-09-01"),
    ));
    revisions.push(stored(11, OTHER_PLAN, 3, RevisionState::Superseded, None));
    assert_eq!(
        states(&revisions, "2026-10-01"),
        [
            (9, RevisionState::Published, Some(EARLIER.to_owned())),
            (1, RevisionState::Superseded, Some(EARLIER.to_owned())),
            (2, RevisionState::Superseded, Some(EARLIER.to_owned())),
            (
                3,
                RevisionState::Published,
                Some("2026-10-01T00:00:00Z".to_owned())
            ),
            (10, RevisionState::Draft, None),
            (11, RevisionState::Superseded, Some(EARLIER.to_owned())),
        ]
    );
    assert_eq!(
        published_rev(Some(4), &revisions, OTHER_PLAN, date("2026-10-01")),
        Some(4),
        "another plan's projection is its own"
    );
}

/// The storage never writes a scheduled revision without a date (the apply schedules only a
/// future one); a row that has none is never due, as `switch_due`'s `available_from <= today`
/// reads it.
#[test]
fn a_scheduled_revision_without_a_date_is_never_due() {
    let revisions = vec![
        stored(2, PLAN, 2, RevisionState::Published, None),
        stored(3, PLAN, 3, RevisionState::Scheduled, None),
    ];
    assert_eq!(
        states(&revisions, "2030-01-01"),
        [
            (2, RevisionState::Published, Some(EARLIER.to_owned())),
            (3, RevisionState::Scheduled, None),
        ]
    );
    assert_eq!(
        published_rev(Some(2), &revisions, PLAN, date("2030-01-01")),
        Some(2)
    );
}

/// D-460: the current revision of a plan is its draft or pending one, else the scheduled one
/// waiting for its date, else the published one in effect; `in_effect` is the published one in
/// effect. Both are judged over the states the revisions read on the day, so a due scheduled
/// revision is both, and its stored-published predecessor neither.
#[test]
fn the_current_revision_and_the_one_in_effect_follow_the_effective_states() {
    let chosen = |revisions: &[StoredRevision], today: &str| {
        let read = effective(revisions, date(today));
        (
            current(&read).map(|r| (r.id.as_u128(), r.state)),
            in_effect(&read).map(|r| r.id.as_u128()),
        )
    };
    assert_eq!(chosen(&[], "2026-09-30"), (None, None), "no revision");
    let draft_only = [stored(1, PLAN, 1, RevisionState::Draft, None)];
    assert_eq!(
        chosen(&draft_only, "2026-09-30"),
        (Some((1, RevisionState::Draft)), None)
    );
    for open in [RevisionState::Draft, RevisionState::Pending] {
        let beside = [
            stored(1, PLAN, 1, RevisionState::Superseded, None),
            stored(2, PLAN, 2, RevisionState::Published, None),
            stored(3, PLAN, 3, open, None),
        ];
        assert_eq!(
            chosen(&beside, "2026-09-30"),
            (Some((3, open)), Some(2)),
            "{open:?} beside the published"
        );
    }
    assert_eq!(
        chosen(&history(), "2026-09-30"),
        (Some((3, RevisionState::Scheduled)), Some(2)),
        "waiting"
    );
    assert_eq!(
        chosen(&history(), "2026-10-01"),
        (Some((3, RevisionState::Published)), Some(3)),
        "due, before its switch is persisted"
    );
    let published_only = [stored(1, PLAN, 1, RevisionState::Published, None)];
    assert_eq!(
        chosen(&published_only, "2026-09-30"),
        (Some((1, RevisionState::Published)), Some(1))
    );
}
