//! Crate-internal `UsageEmitter` tests.
//!
//! The full enqueue-validation matrix (mismatched tenant/resource/module/subject,
//! metric rules, expired auth, ...) lives in `tests/emitter_tests.rs`, which
//! exercises the same checks through the real factory → authorize → enqueue
//! path. This file is restricted to cases that genuinely need crate-internal
//! access — primarily varying `UsageEmitter.max_metadata_bytes` (a
//! `pub(crate)` field) without rebuilding a runtime around a custom collector.

use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use chrono::Utc;
use modkit_db::migration_runner::run_migrations_for_testing;
use modkit_db::outbox::{
    LeasedMessageHandler, MessageResult, Outbox, OutboxHandle, OutboxMessage, Partitions,
    outbox_migrations,
};
use modkit_db::{ConnectOpts, Db, connect_db};
use usage_collector_sdk::models::{AllowedMetric, Subject, UsageKind, UsageRecord};
use uuid::Uuid;

use super::UsageEmitter;
use crate::config::UsageEmitterConfig;
use crate::error::UsageEmitterError;

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

const FIXTURE_RESOURCE_TYPE: &str = "test.resource";

struct Fixture {
    db: Db,
    _handle: OutboxHandle,
    emitter: UsageEmitter,
    tenant: Uuid,
    resource_id: Uuid,
}

impl Fixture {
    async fn build_with_max_metadata_bytes(name: &str, max_metadata_bytes: u32) -> Self {
        let db = build_db(name).await;
        let handle = build_outbox(db.clone()).await;
        let outbox = Arc::clone(handle.outbox());
        let tenant = Uuid::new_v4();
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
            outbox,
            module: "test-module".to_owned(),
            tenant_id: tenant,
            resource_id,
            resource_type: FIXTURE_RESOURCE_TYPE.to_owned(),
            allowed_metrics,
            max_metadata_bytes,
            subject: Some(Subject::with_type(Uuid::nil(), "test.subject")),
            issued_at: Instant::now(),
        };
        Self {
            db,
            _handle: handle,
            emitter,
            tenant,
            resource_id,
        }
    }

    fn record(&self) -> UsageRecord {
        UsageRecord {
            tenant_id: self.tenant,
            module: "test-module".to_owned(),
            metric: "test.gauge".to_owned(),
            kind: UsageKind::Gauge,
            value: 1.0,
            resource_id: self.resource_id,
            resource_type: FIXTURE_RESOURCE_TYPE.to_owned(),
            subject: Some(Subject::with_type(Uuid::nil(), "test.subject")),
            idempotency_key: Uuid::new_v4().to_string(),
            timestamp: Utc::now(),
            metadata: None,
        }
    }

    fn conn(&self) -> modkit_db::DbConn<'_> {
        self.db.conn().unwrap()
    }
}

#[tokio::test]
async fn enqueue_rejects_metadata_above_configured_limit_below_default() {
    // 2 KiB payload against a 1024-byte limit: must be rejected with the
    // configured limit reflected in the error string.
    let f = Fixture::build_with_max_metadata_bytes("ap_metadata_limit_1024", 1024).await;
    let record = UsageRecord {
        metadata: Some(serde_json::Value::String("x".repeat(2048))),
        ..f.record()
    };
    let err = f.emitter.enqueue_in(&f.conn(), record).await.unwrap_err();
    let UsageEmitterError::InvalidArgument { detail, .. } = &err else {
        panic!("expected InvalidArgument, got {err:?}");
    };
    assert!(
        detail.contains("exceeds the 1024-byte limit"),
        "error must reference the configured limit (1024); got `{detail}`"
    );
}

#[tokio::test]
async fn enqueue_accepts_metadata_below_configured_limit_above_default() {
    // 9 KiB payload would be rejected at the old hardcoded 8192 limit but
    // must be accepted when the configured limit is 16 384.
    let f = Fixture::build_with_max_metadata_bytes("ap_metadata_limit_16k", 16_384).await;
    let record = UsageRecord {
        metadata: Some(serde_json::Value::String("x".repeat(9 * 1024))),
        ..f.record()
    };
    f.emitter.enqueue_in(&f.conn(), record).await.unwrap();
}

#[tokio::test]
async fn enqueue_rejects_any_non_none_metadata_when_limit_is_zero() {
    // `max_metadata_bytes == 0` means metadata is disabled. Even
    // `Value::Null` (which serializes as `b"null"`, four bytes) exceeds 0
    // and must be rejected with `exceeds the 0-byte limit`.
    let f = Fixture::build_with_max_metadata_bytes("ap_metadata_limit_zero_null", 0).await;
    let record = UsageRecord {
        metadata: Some(serde_json::Value::Null),
        ..f.record()
    };
    let err = f.emitter.enqueue_in(&f.conn(), record).await.unwrap_err();
    let UsageEmitterError::InvalidArgument { detail, .. } = &err else {
        panic!("expected InvalidArgument, got {err:?}");
    };
    assert!(
        detail.contains("exceeds the 0-byte limit"),
        "error must reference the configured limit (0); got `{detail}`"
    );
}

#[tokio::test]
async fn enqueue_accepts_record_without_metadata_when_limit_is_zero() {
    // `metadata: None` carries no payload, so `max_metadata_bytes == 0`
    // must still accept the record — the disabled-metadata config does not
    // bar records that simply omit metadata.
    let f = Fixture::build_with_max_metadata_bytes("ap_metadata_limit_zero_none", 0).await;
    let record = UsageRecord {
        metadata: None,
        ..f.record()
    };
    f.emitter.enqueue_in(&f.conn(), record).await.unwrap();
}
