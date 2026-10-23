#![allow(clippy::expect_used)]
//! The notification outbox: the enqueue contract, and the one pipeline with
//! its leased handler, on `SQLite`.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use quota_enforcement_sdk::{
    DeliveryOutcome, EventId, NotificationDeliveryV1, NotificationEvent, TenantId,
};
use toolkit_db::outbox::{DeadLetterFilter, DeadLetterScope, Records};
use uuid::Uuid;

use super::{
    AlreadyBound, EnqueueError, NOTIFICATION_PARTITIONS, NOTIFICATION_QUEUE, NotificationOutbox,
    PipelineError, QeOutbox, start_notification_pipeline,
};
use crate::domain::ports::QuotaStore;
use crate::infra::storage::SqlQuotaStore;
use crate::test_support::{
    actor, bound_outbox, draft, enqueued_messages, quota_changed, scope_for, tenant, test_db,
};

fn ctx() -> toolkit_security::SecurityContext {
    toolkit_security::SecurityContext::builder()
        .subject_id(Uuid::from_u128(0x5eed))
        .subject_tenant_id(tenant().as_uuid())
        .build()
        .expect("context")
}

#[test]
fn a_tenant_always_lands_on_the_same_partition_within_the_bound() {
    for raw in [0_u128, 1, 7, 8, u128::MAX, 0x00ac_ce55] {
        let tenant = TenantId::new(Uuid::from_u128(raw));
        let partition = QeOutbox::partition_for(tenant);
        assert!(partition < u32::from(NOTIFICATION_PARTITIONS), "{raw}");
        assert_eq!(partition, QeOutbox::partition_for(tenant), "stable");
    }
}

#[tokio::test]
async fn an_unbound_handle_takes_an_empty_enqueue_and_refuses_any_other() {
    let db = test_db().await;
    let outbox = QeOutbox::new();
    assert!(!outbox.is_bound());
    let conn = db.conn().expect("connection");

    // Bootstrap seeds before delivery starts and enqueues nothing.
    outbox
        .enqueue(&conn, &[])
        .await
        .expect("nothing to write needs no pipeline")
        .discard();
    let err = outbox
        .enqueue(&conn, &[quota_changed(tenant())])
        .await
        .expect_err("an event would be lost");
    assert!(matches!(err, EnqueueError::NotBound), "{err:?}");

    let (handle, bound) = bound_outbox(&db).await;
    assert!(bound.is_bound());
    assert_eq!(bound.bind(Arc::clone(handle.outbox())), Err(AlreadyBound));
    handle.stop().await;
}

#[tokio::test]
async fn events_are_enqueued_in_order_with_their_kind_as_the_payload_type() {
    let db = test_db().await;
    let (handle, outbox) = bound_outbox(&db).await;
    let first = quota_changed(tenant());
    let mut second = quota_changed(tenant());
    second.payload = serde_json::json!({ "change_kind": "updated" });
    {
        let conn = db.conn().expect("connection");
        let wake = outbox
            .enqueue(&conn, &[first.clone(), second.clone()])
            .await
            .expect("enqueued");
        assert_eq!(wake.ids().len(), 2);
        wake.discard();
    }

    let messages = enqueued_messages(&db).await;
    assert_eq!(messages.len(), 2);
    for (message, event) in messages.iter().zip([&first, &second]) {
        assert_eq!(message.payload_type, "quota-changed");
        let decoded: NotificationEvent =
            serde_json::from_slice(&message.payload).expect("event json");
        assert_eq!(&decoded, event);
    }
    handle.stop().await;
}

#[tokio::test]
async fn platform_event_has_no_tenant_and_round_trips_through_outbox() {
    let db = test_db().await;
    let (handle, outbox) = bound_outbox(&db).await;
    let mut event = quota_changed(tenant());
    event.kind = quota_enforcement_sdk::NotificationEventKind::PolicyChanged;
    event.scope = quota_enforcement_sdk::NotificationScope::Platform;
    event.policy_id = Some(quota_enforcement_sdk::PolicyId::global());
    let conn = db.conn().expect("connection");
    outbox
        .enqueue(&conn, &[event.clone()])
        .await
        .expect("enqueue")
        .discard();
    let messages = enqueued_messages(&db).await;
    assert_eq!(messages.len(), 1);
    let json: serde_json::Value = serde_json::from_slice(&messages[0].payload).expect("JSON");
    assert_eq!(json["scope"], "platform");
    assert!(json.get("tenant_id").is_none());
    assert_eq!(
        serde_json::from_value::<NotificationEvent>(json).expect("decode"),
        event
    );
    handle.stop().await;
}

// --- the pipeline ------------------------------------------------------------------

