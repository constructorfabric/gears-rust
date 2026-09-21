//! Deletion and dry-run checks on `PostgreSQL` and `MySQL` (T20).
//! Cover dependant rechecks under the write-order claim, unchanged dry-run entity
//! state and sequence, and publication accepted by `ck_tr_operation_item_state`.

#![cfg(feature = "integration")]
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::doc_markdown)]

mod common;

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use testcontainers::ImageExt;
use testcontainers::runners::AsyncRunner;
use time::OffsetDateTime;
use time::macros::datetime;
use toolkit_db::{DBProvider, DbError, DbTx};
use toolkit_gts::gts_id;
use uuid::Uuid;

use common::{allow_all, provider_for, stores};
use types_registry::config::TypesRegistryConfig;
use types_registry::domain::admission::acceptance::{AcceptanceContext, AcceptanceError, accept};
use types_registry::domain::admission::worker::{ItemOutcome, Tuning, WorkerError, run_operation};
use types_registry::domain::admission::{
    AdmissionFailureReason, Candidate, OperationDispatch, SubmitRequest,
};
use types_registry::domain::enums::{LifecycleStatus, OperationItemStatus, OperationKind};
use types_registry::domain::policy::RegistrationPolicy;
use types_registry::infra::storage::repo::{CoordinationStateRepo, EntityRepo};

const NOW: OffsetDateTime = datetime!(2026-09-11 09:15:30 UTC);
const LATER: OffsetDateTime = datetime!(2026-09-11 10:20:40 UTC);
const DRAFT_07: &str = "http://json-schema.org/draft-07/schema#";

const TARGET: &str = gts_id!("cf.core.delback.target.v1~");
const HOLDER: &str = gts_id!("cf.core.delback.holder.v1~");
const DRY: &str = gts_id!("cf.core.delback.dry.v1~");
const FRESH: &str = gts_id!("cf.core.delback.fresh.v1~");
const BATCH_BASE: &str = gts_id!("cf.core.delback.batchbase.v1~");
const BATCH_HOLDER: &str = gts_id!("cf.core.delback.batchholder.v1~");

struct NoDispatch;

#[async_trait::async_trait]
impl OperationDispatch for NoDispatch {
    async fn enqueue(&self, _tx: &DbTx<'_>, _operation_id: Uuid) -> anyhow::Result<()> {
        Ok(())
    }
}

fn schema(gts_id: &str) -> Value {
    json!({
        "$id": format!("gts://{gts_id}"),
        "$schema": DRAFT_07,
        "type": "object",
        "properties": { "name": { "type": "string" } },
    })
}

fn referencing(gts_id: &str, target: &str) -> Value {
    let mut doc = schema(gts_id);
    doc["properties"] = json!({ "target": { "$ref": format!("gts://{target}") } });
    doc
}

async fn pass(
    db: &Arc<DBProvider<DbError>>,
    key: &str,
    kind: OperationKind,
    dry_run: bool,
    candidate: Candidate,
) -> ItemOutcome {
    let config = TypesRegistryConfig::default();
    let provider = DBProvider::<AcceptanceError>::new(db.db());
    let operation_id = accept(
        &stores(),
        &provider,
        &allow_all(),
        &AcceptanceContext {
            policy: &RegistrationPolicy::default(),
            config: &config,
            metrics: &common::metrics(),
        },
        &(Arc::new(NoDispatch) as Arc<dyn OperationDispatch>),
        &SubmitRequest {
            idempotency_key: key.to_owned(),
            kind,
            dry_run,
            candidates: vec![candidate],
        },
        NOW,
    )
    .await
    .expect("the request reaches the worker")
    .operation_id;

    run_operation(
        &stores(),
        &DBProvider::<WorkerError>::new(db.db()),
        &allow_all(),
        Tuning {
            limits: &common::limits(),
            worker: &common::worker_settings(),
            metrics: &common::metrics(),
            allow_compatibility_force: false,
        },
        operation_id,
        LATER,
    )
    .await
    .expect("the admission pass completes")
    .items
    .remove(0)
}

fn creation(gts_id: &str, content: Value) -> Candidate {
    Candidate {
        gts_id: gts_id.to_owned(),
        content: Some(content),
        expected_resource_version: None,
        force: false,
    }
}

fn removal(gts_id: &str, expected: i64) -> Candidate {
    Candidate {
        gts_id: gts_id.to_owned(),
        content: None,
        expected_resource_version: Some(expected),
        force: false,
    }
}

