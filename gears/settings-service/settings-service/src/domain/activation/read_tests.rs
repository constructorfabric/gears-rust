//! The derivation and the shape of the activation read, over plain rows: the
//! outcome precedence, the counts, the excluded entries, the entry order and
//! the identity disclosure — DESIGN-activation §7 Level 1, *Change outcome
//! derivation* and *Excluded silent entries*.

use time::OffsetDateTime;
use uuid::Uuid;

use super::{ActivationRead, ActivationRow, ChangeOutcome, build_items};
use crate::domain::activation::{ActivationStatus, Subscription};

const SUBSCRIBER: &str = "cf.example.mailer";

fn at(seconds: i64) -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(1_800_000_000 + seconds).expect("a timestamp")
}

fn row(
    subscriber: &str,
    instance: u128,
    change: u128,
    status: ActivationStatus,
    silent_since: Option<i64>,
) -> ActivationRow {
    ActivationRow {
        subscriber: subscriber.to_owned(),
        instance_id: Uuid::from_u128(instance),
        instance_label: Some(format!("host-{instance}")),
        change_ref: Uuid::from_u128(change),
        status,
        silent_since: silent_since.map(at),
        reported_at: Some(at(5)),
        detail: Some("pool full".to_owned()),
    }
}

fn silent_subscription(subscriber: &str, instance: u128, since: i64) -> Subscription {
    Subscription {
        id: Uuid::from_u128(0xf000 + instance),
        subscriber: subscriber.to_owned(),
        instance_id: Uuid::from_u128(instance),
        instance_label: Some(format!("host-{instance}")),
        key: "k".to_owned(),
        tenant: Uuid::nil().to_string(),
        silent_since: Some(at(since)),
        created_at: at(0),
    }
}

use ActivationStatus::{Awaiting, Failed, Silent, Succeeded};

fn outcome_of(statuses: &[ActivationStatus]) -> ChangeOutcome {
    let rows = statuses
        .iter()
        .enumerate()
        .map(|(n, status)| row(SUBSCRIBER, n as u128 + 1, 1, *status, None))
        .collect();
    build_items(rows, &[])[0].outcome
}

#[test]
fn the_outcome_follows_failed_over_incomplete_over_awaiting_over_succeeded() {
    use ChangeOutcome as O;
    assert_eq!(outcome_of(&[Failed, Silent]), O::Failed);
    assert_eq!(outcome_of(&[Failed, Awaiting, Succeeded]), O::Failed);
    assert_eq!(outcome_of(&[Silent, Awaiting]), O::Incomplete);
    assert_eq!(outcome_of(&[Silent, Succeeded]), O::Incomplete);
    assert_eq!(outcome_of(&[Awaiting, Succeeded]), O::Awaiting);
    assert_eq!(outcome_of(&[Succeeded, Succeeded]), O::Succeeded);
    // Every other combination follows the same order.
    let all = [Awaiting, Succeeded, Failed, Silent];
    for a in all {
        for b in all {
            for c in all {
                let expected = if [a, b, c].contains(&Failed) {
                    O::Failed
                } else if [a, b, c].contains(&Silent) {
                    O::Incomplete
                } else if [a, b, c].contains(&Awaiting) {
                    O::Awaiting
                } else {
                    O::Succeeded
                };
                assert_eq!(outcome_of(&[a, b, c]), expected, "{a:?} {b:?} {c:?}");
            }
        }
    }
}

#[test]
fn the_counts_are_the_rows_and_the_excluded_entries_enter_none_of_them() {
    let rows = vec![
        row(SUBSCRIBER, 1, 7, Succeeded, None),
        row(SUBSCRIBER, 2, 7, Succeeded, None),
        row(SUBSCRIBER, 3, 7, Failed, None),
        row(SUBSCRIBER, 4, 7, Silent, Some(1)),
        row(SUBSCRIBER, 5, 7, Awaiting, None),
    ];
    let left_out = [silent_subscription(SUBSCRIBER, 9, 2)];
    let items = build_items(rows, &left_out);
    let item = &items[0];
    assert_eq!(
        (
            item.expected,
            item.confirmed,
            item.failed,
            item.silent,
            item.silent_excluded
        ),
        (5, 2, 1, 1, 1)
    );
    assert_eq!(item.instances.len(), 6);
    assert_eq!(item.outcome, ChangeOutcome::Failed);
}

