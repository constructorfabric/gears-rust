//! The store's transactions over `SQLite`: the call of silence — guarded, the
//! state before the subscription, one `activation` record with the instance,
//! the account's transition judged in the transaction — and the forgetting of
//! one subscription, conditional on its `silent_since` and writing no record.

use std::sync::Arc;
use std::time::Duration;

use time::OffsetDateTime;
use toolkit_db::{DBProvider, DbError};
use toolkit_security::AccessScope;
use uuid::Uuid;

use super::DbNotificationStore;
use crate::audit::{ActorClassification, AuditOperation, AuditOutcome, StoredAuditRecord};
use crate::domain::activation::publisher::{StateRow, StateWrite};
use crate::domain::activation::subscriptions::ActivationRepository;
use crate::domain::activation::{
    ActivationStatus, DueRow, InstanceKey, NotificationStore, NotifiedChange, SILENT_FORGET,
    Silenced, SubscriberTenant, Subscription,
};
use crate::infra::storage::activation_repo::ActivationRepo;
use crate::infra::storage::audit_store::AuditStore;
use crate::test_support::sqlite_provider;

const T1: &str = "00000000-0000-0000-0000-0000000000c1";
const KEY: &str = "gts.cf.core.settings.setting_type.v1~acme.net.alpha.v1~";

fn instance(n: u128) -> InstanceKey {
    InstanceKey {
        subscriber: "gear-x".to_owned(),
        instance_id: Uuid::from_u128(0x100 + n),
        key: KEY.to_owned(),
        tenant: T1.to_owned(),
    }
}

