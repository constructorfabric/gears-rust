//! The queries the notification half and the Recorder add to the activation
//! repository, over `SQLite`: the awaiting rows of a pair, the guarded stamp,
//! the due rows, the orphan sweep, the counts and the guarded report.

use std::sync::Arc;

use time::OffsetDateTime;
use toolkit_db::{DBProvider, DbError};
use uuid::Uuid;

use super::ActivationRepo;
use crate::domain::activation::publisher::{StateRow, StateWrite};
use crate::domain::activation::recorder::Transition;
use crate::domain::activation::subscriptions::ActivationRepository;
use crate::domain::activation::{ActivationStatus, NotifiedChange, SubscriberTenant};
use crate::test_support::{sqlite_provider, sqlite_provider_recorded};

const T1: &str = "00000000-0000-0000-0000-0000000000c1";
const T2: &str = "00000000-0000-0000-0000-0000000000c2";

fn id(n: u128) -> Uuid {
    Uuid::from_u128(n)
}

fn pair(subscriber: &str, tenant: &str) -> SubscriberTenant {
    SubscriberTenant {
        subscriber: subscriber.to_owned(),
        tenant: tenant.to_owned(),
    }
}

/// Subscribe and write an `awaiting` state, as a change would.
async fn seed(
    db: &Arc<DBProvider<DbError>>,
    subscriber: &str,
    key: &str,
    tenant: &str,
    change_ref: Uuid,
) {
    let conn = db.conn().expect("connection");
    let subscription = ActivationRepo
        .register(&conn, subscriber, key, tenant)
        .await
        .expect("registered");
    ActivationRepo
        .write_states(
            &conn,
            &StateWrite {
                key: key.to_owned(),
                change_ref,
                rows: vec![StateRow {
                    subscription_id: subscription.id,
                    tenant: tenant.to_owned(),
                    expected_value: "1".to_owned(),
                }],
            },
        )
        .await
        .expect("state written");
}

fn change(key: &str, n: u128) -> NotifiedChange {
    NotifiedChange {
        key: key.to_owned(),
        change_ref: id(n),
    }
}

async fn state(
    db: &Arc<DBProvider<DbError>>,
    subscriber: &str,
    key: &str,
    tenant: &str,
) -> Option<crate::domain::activation::ActivationState> {
    let conn = db.conn().expect("connection");
    ActivationRepo
        .state(&conn, subscriber, key, tenant)
        .await
        .expect("state read")
}

#[tokio::test]
async fn the_awaiting_rows_of_a_pair_come_in_key_order_and_only_those() {
    let db = sqlite_provider().await;
    seed(&db, "gear-x", "k.b", T1, id(2)).await;
    seed(&db, "gear-x", "k.a", T1, id(1)).await;
    seed(&db, "gear-x", "k.other-tenant", T2, id(3)).await;
    seed(&db, "gear-y", "k.other-subscriber", T1, id(4)).await;
    seed(&db, "gear-x", "k.settled", T1, id(5)).await;
    let conn = db.conn().expect("connection");
    ActivationRepo
        .report(
            &conn,
            &Transition {
                subscriber: "gear-x".to_owned(),
                key: "k.settled".to_owned(),
                tenant: T1.to_owned(),
                change_ref: id(5),
                status: ActivationStatus::Succeeded,
                applied_value: Some("1".to_owned()),
                detail: None,
            },
        )
        .await
        .expect("settled");

    let rows = ActivationRepo
        .awaiting_of(&conn, &pair("gear-x", T1))
        .await
        .expect("read");
    assert_eq!(rows, [change("k.a", 1), change("k.b", 2)]);
}

