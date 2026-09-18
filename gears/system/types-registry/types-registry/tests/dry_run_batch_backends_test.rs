//! Whole-batch parity, snapshot coherence and atomic publication on all backends.
//! Use `SQLite` WAL and `PostgreSQL`/`MySQL` `REPEATABLE READ` to commit a concurrent
//! writer while prediction retains its original snapshot.
//!
//! Run dry run before commit on the same database, checking unchanged entity
//! tables between them to establish identical initial state.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use std::sync::Arc;

use sea_orm::EntityTrait;
use serde_json::{Value, json};
use time::OffsetDateTime;
use time::macros::datetime;
use toolkit_db::secure::SecureEntityExt;
use toolkit_db::{DBProvider, DbError, DbTx};
use toolkit_gts::gts_id;
use uuid::Uuid;

use common::{PausePoint, TestDir, TestStores, allow_all, stores};
use types_registry::config::TypesRegistryConfig;
use types_registry::domain::admission::AdmissionFailureReason;
use types_registry::domain::admission::acceptance::{AcceptanceContext, AcceptanceError, accept};
use types_registry::domain::admission::worker::{ItemOutcome, Tuning, WorkerError, run_operation};
use types_registry::domain::admission::{Candidate, OperationDispatch, SubmitRequest};
use types_registry::domain::enums::{OperationItemStatus, OperationKind};
use types_registry::domain::policy::RegistrationPolicy;
use types_registry::domain::ports::Stores;
use types_registry::infra::storage::entity::{
    coordination_state, dependency, entity, instance, instance_revision, type_schema,
    type_schema_revision, version_family,
};

type Provider = Arc<DBProvider<DbError>>;

const NOW: OffsetDateTime = datetime!(2026-09-13 09:15:30 UTC);
const LATER: OffsetDateTime = datetime!(2026-09-13 10:20:40 UTC);

const BASE: &str = gts_id!("cf.core.drybb.base.v1~");
const REFERRER: &str = gts_id!("cf.core.drybb.holder.v1~");
const SUBJECT_ONE: &str = gts_id!("cf.core.drybb.subjone.v1~");
const SUBJECT_TWO: &str = gts_id!("cf.core.drybb.subjtwo.v1~");

struct NoDispatch;

