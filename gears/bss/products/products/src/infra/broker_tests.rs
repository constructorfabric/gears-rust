#![allow(clippy::expect_used, clippy::unwrap_used)]
use super::*;
use crate::infra::events::{self, enqueue_typed};
use crate::test_support::{at, enqueued_event_count, enqueued_event_envelope, test_db};
use event_broker_sdk::TypedEvent;
use serde::{Deserialize, Serialize};
use toolkit_db::secure::TxConfig;
use toolkit_db::{Db, DbError};

#[test]
fn five_event_contracts_have_stable_type_subject_source_and_tenant() {
    fn check<E: TypedEvent>(
        event: &E,
        token: &str,
        subject_type: &str,
        subject: Uuid,
        tenant: Uuid,
    ) {
        assert_eq!(
            E::TYPE_ID,
            format!("gts.cf.core.events.event.v1~cf.bss.products.{token}.v1~")
        );
        assert_eq!(E::SUBJECT_TYPE, subject_type);
        assert_eq!(E::SOURCE, "bss-products");
        assert_eq!(event.subject(), subject.to_string());
        assert_eq!(event.tenant_id(), Some(tenant));
        assert!(gts::GtsId::try_new(E::TYPE_ID).is_ok());
    }
    let tenant = Uuid::new_v4();
    let sku = Uuid::new_v4();
    let unit = Uuid::new_v4();
    let actor = Uuid::new_v4();
    check(
        &SkuPublished {
            tenant_id: tenant,
            sku_id: sku,
            published_version: 1,
            actor_ref: actor,
        },
        "sku_published",
        SKU_SUBJECT_TYPE,
        sku,
        tenant,
    );
    let changed = SkuChanged {
        tenant_id: tenant,
        sku_id: sku,
        changed: vec!["gl_code".into()],
        effective_from: at(9).date(),
        published_version: 2,
        actor_ref: actor,
    };
    check(&changed, "sku_changed", SKU_SUBJECT_TYPE, sku, tenant);
    let json = serde_json::to_value(&changed).unwrap();
    assert_eq!(json["effectiveFrom"], "2026-09-02");
    assert_eq!(json["skuId"], sku.to_string());
    assert_eq!(json["changed"], serde_json::json!(["gl_code"]));
    assert!(json.get("effective_from").is_none());
    assert_eq!(serde_json::from_value::<SkuChanged>(json).unwrap(), changed);
    check(
        &SkuRetired {
            tenant_id: tenant,
            sku_id: sku,
            actor_ref: actor,
        },
        "sku_retired",
        SKU_SUBJECT_TYPE,
        sku,
        tenant,
    );
    let decided = ApprovalUnitDecided {
        tenant_id: tenant,
        unit_id: unit,
        kind: "sku_publish".into(),
        state: "approved".into(),
        generation: 2,
        actors: vec![actor],
    };
    check(
        &decided,
        "approval_unit_decided",
        APPROVAL_UNIT_SUBJECT_TYPE,
        unit,
        tenant,
    );
    assert_eq!(serde_json::to_value(&decided).unwrap()["generation"], 2);
    let released = ReferenceForceReleased {
        tenant_id: tenant,
        sku_id: sku,
        reference_id: Uuid::new_v4(),
        owner: "pricing".into(),
        kind: "price_book_entry".into(),
        ref_id: Uuid::new_v4(),
        actor_ref: actor,
        reason: "abandoned reservation".into(),
    };
    check(
        &released,
        "reference_force_released",
        SKU_SUBJECT_TYPE,
        sku,
        tenant,
    );
    assert_eq!(
        serde_json::from_value::<ReferenceForceReleased>(serde_json::to_value(&released).unwrap())
            .unwrap(),
        released
    );
}

#[test]
fn optional_dates_and_timestamps_have_literal_wire_strings_and_reject_bad_dates() {
    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Dates {
        #[serde(with = "crate::infra::serde_date::option")]
        date: Option<time::Date>,
        #[serde(with = "time::serde::rfc3339")]
        at: time::OffsetDateTime,
    }
    let dates = Dates {
        date: Some(at(9).date()),
        at: at(9),
    };
    let json = serde_json::to_value(&dates).unwrap();
    assert_eq!(
        json,
        serde_json::json!({"date":"2026-09-02","at":"2026-09-02T09:00:00Z"})
    );
    assert_eq!(serde_json::from_value::<Dates>(json).unwrap(), dates);
    let none = Dates {
        date: None,
        at: at(9),
    };
    let json = serde_json::to_value(&none).unwrap();
    assert!(json["date"].is_null());
    assert_eq!(serde_json::from_value::<Dates>(json).unwrap(), none);
    assert!(
        serde_json::from_value::<Dates>(
            serde_json::json!({"date":"2026-02-30","at":"2026-09-02T09:00:00Z"})
        )
        .is_err()
    );
}