/// A delivery callback that answers from a script (then `Delivered`) and
/// records what it was handed.
#[derive(Default)]
struct RecordingDelivery {
    script: Mutex<VecDeque<DeliveryOutcome>>,
    seen: Mutex<Vec<(EventId, u16)>>,
    undeliverable: Mutex<Vec<(String, String)>>,
}

impl RecordingDelivery {
    fn answering(outcomes: Vec<DeliveryOutcome>) -> Arc<Self> {
        let delivery = Arc::new(Self::default());
        *delivery.script.lock().expect("script") = outcomes.into();
        delivery
    }

    fn seen(&self) -> Vec<(EventId, u16)> {
        self.seen.lock().expect("seen").clone()
    }
}

#[async_trait]
impl NotificationDeliveryV1 for RecordingDelivery {
    async fn deliver(
        &self,
        event: NotificationEvent,
        attempts: u16,
        budget: Duration,
    ) -> DeliveryOutcome {
        assert!(
            !budget.is_zero(),
            "the handler never delivers on a spent lease"
        );
        self.seen
            .lock()
            .expect("seen")
            .push((event.event_id, attempts));
        self.script
            .lock()
            .expect("script")
            .pop_front()
            .unwrap_or(DeliveryOutcome::Delivered)
    }

    fn undeliverable(&self, payload_type: &str, reason: &str) {
        self.undeliverable
            .lock()
            .expect("undeliverable")
            .push((payload_type.to_owned(), reason.to_owned()));
    }
}