/// Register each instance with a label of its own and write one change's
/// `awaiting` states for all of them.
async fn subscribed(
    db: &Arc<DBProvider<DbError>>,
    instances: &[&InstanceKey],
    change_ref: Uuid,
) -> Vec<Subscription> {
    let conn = db.conn().expect("connection");
    let mut subscriptions = Vec::new();
    for of in instances {
        let label = format!("host-{}", of.instance_id.as_u128() - 0x100);
        subscriptions.push(
            ActivationRepo
                .register(&conn, of, Some(&label))
                .await
                .expect("registered"),
        );
    }
    ActivationRepo
        .write_states(
            &conn,
            &StateWrite {
                key: KEY.to_owned(),
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
        .expect("states written");
    subscriptions
}

fn due(of: &InstanceKey, change_ref: Uuid) -> DueRow {
    DueRow {
        pair: SubscriberTenant {
            subscriber: of.subscriber.clone(),
            tenant: of.tenant.clone(),
        },
        instance_id: of.instance_id,
        change: NotifiedChange {
            key: of.key.clone(),
            change_ref,
        },
        deliveries: 20,
    }
}

async fn records(db: &Arc<DBProvider<DbError>>, change_ref: Uuid) -> Vec<StoredAuditRecord> {
    AuditStore
        .reports_for(
            &db.conn().expect("connection"),
            &AccessScope::allow_all(),
            KEY,
            &[change_ref],
        )
        .await
        .expect("records read")
}

async fn subscription(db: &Arc<DBProvider<DbError>>, of: &InstanceKey) -> Option<Subscription> {
    ActivationRepo
        .find(&db.conn().expect("connection"), of)
        .await
        .expect("read")
}

#[tokio::test]
async fn a_call_of_silence_writes_the_state_the_subscription_and_one_record_naming_the_instance() {
    let db = sqlite_provider().await;
    let store = DbNotificationStore::new(Arc::clone(&db));
    let (a, b) = (instance(1), instance(2));
    let change_ref = Uuid::from_u128(7);
    subscribed(&db, &[&a, &b], change_ref).await;
    let before = OffsetDateTime::now_utc() - time::Duration::seconds(1);

    let first = store.silence(&due(&a, change_ref)).await.expect("called");
    let second = store.silence(&due(&b, change_ref)).await.expect("called");

    assert_eq!(
        first,
        Some(Silenced { incomplete: true }),
        "the account turned incomplete"
    );
    assert_eq!(
        second,
        Some(Silenced { incomplete: false }),
        "already incomplete: no second transition"
    );
    let conn = db.conn().expect("connection");
    let state = ActivationRepo
        .state(&conn, &a)
        .await
        .unwrap()
        .expect("kept");
    assert_eq!(state.status, ActivationStatus::Silent);
    let silent_since = state.silent_since.expect("set");
    assert!(silent_since > before);
    assert_eq!(
        subscription(&db, &a).await.unwrap().silent_since,
        Some(silent_since),
        "the subscription is silent from the same moment"
    );
    let records = records(&db, change_ref).await;
    assert_eq!(records.len(), 2, "one record per call");
    let record = records
        .iter()
        .find(|r| r.instance_id == Some(a.instance_id))
        .expect("the record of `a`");
    assert_eq!(record.operation, AuditOperation::Activation);
    assert_eq!(record.outcome, AuditOutcome::Silent);
    assert_eq!(record.instance_label.as_deref(), Some("host-1"));
    assert_eq!(record.change_ref, Some(change_ref));
    assert_eq!(record.actor, "gear-x");
    assert_eq!(record.actor_classification, ActorClassification::Public);
    assert_eq!(record.tenant_id, Some(Uuid::parse_str(T1).unwrap()));
    assert_eq!(record.detail, None);
    assert_eq!((&record.pre_image, &record.post_image), (&None, &None));
    assert!(
        Uuid::parse_str(&record.request_id).is_ok(),
        "a minted request id"
    );
}

#[tokio::test]
async fn a_call_of_silence_that_lost_its_race_writes_nothing_else() {
    let db = sqlite_provider().await;
    let store = DbNotificationStore::new(Arc::clone(&db));
    let a = instance(1);
    subscribed(&db, &[&a], Uuid::from_u128(1)).await;
    // A later change replaced the state the scan read.
    let called = store
        .silence(&due(&a, Uuid::from_u128(9)))
        .await
        .expect("ran");
    assert_eq!(called, None);
    assert_eq!(subscription(&db, &a).await.unwrap().silent_since, None);
    assert!(records(&db, Uuid::from_u128(9)).await.is_empty());
    assert!(records(&db, Uuid::from_u128(1)).await.is_empty());
    let state = ActivationRepo
        .state(&db.conn().unwrap(), &a)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(state.status, ActivationStatus::Awaiting);
}

#[tokio::test]
async fn a_call_on_a_failed_account_counts_no_transition() {
    let db = sqlite_provider().await;
    let store = DbNotificationStore::new(Arc::clone(&db));
    let (a, b) = (instance(1), instance(2));
    let change_ref = Uuid::from_u128(3);
    subscribed(&db, &[&a, &b], change_ref).await;
    ActivationRepo
        .report(
            &db.conn().unwrap(),
            &crate::domain::activation::recorder::Transition {
                subscriber: a.subscriber.clone(),
                instance_id: a.instance_id,
                key: a.key.clone(),
                tenant: a.tenant.clone(),
                change_ref,
                status: ActivationStatus::Failed,
                applied_value: None,
                detail: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        store.silence(&due(&b, change_ref)).await.unwrap(),
        Some(Silenced { incomplete: false })
    );
}

#[tokio::test]
async fn the_forgetting_deletes_a_day_silent_instance_with_its_state_and_writes_no_record() {
    let db = sqlite_provider().await;
    let store = DbNotificationStore::new(Arc::clone(&db));
    let (old, young, other) = (instance(1), instance(2), instance(3));
    let change_ref = Uuid::from_u128(5);
    subscribed(&db, &[&old, &young, &other], change_ref).await;
    let conn = db.conn().expect("connection");
    let now = OffsetDateTime::now_utc();
    for (of, ago) in [
        (&old, SILENT_FORGET + Duration::from_secs(60)),
        (&young, Duration::from_secs(60)),
    ] {
        let at = now - time::Duration::try_from(ago).unwrap();
        assert!(
            ActivationRepo
                .call_silent(&conn, of, change_ref, at)
                .await
                .unwrap()
        );
        assert!(ActivationRepo.mark_silent(&conn, of, at).await.unwrap());
    }

    let past = store.silent_past(SILENT_FORGET).await.expect("read");
    assert_eq!(
        past,
        std::slice::from_ref(&old),
        "only the one silent for over a day"
    );
    assert!(store.forget_silent(&old, SILENT_FORGET).await.unwrap());

    assert!(subscription(&db, &old).await.is_none());
    assert!(ActivationRepo.state(&conn, &old).await.unwrap().is_none());
    for kept in [&young, &other] {
        assert!(subscription(&db, kept).await.is_some());
        assert!(ActivationRepo.state(&conn, kept).await.unwrap().is_some());
    }
    assert!(
        records(&db, change_ref).await.is_empty(),
        "no audit record is written"
    );
    let counts = store.counts().await.unwrap();
    assert_eq!(
        (
            counts.subscriptions,
            counts.awaiting,
            counts.silent,
            counts.silent_subscriptions
        ),
        (2, 1, 1, 1)
    );
}

#[tokio::test]
async fn a_subscription_whose_silence_ended_after_the_read_is_kept_with_its_state() {
    let db = sqlite_provider().await;
    let store = DbNotificationStore::new(Arc::clone(&db));
    let a = instance(1);
    let change_ref = Uuid::from_u128(5);
    subscribed(&db, &[&a], change_ref).await;
    let conn = db.conn().expect("connection");
    let at = OffsetDateTime::now_utc() - time::Duration::days(2);
    assert!(ActivationRepo.mark_silent(&conn, &a, at).await.unwrap());
    assert_eq!(
        store.silent_past(SILENT_FORGET).await.unwrap(),
        std::slice::from_ref(&a)
    );
    // A report of the instance ends its silence before the forgetting.
    assert!(ActivationRepo.clear_silent(&conn, &a).await.unwrap());
    assert!(!store.forget_silent(&a, SILENT_FORGET).await.unwrap());
    assert!(subscription(&db, &a).await.is_some());
    assert!(ActivationRepo.state(&conn, &a).await.unwrap().is_some());
}

#[tokio::test]
async fn the_end_of_request_read_and_stamp_count_only_the_due_rows_named() {
    let db = sqlite_provider().await;
    let store = DbNotificationStore::new(Arc::clone(&db));
    let (a, b) = (instance(1), instance(2));
    let change_ref = Uuid::from_u128(4);
    subscribed(&db, &[&a, &b], change_ref).await;
    let pair = SubscriberTenant {
        subscriber: "gear-x".to_owned(),
        tenant: T1.to_owned(),
    };
    let rows = store
        .due_of(&pair, Duration::from_secs(30))
        .await
        .expect("read");
    assert_eq!(rows.len(), 2, "both instances' rows are due");
    assert_eq!(store.stamp_due(&rows[..1]).await.unwrap(), 1);
    let rows = store.due_of(&pair, Duration::from_secs(30)).await.unwrap();
    assert_eq!(rows.len(), 1, "the stamped row is no longer due");
    assert_eq!(rows[0].instance_id, b.instance_id);
    let conn = db.conn().unwrap();
    let a_state = ActivationRepo.state(&conn, &a).await.unwrap().unwrap();
    assert_eq!(a_state.deliveries, 1);
    assert!(a_state.last_notified_at.is_some());
    assert_eq!(store.stamp_due(&[]).await.unwrap(), 0);
}

/// *Report Timeout, Silence and Forgetting*: a delivery counts only as a
/// publication the broker accepted while the state was due. The end of a
/// request (the exact interval) and a tick of the delivery loop (its tick age)
/// read the same due row before either stamps; both publications are accepted,
/// and the second stamp finds the row no longer as it read it due — one
/// delivery, not two.
#[tokio::test]
async fn two_publishers_that_read_one_due_row_count_one_delivery() {
    let db = sqlite_provider().await;
    let store = DbNotificationStore::new(Arc::clone(&db));
    let a = instance(1);
    subscribed(&db, &[&a], Uuid::from_u128(4)).await;
    let pair = SubscriberTenant {
        subscriber: "gear-x".to_owned(),
        tenant: T1.to_owned(),
    };
    let interval = Duration::from_secs(30);
    let by_request = store.due_of(&pair, interval).await.expect("read");
    let by_tick = store
        .due(crate::domain::activation::notifier::tick_age(interval))
        .await
        .expect("read");
    assert_eq!(
        (by_request.len(), by_tick.len()),
        (1, 1),
        "both read it due"
    );

    assert_eq!(store.stamp_due(&by_request).await.unwrap(), 1);
    assert_eq!(
        store.stamp_due(&by_tick).await.unwrap(),
        0,
        "the row was stamped since the tick read it: no longer due"
    );
    let conn = db.conn().unwrap();
    let state = ActivationRepo.state(&conn, &a).await.unwrap().unwrap();
    assert_eq!(state.deliveries, 1, "one delivery, not two");
}
