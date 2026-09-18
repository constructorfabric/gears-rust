//! `PostgreSQL`/`MySQL` partial admission (T19): per-candidate transactions must
//! preserve independent successes and leave no state for blocked candidates.

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
use types_registry::domain::admission::worker::{
    OperationOutcome, Tuning, WorkerError, run_operation,
};
use types_registry::domain::admission::{
    AdmissionFailureReason, Candidate, OperationDispatch, SubmitRequest,
};
use types_registry::domain::enums::{OperationItemStatus, OperationKind};
use types_registry::domain::policy::RegistrationPolicy;
use types_registry::infra::storage::repo::EntityRepo;

const NOW: OffsetDateTime = datetime!(2026-09-11 09:15:30 UTC);
const LATER: OffsetDateTime = datetime!(2026-09-11 10:20:40 UTC);
const DRAFT_07: &str = "http://json-schema.org/draft-07/schema#";

const BASE: &str = gts_id!("cf.core.batch_backend.base.v1~");
const REFERRER: &str = gts_id!("cf.core.batch_backend.referrer.v1~");
const BROKEN: &str = gts_id!("cf.core.batch_backend.broken.v1~");
const STANDALONE: &str = gts_id!("cf.core.batch_backend.standalone.v1~");
const DANGLING: &str = gts_id!("cf.core.batch_backend.dangling.v1~");
const DEPENDENT: &str = gts_id!("cf.core.batch_backend.dependent.v1~");
const V1_0: &str = gts_id!("cf.core.batch_backend.minor.v1.0~");
const V1_1: &str = gts_id!("cf.core.batch_backend.minor.v1.1~");
const LOOP_A: &str = gts_id!("cf.core.batch_backend.loop_a.v1~");
const LOOP_B: &str = gts_id!("cf.core.batch_backend.loop_b.v1~");
const ABSENT: &str = gts_id!("cf.core.batch_backend.absent.v1~");

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
    json!({
        "$id": format!("gts://{gts_id}"),
        "$schema": DRAFT_07,
        "type": "object",
        "allOf": [{ "$ref": format!("gts://{target}") }],
    })
}

fn create(gts_id: &str, content: Value) -> Candidate {
    Candidate {
        gts_id: gts_id.to_owned(),
        content: Some(content),
        expected_resource_version: None,
        force: false,
    }
}

async fn admit_batch(
    db: &Arc<DBProvider<DbError>>,
    key: &str,
    candidates: Vec<Candidate>,
) -> OperationOutcome {
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
            kind: OperationKind::Registration,
            dry_run: false,
            candidates,
        },
        NOW,
    )
    .await
    .expect("the batch reaches the worker")
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
}

fn assert_succeeded(outcome: &OperationOutcome, gts_id: &str, backend: &str) {
    let item = outcome
        .items
        .iter()
        .find(|item| item.gts_id == gts_id)
        .unwrap_or_else(|| panic!("{gts_id} is owed an outcome on {backend}"));
    assert_eq!(
        item.status,
        OperationItemStatus::Succeeded,
        "{gts_id} must be admitted on {backend}: {:?}",
        item.failure,
    );
}

fn assert_refused(
    outcome: &OperationOutcome,
    gts_id: &str,
    reason: &AdmissionFailureReason,
    backend: &str,
) {
    let item = outcome
        .items
        .iter()
        .find(|item| item.gts_id == gts_id)
        .unwrap_or_else(|| panic!("{gts_id} is owed an outcome on {backend}"));
    assert_eq!(
        (item.status, item.failure.as_ref().map(|f| &f.reason)),
        (OperationItemStatus::Failed, Some(reason)),
        "{gts_id} must be refused for the named reason on {backend}: {:?}",
        item.failure,
    );
}

async fn assert_no_entity(db: &Arc<DBProvider<DbError>>, gts_id: &str, backend: &str) {
    let provider = DBProvider::<WorkerError>::new(db.db());
    let conn = provider.conn().expect("conn");
    assert!(
        EntityRepo::find_by_gts_id(&conn, &allow_all(), gts_id)
            .await
            .expect("read")
            .is_none(),
        "{gts_id} must have written no entity row on {backend}",
    );
}