#[tokio::test]
async fn the_stamp_lands_only_on_rows_still_naming_their_change_ref_and_awaiting() {
    let db = sqlite_provider().await;
    seed(&db, "gear-x", "k.a", T1, id(1)).await;
    seed(&db, "gear-x", "k.b", T1, id(2)).await;
    seed(&db, "gear-x", "k.c", T1, id(3)).await;
    let conn = db.conn().expect("connection");
    ActivationRepo
        .report(
            &conn,
            &Transition {
                subscriber: "gear-x".to_owned(),
                key: "k.c".to_owned(),
                tenant: T1.to_owned(),
                change_ref: id(3),
                status: ActivationStatus::Failed,
                applied_value: None,
                detail: None,
            },
        )
        .await
        .expect("settled");
    let at = OffsetDateTime::UNIX_EPOCH + time::Duration::days(1);

    // `k.b` was overwritten since the read (its ref is no longer 99).
    let stamped = ActivationRepo
        .stamp(
            &conn,
            &pair("gear-x", T1),
            &[change("k.a", 1), change("k.b", 99), change("k.c", 3)],
            at,
        )
        .await
        .expect("stamped");

    assert_eq!(stamped, 1, "only the row that still names its change_ref");
    assert_eq!(
        state(&db, "gear-x", "k.a", T1)
            .await
            .unwrap()
            .last_notified_at,
        Some(at)
    );
    assert_eq!(
        state(&db, "gear-x", "k.b", T1)
            .await
            .unwrap()
            .last_notified_at,
        None
    );
    assert_eq!(
        state(&db, "gear-x", "k.c", T1)
            .await
            .unwrap()
            .last_notified_at,
        None,
        "a terminal row is never stamped"
    );
}

#[tokio::test]
async fn a_stamp_over_more_rows_than_one_statement_names_stamps_them_all() {
    let db = sqlite_provider().await;
    let mut named = Vec::new();
    for n in 0..450u128 {
        let key = format!("k.{n:04}");
        seed(&db, "gear-x", &key, T1, id(n + 1)).await;
        named.push(change(&key, n + 1));
    }
    let conn = db.conn().expect("connection");
    let stamped = ActivationRepo
        .stamp(
            &conn,
            &pair("gear-x", T1),
            &named,
            OffsetDateTime::UNIX_EPOCH,
        )
        .await
        .expect("stamped");
    assert_eq!(stamped, 450);
}

#[tokio::test]
async fn the_due_rows_are_the_awaiting_ones_never_stamped_or_stamped_before_the_cutoff() {
    let db = sqlite_provider().await;
    seed(&db, "gear-y", "k.fresh", T1, id(1)).await;
    seed(&db, "gear-x", "k.old", T2, id(2)).await;
    seed(&db, "gear-x", "k.recent", T1, id(3)).await;
    seed(&db, "gear-x", "k.never", T1, id(4)).await;
    let conn = db.conn().expect("connection");
    let old = OffsetDateTime::UNIX_EPOCH + time::Duration::days(1);
    let recent = OffsetDateTime::UNIX_EPOCH + time::Duration::days(3);
    let cutoff = OffsetDateTime::UNIX_EPOCH + time::Duration::days(2);
    ActivationRepo
        .stamp(&conn, &pair("gear-x", T2), &[change("k.old", 2)], old)
        .await
        .unwrap();
    ActivationRepo
        .stamp(&conn, &pair("gear-x", T1), &[change("k.recent", 3)], recent)
        .await
        .unwrap();
    ActivationRepo
        .stamp(&conn, &pair("gear-y", T1), &[change("k.fresh", 1)], recent)
        .await
        .unwrap();

    let due = ActivationRepo.due(&conn, cutoff).await.expect("read");

    let seen: Vec<(String, String, String)> = due
        .iter()
        .map(|d| {
            (
                d.pair.subscriber.clone(),
                d.pair.tenant.clone(),
                d.change.key.clone(),
            )
        })
        .collect();
    assert_eq!(
        seen,
        [
            ("gear-x".to_owned(), T1.to_owned(), "k.never".to_owned()),
            ("gear-x".to_owned(), T2.to_owned(), "k.old".to_owned()),
        ],
        "ordered by subscriber, tenant and key; stamped after the cutoff is not due"
    );
}