async fn lifecycle_of(db: &Arc<DBProvider<DbError>>, gts_id: &str) -> Option<LifecycleStatus> {
    let provider = DBProvider::<WorkerError>::new(db.db());
    let conn = provider.conn().expect("conn");
    EntityRepo::find_by_gts_id(&conn, &allow_all(), gts_id)
        .await
        .expect("read")
        .map(|row| row.lifecycle_status)
}

fn assert_succeeded(item: &ItemOutcome, backend: &str) {
    assert_eq!(
        item.status,
        OperationItemStatus::Succeeded,
        "on {backend}: {:?}",
        item.failure,
    );
}

async fn assert_deletion(db: &Arc<DBProvider<DbError>>, backend: &str) {
    assert_succeeded(
        &pass(
            db,
            "seed",
            OperationKind::Registration,
            false,
            creation(TARGET, schema(TARGET)),
        )
        .await,
        backend,
    );
    assert_succeeded(
        &pass(
            db,
            "holder",
            OperationKind::Registration,
            false,
            creation(HOLDER, referencing(HOLDER, TARGET)),
        )
        .await,
        backend,
    );

    // The dependants recheck runs under the write-order claim, inside the
    // commit transaction each engine opens differently.
    let blocked = pass(
        db,
        "del-blocked",
        OperationKind::Deletion,
        false,
        removal(TARGET, 1),
    )
    .await;
    assert_eq!(
        (blocked.status, blocked.failure.as_ref().map(|f| &f.reason)),
        (
            OperationItemStatus::Failed,
            Some(&AdmissionFailureReason::HasRegisteredDependents)
        ),
        "on {backend}: {:?}",
        blocked.failure,
    );
    assert_eq!(
        lifecycle_of(db, TARGET).await,
        Some(LifecycleStatus::Active),
        "a refused deletion writes nothing on {backend}",
    );

    // Remove the dependant, and the same deletion now commits.
    assert_succeeded(
        &pass(
            db,
            "del-holder",
            OperationKind::Deletion,
            false,
            removal(HOLDER, 1),
        )
        .await,
        backend,
    );
    let deleted = pass(
        db,
        "del",
        OperationKind::Deletion,
        false,
        removal(TARGET, 1),
    )
    .await;
    assert_succeeded(&deleted, backend);
    assert_eq!(deleted.resource_version, Some(2));
    assert_eq!(deleted.revision_no, None, "no revision on {backend}");
    assert_eq!(
        lifecycle_of(db, TARGET).await,
        Some(LifecycleStatus::Deleted),
        "the row survives as a tombstone on {backend}",
    );
}

/// The persisted write-order sequence, read on its own connection.
async fn write_sequence(db: &Arc<DBProvider<DbError>>) -> i64 {
    let conn = db.conn().expect("conn");
    CoordinationStateRepo::entity_write_sequence(&conn, &allow_all())
        .await
        .expect("the migration seeds the state row")
}

async fn assert_dry_run(db: &Arc<DBProvider<DbError>>, backend: &str) {
    let before = write_sequence(db).await;

    let creation_pass = pass(
        db,
        "dry-create",
        OperationKind::Registration,
        true,
        creation(FRESH, schema(FRESH)),
    )
    .await;
    assert_succeeded(&creation_pass, backend);
    assert_eq!(
        creation_pass.resource_version, None,
        "a dry run moved no version on {backend}",
    );
    assert_eq!(
        lifecycle_of(db, FRESH).await,
        None,
        "a dry run wrote no entity on {backend}",
    );

    assert_succeeded(
        &pass(
            db,
            "dry-seed",
            OperationKind::Registration,
            false,
            creation(DRY, schema(DRY)),
        )
        .await,
        backend,
    );
    let deletion_pass = pass(
        db,
        "dry-del",
        OperationKind::Deletion,
        true,
        removal(DRY, 1),
    )
    .await;
    assert_succeeded(&deletion_pass, backend);
    assert_eq!(deletion_pass.resource_version, None);
    assert_eq!(
        lifecycle_of(db, DRY).await,
        Some(LifecycleStatus::Active),
        "a dry-run deletion deleted nothing on {backend}",
    );

    let after = write_sequence(db).await;
    assert_eq!(
        after - before,
        1,
        "on {backend}: only the one committing registration kept its claim",
    );
}