#[derive(Debug, thiserror::Error)]
enum TxError {
    #[error(transparent)]
    Db(#[from] DbError),
    #[error(transparent)]
    Event(#[from] events::EventsError),
    #[error("rollback probe")]
    Rollback,
}
async fn persisted<E: TypedEvent + Clone + PartialEq + std::fmt::Debug>(
    db: &Db,
    sink: &EventSink,
    dsn: &str,
    event: E,
) {
    let expected = event.clone();
    events::transaction::<(), TxError, _, _>(
        db,
        sink,
        TxConfig::default(),
        |_| None,
        move |tx, outbox| {
            let event = event.clone();
            Box::pin(async move {
                enqueue_typed(&outbox, tx, event).await?;
                Ok(())
            })
        },
    )
    .await
    .unwrap();
    let raw = crate::test_support::raw_string_opt(
        dsn,
        "SELECT CAST(payload AS TEXT) AS v FROM bss_products_outbox_body ORDER BY id DESC LIMIT 1",
    )
    .await
    .unwrap();
    let envelope: event_broker_sdk::producer::ProducerOutboxEnvelope =
        serde_json::from_str(&raw).expect("interim backlog must decode with the SDK decoder");
    let wire = serde_json::to_value(envelope).unwrap();
    assert_eq!(wire["version"], 1);
    assert_eq!(wire["type"], E::TYPE_ID);
    assert_eq!(wire["topic"], TOPIC);
    assert_eq!(wire["source"], E::SOURCE);
    assert_eq!(wire["subject"], expected.subject().as_ref());
    assert_eq!(wire["tenant_id"], expected.tenant_id().unwrap().to_string());
    assert_eq!(wire["producer_mode"], "stateless");
    assert_eq!(
        serde_json::from_value::<E>(wire["data"].clone()).unwrap(),
        expected
    );
    assert_eq!(enqueued_event_count(dsn, E::TYPE_ID).await, 1);
    assert_eq!(
        serde_json::from_value::<E>(enqueued_event_envelope(dsn, E::TYPE_ID).await).unwrap(),
        expected
    );
}
#[tokio::test]
async fn every_event_round_trips_through_the_interim_outbox_and_rollback_leaves_none() {
    let (db, _, tenant, dsn) = test_db().await;
    let handle = toolkit_db::outbox::Outbox::builder(db.db())
        .table_prefix(events::OUTBOX_TABLE_PREFIX)
        .unwrap()
        .queue(
            events::QUEUE_NAME,
            toolkit_db::outbox::Partitions::of(events::PARTITIONS),
        )
        .leased(events::PendingBrokerProducer)
        .start()
        .await
        .unwrap();
    let sink = EventSink::Interim(Arc::clone(handle.outbox()));
    let sku = Uuid::new_v4();
    let actor = Uuid::new_v4();
    let changed = SkuChanged {
        tenant_id: tenant,
        sku_id: sku,
        changed: vec!["gl_code".into()],
        effective_from: at(9).date(),
        published_version: 2,
        actor_ref: actor,
    };
    let failed_event = changed.clone();
    let result = events::transaction::<(), TxError, _, _>(
        &db.db(),
        &sink,
        TxConfig::default(),
        |_| None,
        move |tx, outbox| {
            let event = failed_event.clone();
            Box::pin(async move {
                enqueue_typed(&outbox, tx, event).await?;
                Err(TxError::Rollback)
            })
        },
    )
    .await;
    assert!(matches!(result, Err(TxError::Rollback)));
    assert_eq!(enqueued_event_count(&dsn, SkuChanged::TYPE_ID).await, 0);
    persisted(
        &db.db(),
        &sink,
        &dsn,
        SkuPublished {
            tenant_id: tenant,
            sku_id: sku,
            published_version: 1,
            actor_ref: actor,
        },
    )
    .await;
    persisted(&db.db(), &sink, &dsn, changed).await;
    persisted(
        &db.db(),
        &sink,
        &dsn,
        SkuRetired {
            tenant_id: tenant,
            sku_id: sku,
            actor_ref: actor,
        },
    )
    .await;
    persisted(
        &db.db(),
        &sink,
        &dsn,
        ApprovalUnitDecided {
            tenant_id: tenant,
            unit_id: Uuid::new_v4(),
            kind: "sku_publish".into(),
            state: "approved".into(),
            generation: 1,
            actors: vec![actor],
        },
    )
    .await;
    persisted(
        &db.db(),
        &sink,
        &dsn,
        ReferenceForceReleased {
            tenant_id: tenant,
            sku_id: sku,
            reference_id: Uuid::new_v4(),
            owner: "pricing".into(),
            kind: "price_book_entry".into(),
            ref_id: Uuid::new_v4(),
            actor_ref: actor,
            reason: "abandoned reservation".into(),
        },
    )
    .await;
    assert_eq!(
        enqueued_event_envelope(&dsn, SkuChanged::TYPE_ID).await["effectiveFrom"],
        "2026-09-02"
    );
    let partition = u32::from(
        u16::from_le_bytes([tenant.as_bytes()[14], tenant.as_bytes()[15]]) % events::PARTITIONS,
    );
    assert_eq!(crate::test_support::raw_i64(&dsn,&format!("SELECT COUNT(*) AS v FROM (SELECT body_id, partition_id FROM bss_products_outbox_incoming UNION SELECT body_id, partition_id FROM bss_products_outbox_outgoing) b JOIN bss_products_outbox_partitions p ON p.id=b.partition_id WHERE p.partition={partition} AND p.queue='bss_products_events'")).await,5);
    handle.stop().await;
}

#[tokio::test]
async fn interim_outbox_retains_driver_error() {
    use std::error::Error;
    let (db, _, tenant, dsn) = test_db().await;
    let handle = toolkit_db::outbox::Outbox::builder(db.db())
        .table_prefix(events::OUTBOX_TABLE_PREFIX)
        .unwrap()
        .queue(
            events::QUEUE_NAME,
            toolkit_db::outbox::Partitions::of(events::PARTITIONS),
        )
        .leased(events::PendingBrokerProducer)
        .start()
        .await
        .unwrap();
    crate::test_support::drop_table(&dsn, "bss_products_outbox_body").await;
    let error = enqueue_typed(
        &events::TxOutbox::new(EventSink::Interim(Arc::clone(handle.outbox()))),
        &db.conn().unwrap(),
        SkuPublished {
            tenant_id: tenant,
            sku_id: Uuid::new_v4(),
            published_version: 1,
            actor_ref: tenant,
        },
    )
    .await
    .unwrap_err();
    assert!(
        error
            .source()
            .and_then(|e| e.downcast_ref::<sea_orm::DbErr>())
            .is_some(),
        "{error}"
    );
    handle.stop().await;
}

/// RT-15: an outbox enqueue's database error reaches the retry classifier typed, the interim
/// outbox's and the SDK producer's alike; one copy of the check, beside the SDK producer's.
#[test]
fn an_interim_outbox_database_error_reaches_the_retry_classifier() {
    let approval = bss_approval::ApprovalError::from(events::EventsError::from(
        toolkit_db::outbox::OutboxError::Database(sea_orm::DbErr::Custom("retry probe".into())),
    ));
    let tx_error = crate::api::rest::TxError::from(approval);
    assert!(crate::api::rest::contention_db_err(&tx_error).is_some());
}
#[test]
fn sdk_producer_errors_preserve_any_exposed_database_cause() {
    let sdk_error = event_broker_sdk::EventBrokerError::OffsetManager(
        event_broker_sdk::error::OffsetManagerError::persist_failed(
            "retry probe",
            "",
            sea_orm::DbErr::Custom("driver cause".into()),
        ),
    );
    let error = crate::api::rest::TxError::from(events::EventsError::from(sdk_error));
    assert!(crate::api::rest::contention_db_err(&error).is_some());
    let opaque = events::EventsError::from(event_broker_sdk::EventBrokerError::Internal(
        "producer outbox enqueue: opaque upstream error".into(),
    ));
    assert!(matches!(opaque, events::EventsError::Producer(_)));
}

/// RS-24 (P-D-226): the SDK's `SkuChangedPayload` reads the `SkuChanged` event the broker emits
/// (camelCase, the date `YYYY-MM-DD`, the actor) and writes it back unchanged.
#[test]
fn the_sdk_payload_reads_the_emitted_sku_changed_event() {
    let changed = SkuChanged {
        tenant_id: Uuid::new_v4(),
        sku_id: Uuid::new_v4(),
        changed: vec!["gl_code".into(), "name".into()],
        effective_from: at(9).date(),
        published_version: 3,
        actor_ref: Uuid::new_v4(),
    };
    let emitted = serde_json::to_value(&changed).unwrap();
    let payload: bss_products_sdk::models::SkuChangedPayload =
        serde_json::from_value(emitted.clone()).unwrap();
    assert_eq!(payload.sku_id, changed.sku_id);
    assert_eq!(payload.tenant_id, changed.tenant_id);
    assert_eq!(payload.changed, changed.changed);
    assert_eq!(payload.effective_from, changed.effective_from);
    assert_eq!(payload.published_version, changed.published_version);
    assert_eq!(payload.actor_ref, changed.actor_ref);
    assert_eq!(serde_json::to_value(&payload).unwrap(), emitted);
}
