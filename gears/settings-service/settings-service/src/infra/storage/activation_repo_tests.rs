//! The queries the notification half and the Recorder add to the activation
//! repository, over `SQLite`: the due rows of a pair, the guarded counted
//! stamp, the due rows, the orphan sweep, the counts and the guarded report; and the
//! per-instance primitives — the registration and its cap, the expected set of
//! a change, the re-arm, the clear of `silent_since`, the call of silence, the
//! counted stamp and the forgetting — with the in-memory double's parity.

use std::sync::Arc;

use time::OffsetDateTime;
use toolkit_db::{DBProvider, DbError};
use uuid::Uuid;

use super::ActivationRepo;
use crate::domain::activation::publisher::{StateRow, StateWrite};
use crate::domain::activation::recorder::Transition;
use crate::domain::activation::subscriptions::ActivationRepository;
use crate::domain::activation::testing::one;
use crate::domain::activation::{
    ActivationState, ActivationStatus, DueRow, InstanceKey, NotifiedChange, SubscriberTenant,
    Subscription,
};
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
        .register(&conn, &one(subscriber, key, tenant), None)
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

/// The due row of the one instance of `subscriber` for `key` at `tenant`, as
/// a scan read it naming `change_ref` `n`.
fn due(subscriber: &str, key: &str, tenant: &str, n: u128) -> DueRow {
    DueRow {
        pair: pair(subscriber, tenant),
        instance_id: Uuid::nil(),
        change: change(key, n),
        deliveries: 0,
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
        .state(&conn, &one(subscriber, key, tenant))
        .await
        .expect("state read")
}

#[tokio::test]
async fn the_due_rows_of_a_pair_come_in_key_order_and_only_those() {
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
                instance_id: Uuid::nil(),
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

    // `k.recent` is awaiting but published after the cutoff: not due.
    seed(&db, "gear-x", "k.recent", T1, id(6)).await;
    let cutoff = OffsetDateTime::UNIX_EPOCH + time::Duration::days(2);
    ActivationRepo
        .stamp_due(
            &conn,
            &[due("gear-x", "k.recent", T1, 6)],
            OffsetDateTime::UNIX_EPOCH + time::Duration::days(3),
        )
        .await
        .unwrap();

    let rows = ActivationRepo
        .due_of(&conn, &pair("gear-x", T1), cutoff)
        .await
        .expect("read");
    assert_eq!(
        rows,
        [due("gear-x", "k.a", T1, 1), due("gear-x", "k.b", T1, 2)]
    );
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
                instance_id: Uuid::nil(),
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
        .stamp_due(
            &conn,
            &[
                due("gear-x", "k.a", T1, 1),
                due("gear-x", "k.b", T1, 99),
                due("gear-x", "k.c", T1, 3),
            ],
            at,
        )
        .await
        .expect("stamped");

    assert_eq!(stamped, 1, "only the row that still names its change_ref");
    let a = state(&db, "gear-x", "k.a", T1).await.unwrap();
    assert_eq!((a.last_notified_at, a.deliveries), (Some(at), 1));
    let b = state(&db, "gear-x", "k.b", T1).await.unwrap();
    assert_eq!((b.last_notified_at, b.deliveries), (None, 0));
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
        named.push(due("gear-x", &key, T1, n + 1));
    }
    let conn = db.conn().expect("connection");
    let stamped = ActivationRepo
        .stamp_due(&conn, &named, OffsetDateTime::UNIX_EPOCH)
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
        .stamp_due(&conn, &[due("gear-x", "k.old", T2, 2)], old)
        .await
        .unwrap();
    ActivationRepo
        .stamp_due(&conn, &[due("gear-x", "k.recent", T1, 3)], recent)
        .await
        .unwrap();
    ActivationRepo
        .stamp_due(&conn, &[due("gear-y", "k.fresh", T1, 1)], recent)
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
                instance_id: Uuid::nil(),
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
async fn the_counts_are_the_subscriptions_the_awaiting_and_silent_states_and_the_silent_subscriptions()
 {
    let db = sqlite_provider().await;
    seed(&db, "gear-x", "k.a", T1, id(1)).await;
    seed(&db, "gear-x", "k.b", T1, id(2)).await;
    let conn = db.conn().expect("connection");
    // A subscription with no state yet counts as a subscription only.
    ActivationRepo
        .register(&conn, &one("gear-x", "k.quiet", T1), None)
        .await
        .unwrap();
    ActivationRepo
        .report(
            &conn,
            &Transition {
                subscriber: "gear-x".to_owned(),
                instance_id: Uuid::nil(),
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
    seed(&db, "gear-x", "k.c", T1, id(3)).await;
    silence(&db, &one("gear-x", "k.c", T1), id(3), day(1)).await;
    // A subscription silent for its key whatever its state.
    ActivationRepo
        .mark_silent(&conn, &one("gear-x", "k.quiet", T1), day(1))
        .await
        .unwrap();
    let counts = ActivationRepo.counts(&conn).await.expect("counted");
    assert_eq!(
        counts,
        crate::domain::activation::ActivationCounts {
            subscriptions: 4,
            awaiting: 1,
            silent: 1,
            silent_subscriptions: 2,
        }
    );
}

#[tokio::test]
async fn the_report_guard_matches_only_the_decided_change_ref_while_awaiting() {
    let db = sqlite_provider().await;
    seed(&db, "gear-x", "k.a", T1, id(1)).await;
    let conn = db.conn().expect("connection");
    let transition = |change_ref: u128, status| Transition {
        subscriber: "gear-x".to_owned(),
        instance_id: Uuid::nil(),
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

// ---------------------------------------------------------------------------
// Per instance
// ---------------------------------------------------------------------------

const KEY: &str = "k.per-instance";

/// The identity of instance `n` of `subscriber` for `key` at `tenant`.
fn inst(subscriber: &str, n: u128, key: &str, tenant: &str) -> InstanceKey {
    InstanceKey {
        subscriber: subscriber.to_owned(),
        instance_id: Uuid::from_u128(0x1000 + n),
        key: key.to_owned(),
        tenant: tenant.to_owned(),
    }
}

async fn register(db: &Arc<DBProvider<DbError>>, instance: &InstanceKey) -> Subscription {
    ActivationRepo
        .register(&db.conn().expect("connection"), instance, None)
        .await
        .expect("registered")
}

/// A change of `key` whose read set is `subscriptions`, every one at its own
/// tenant expecting `"1"`.
async fn change_of(
    db: &Arc<DBProvider<DbError>>,
    key: &str,
    change_ref: Uuid,
    subscriptions: &[&Subscription],
) -> Vec<SubscriberTenant> {
    ActivationRepo
        .write_states(
            &db.conn().expect("connection"),
            &StateWrite {
                key: key.to_owned(),
                change_ref,
                rows: subscriptions
                    .iter()
                    .map(|s| StateRow {
                        subscription_id: s.id,
                        tenant: s.tenant.clone(),
                        expected_value: "1".to_owned(),
                    })
                    .collect(),
            },
        )
        .await
        .expect("states written")
}

async fn state_of(
    db: &Arc<DBProvider<DbError>>,
    instance: &InstanceKey,
) -> Option<ActivationState> {
    ActivationRepo
        .state(&db.conn().expect("connection"), instance)
        .await
        .expect("state read")
}

async fn subscription_of(
    db: &Arc<DBProvider<DbError>>,
    instance: &InstanceKey,
) -> Option<Subscription> {
    ActivationRepo
        .find(&db.conn().expect("connection"), instance)
        .await
        .expect("subscription read")
}

/// Make the instance's state of `change_ref` silent and mark its subscription,
/// as the delivery loop's call of silence does.
async fn silence(
    db: &Arc<DBProvider<DbError>>,
    instance: &InstanceKey,
    change_ref: Uuid,
    at: OffsetDateTime,
) {
    let conn = db.conn().expect("connection");
    assert!(
        ActivationRepo
            .call_silent(&conn, instance, change_ref, at)
            .await
            .unwrap(),
        "the state was awaiting the change"
    );
    ActivationRepo
        .mark_silent(&conn, instance, at)
        .await
        .unwrap();
}

fn day(n: i64) -> OffsetDateTime {
    OffsetDateTime::UNIX_EPOCH + time::Duration::days(n)
}

#[tokio::test]
async fn a_registration_is_per_instance_and_a_repeat_refreshes_only_the_label() {
    let db = sqlite_provider().await;
    let conn = db.conn().expect("connection");
    let first = inst("gear", 1, KEY, T1);
    let created = ActivationRepo
        .register(&conn, &first, Some("host-a"))
        .await
        .unwrap();
    assert_eq!(created.instance_id, first.instance_id);
    assert_eq!(created.instance_label.as_deref(), Some("host-a"));
    assert_eq!(created.silent_since, None);
    let sibling = register(&db, &inst("gear", 2, KEY, T1)).await;
    assert_ne!(sibling.id, created.id, "a sibling instance is its own row");
    ActivationRepo
        .mark_silent(&conn, &first, day(1))
        .await
        .unwrap();

    let again = ActivationRepo
        .register(&conn, &first, Some("host-b"))
        .await
        .unwrap();

    assert_eq!(again.id, created.id, "the row is kept");
    assert_eq!(again.created_at, created.created_at);
    assert_eq!(again.instance_label.as_deref(), Some("host-b"));
    let stored = subscription_of(&db, &first).await.unwrap();
    assert_eq!(stored.instance_label.as_deref(), Some("host-b"));
    assert_eq!(
        stored.silent_since,
        Some(day(1)),
        "the repeat leaves `silent_since` to its guarded clear"
    );
    assert_eq!(ActivationRepo.of_key(&conn, KEY).await.unwrap().len(), 2);
}

#[tokio::test]
async fn the_cap_counts_a_subscribers_instances_and_evicts_the_oldest_silent_or_stateless() {
    let db = sqlite_provider().await;
    let conn = db.conn().expect("connection");
    let a = register(&db, &inst("gear", 1, KEY, T1)).await;
    let b = register(&db, &inst("gear", 2, KEY, T1)).await;
    let c = register(&db, &inst("gear", 3, KEY, T1)).await;
    // Not counted: another tenant, another subscriber, another key.
    register(&db, &inst("gear", 4, KEY, T2)).await;
    register(&db, &inst("other", 5, KEY, T1)).await;
    register(&db, &inst("gear", 6, "k.other", T1)).await;
    change_of(&db, KEY, id(1), &[&a, &b, &c]).await;

    assert_eq!(
        ActivationRepo
            .registrations(&conn, "gear", KEY, T1)
            .await
            .unwrap(),
        3
    );
    assert_eq!(
        ActivationRepo
            .oldest_evictable(&conn, "gear", KEY, T1)
            .await
            .unwrap(),
        None,
        "every instance is expected and none is silent"
    );

    silence(&db, &b.instance_key(), id(1), day(1)).await;
    let d = register(&db, &inst("gear", 7, KEY, T1)).await;
    let oldest = ActivationRepo
        .oldest_evictable(&conn, "gear", KEY, T1)
        .await
        .unwrap()
        .expect("one is evictable");
    assert_eq!(
        oldest.id, b.id,
        "the silent one, registered before the stateless one"
    );

    assert!(
        ActivationRepo
            .unsubscribe(&conn, &b.instance_key())
            .await
            .unwrap()
    );
    let oldest = ActivationRepo
        .oldest_evictable(&conn, "gear", KEY, T1)
        .await
        .unwrap()
        .expect("one is evictable");
    assert_eq!(oldest.id, d.id, "a registration that holds no state");
}

#[tokio::test]
async fn an_unsubscribe_deletes_the_instances_state_and_subscription_only() {
    let db = sqlite_provider().await;
    let conn = db.conn().expect("connection");
    let a = register(&db, &inst("gear", 1, KEY, T1)).await;
    let b = register(&db, &inst("gear", 2, KEY, T1)).await;
    change_of(&db, KEY, id(1), &[&a, &b]).await;

    assert!(
        ActivationRepo
            .unsubscribe(&conn, &a.instance_key())
            .await
            .unwrap()
    );

    assert!(state_of(&db, &a.instance_key()).await.is_none());
    assert!(subscription_of(&db, &a.instance_key()).await.is_none());
    assert!(
        state_of(&db, &b.instance_key()).await.is_some(),
        "the sibling is untouched"
    );
    assert!(subscription_of(&db, &b.instance_key()).await.is_some());
    assert!(
        !ActivationRepo
            .unsubscribe(&conn, &a.instance_key())
            .await
            .unwrap(),
        "nothing left to delete"
    );
}

#[tokio::test]
async fn a_change_expects_the_instances_not_silent_and_all_of_them_when_all_are() {
    let db = sqlite_provider().await;
    let a = register(&db, &inst("gear", 1, KEY, T1)).await;
    let b = register(&db, &inst("gear", 2, KEY, T1)).await;
    let c = register(&db, &inst("gear", 3, KEY, T1)).await;
    let lone = register(&db, &inst("other", 4, KEY, T1)).await;
    change_of(&db, KEY, id(1), &[&a, &b, &c, &lone]).await;
    silence(&db, &b.instance_key(), id(1), day(1)).await;
    silence(&db, &lone.instance_key(), id(1), day(1)).await;

    let pairs = change_of(&db, KEY, id(2), &[&a, &b, &c, &lone]).await;

    assert_eq!(
        pairs,
        [pair("gear", T1), pair("other", T1)],
        "each pair once"
    );
    for kept in [&a, &c] {
        let state = state_of(&db, &kept.instance_key()).await.unwrap();
        assert_eq!(state.change_ref, id(2));
        assert_eq!(state.status, ActivationStatus::Awaiting);
        assert_eq!(state.deliveries, 0);
    }
    assert!(
        state_of(&db, &b.instance_key()).await.is_none(),
        "left out while a sibling is expected, its earlier state deleted"
    );
    assert_eq!(
        state_of(&db, &lone.instance_key())
            .await
            .unwrap()
            .change_ref,
        id(2),
        "the subscriber's only instance is silent: expected again"
    );

    // Every instance of `gear` silent now: all of them are expected again.
    silence(&db, &a.instance_key(), id(2), day(2)).await;
    silence(&db, &c.instance_key(), id(2), day(2)).await;
    change_of(&db, KEY, id(3), &[&a, &b, &c, &lone]).await;
    for instance in [&a, &b, &c] {
        let state = state_of(&db, &instance.instance_key()).await.unwrap();
        assert_eq!(
            (state.change_ref, state.status),
            (id(3), ActivationStatus::Awaiting)
        );
        assert_eq!(state.silent_since, None);
    }
}

#[tokio::test]
async fn a_change_overwrites_any_status_and_stamps_changed_at_from_the_database() {
    let db = sqlite_provider().await;
    let conn = db.conn().expect("connection");
    let a = register(&db, &inst("gear", 1, KEY, T1)).await;
    let b = register(&db, &inst("gear", 2, KEY, T1)).await;
    change_of(&db, KEY, id(1), &[&a, &b]).await;
    let first = state_of(&db, &a.instance_key()).await.unwrap();
    ActivationRepo
        .stamp_due(
            &conn,
            &ActivationRepo.due(&conn, day(100_000)).await.unwrap(),
            day(1),
        )
        .await
        .unwrap();
    ActivationRepo
        .report(
            &conn,
            &Transition {
                subscriber: "gear".to_owned(),
                instance_id: a.instance_id,
                key: KEY.to_owned(),
                tenant: T1.to_owned(),
                change_ref: id(1),
                status: ActivationStatus::Failed,
                applied_value: Some("2".to_owned()),
                detail: Some("no".to_owned()),
            },
        )
        .await
        .unwrap()
        .expect("settled");
    let earlier = OffsetDateTime::now_utc() - time::Duration::minutes(1);
    assert!(
        first.changed_at > earlier,
        "the database's now, not a client clock: {}",
        first.changed_at
    );

    change_of(&db, KEY, id(2), &[&a, &b]).await;

    for instance in [&a, &b] {
        let state = state_of(&db, &instance.instance_key()).await.unwrap();
        assert_eq!(state.change_ref, id(2));
        assert_eq!(state.status, ActivationStatus::Awaiting);
        assert_eq!(state.deliveries, 0);
        assert_eq!(
            (
                state.applied_value,
                state.detail,
                state.reported_at,
                state.last_notified_at
            ),
            (None, None, None, None)
        );
    }
}

#[tokio::test]
async fn a_report_settles_a_silent_state_keeping_its_silent_since() {
    let db = sqlite_provider().await;
    let conn = db.conn().expect("connection");
    let a = register(&db, &inst("gear", 1, KEY, T1)).await;
    change_of(&db, KEY, id(1), &[&a]).await;
    silence(&db, &a.instance_key(), id(1), day(1)).await;

    let at = ActivationRepo
        .report(
            &conn,
            &Transition {
                subscriber: "gear".to_owned(),
                instance_id: a.instance_id,
                key: KEY.to_owned(),
                tenant: T1.to_owned(),
                change_ref: id(1),
                status: ActivationStatus::Succeeded,
                applied_value: Some("1".to_owned()),
                detail: None,
            },
        )
        .await
        .unwrap()
        .expect("a silent state is not terminal");

    let state = state_of(&db, &a.instance_key()).await.unwrap();
    assert_eq!(state.status, ActivationStatus::Succeeded);
    assert_eq!(state.reported_at, Some(at));
    assert_eq!(
        state.silent_since,
        Some(day(1)),
        "the read shows it came late"
    );
}

#[tokio::test]
async fn the_rearm_returns_only_the_silent_state_of_the_change_it_read() {
    let db = sqlite_provider().await;
    let conn = db.conn().expect("connection");
    let a = register(&db, &inst("gear", 1, KEY, T1)).await;
    change_of(&db, KEY, id(1), &[&a]).await;
    let due = ActivationRepo.due(&conn, day(100_000)).await.unwrap();
    ActivationRepo.stamp_due(&conn, &due, day(1)).await.unwrap();
    let key = a.instance_key();
    assert!(
        !ActivationRepo.rearm(&conn, &key, id(1)).await.unwrap(),
        "an awaiting state is not re-armed"
    );
    silence(&db, &key, id(1), day(2)).await;

    assert!(
        !ActivationRepo.rearm(&conn, &key, id(9)).await.unwrap(),
        "another change's re-arm matches nothing"
    );
    assert!(ActivationRepo.rearm(&conn, &key, id(1)).await.unwrap());

    let state = state_of(&db, &key).await.unwrap();
    assert_eq!(state.status, ActivationStatus::Awaiting);
    assert_eq!(state.deliveries, 0);
    assert_eq!((state.silent_since, state.last_notified_at), (None, None));
    assert_eq!(state.change_ref, id(1));
    assert!(
        !ActivationRepo.rearm(&conn, &key, id(1)).await.unwrap(),
        "no longer silent"
    );
}

#[tokio::test]
async fn silent_since_is_cleared_only_while_the_instance_holds_no_silent_state() {
    let db = sqlite_provider().await;
    let conn = db.conn().expect("connection");
    let a = register(&db, &inst("gear", 1, KEY, T1)).await;
    let b = register(&db, &inst("gear", 2, KEY, T1)).await;
    change_of(&db, KEY, id(1), &[&a, &b]).await;
    silence(&db, &a.instance_key(), id(1), day(1)).await;
    silence(&db, &b.instance_key(), id(1), day(1)).await;

    assert!(
        !ActivationRepo
            .clear_silent(&conn, &a.instance_key())
            .await
            .unwrap(),
        "its silent state keeps it silent"
    );
    assert!(
        ActivationRepo
            .rearm(&conn, &a.instance_key(), id(1))
            .await
            .unwrap()
    );
    assert!(
        ActivationRepo
            .clear_silent(&conn, &a.instance_key())
            .await
            .unwrap()
    );

    assert_eq!(
        subscription_of(&db, &a.instance_key())
            .await
            .unwrap()
            .silent_since,
        None
    );
    assert_eq!(
        subscription_of(&db, &b.instance_key())
            .await
            .unwrap()
            .silent_since,
        Some(day(1)),
        "a sibling's silence is its own"
    );
    assert!(
        !ActivationRepo
            .clear_silent(&conn, &a.instance_key())
            .await
            .unwrap(),
        "nothing left to clear"
    );
    let silent = ActivationRepo
        .silent_subscriptions(&conn, KEY, T1)
        .await
        .unwrap();
    assert_eq!(
        silent.iter().map(|s| s.id).collect::<Vec<_>>(),
        [b.id],
        "only the subscriptions whose `silent_since` is set"
    );
}

#[tokio::test]
async fn the_call_of_silence_is_guarded_and_marks_the_subscription_once() {
    let db = sqlite_provider().await;
    let conn = db.conn().expect("connection");
    let a = register(&db, &inst("gear", 1, KEY, T1)).await;
    change_of(&db, KEY, id(1), &[&a]).await;
    let key = a.instance_key();

    assert!(
        !ActivationRepo
            .call_silent(&conn, &key, id(9), day(1))
            .await
            .unwrap(),
        "a replaced expectation is left alone"
    );
    assert!(
        ActivationRepo
            .call_silent(&conn, &key, id(1), day(1))
            .await
            .unwrap()
    );
    assert!(
        !ActivationRepo
            .call_silent(&conn, &key, id(1), day(2))
            .await
            .unwrap(),
        "only an awaiting state is called silent"
    );
    let state = state_of(&db, &key).await.unwrap();
    assert_eq!(state.status, ActivationStatus::Silent);
    assert_eq!(state.silent_since, Some(day(1)));
    assert_eq!(state.updated_at, day(1));

    assert!(
        ActivationRepo
            .mark_silent(&conn, &key, day(1))
            .await
            .unwrap()
    );
    assert!(
        !ActivationRepo
            .mark_silent(&conn, &key, day(2))
            .await
            .unwrap(),
        "the first call of silence is the one the day counts from"
    );
    assert_eq!(
        subscription_of(&db, &key).await.unwrap().silent_since,
        Some(day(1))
    );
}

#[tokio::test]
async fn the_due_rows_carry_their_instance_and_count_and_a_silent_state_is_never_due() {
    let db = sqlite_provider().await;
    let conn = db.conn().expect("connection");
    let a = register(&db, &inst("gear", 1, KEY, T1)).await;
    let b = register(&db, &inst("gear", 2, KEY, T1)).await;
    let c = register(&db, &inst("gear", 3, KEY, T1)).await;
    change_of(&db, KEY, id(1), &[&a, &b, &c]).await;
    let all = ActivationRepo.due(&conn, day(100_000)).await.unwrap();
    assert_eq!(all.len(), 3);
    // `a` delivered once; `c` called silent; a stale bundle names `b`.
    let a_row = all
        .iter()
        .find(|d| d.instance_id == a.instance_id)
        .unwrap()
        .clone();
    let mut stale = all
        .iter()
        .find(|d| d.instance_id == b.instance_id)
        .unwrap()
        .clone();
    stale.change.change_ref = id(9);
    silence(&db, &c.instance_key(), id(1), day(1)).await;

    let stamped = ActivationRepo
        .stamp_due(&conn, &[a_row.clone(), stale, all[2].clone()], day(2))
        .await
        .unwrap();

    assert_eq!(
        stamped, 1,
        "only the bundled row still naming its change and awaiting"
    );
    let a_state = state_of(&db, &a.instance_key()).await.unwrap();
    assert_eq!(
        (a_state.deliveries, a_state.last_notified_at),
        (1, Some(day(2)))
    );
    let b_state = state_of(&db, &b.instance_key()).await.unwrap();
    assert_eq!((b_state.deliveries, b_state.last_notified_at), (0, None));
    // A later scan reads `a` due again at its new count and publishes it once
    // more.
    let a_row = ActivationRepo
        .due(&conn, day(100_000))
        .await
        .unwrap()
        .into_iter()
        .find(|d| d.instance_id == a.instance_id)
        .unwrap();
    assert_eq!(a_row.deliveries, 1);
    ActivationRepo
        .stamp_due(&conn, &[a_row], day(3))
        .await
        .unwrap();

    let due = ActivationRepo.due(&conn, day(100_000)).await.unwrap();
    let seen: Vec<(Uuid, u32)> = due.iter().map(|d| (d.instance_id, d.deliveries)).collect();
    assert_eq!(
        seen,
        [(a.instance_id, 2), (b.instance_id, 0)],
        "the silent one is not due; ordered by instance within the key"
    );
}

#[tokio::test]
async fn the_forgetting_deletes_a_silent_instance_only_while_it_is_still_silent() {
    let db = sqlite_provider().await;
    let conn = db.conn().expect("connection");
    let a = register(&db, &inst("gear", 1, KEY, T1)).await;
    let b = register(&db, &inst("gear", 2, KEY, T1)).await;
    let c = register(&db, &inst("gear", 3, KEY, T1)).await;
    change_of(&db, KEY, id(1), &[&a, &b, &c]).await;
    silence(&db, &a.instance_key(), id(1), day(1)).await;
    silence(&db, &b.instance_key(), id(1), day(2)).await;
    silence(&db, &c.instance_key(), id(1), day(5)).await;

    let forgettable = ActivationRepo.forgettable(&conn, day(3)).await.unwrap();
    assert_eq!(
        forgettable.iter().map(|s| s.id).collect::<Vec<_>>(),
        [a.id, b.id],
        "silent before the cutoff, oldest silence first"
    );
    // `b` re-enters before the forgetting reaches it.
    ActivationRepo
        .rearm(&conn, &b.instance_key(), id(1))
        .await
        .unwrap();
    ActivationRepo
        .clear_silent(&conn, &b.instance_key())
        .await
        .unwrap();

    assert!(
        ActivationRepo
            .forget(&conn, &a.instance_key(), day(3))
            .await
            .unwrap()
    );
    assert!(
        !ActivationRepo
            .forget(&conn, &b.instance_key(), day(3))
            .await
            .unwrap(),
        "a cleared subscription is kept"
    );
    assert!(
        !ActivationRepo
            .forget(&conn, &c.instance_key(), day(3))
            .await
            .unwrap(),
        "silent since after the cutoff"
    );

    assert!(subscription_of(&db, &a.instance_key()).await.is_none());
    assert!(
        state_of(&db, &a.instance_key()).await.is_none(),
        "its state went first"
    );
    assert!(state_of(&db, &b.instance_key()).await.is_some());
    assert!(state_of(&db, &c.instance_key()).await.is_some());
}

#[tokio::test]
async fn the_orphan_sweep_and_the_purge_are_per_instance() {
    let db = sqlite_provider().await;
    let conn = db.conn().expect("connection");
    let a = register(&db, &inst("gear", 1, KEY, T1)).await;
    let b = register(&db, &inst("gear", 2, KEY, T1)).await;
    let other = register(&db, &inst("gear", 3, "k.kept", T1)).await;
    change_of(&db, KEY, id(1), &[&a, &b]).await;
    change_of(&db, "k.kept", id(2), &[&other]).await;
    {
        use crate::infra::storage::entity::subscription::{Column, Entity};
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
        use toolkit_db::secure::SecureDeleteExt;
        Entity::delete_many()
            .filter(Column::Id.eq(a.id))
            .secure()
            .scope_with(&toolkit_security::AccessScope::allow_all())
            .exec(&conn)
            .await
            .unwrap();
    }

    assert_eq!(ActivationRepo.sweep_orphans(&conn).await.unwrap(), 1);
    assert!(state_of(&db, &a.instance_key()).await.is_none());
    assert!(
        state_of(&db, &b.instance_key()).await.is_some(),
        "its sibling is subscribed"
    );

    assert_eq!(ActivationRepo.delete_key(&conn, KEY).await.unwrap(), (1, 1));
    assert!(subscription_of(&db, &b.instance_key()).await.is_none());
    assert!(
        state_of(&db, &other.instance_key()).await.is_some(),
        "another key is kept"
    );
}

/// Two publishers read one due row before either stamps — the end of a request
/// and a tick, or two replicas' ticks — and both publications are accepted:
/// the first stamp counts the delivery, the second finds the row no longer at
/// the count it read it due with and counts nothing. Over the tables and over
/// the in-memory double alike.
#[tokio::test]
async fn a_second_stamp_of_one_read_counts_nothing_over_the_tables_and_the_double() {
    use crate::domain::activation::NotificationStore;
    use crate::domain::activation::testing::InMemoryNotificationRepo;
    use std::time::Duration;

    let db = sqlite_provider().await;
    let conn = db.conn().expect("connection");
    let a = register(&db, &inst("gear", 1, KEY, T1)).await;
    change_of(&db, KEY, id(1), &[&a]).await;
    let first = ActivationRepo.due(&conn, day(100_000)).await.unwrap();
    let second = ActivationRepo.due(&conn, day(100_000)).await.unwrap();
    assert_eq!(first, second, "both read the one row due");
    assert_eq!(
        ActivationRepo
            .stamp_due(&conn, &first, day(1))
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        ActivationRepo
            .stamp_due(&conn, &second, day(1))
            .await
            .unwrap(),
        0,
        "stamped since the read: not counted again"
    );
    let state = state_of(&db, &a.instance_key()).await.unwrap();
    assert_eq!(
        (state.deliveries, state.last_notified_at),
        (1, Some(day(1)))
    );

    let memory = InMemoryNotificationRepo::new();
    memory.add("gear", T1, KEY, id(1));
    let first = memory.due(Duration::ZERO).await.unwrap();
    let second = memory.due(Duration::ZERO).await.unwrap();
    assert_eq!(memory.stamp_due(&first).unwrap(), 1);
    assert_eq!(memory.stamp_due(&second).unwrap(), 0, "the double agrees");
    assert_eq!(memory.row("gear", T1, KEY).unwrap().deliveries, 1);
}

/// One scenario of the delivery loop's primitives over the tables and over the
/// in-memory double, which must answer alike.
#[tokio::test]
async fn the_in_memory_double_answers_the_delivery_primitives_as_the_tables_do() {
    use crate::domain::activation::NotificationStore;
    use crate::domain::activation::testing::InMemoryNotificationRepo;
    use std::time::Duration;

    let db = sqlite_provider().await;
    let conn = db.conn().expect("connection");
    let memory = InMemoryNotificationRepo::new();
    let [a, b] = [inst("gear", 1, KEY, T1), inst("gear", 2, KEY, T1)];
    let mut subscriptions = Vec::new();
    for instance in [&a, &b] {
        subscriptions.push(register(&db, instance).await);
        memory.add_instance(&instance.subscriber, instance.instance_id, T1, KEY, id(1));
    }
    change_of(&db, KEY, id(1), &subscriptions.iter().collect::<Vec<_>>()).await;
    let fake = |days: u64| Duration::from_secs(days * 86_400);

    // Every row due, never published.
    let db_due = ActivationRepo.due(&conn, day(100_000)).await.unwrap();
    let mem_due = memory.due(Duration::ZERO).await.unwrap();
    assert_eq!(db_due, mem_due);

    // One counted delivery on `a` only.
    memory.advance(fake(1));
    assert_eq!(
        ActivationRepo
            .stamp_due(&conn, &db_due[..1], day(1))
            .await
            .unwrap(),
        memory.stamp_due(&mem_due[..1]).unwrap()
    );
    // A day later, with a half-day interval: the stamped row is due again.
    memory.advance(fake(1));
    let half_day = fake(1) / 2;
    let db_due = ActivationRepo
        .due(&conn, day(2) - time::Duration::hours(12))
        .await
        .unwrap();
    let mem_due = memory.due(half_day).await.unwrap();
    assert_eq!(db_due.len(), 2);
    assert_eq!(db_due, mem_due, "deliveries and the stamp agree");

    // `b` called silent once, guarded on its change.
    for (change_ref, expected) in [(id(9), false), (id(1), true), (id(1), false)] {
        assert_eq!(
            ActivationRepo
                .call_silent(&conn, &b, change_ref, day(1))
                .await
                .unwrap(),
            expected
        );
        assert_eq!(memory.call_silent(&b, change_ref).unwrap(), expected);
    }
    for expected in [true, false] {
        assert_eq!(
            ActivationRepo.mark_silent(&conn, &b, day(1)).await.unwrap(),
            expected
        );
        assert_eq!(memory.mark_silent(&b).unwrap(), expected);
    }
    assert_eq!(
        ActivationRepo.due(&conn, day(100_000)).await.unwrap(),
        memory.due(Duration::ZERO).await.unwrap(),
        "the silent row is due on neither"
    );

    // Forgotten a day later, with its state.
    let db_forgettable: Vec<InstanceKey> = ActivationRepo
        .forgettable(&conn, day(3))
        .await
        .unwrap()
        .iter()
        .map(Subscription::instance_key)
        .collect();
    assert_eq!(db_forgettable, memory.forgettable(fake(3)).unwrap());
    assert_eq!(
        ActivationRepo.forget(&conn, &b, day(3)).await.unwrap(),
        memory.forget(&b, fake(3)).unwrap()
    );
    assert!(state_of(&db, &b).await.is_none() && memory.row_of(&b).is_none());
    let db_counts = ActivationRepo.counts(&conn).await.unwrap();
    let mem_counts = memory.counts().await.unwrap();
    assert_eq!(db_counts, mem_counts);
}

/// *Delivery backoff*, at its boundary: a stamp exactly one interval old is due
/// and one a millisecond younger is not — for the pair's read and for the
/// scan, over the tables and over the in-memory double alike.
#[tokio::test]
async fn a_stamp_exactly_one_interval_old_is_due_and_one_a_millisecond_younger_is_not() {
    use crate::domain::activation::NotificationStore;
    use crate::domain::activation::testing::InMemoryNotificationRepo;
    use std::time::Duration;

    let interval = Duration::from_secs(30);
    let millisecond = Duration::from_millis(1);
    let span = |d: Duration| time::Duration::try_from(d).unwrap();

    let db = sqlite_provider().await;
    seed(&db, "gear-x", "k.a", T1, id(1)).await;
    let conn = db.conn().expect("connection");
    let stamped_at = day(1);
    ActivationRepo
        .stamp_due(&conn, &[due("gear-x", "k.a", T1, 1)], stamped_at)
        .await
        .unwrap();
    // The cutoff is `now` less the interval: `now` one interval after the
    // stamp, then a millisecond short of it.
    for (now, expected) in [
        (stamped_at + span(interval), 1),
        (stamped_at + span(interval) - span(millisecond), 0),
    ] {
        let cutoff = now - span(interval);
        assert_eq!(
            ActivationRepo.due(&conn, cutoff).await.unwrap().len(),
            expected,
            "the scan at {now}"
        );
        assert_eq!(
            ActivationRepo
                .due_of(&conn, &pair("gear-x", T1), cutoff)
                .await
                .unwrap()
                .len(),
            expected,
            "the pair's read at {now}"
        );
    }

    for (elapsed, expected) in [(interval, 1), (interval.saturating_sub(millisecond), 0)] {
        let memory = InMemoryNotificationRepo::new();
        memory.add("gear-x", T1, "k.a", id(1));
        let rows = memory.due(Duration::ZERO).await.unwrap();
        memory.stamp_due(&rows).unwrap();
        memory.advance(elapsed);
        assert_eq!(
            memory.due(interval).await.unwrap().len(),
            expected,
            "the double's scan {elapsed:?} after the stamp"
        );
        assert_eq!(
            memory
                .due_of(&pair("gear-x", T1), interval)
                .await
                .unwrap()
                .len(),
            expected,
            "the double's pair read {elapsed:?} after the stamp"
        );
    }
}