/// A deletion batch orders by the reverse relation, and its edges come from
/// `dependency` — a read, so it is worth running against the real engines.
async fn assert_batch_order(db: &Arc<DBProvider<DbError>>, backend: &str) {
    assert_succeeded(
        &pass(
            db,
            "batch-base",
            OperationKind::Registration,
            false,
            creation(BATCH_BASE, schema(BATCH_BASE)),
        )
        .await,
        backend,
    );
    assert_succeeded(
        &pass(
            db,
            "batch-holder",
            OperationKind::Registration,
            false,
            creation(BATCH_HOLDER, referencing(BATCH_HOLDER, BATCH_BASE)),
        )
        .await,
        backend,
    );

    // Submitted target-first, which is the order that fails without ordering.
    let config = TypesRegistryConfig::default();
    let provider = DBProvider::<AcceptanceError>::new(db.db());
    let operation_id = accept(
        &stores(),
        &provider,
        &allow_all(),
        &AcceptanceContext {
            policy: &RegistrationPolicy::default(),
            config: &config,
            metrics: &common::metrics(),
        },
        &(Arc::new(NoDispatch) as Arc<dyn OperationDispatch>),
        &SubmitRequest {
            idempotency_key: "batch-del".to_owned(),
            kind: OperationKind::Deletion,
            dry_run: false,
            candidates: vec![removal(BATCH_BASE, 1), removal(BATCH_HOLDER, 1)],
        },
        NOW,
    )
    .await
    .expect("the batch reaches the worker")
    .operation_id;
    let outcome = run_operation(
        &stores(),
        &DBProvider::<WorkerError>::new(db.db()),
        &allow_all(),
        Tuning {
            limits: &common::limits(),
            worker: &common::worker_settings(),
            metrics: &common::metrics(),
            allow_compatibility_force: false,
        },
        operation_id,
        LATER,
    )
    .await
    .expect("the admission pass completes");

    for item in &outcome.items {
        assert_succeeded(item, backend);
    }
    for gts_id in [BATCH_BASE, BATCH_HOLDER] {
        assert_eq!(
            lifecycle_of(db, gts_id).await,
            Some(LifecycleStatus::Deleted),
            "{gts_id} must be deleted on {backend}",
        );
    }
}

async fn assert_t20(db: &Arc<DBProvider<DbError>>, backend: &str) {
    assert_deletion(db, backend).await;
    assert_batch_order(db, backend).await;
    assert_dry_run(db, backend).await;
}

async fn wait_for_tcp(host: &str, port: u16, timeout: Duration) {
    use tokio::net::TcpStream;
    use tokio::time::{Instant, sleep};

    let deadline = Instant::now() + timeout;
    loop {
        if TcpStream::connect((host, port)).await.is_ok() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timeout waiting for {host}:{port}"
        );
        sleep(Duration::from_millis(200)).await;
    }
}

#[tokio::test]
async fn deletion_and_dry_run_behave_on_sqlite_control() {
    let db = common::test_db().await;
    assert_t20(&db, "sqlite").await;
}

#[tokio::test]
async fn deletion_and_dry_run_behave_on_postgres() {
    let request = test_containers::postgres()
        .with_env_var("POSTGRES_PASSWORD", "pass")
        .with_env_var("POSTGRES_USER", "user")
        .with_env_var("POSTGRES_DB", "app");
    let container = request.start().await.expect("start postgres container");
    let port = container
        .get_host_port_ipv4(5432)
        .await
        .expect("postgres port");
    let host = container
        .get_host()
        .await
        .expect("postgres host")
        .to_string();
    wait_for_tcp(host.trim_matches(['[', ']']), port, Duration::from_mins(1)).await;

    let db = provider_for(&format!("postgres://user:pass@{host}:{port}/app"), 4).await;
    assert_t20(&db, "postgres").await;
}

#[tokio::test]
async fn deletion_and_dry_run_behave_on_mysql() {
    let container = test_containers::mysql()
        .start()
        .await
        .expect("start mysql container");
    let port = container
        .get_host_port_ipv4(3306)
        .await
        .expect("mysql port");
    let host = container.get_host().await.expect("mysql host").to_string();
    wait_for_tcp(host.trim_matches(['[', ']']), port, Duration::from_mins(2)).await;

    let db = provider_for(&format!("mysql://root@{host}:{port}/test"), 4).await;
    assert_t20(&db, "mysql").await;
}