#[test]
fn all_silent_subscriber_reads_incomplete_zero_of_m() {
    let rows = (1..=3)
        .map(|n| row(SUBSCRIBER, n, 4, Silent, Some(1)))
        .collect();
    let item = &build_items(rows, &[])[0];
    assert_eq!(item.outcome, ChangeOutcome::Incomplete);
    assert_eq!((item.confirmed, item.expected, item.silent), (0, 3, 3));
}

#[test]
fn seven_instances_six_confirmed_one_silent_read_incomplete_and_a_late_report_settles_it() {
    let mut rows: Vec<ActivationRow> = (1..=6)
        .map(|n| row(SUBSCRIBER, n, 4, Succeeded, None))
        .collect();
    rows.push(row(SUBSCRIBER, 7, 4, Silent, Some(10)));
    let item = &build_items(rows.clone(), &[])[0];
    assert_eq!(item.outcome, ChangeOutcome::Incomplete);
    assert_eq!((item.expected, item.confirmed, item.silent), (7, 6, 1));

    // Late confirmation: the row is `succeeded`, `silent_since` kept.
    rows[6].status = Succeeded;
    let item = &build_items(rows.clone(), &[])[0];
    assert_eq!(item.outcome, ChangeOutcome::Succeeded);
    assert_eq!((item.expected, item.confirmed, item.silent), (7, 7, 0));
    let late = item
        .instances
        .iter()
        .find(|e| e.instance_id == Some(Uuid::from_u128(7)))
        .expect("the instance");
    assert!(late.silent_since.is_some() && late.reported_at.is_some());

    // Late failure: the outcome is `failed`.
    rows[6].status = Failed;
    assert_eq!(build_items(rows, &[])[0].outcome, ChangeOutcome::Failed);
}

#[test]
fn a_late_confirmation_shows_reported_at_for_settled_rows_and_detail_for_failed_ones_only() {
    let rows = vec![
        row(SUBSCRIBER, 1, 1, Awaiting, None),
        row(SUBSCRIBER, 2, 1, Silent, Some(3)),
        row(SUBSCRIBER, 3, 1, Succeeded, None),
        row(SUBSCRIBER, 4, 1, Failed, None),
    ];
    let item = &build_items(rows, &[])[0];
    let shown: Vec<_> = item
        .instances
        .iter()
        .map(|e| {
            (
                e.status,
                e.reported_at.is_some(),
                e.silent_since.is_some(),
                e.detail.is_some(),
            )
        })
        .collect();
    assert_eq!(
        shown,
        [
            (Awaiting, false, false, false),
            (Succeeded, true, false, false),
            (Failed, true, false, true),
            (Silent, false, true, false),
        ]
    );
}

#[test]
fn an_excluded_entry_is_a_silent_instance_of_a_subscriber_with_an_item_and_no_row() {
    let rows = vec![row(SUBSCRIBER, 1, 2, Awaiting, None)];
    let silent = [
        silent_subscription(SUBSCRIBER, 2, 4),
        // A subscriber with a registration and no row has no item.
        silent_subscription("cf.example.other", 3, 4),
        // An instance with a row is listed once, as its row.
        silent_subscription(SUBSCRIBER, 1, 4),
    ];
    let items = build_items(rows, &silent);
    assert_eq!(items.len(), 1, "no item for a subscriber with no row");
    let item = &items[0];
    assert_eq!(
        (item.expected, item.silent_excluded, item.silent),
        (1, 1, 0)
    );
    let excluded = &item.instances[1];
    assert!(excluded.excluded && excluded.status == Silent);
    assert_eq!(excluded.silent_since, Some(at(4)));
    assert!(excluded.reported_at.is_none() && excluded.detail.is_none());
    assert!(!item.instances[0].excluded, "`excluded` on a row is false");
}

