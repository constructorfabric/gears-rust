// Created: 2026-08-07 by Constructor Tech
//! What each recording actually emits: instrument name and attributes.
//!
//! The point of asserting the name is that it is the contract with the
//! dashboards and alerts, not an implementation detail -- renaming an
//! instrument breaks them silently, and nothing else in the build would
//! notice.

use super::harness::MetricsHarness;
use crate::domain::metrics::{NoopMetrics, Operation, Outcome, RgMetricsPort};

#[test]
fn operation_duration_carries_operation_and_outcome() {
    let h = MetricsHarness::new();
    let m = h.metrics();

    m.operation_duration(Operation::Move, Outcome::Ok, 0.25);
    m.operation_duration(Operation::Move, Outcome::Error, 0.5);

    let mut points = h.points("rg_operation_duration_seconds");
    points.sort();
    assert_eq!(
        points,
        vec![
            (
                vec![
                    ("operation".to_owned(), "move_group".to_owned()),
                    ("outcome".to_owned(), "error".to_owned()),
                ],
                1
            ),
            (
                vec![
                    ("operation".to_owned(), "move_group".to_owned()),
                    ("outcome".to_owned(), "ok".to_owned()),
                ],
                1
            ),
        ]
    );
}

#[test]
fn a_failed_operation_is_not_counted_as_a_successful_one() {
    // The split exists so a latency tail made of failures cannot be read as
    // slow successful work; if both landed on one series it would be.
    let h = MetricsHarness::new();
    h.metrics()
        .operation_duration(Operation::Create, Outcome::Error, 1.0);

    let points = h.points("rg_operation_duration_seconds");
    assert_eq!(points.len(), 1, "expected one series, got {points:?}");
    assert!(
        points[0]
            .0
            .contains(&("outcome".to_owned(), "error".to_owned()))
    );
}

#[test]
fn subtree_and_closure_rows_record_under_their_operation() {
    let h = MetricsHarness::new();
    let m = h.metrics();

    m.subtree_nodes(Operation::Move, 15);
    m.closure_rows_written(Operation::Move, 45);

    assert_eq!(
        h.points("rg_subtree_nodes"),
        vec![(vec![("operation".to_owned(), "move_group".to_owned())], 1)]
    );
    assert_eq!(
        h.points("rg_closure_rows_written"),
        vec![(vec![("operation".to_owned(), "move_group".to_owned())], 1)]
    );
}

#[test]
fn isolation_escalation_sums_across_calls() {
    // A counter, not a histogram: the question it answers is "how often was
    // the pre-transaction hint wrong", and the answer is a rate.
    let h = MetricsHarness::new();
    let m = h.metrics();

    m.isolation_escalation(Operation::Update);
    m.isolation_escalation(Operation::Update);
    m.isolation_escalation(Operation::Update);

    assert_eq!(
        h.points("rg_isolation_escalation_total"),
        vec![(vec![("operation".to_owned(), "update_group".to_owned())], 3)]
    );
}

#[test]
fn metadata_validation_duration_records() {
    let h = MetricsHarness::new();
    h.metrics()
        .metadata_validation_duration(Operation::Create, 0.01);

    assert_eq!(
        h.points("rg_metadata_validation_duration_seconds"),
        vec![(vec![("operation".to_owned(), "create_group".to_owned())], 1)]
    );
}

#[test]
fn every_operation_has_a_distinct_label() {
    // Two operations sharing a label would silently merge their series.
    let all = [
        Operation::Create,
        Operation::Update,
        Operation::Move,
        Operation::Delete,
        Operation::ForceDelete,
    ];
    let mut labels: Vec<&str> = all.iter().map(|o| o.as_str()).collect();
    let count = labels.len();
    labels.sort_unstable();
    labels.dedup();
    assert_eq!(labels.len(), count, "duplicate operation label: {labels:?}");
}

#[test]
fn the_noop_port_accepts_every_recording_without_panicking() {
    // Named for what it checks. There is no assertion here and there cannot
    // usefully be one: `NoopMetrics` holds no instrument to inspect, so this
    // proves the calls compile and return, not that nothing was recorded. A
    // `NoopMetrics` that forwarded to the global provider would pass it.
    //
    // What keeps that honest is the type: the struct has no fields, so there
    // is nothing for it to forward to.
    let n = NoopMetrics;
    n.operation_duration(Operation::Move, Outcome::Ok, 1.0);
    n.subtree_nodes(Operation::Move, 1);
    n.closure_rows_written(Operation::Move, 1);
    n.isolation_escalation(Operation::Update);
    n.metadata_validation_duration(Operation::Create, 1.0);
}
