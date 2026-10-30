use std::collections::HashMap;

use event_broker_sdk::backend::Partition;
use uuid::Uuid;

use crate::maintenance::catalog::SegmentState;
use crate::maintenance::crosscheck::{Case, Finding, Reaction, Relation, Row, Stats, compare};
use crate::maintenance::prioritizer::Key;
use crate::names;

const TOPIC: Uuid = Uuid::from_u128(0x3f2a_9c1e_0b7d_4e21_a8c6_5f0d_9e14_b7a3);

fn key() -> Key {
    (TOPIC, Partition::new(0))
}

fn name(first: i64) -> String {
    names::segment(&names::parent(TOPIC, Partition::new(0)), first)
}

fn row(first: i64, end: i64, state: SegmentState) -> Row {
    Row {
        key: key(),
        first,
        end,
        state,
        table: name(first),
    }
}

fn attached(first: i64, end: i64) -> Relation {
    Relation {
        name: name(first),
        attached: true,
        pending: false,
        bound: Some((first, end)),
    }
}

fn pending(first: i64) -> Relation {
    Relation {
        name: name(first),
        attached: true,
        pending: true,
        bound: None,
    }
}

fn detached(first: i64) -> Relation {
    Relation {
        name: name(first),
        attached: false,
        pending: false,
        bound: None,
    }
}

fn finding(case: Case, first: i64) -> Finding {
    Finding {
        key: key(),
        case,
        segment: name(first),
    }
}

/// What `rows` and `relations` disagree on, for a head of 1200 and a floor
/// of 500.
fn check(rows: &[Row], relations: &[Relation]) -> Vec<Finding> {
    let stats = HashMap::from([(
        key(),
        Stats {
            hwm: 1200,
            floor: 500,
        },
    )]);
    compare(rows, relations, &stats)
}

/// An expired segment, a sealed one, two open ones: in step with the catalog.
fn rows() -> Vec<Row> {
    vec![
        row(1, 501, SegmentState::Expired),
        row(501, 1001, SegmentState::Sealed),
        row(1001, 1501, SegmentState::Open),
        row(1501, 2001, SegmentState::Open),
    ]
}

fn relations() -> Vec<Relation> {
    vec![
        attached(1, 501),
        attached(501, 1001),
        attached(1001, 1501),
        attached(1501, 2001),
    ]
}

/// `relations()` with the table of `first` replaced by `with`, or dropped.
fn relations_with(first: i64, with: &[Relation]) -> Vec<Relation> {
    let mut relations: Vec<_> = relations()
        .into_iter()
        .filter(|r| r.name != name(first))
        .collect();
    relations.extend_from_slice(with);
    relations
}

#[test]
fn rows_in_step_with_the_catalog_agree() {
    assert_eq!(check(&rows(), &relations()), []);
}

#[test]
fn the_detachers_own_steps_agree() {
    let mut rows = rows();
    rows[0].state = SegmentState::Detaching;
    for relations in [
        relations(),
        relations_with(1, &[pending(1)]),
        relations_with(1, &[detached(1)]),
        relations_with(1, &[]),
    ] {
        assert_eq!(check(&rows, &relations), [], "{relations:?}");
    }
}

#[test]
fn an_expired_row_whose_table_is_not_attached_goes_to_the_detacher() {
    assert_eq!(
        [
            check(&rows(), &relations_with(1, &[detached(1)])),
            check(&rows(), &relations_with(1, &[])),
        ],
        [
            vec![finding(Case::Undetached, 1)],
            vec![finding(Case::Undetached, 1)],
        ]
    );
    assert_eq!(Case::Undetached.reaction(), Reaction::Detach);
}

#[test]
fn a_missing_table_above_the_head_is_unbuilt_and_below_it_lost() {
    assert_eq!(
        [
            check(&rows(), &relations_with(1501, &[])),
            check(&rows(), &relations_with(1001, &[])),
            check(&rows(), &relations_with(501, &[])),
        ],
        [
            vec![finding(Case::Unbuilt, 1501)],
            vec![finding(Case::Lost, 1001)],
            vec![finding(Case::Lost, 501)],
        ]
    );
    assert_eq!(
        [Case::Unbuilt.reaction(), Case::Lost.reaction()],
        [Reaction::Reconcile, Reaction::Reconcile]
    );
}

#[test]
fn a_live_row_whose_table_is_detached_is_hidden() {
    assert_eq!(
        [
            check(&rows(), &relations_with(501, &[detached(501)])),
            check(&rows(), &relations_with(1001, &[pending(1001)])),
        ],
        [
            vec![finding(Case::Hidden, 501)],
            vec![finding(Case::Hidden, 1001)],
        ]
    );
    assert_eq!(Case::Hidden.reaction(), Reaction::Report);
}

#[test]
fn an_attached_table_with_no_row_is_unrecorded() {
    assert_eq!(
        check(&rows(), &relations_with(2001, &[attached(2001, 2501)])),
        [finding(Case::Unrecorded, 2001)]
    );
    assert_eq!(Case::Unrecorded.reaction(), Reaction::Reconcile);
}

#[test]
fn a_detached_table_with_no_row_is_dropped_only_below_the_floor() {
    // A table before the first row ends where that row begins: at 0, below
    // the floor of 500.
    let mut rows = rows();
    rows.remove(0);
    assert_eq!(
        [
            check(&rows, &relations_with(1, &[detached(0)])),
            check(&rows, &relations_with(1, &[detached(800)])),
            check(&rows, &relations_with(1, &[detached(3000)])),
        ],
        [
            vec![finding(Case::Leftover, 0)],
            vec![finding(Case::Stray, 800)],
            vec![finding(Case::Stray, 3000)],
        ]
    );
    assert_eq!(
        [Case::Leftover.reaction(), Case::Stray.reaction()],
        [Reaction::Reconcile, Reaction::Report]
    );
}

#[test]
fn an_attached_table_whose_bound_is_not_its_rows_range_is_misbound() {
    assert_eq!(
        check(&rows(), &relations_with(1001, &[attached(1001, 1401)])),
        [finding(Case::Misbound, 1001)]
    );
    assert_eq!(Case::Misbound.reaction(), Reaction::Report);
}

#[test]
fn rows_that_leave_a_gap_above_the_floor_are_reported_where_it_ends() {
    let mut rows = rows();
    rows.remove(2);
    assert_eq!(
        check(&rows, &relations_with(1001, &[])),
        [finding(Case::Gap, 1501)]
    );
    let mut rows = self::rows();
    rows[1].first = 601;
    rows[1].table = name(601);
    assert_eq!(
        check(&rows, &relations_with(501, &[attached(601, 1001)])),
        [finding(Case::Gap, 601)]
    );
    assert_eq!(Case::Gap.reaction(), Reaction::Reconcile);
}

#[test]
fn partitions_with_no_stats_and_foreign_names_are_left_alone() {
    let other = (Uuid::from_u128(9), Partition::new(0));
    let mut rows = rows();
    for row in &mut rows {
        row.key = other;
    }
    let foreign = Relation {
        name: "evbk__partition".to_owned(),
        attached: false,
        pending: false,
        bound: None,
    };
    assert_eq!(check(&rows, &[foreign]), []);
}