#[tokio::test]
async fn the_orphan_sweep_is_one_conditional_delete_whatever_the_status() {
    let (db, recorder) = sqlite_provider_recorded().await;
    seed(&db, "gear-x", "k.kept", T1, id(1)).await;
    seed(&db, "gear-x", "k.gone", T1, id(2)).await;
    seed(&db, "gear-y", "k.settled-gone", T1, id(3)).await;
    let conn = db.conn().expect("connection");
    ActivationRepo
        .report(
            &conn,
            &Transition {
                subscriber: "gear-y".to_owned(),
                key: "k.settled-gone".to_owned(),
                tenant: T1.to_owned(),
                change_ref: id(3),
                status: ActivationStatus::Succeeded,
                applied_value: None,
                detail: None,
            },
        )
        .await
        .unwrap();
    // The subscriptions of two states are deleted behind the states' backs.
    {
        use crate::infra::storage::entity::subscription::{Column, Entity};
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
        use toolkit_db::secure::SecureDeleteExt;
        for key in ["k.gone", "k.settled-gone"] {
            Entity::delete_many()
                .filter(Column::Key.eq(key))
                .secure()
                .scope_with(&toolkit_security::AccessScope::allow_all())
                .exec(&conn)
                .await
                .unwrap();
        }
    }
    recorder.clear();

    let swept = ActivationRepo.sweep_orphans(&conn).await.expect("swept");

    assert_eq!(swept, 2);
    assert_eq!(recorder.total(), 1, "a single statement");
    assert!(state(&db, "gear-x", "k.kept", T1).await.is_some());
    assert!(state(&db, "gear-x", "k.gone", T1).await.is_none());
    assert!(state(&db, "gear-y", "k.settled-gone", T1).await.is_none());
}

#[tokio::test]
async fn the_counts_are_the_subscriptions_and_the_awaiting_states() {
    let db = sqlite_provider().await;
    seed(&db, "gear-x", "k.a", T1, id(1)).await;
    seed(&db, "gear-x", "k.b", T1, id(2)).await;
    let conn = db.conn().expect("connection");
    // A subscription with no state yet counts as a subscription only.
    ActivationRepo
        .register(&conn, "gear-x", "k.quiet", T1)
        .await
        .unwrap();
    ActivationRepo
        .report(
            &conn,
            &Transition {
                subscriber: "gear-x".to_owned(),
                key: "k.b".to_owned(),
                tenant: T1.to_owned(),
                change_ref: id(2),
                status: ActivationStatus::Succeeded,
                applied_value: None,
                detail: None,
            },
        )
        .await
        .unwrap();
    let counts = ActivationRepo.counts(&conn).await.expect("counted");
    assert_eq!((counts.subscriptions, counts.awaiting), (3, 1));
}

#[tokio::test]
async fn the_report_guard_matches_only_the_decided_change_ref_while_awaiting() {
    let db = sqlite_provider().await;
    seed(&db, "gear-x", "k.a", T1, id(1)).await;
    let conn = db.conn().expect("connection");
    let transition = |change_ref: u128, status| Transition {
        subscriber: "gear-x".to_owned(),
        key: "k.a".to_owned(),
        tenant: T1.to_owned(),
        change_ref: id(change_ref),
        status,
        applied_value: Some("reported".to_owned()),
        detail: Some("a detail".to_owned()),
    };
    // The expectation was replaced since the decision: no row matches.
    assert_eq!(
        ActivationRepo
            .report(&conn, &transition(9, ActivationStatus::Succeeded))
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        state(&db, "gear-x", "k.a", T1).await.unwrap().status,
        ActivationStatus::Awaiting
    );

    let at = ActivationRepo
        .report(&conn, &transition(1, ActivationStatus::Failed))
        .await
        .unwrap()
        .expect("the guard matched");
    let settled = state(&db, "gear-x", "k.a", T1).await.unwrap();
    assert_eq!(settled.status, ActivationStatus::Failed);
    assert_eq!(settled.applied_value.as_deref(), Some("reported"));
    assert_eq!(settled.detail.as_deref(), Some("a detail"));
    assert_eq!(settled.reported_at, Some(at));
    assert_eq!(settled.updated_at, at);
    assert_eq!(settled.change_ref, id(1), "never touched by a report");
    assert_eq!(settled.expected_value, "1", "never touched by a report");

    // A second report on the settled row matches nothing.
    assert_eq!(
        ActivationRepo
            .report(&conn, &transition(1, ActivationStatus::Succeeded))
            .await
            .unwrap(),
        None
    );
}