/// The dependency order wins over the submission order: the referrer is item 0
/// and still commits, because its `$ref` target committed first.
async fn assert_ordering(db: &Arc<DBProvider<DbError>>, backend: &str) {
    let outcome = admit_batch(
        db,
        "order",
        vec![
            create(REFERRER, referencing(REFERRER, BASE)),
            create(BASE, schema(BASE)),
        ],
    )
    .await;
    assert_succeeded(&outcome, BASE, backend);
    assert_succeeded(&outcome, REFERRER, backend);
}

/// One candidate's failure commits the independent branch and blocks only its
/// own downstream — each in its own transaction, so this is the case where a
/// backend that aborted the wrong one would show.
async fn assert_partial_commit(db: &Arc<DBProvider<DbError>>, backend: &str) {
    let outcome = admit_batch(
        db,
        "partial",
        vec![
            create(BROKEN, referencing(BROKEN, ABSENT)),
            create(STANDALONE, schema(STANDALONE)),
        ],
    )
    .await;
    assert_refused(
        &outcome,
        BROKEN,
        &AdmissionFailureReason::DependencyNotFound,
        backend,
    );
    assert_succeeded(&outcome, STANDALONE, backend);
    assert_no_entity(db, BROKEN, backend).await;
}

async fn assert_blocked_dependency(db: &Arc<DBProvider<DbError>>, backend: &str) {
    let outcome = admit_batch(
        db,
        "blocked-dep",
        vec![
            create(DANGLING, referencing(DANGLING, ABSENT)),
            create(DEPENDENT, referencing(DEPENDENT, DANGLING)),
        ],
    )
    .await;
    assert_refused(
        &outcome,
        DANGLING,
        &AdmissionFailureReason::DependencyNotFound,
        backend,
    );
    assert_refused(
        &outcome,
        DEPENDENT,
        &AdmissionFailureReason::BlockedByDependency,
        backend,
    );
    assert_no_entity(db, DEPENDENT, backend).await;
}

async fn assert_blocked_predecessor(db: &Arc<DBProvider<DbError>>, backend: &str) {
    let outcome = admit_batch(
        db,
        "blocked-pred",
        vec![
            create(V1_0, referencing(V1_0, ABSENT)),
            create(V1_1, schema(V1_1)),
        ],
    )
    .await;
    assert_refused(
        &outcome,
        V1_0,
        &AdmissionFailureReason::DependencyNotFound,
        backend,
    );
    assert_refused(
        &outcome,
        V1_1,
        &AdmissionFailureReason::BlockedByPredecessor,
        backend,
    );
    assert_no_entity(db, V1_1, backend).await;
}

/// A cycle is refused before any candidate is evaluated, so neither member
/// reaches a commit transaction at all.
async fn assert_cycle_writes_nothing(db: &Arc<DBProvider<DbError>>, backend: &str) {
    let outcome = admit_batch(
        db,
        "cycle",
        vec![
            create(LOOP_A, referencing(LOOP_A, LOOP_B)),
            create(LOOP_B, referencing(LOOP_B, LOOP_A)),
        ],
    )
    .await;
    assert_refused(
        &outcome,
        LOOP_A,
        &AdmissionFailureReason::InvalidSchema,
        backend,
    );
    assert_refused(
        &outcome,
        LOOP_B,
        &AdmissionFailureReason::InvalidSchema,
        backend,
    );
    assert_no_entity(db, LOOP_A, backend).await;
    assert_no_entity(db, LOOP_B, backend).await;
}

async fn assert_t19(db: &Arc<DBProvider<DbError>>, backend: &str) {
    assert_ordering(db, backend).await;
    assert_partial_commit(db, backend).await;
    assert_blocked_dependency(db, backend).await;
    assert_blocked_predecessor(db, backend).await;
    assert_cycle_writes_nothing(db, backend).await;
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
async fn batch_admission_behaves_on_sqlite_control() {
    let db = common::test_db().await;
    assert_t19(&db, "sqlite").await;
}

#[tokio::test]
async fn batch_admission_behaves_on_postgres() {
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
    assert_t19(&db, "postgres").await;
}

#[tokio::test]
async fn batch_admission_behaves_on_mysql() {
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
    assert_t19(&db, "mysql").await;
}