#[test]
fn a_key_with_registrations_only_has_no_items() {
    let items = build_items(
        Vec::new(),
        &[
            silent_subscription(SUBSCRIBER, 1, 1),
            silent_subscription(SUBSCRIBER, 2, 1),
        ],
    );
    assert!(items.is_empty());
}

#[test]
fn the_newest_change_is_current_and_only_its_rows_count() {
    let rows = vec![
        row(SUBSCRIBER, 1, 3, Succeeded, None),
        row(SUBSCRIBER, 2, 5, Awaiting, None),
        row(SUBSCRIBER, 3, 4, Failed, None),
    ];
    let item = &build_items(rows, &[])[0];
    assert_eq!(item.change_ref, Uuid::from_u128(5));
    assert_eq!((item.expected, item.confirmed, item.failed), (1, 0, 0));
    assert_eq!(item.outcome, ChangeOutcome::Awaiting);
}

#[test]
fn items_are_in_subscriber_order() {
    let rows = vec![
        row("gear-c", 1, 1, Awaiting, None),
        row("gear-a", 1, 2, Awaiting, None),
        row("gear-b", 1, 3, Awaiting, None),
    ];
    let subscribers: Vec<_> = build_items(rows, &[])
        .into_iter()
        .map(|item| item.subscriber)
        .collect();
    assert_eq!(subscribers, ["gear-a", "gear-b", "gear-c"]);
}

#[test]
fn entries_are_rows_then_excluded_each_by_status_then_by_lowercase_identity() {
    let rows = vec![
        row(SUBSCRIBER, 0xb, 1, Silent, Some(1)),
        row(SUBSCRIBER, 0xa, 1, Failed, None),
        row(SUBSCRIBER, 0x2, 1, Succeeded, None),
        row(SUBSCRIBER, 0x1, 1, Succeeded, None),
        row(SUBSCRIBER, 0xc, 1, Awaiting, None),
    ];
    let left_out = [
        silent_subscription(SUBSCRIBER, 0x20, 1),
        silent_subscription(SUBSCRIBER, 0x10, 1),
    ];
    let item = &build_items(rows, &left_out)[0];
    let order: Vec<_> = item
        .instances
        .iter()
        .map(|e| (e.instance_id.map_or(0, |id| id.as_u128()), e.excluded))
        .collect();
    assert_eq!(
        order,
        [
            (0xc, false),
            (0x1, false),
            (0x2, false),
            (0xa, false),
            (0xb, false),
            (0x10, true),
            (0x20, true),
        ]
    );
}

#[test]
fn disclosure_keeps_identity_for_the_root_and_removes_only_it_for_every_other_reader() {
    let rows = vec![
        row(SUBSCRIBER, 1, 1, Failed, None),
        row(SUBSCRIBER, 2, 1, Silent, Some(1)),
    ];
    let read = ActivationRead {
        tenant: Uuid::nil(),
        items: build_items(rows, &[silent_subscription(SUBSCRIBER, 3, 2)]),
    };
    let platform = read.clone().disclosed_to(true);
    assert_eq!(platform, read, "nothing changes for the root's reader");
    assert!(
        platform.items[0]
            .instances
            .iter()
            .all(|e| e.instance_id.is_some() && e.instance_label.is_some())
    );

    let tenant = read.disclosed_to(false);
    assert!(
        tenant.items[0]
            .instances
            .iter()
            .all(|e| e.instance_id.is_none() && e.instance_label.is_none()),
        "excluded entries included"
    );
    // Everything else is the same, order included.
    let strip = |read: &ActivationRead| -> Vec<_> {
        read.items[0]
            .instances
            .iter()
            .map(|e| {
                (
                    e.status,
                    e.excluded,
                    e.reported_at,
                    e.silent_since,
                    e.detail.clone(),
                )
            })
            .collect()
    };
    assert_eq!(strip(&tenant), strip(&platform));
    assert_eq!(
        (tenant.items[0].outcome, tenant.items[0].expected),
        (platform.items[0].outcome, platform.items[0].expected)
    );
}
