use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use modkit_db::migration_runner::run_migrations_for_testing;
use modkit_db::outbox::{
    LeasedMessageHandler, MessageResult, Outbox, OutboxHandle, OutboxMessage, Partitions,
    outbox_migrations,
};
use modkit_db::{ConnectOpts, Db, connect_db};
use usage_collector_sdk::models::{AllowedMetric, Subject, UsageKind, UsageRecord};
use uuid::Uuid;

use crate::config::UsageEmitterConfig;
use crate::domain::emitter::UsageEmitter;
use crate::domain::usage_record_builder::UsageRecordBuilder;
use crate::error::UsageEmitterError;

// ── Infrastructure ────────────────────────────────────────────────────────────

struct NoopHandler;

#[async_trait]
impl LeasedMessageHandler for NoopHandler {
    async fn handle(&self, _msg: &OutboxMessage) -> MessageResult {
        MessageResult::Ok
    }
}

async fn build_db(name: &str) -> Db {
    let url = format!("sqlite:file:{name}?mode=memory&cache=shared");
    let db = connect_db(
        &url,
        ConnectOpts {
            max_conns: Some(1),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    run_migrations_for_testing(&db, outbox_migrations())
        .await
        .unwrap();
    db
}

async fn build_outbox(db: Db) -> OutboxHandle {
    let cfg = UsageEmitterConfig::default();
    Outbox::builder(db)
        .queue(
            cfg.outbox_queue.as_str(),
            Partitions::of(cfg.outbox_partition_count),
        )
        .leased(NoopHandler)
        .start()
        .await
        .unwrap()
}

// ── Test fixture ──────────────────────────────────────────────────────────────

struct Fixture {
    db: Db,
    _handle: OutboxHandle,
    emitter: UsageEmitter,
    tenant_id: Uuid,
    resource_id: Uuid,
}

impl Fixture {
    async fn build(name: &str) -> Self {
        let db = build_db(name).await;
        let handle = build_outbox(db.clone()).await;
        let tenant_id = Uuid::new_v4();
        let resource_id = Uuid::new_v4();
        let allowed_metrics = vec![
            AllowedMetric {
                name: "test.gauge".to_owned(),
                kind: UsageKind::Gauge,
            },
            AllowedMetric {
                name: "test.counter".to_owned(),
                kind: UsageKind::Counter,
            },
        ];
        let emitter = UsageEmitter {
            config: Arc::new(UsageEmitterConfig::default()),
            db: db.clone(),
            outbox: Arc::clone(handle.outbox()),
            module: "test-module".to_owned(),
            tenant_id,
            resource_id,
            resource_type: "test.resource".to_owned(),
            allowed_metrics,
            max_metadata_bytes: 8192,
            subject: Some(Subject::with_type(Uuid::nil(), "test.subject")),
            issued_at: Instant::now(),
        };
        Self {
            db,
            _handle: handle,
            emitter,
            tenant_id,
            resource_id,
        }
    }

    fn conn(&self) -> modkit_db::DbConn<'_> {
        self.db.conn().unwrap()
    }
}

// ── Standalone builder (dumb) ─────────────────────────────────────────────────

fn full_builder() -> UsageRecordBuilder {
    UsageRecordBuilder::new()
        .with_module("test-module")
        .with_tenant_id(Uuid::new_v4())
        .with_metric("test.gauge", UsageKind::Gauge)
        .with_value(1.0)
        .with_resource(Uuid::new_v4(), "test.resource")
}

#[test]
fn build_succeeds_with_all_required_fields() {
    let record = full_builder().build().expect("required fields populated");
    assert_eq!(record.module, "test-module");
    assert_eq!(record.metric, "test.gauge");
    assert_eq!(record.kind, UsageKind::Gauge);
    assert!((record.value - 1.0).abs() < f64::EPSILON);
    assert!(record.idempotency_key.is_empty());
    assert!(record.subject.is_none());
}

#[test]
fn build_defaults_timestamp_to_now() {
    let before = Utc::now();
    let record = full_builder().build().unwrap();
    let after = Utc::now();
    assert!(record.timestamp >= before && record.timestamp <= after);
}

#[test]
fn build_uses_provided_timestamp() {
    let ts = DateTime::parse_from_rfc3339("2025-01-01T00:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let record = full_builder().with_timestamp(ts).build().unwrap();
    assert_eq!(record.timestamp, ts);
}

#[test]
fn build_carries_subject_for_all_shapes() {
    let id = Uuid::new_v4();
    let cases: Vec<(&str, Option<Subject>)> = vec![
        ("none", None),
        ("id only", Some(Subject::new(id))),
        ("id + type", Some(Subject::with_type(id, "test.subject"))),
    ];
    for (label, subject) in cases {
        let mut builder = full_builder();
        if let Some(s) = subject.clone() {
            builder = builder.with_subject(s);
        }
        let record = builder.build().unwrap();
        assert_eq!(
            record.subject, subject,
            "case `{label}` must carry the expected subject"
        );
    }
}

// `with_metric` is the only non-trivial overwrite — it sets two correlated
// fields (`metric` *and* `kind`) and the second call must overwrite both.
// Single-field setters reduce to `Option::Some(_) = …` and are compiler-guaranteed.
#[test]
fn build_with_metric_called_twice_overwrites_both_name_and_kind() {
    let record = full_builder()
        .with_metric("test.gauge", UsageKind::Gauge)
        .with_metric("test.counter", UsageKind::Counter)
        .with_idempotency_key("idem")
        .build()
        .expect("required fields populated");
    assert_eq!(record.metric, "test.counter");
    assert_eq!(record.kind, UsageKind::Counter);
}

#[test]
fn build_rejects_missing_required_fields_and_lists_them() {
    let err = UsageRecordBuilder::new().build().unwrap_err();
    let UsageEmitterError::InvalidArgument { detail, .. } = &err else {
        panic!("expected InvalidArgument, got {err:?}");
    };
    for field in [
        "module",
        "tenant_id",
        "metric",
        "value",
        "resource_id",
        "resource_type",
    ] {
        assert!(
            detail.contains(field),
            "missing-field error must mention `{field}`; got `{detail}`"
        );
    }
}

#[test]
fn build_rejects_only_partial_missing_fields() {
    let err = UsageRecordBuilder::new()
        .with_module("m")
        .with_tenant_id(Uuid::new_v4())
        .with_metric("test.gauge", UsageKind::Gauge)
        .with_value(1.0)
        // resource omitted
        .build()
        .unwrap_err();
    let UsageEmitterError::InvalidArgument { detail, .. } = &err else {
        panic!("expected InvalidArgument, got {err:?}");
    };
    assert!(detail.contains("resource_id"));
    assert!(detail.contains("resource_type"));
    assert!(
        !detail.contains("module"),
        "fields already set must not appear: {detail}"
    );
}

// ── usage_record_builder() prefill from UsageEmitter ──────────────────────────

#[tokio::test]
async fn usage_record_builder_prefills_authorized_fields_for_gauge() {
    let f = Fixture::build("urb_prefill_gauge").await;
    let record = f
        .emitter
        .usage_record_builder("test.gauge", 7.0)
        .expect("metric is allowed")
        .build()
        .expect("required fields prefilled");
    assert_eq!(record.module, "test-module");
    assert_eq!(record.tenant_id, f.tenant_id);
    assert_eq!(record.resource_id, f.resource_id);
    assert_eq!(record.resource_type, "test.resource");
    assert_eq!(record.metric, "test.gauge");
    assert_eq!(record.kind, UsageKind::Gauge);
    assert!((record.value - 7.0).abs() < f64::EPSILON);
    assert_eq!(
        record.subject,
        Some(Subject::with_type(Uuid::nil(), "test.subject"))
    );
}

#[tokio::test]
async fn usage_record_builder_resolves_counter_kind_from_allowed_metrics() {
    let f = Fixture::build("urb_prefill_counter").await;
    let record = f
        .emitter
        .usage_record_builder("test.counter", 1.0)
        .unwrap()
        .with_idempotency_key("idem-key")
        .build()
        .unwrap();
    assert_eq!(record.kind, UsageKind::Counter);
    assert_eq!(record.idempotency_key, "idem-key");
}

#[tokio::test]
async fn usage_record_builder_rejects_unknown_metric() {
    let f = Fixture::build("urb_unknown_metric").await;
    let err = f
        .emitter
        .usage_record_builder("unknown.metric", 1.0)
        .unwrap_err();
    assert!(
        matches!(err, UsageEmitterError::PermissionDenied { .. }),
        "expected PermissionDenied for unknown metric, got {err:?}"
    );
}

#[tokio::test]
async fn usage_record_builder_omits_subject_when_emitter_has_none() {
    let db = build_db("urb_no_subject").await;
    let handle = build_outbox(db.clone()).await;
    let tenant_id = Uuid::new_v4();
    let resource_id = Uuid::new_v4();
    let emitter = UsageEmitter {
        config: Arc::new(UsageEmitterConfig::default()),
        db: db.clone(),
        outbox: Arc::clone(handle.outbox()),
        module: "test-module".to_owned(),
        tenant_id,
        resource_id,
        resource_type: "test.resource".to_owned(),
        allowed_metrics: vec![AllowedMetric {
            name: "test.gauge".to_owned(),
            kind: UsageKind::Gauge,
        }],
        max_metadata_bytes: 8192,
        subject: None,
        issued_at: Instant::now(),
    };
    let record = emitter
        .usage_record_builder("test.gauge", 1.0)
        .unwrap()
        .build()
        .unwrap();
    assert!(record.subject.is_none());
}

// ── End-to-end: builder → emitter.enqueue_in ─────────────────────────────────

#[tokio::test]
async fn built_gauge_record_enqueues_successfully() {
    let f = Fixture::build("urb_e2e_gauge").await;
    let record = f
        .emitter
        .usage_record_builder("test.gauge", 42.0)
        .unwrap()
        .build()
        .unwrap();
    f.emitter.enqueue_in(&f.conn(), record).await.unwrap();
}

#[tokio::test]
async fn built_counter_record_enqueues_with_idempotency_key() {
    let f = Fixture::build("urb_e2e_counter").await;
    let record = f
        .emitter
        .usage_record_builder("test.counter", 1.0)
        .unwrap()
        .with_idempotency_key("idem-key")
        .build()
        .unwrap();
    f.emitter.enqueue_in(&f.conn(), record).await.unwrap();
}

#[tokio::test]
async fn built_counter_record_without_idempotency_key_is_rejected_at_enqueue() {
    let f = Fixture::build("urb_e2e_counter_no_idem").await;
    let record = f
        .emitter
        .usage_record_builder("test.counter", 1.0)
        .unwrap()
        .build()
        .unwrap();
    let err = f.emitter.enqueue_in(&f.conn(), record).await.unwrap_err();
    assert!(matches!(err, UsageEmitterError::InvalidArgument { .. }));
}

#[tokio::test]
async fn built_counter_record_with_negative_value_is_rejected_at_enqueue() {
    let f = Fixture::build("urb_e2e_counter_negative").await;
    let record = f
        .emitter
        .usage_record_builder("test.counter", -1.0)
        .unwrap()
        .with_idempotency_key("idem-key")
        .build()
        .unwrap();
    let err = f.emitter.enqueue_in(&f.conn(), record).await.unwrap_err();
    assert!(matches!(err, UsageEmitterError::InvalidArgument { .. }));
}

#[tokio::test]
async fn built_gauge_record_with_negative_value_is_accepted() {
    let f = Fixture::build("urb_e2e_gauge_negative").await;
    let record = f
        .emitter
        .usage_record_builder("test.gauge", -5.0)
        .unwrap()
        .build()
        .unwrap();
    f.emitter.enqueue_in(&f.conn(), record).await.unwrap();
}

// ── Gauge UUID fallback at enqueue time ──────────────────────────────────────

#[tokio::test]
async fn gauge_with_blank_idempotency_key_gets_uuid_at_enqueue() {
    use std::sync::Mutex;

    struct CaptureHandler {
        captured: Arc<Mutex<Option<Vec<u8>>>>,
    }

    #[async_trait]
    impl LeasedMessageHandler for CaptureHandler {
        async fn handle(&self, msg: &OutboxMessage) -> MessageResult {
            *self.captured.lock().unwrap() = Some(msg.payload.clone());
            MessageResult::Ok
        }
    }

    let name = "urb_gauge_uuid_fallback";
    let url = format!("sqlite:file:{name}?mode=memory&cache=shared");
    let db = connect_db(
        &url,
        ConnectOpts {
            max_conns: Some(1),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    run_migrations_for_testing(&db, outbox_migrations())
        .await
        .unwrap();

    let captured: Arc<Mutex<Option<Vec<u8>>>> = Arc::new(Mutex::new(None));
    let cfg = UsageEmitterConfig::default();
    let handle = Outbox::builder(db.clone())
        .queue(
            cfg.outbox_queue.as_str(),
            Partitions::of(cfg.outbox_partition_count),
        )
        .leased(CaptureHandler {
            captured: Arc::clone(&captured),
        })
        .start()
        .await
        .unwrap();

    let tenant_id = Uuid::new_v4();
    let resource_id = Uuid::new_v4();
    let emitter = UsageEmitter {
        config: Arc::new(cfg),
        db: db.clone(),
        outbox: Arc::clone(handle.outbox()),
        module: "test-module".to_owned(),
        tenant_id,
        resource_id,
        resource_type: "test.resource".to_owned(),
        allowed_metrics: vec![AllowedMetric {
            name: "test.gauge".to_owned(),
            kind: UsageKind::Gauge,
        }],
        max_metadata_bytes: 8192,
        subject: Some(Subject::with_type(Uuid::nil(), "test.subject")),
        issued_at: Instant::now(),
    };

    {
        let record = emitter
            .usage_record_builder("test.gauge", 1.0)
            .unwrap()
            .with_idempotency_key("")
            .build()
            .unwrap();
        let conn = db.conn().unwrap();
        emitter.enqueue_in(&conn, record).await.unwrap();
    }

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if captured.lock().unwrap().is_some() {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for outbox delivery"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    let payload = captured.lock().unwrap().take().unwrap();
    let record: UsageRecord = serde_json::from_slice(&payload).unwrap();
    assert!(
        !record.idempotency_key.is_empty(),
        "gauge records with blank caller key must receive a generated UUID, got empty string"
    );
    assert!(
        Uuid::parse_str(&record.idempotency_key).is_ok(),
        "gauge idempotency_key fallback must be a valid UUID, got {:?}",
        record.idempotency_key
    );
    drop(handle);
}