#[async_trait::async_trait]
impl OperationDispatch for NoDispatch {
    async fn enqueue(&self, _tx: &DbTx<'_>, _operation_id: Uuid) -> anyhow::Result<()> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Documents and submission
// ---------------------------------------------------------------------------

fn schema(gts_id: &str, marker: &str) -> Value {
    json!({
        "$id": format!("gts://{gts_id}"),
        "$schema": "http://json-schema.org/draft-07/schema#",
        "title": marker,
        "type": "object",
        "properties": { "name": { "type": "string" } },
    })
}

fn referencing(gts_id: &str, target: &str) -> Value {
    let mut document = schema(gts_id, "holder");
    document["properties"] = json!({ "target": { "$ref": format!("gts://{target}") } });
    document
}

fn creation(gts_id: &str, content: Value) -> Candidate {
    Candidate {
        gts_id: gts_id.to_owned(),
        content: Some(content),
        expected_resource_version: None,
        force: false,
    }
}

fn revision(gts_id: &str, content: Value, expected: i64) -> Candidate {
    Candidate {
        gts_id: gts_id.to_owned(),
        content: Some(content),
        expected_resource_version: Some(expected),
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

fn worker(db: &Provider) -> DBProvider<WorkerError> {
    DBProvider::new(db.db())
}

async fn submit(
    db: &Provider,
    key: &str,
    kind: OperationKind,
    dry_run: bool,
    candidates: Vec<Candidate>,
) -> Uuid {
    let provider: DBProvider<AcceptanceError> = DBProvider::new(db.db());
    let dispatch: Arc<dyn OperationDispatch> = Arc::new(NoDispatch);
    accept(
        &stores(),
        &provider,
        &allow_all(),
        &AcceptanceContext {
            policy: &RegistrationPolicy::default(),
            config: &TypesRegistryConfig::default(),
            metrics: &common::metrics(),
        },
        &dispatch,
        &SubmitRequest {
            idempotency_key: key.to_owned(),
            kind,
            dry_run,
            candidates,
        },
        NOW,
    )
    .await
    .expect("the batch is accepted")
    .operation_id
}

async fn run_on(db: &Provider, ports: &Arc<dyn Stores>, operation_id: Uuid) -> Vec<ItemOutcome> {
    run_operation(
        ports,
        &worker(db),
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
    .expect("the worker itself must not fail")
    .items
}

async fn run_batch(
    db: &Provider,
    key: &str,
    kind: OperationKind,
    dry_run: bool,
    candidates: Vec<Candidate>,
) -> Vec<ItemOutcome> {
    let operation_id = submit(db, key, kind, dry_run, candidates).await;
    run_on(db, &stores(), operation_id).await
}

async fn seed(db: &Provider, key: &str, candidate: Candidate) {
    let items = run_batch(db, key, OperationKind::Registration, false, vec![candidate]).await;
    assert_eq!(
        items[0].status,
        OperationItemStatus::Succeeded,
        "the seed must land: {items:?}",
    );
}

// ---------------------------------------------------------------------------
// Assertions
// ---------------------------------------------------------------------------

#[derive(Debug, PartialEq, Eq)]
struct Verdict {
    gts_id: String,
    status: OperationItemStatus,
    reason: Option<AdmissionFailureReason>,
}

fn verdicts(items: &[ItemOutcome]) -> Vec<Verdict> {
    items
        .iter()
        .map(|item| Verdict {
            gts_id: item.gts_id.clone(),
            status: item.status,
            reason: item.failure.as_ref().map(|failure| failure.reason.clone()),
        })
        .collect()
}

fn ok(gts_id: &str) -> Verdict {
    Verdict {
        gts_id: gts_id.to_owned(),
        status: OperationItemStatus::Succeeded,
        reason: None,
    }
}

/// Every entity-state table, dumped whole as sorted `Debug` lines.
#[derive(Debug, PartialEq, Eq)]
struct EntityState(Vec<Vec<String>>);

macro_rules! dump {
    ($conn:expr, $scope:expr, $entity:ty) => {{
        let mut rows: Vec<String> = <$entity>::find()
            .secure()
            .scope_with($scope)
            .all($conn)
            .await
            .expect("read the table whole")
            .into_iter()
            .map(|row| format!("{row:?}"))
            .collect();
        rows.sort();
        rows
    }};
}

async fn entity_state(db: &Provider) -> EntityState {
    let scope = allow_all();
    let conn = db.conn().expect("conn");
    EntityState(vec![
        dump!(&conn, &scope, version_family::Entity),
        dump!(&conn, &scope, entity::Entity),
        dump!(&conn, &scope, type_schema_revision::Entity),
        dump!(&conn, &scope, type_schema::Entity),
        dump!(&conn, &scope, instance_revision::Entity),
        dump!(&conn, &scope, instance::Entity),
        dump!(&conn, &scope, dependency::Entity),
        dump!(&conn, &scope, coordination_state::Entity),
    ])
}

/// Predict, assert unchanged entity tables, then commit on the same database.
/// Require matching per-candidate verdicts.
async fn assert_parity(
    db: &Provider,
    kind: OperationKind,
    candidates: Vec<Candidate>,
    expected: Vec<Verdict>,
) {
    let before = entity_state(db).await;

    let spy = TestStores::forbidding_entity_writes();
    let ports: Arc<dyn Stores> = Arc::clone(&spy) as _;
    let operation_id = submit(db, "dry", kind, true, candidates.clone()).await;
    let predicted = run_on(db, &ports, operation_id).await;
    assert_eq!(
        spy.entity_write_attempts(),
        Vec::<&str>::new(),
        "a dry run issued an entity-state write or claimed the write order",
    );
    assert_eq!(
        entity_state(db).await,
        before,
        "a dry run leaves every entity-state table exactly as it found it",
    );
    for item in &predicted {
        if item.status == OperationItemStatus::Succeeded {
            assert_eq!(
                (item.revision_no, item.resource_version),
                (None, None),
                "a predicted success allocated no revision and moved no version: {item:?}",
            );
        }
    }

    let committed = run_batch(db, "real", kind, false, candidates).await;
    assert_eq!(
        verdicts(&committed),
        expected,
        "the committing batch is the comparator: {committed:?}",
    );
    assert_eq!(
        verdicts(&predicted),
        expected,
        "and the prediction must be the same: {predicted:?}",
    );
}

// ---------------------------------------------------------------------------
// The cases, each a function of the provider
// ---------------------------------------------------------------------------

/// A referrer whose base the same batch creates — the false negative a
/// rollback-per-candidate prediction produced.
async fn assert_in_batch_reference_is_predicted(db: &Provider) {
    assert_parity(
        db,
        OperationKind::Registration,
        vec![
            creation(BASE, schema(BASE, "base")),
            creation(REFERRER, referencing(REFERRER, BASE)),
        ],
        vec![ok(BASE), ok(REFERRER)],
    )
    .await;
}

const DEL_BASE: &str = gts_id!("cf.core.drybb.dbase.v1~");
const DEL_HOLDER: &str = gts_id!("cf.core.drybb.dholder.v1~");

/// A deletion batch that removes a base and the dependant blocking it — the
/// same defect in the opposite ordering relation.
async fn assert_deletion_batch_is_predicted(db: &Provider) {
    seed(db, "del-base", creation(DEL_BASE, schema(DEL_BASE, "base"))).await;
    seed(
        db,
        "del-holder",
        creation(DEL_HOLDER, referencing(DEL_HOLDER, DEL_BASE)),
    )
    .await;

    assert_parity(
        db,
        OperationKind::Deletion,
        vec![removal(DEL_BASE, 1), removal(DEL_HOLDER, 1)],
        vec![ok(DEL_BASE), ok(DEL_HOLDER)],
    )
    .await;
}

/// Pause after reading candidate one, then commit a revision of candidate two's
/// subject. The original snapshot still admits version 1; a fresh snapshot would
/// see version 2 and refuse `precondition_failed`.
async fn assert_snapshot_survives_a_concurrent_commit(db: &Provider) {
    for (key, gts_id) in [("one", SUBJECT_ONE), ("two", SUBJECT_TWO)] {
        seed(db, key, creation(gts_id, schema(gts_id, "first"))).await;
    }

    let operation_id = submit(
        db,
        "dry",
        OperationKind::Registration,
        true,
        vec![
            revision(SUBJECT_ONE, schema(SUBJECT_ONE, "predicted"), 1),
            revision(SUBJECT_TWO, schema(SUBJECT_TWO, "predicted"), 1),
        ],
    )
    .await;

    let (paused, reached, resume) = TestStores::pausing(PausePoint::CurrentDocuments);
    let ports: Arc<dyn Stores> = paused;
    let pass_db = Arc::clone(db);
    let pass = tokio::spawn(async move { run_on(&pass_db, &ports, operation_id).await });
    reached
        .await
        .expect("the pass reached its first current-document read");

    let deadline = std::time::Duration::from_mins(1);
    let writer_op = submit(
        db,
        "concurrent",
        OperationKind::Registration,
        false,
        vec![revision(SUBJECT_TWO, schema(SUBJECT_TWO, "committed"), 1)],
    )
    .await;
    let committed = tokio::time::timeout(deadline, run_on(db, &stores(), writer_op))
        .await
        .expect("the concurrent write must not block on the held snapshot");
    assert_eq!(
        committed[0].status,
        OperationItemStatus::Succeeded,
        "the concurrent revision really did land, before the pass resumed: {committed:?}",
    );

    resume.send(()).expect("resume the paused pass");
    let predicted = tokio::time::timeout(deadline, pass)
        .await
        .expect("the prediction must not deadlock against the concurrent writer")
        .expect("pass task");

    assert_eq!(
        verdicts(&predicted),
        vec![ok(SUBJECT_ONE), ok(SUBJECT_TWO)],
        "the second candidate was predicted against the version its snapshot held, \
         not against the one committed underneath it: {predicted:?}",
    );
}

// ---------------------------------------------------------------------------
// SQLite
// ---------------------------------------------------------------------------

/// WAL, so a writer commits past an open read snapshot — the behaviour the two
/// MVCC engines give by default, and the one the coherence case needs.
async fn sqlite(dir: &TestDir) -> Provider {
    common::test_db_file_wal(&dir.path().join("registry.db")).await
}

#[tokio::test]
async fn in_batch_reference_is_predicted_on_sqlite() {
    let dir = TestDir::new("types-registry-dry-run-backends");
    assert_in_batch_reference_is_predicted(&sqlite(&dir).await).await;
}

#[tokio::test]
async fn deletion_batch_is_predicted_on_sqlite() {
    let dir = TestDir::new("types-registry-dry-run-backends");
    assert_deletion_batch_is_predicted(&sqlite(&dir).await).await;
}

#[tokio::test]
async fn snapshot_survives_a_concurrent_commit_on_sqlite() {
    let dir = TestDir::new("types-registry-dry-run-backends");
    assert_snapshot_survives_a_concurrent_commit(&sqlite(&dir).await).await;
}

// ---------------------------------------------------------------------------
// PostgreSQL and MySQL
// ---------------------------------------------------------------------------

#[cfg(feature = "integration")]
mod backends {
    use testcontainers::ImageExt;
    use testcontainers::runners::AsyncRunner;

    use super::*;

    async fn postgres() -> (impl Sized, Provider) {
        let container = test_containers::postgres()
            .with_env_var("POSTGRES_PASSWORD", "pass")
            .with_env_var("POSTGRES_USER", "user")
            .with_env_var("POSTGRES_DB", "app")
            .start()
            .await
            .expect("start postgres");
        let port = container.get_host_port_ipv4(5432).await.unwrap();
        let host = container.get_host().await.unwrap();
        let db = common::provider_for(&format!("postgres://user:pass@{host}:{port}/app"), 4).await;
        (container, db)
    }

    async fn mysql() -> (impl Sized, Provider) {
        let container = test_containers::mysql().start().await.expect("start mysql");
        let port = container.get_host_port_ipv4(3306).await.unwrap();
        let host = container.get_host().await.unwrap();
        let db = common::provider_for(&format!("mysql://root@{host}:{port}/test"), 4).await;
        (container, db)
    }

    #[tokio::test]
    async fn in_batch_reference_is_predicted_on_postgres() {
        let (_container, db) = postgres().await;
        assert_in_batch_reference_is_predicted(&db).await;
    }

    #[tokio::test]
    async fn in_batch_reference_is_predicted_on_mysql() {
        let (_container, db) = mysql().await;
        assert_in_batch_reference_is_predicted(&db).await;
    }

    #[tokio::test]
    async fn deletion_batch_is_predicted_on_postgres() {
        let (_container, db) = postgres().await;
        assert_deletion_batch_is_predicted(&db).await;
    }

    #[tokio::test]
    async fn deletion_batch_is_predicted_on_mysql() {
        let (_container, db) = mysql().await;
        assert_deletion_batch_is_predicted(&db).await;
    }

    #[tokio::test]
    async fn snapshot_survives_a_concurrent_commit_on_postgres() {
        let (_container, db) = postgres().await;
        assert_snapshot_survives_a_concurrent_commit(&db).await;
    }

    #[tokio::test]
    async fn snapshot_survives_a_concurrent_commit_on_mysql() {
        let (_container, db) = mysql().await;
        assert_snapshot_survives_a_concurrent_commit(&db).await;
    }
}