/// Wait until `done` holds, or fail after `within`.
async fn eventually(within: Duration, mut done: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + within;
    while !done() {
        assert!(tokio::time::Instant::now() < deadline, "timed out waiting");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Long enough for the pipeline's start-up reconcile to finish, so a later
/// delivery within a few seconds can only have come from a fired wake: the
/// reconciler sleeps a minute when idle.
const SETTLE: Duration = Duration::from_millis(500);
const PROMPTLY: Duration = Duration::from_secs(5);

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_committed_mutation_reaches_the_delivery_callback_at_once() {
    let db = test_db().await;
    let outbox = Arc::new(QeOutbox::new());
    let delivery = RecordingDelivery::answering(Vec::new());
    let handle = start_notification_pipeline(db.clone(), &outbox, Arc::clone(&delivery) as _)
        .await
        .expect("start");
    assert!(outbox.is_bound());
    tokio::time::sleep(SETTLE).await;

    let quotas = SqlQuotaStore::new(db.clone(), Arc::clone(&outbox) as _);
    let event = quota_changed(tenant());
    quotas
        .create_quota(
            &actor(),
            &scope_for(tenant()),
            draft(tenant(), "u1", Some(10)),
            std::slice::from_ref(&event),
        )
        .await
        .expect("create quota");

    eventually(PROMPTLY, || {
        delivery.seen().iter().any(|(id, _)| *id == event.event_id)
    })
    .await;
    handle.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_handler_retries_the_head_dead_letters_rejections_and_reports_undecodable_rows() {
    let db = test_db().await;
    let outbox = Arc::new(QeOutbox::new());
    let delivery = RecordingDelivery::answering(vec![
        DeliveryOutcome::Retry,
        DeliveryOutcome::Delivered,
        DeliveryOutcome::Reject("sink refused".to_owned()),
    ]);
    let handle = start_notification_pipeline(db.clone(), &outbox, Arc::clone(&delivery) as _)
        .await
        .expect("start");
    tokio::time::sleep(SETTLE).await;
    let retried = quota_changed(tenant());
    let rejected = quota_changed(tenant());
    {
        let conn = db.conn().expect("connection");
        outbox
            .enqueue(&conn, &[retried.clone(), rejected.clone()])
            .await
            .expect("enqueue")
            .fire();
        // A row no event decodes from, on the same partition.
        let partition = QeOutbox::partition_for(tenant());
        handle
            .outbox()
            .enqueue_batch(
                &conn,
                Records::to(NOTIFICATION_QUEUE)
                    .payload_type("quota-changed")
                    .push(partition, b"not an event".to_vec())
                    .build()
                    .expect("record"),
            )
            .await
            .expect("enqueue garbage")
            .fire();
    }

    // The first delivery of `retried` asks for a retry; ToolKit backs off
    // about a second before the second.
    eventually(Duration::from_secs(15), || {
        delivery.undeliverable.lock().expect("undeliverable").len() == 1
    })
    .await;
    assert_eq!(
        delivery.seen(),
        vec![
            (retried.event_id, 0),
            (retried.event_id, 1),
            (rejected.event_id, 0)
        ],
        "the retried event is handed back with one failed attempt"
    );
    let conn = db.conn().expect("connection");
    let dead = handle
        .outbox()
        .dead_letter_list(
            &conn,
            &DeadLetterFilter::from_scope(DeadLetterScope::default().queue(NOTIFICATION_QUEUE)),
        )
        .await
        .expect("dead letters");
    let reasons: Vec<&str> = dead
        .iter()
        .filter_map(|letter| letter.last_error.as_deref())
        .collect();
    assert_eq!(
        dead.len(),
        2,
        "the rejected event and the undecodable row: {reasons:?}"
    );
    assert!(reasons.contains(&"sink refused"), "{reasons:?}");
    assert!(
        reasons
            .iter()
            .any(|reason| reason.starts_with("undecodable notification event")),
        "{reasons:?}"
    );
    handle.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_first_failure_behind_an_acknowledged_prefix_goes_uncounted() {
    let db = test_db().await;
    let outbox = Arc::new(QeOutbox::new());
    let delivery = RecordingDelivery::answering(vec![
        DeliveryOutcome::Delivered,
        DeliveryOutcome::Retry,
        DeliveryOutcome::Retry,
    ]);
    let handle = start_notification_pipeline(db.clone(), &outbox, Arc::clone(&delivery) as _)
        .await
        .expect("start");
    tokio::time::sleep(SETTLE).await;
    let delivered = quota_changed(tenant());
    let retried = quota_changed(tenant());
    {
        let conn = db.conn().expect("connection");
        outbox
            .enqueue(&conn, &[delivered.clone(), retried.clone()])
            .await
            .expect("enqueue")
            .fire();
    }

    // Two backoffs, about one and two seconds, before the fourth delivery.
    eventually(Duration::from_secs(15), || delivery.seen().len() == 4).await;
    assert_eq!(
        delivery.seen(),
        vec![
            (delivered.event_id, 0),
            (retried.event_id, 0),
            (retried.event_id, 0),
            (retried.event_id, 1)
        ],
        "advancing past the acknowledged event resets the count once"
    );
    handle.stop().await;
}

#[tokio::test]
async fn one_pipeline_per_outbox_handle() {
    let db = test_db().await;
    let outbox = QeOutbox::new();
    let delivery = RecordingDelivery::answering(Vec::new());
    let handle = start_notification_pipeline(db.clone(), &outbox, Arc::clone(&delivery) as _)
        .await
        .expect("start");
    let second = start_notification_pipeline(db.clone(), &outbox, delivery as _).await;
    assert!(
        matches!(second, Err(PipelineError::AlreadyBound(_))),
        "never a second pipeline"
    );
    handle.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cold_bootstrap_runs_unbound_and_delivery_starts_after_it() {
    let db = test_db().await;
    let outbox = Arc::new(QeOutbox::new());
    let plugin = crate::domain::StoragePlugin::new(
        Arc::new(crate::infra::storage::SqlFoundationStore::new(db.clone())),
        Arc::new(SqlQuotaStore::new(db.clone(), Arc::clone(&outbox) as _)),
        Arc::new(crate::infra::storage::SqlPolicyStore::new(
            db.clone(),
            Arc::clone(&outbox) as _,
        )),
        Arc::new(crate::infra::storage::SqlConsumptionStore::new(
            db.clone(),
            Arc::clone(&outbox) as _,
        )),
        Arc::new(crate::infra::storage::SqlConsumptionStore::new(
            db.clone(),
            Arc::clone(&outbox) as _,
        )),
    )
    .with_notifications(Arc::new(super::SqlNotificationPipeline::new(
        db.clone(),
        Arc::clone(&outbox),
    )));

    // A fresh database: bootstrap seeds the global policy with no event, and
    // the outbox is not bound yet.
    plugin
        .bootstrap(&quota_enforcement_sdk::testing::bundle_with_global_policy())
        .await
        .expect("a cold bootstrap needs no pipeline");
    let refused = plugin
        .create_quota(
            &ctx(),
            &scope_for(tenant()),
            draft(tenant(), "u1", Some(10)),
            &[quota_changed(tenant())],
        )
        .await;
    assert!(
        refused.is_err(),
        "an event before delivery starts would be lost"
    );

    let delivery = RecordingDelivery::answering(Vec::new());
    let handle = plugin
        .start_notification_delivery(Arc::clone(&delivery) as _)
        .await
        .expect("start delivery");
    let again = plugin
        .start_notification_delivery(Arc::clone(&delivery) as _)
        .await;
    assert!(
        matches!(again, Err(quota_enforcement_sdk::StorageError::Internal(_))),
        "the plugin never starts a second pipeline"
    );
    let event = quota_changed(tenant());
    plugin
        .create_quota(
            &ctx(),
            &scope_for(tenant()),
            draft(tenant(), "u1", Some(10)),
            std::slice::from_ref(&event),
        )
        .await
        .expect("create after delivery started");
    eventually(PROMPTLY, || {
        delivery.seen().iter().any(|(id, _)| *id == event.event_id)
    })
    .await;
    handle.stop().await;
}
