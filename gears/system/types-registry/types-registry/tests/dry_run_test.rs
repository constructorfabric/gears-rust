//! Single-candidate dry runs for registration and deletion (T20).
//! Check ordinary admission rules, zero entity writes and durable outcomes.
//! Batch lifecycle and parity live in `dry_run_batch_test.rs` and
//! `dry_run_parity_test.rs`.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;

use serde_json::{Value, json};
use time::OffsetDateTime;
use time::macros::datetime;
use toolkit_db::{DBProvider, DbError, DbTx};
use toolkit_gts::gts_id;
use uuid::Uuid;

use types_registry::config::TypesRegistryConfig;
use types_registry::domain::admission::AdmissionFailureReason;
use types_registry::domain::admission::acceptance::{AcceptanceContext, AcceptanceError, accept};
use types_registry::domain::admission::worker::{
    ItemOutcome, OperationOutcome, Tuning, WorkerError, run_operation,
};
use types_registry::domain::admission::{Candidate, OperationDispatch, SubmitRequest};
use types_registry::domain::enums::{LifecycleStatus, OperationItemStatus, OperationKind};
use types_registry::domain::policy::RegistrationPolicy;
use types_registry::domain::ports::EntityRow;
use types_registry::infra::storage::repo::{CoordinationStateRepo, EntityRepo};

mod common;
use common::{allow_all, stores, test_db};

const NOW: OffsetDateTime = datetime!(2026-09-11 09:15:30 UTC);
const LATER: OffsetDateTime = datetime!(2026-09-11 10:20:40 UTC);

const SUBJECT: &str = gts_id!("cf.core.dry.subject.v1~");
const FRESH: &str = gts_id!("cf.core.dry.fresh.v1~");
const HOLDER: &str = gts_id!("cf.core.dry.holder.v1~");

type Provider = Arc<DBProvider<DbError>>;

struct NoDispatch;

#[async_trait::async_trait]
impl OperationDispatch for NoDispatch {
    async fn enqueue(
        &self,
        _tx: &DbTx<'_>,
        _operation_id: Uuid,
    ) -> Result<toolkit_db::outbox::Wake, types_registry::domain::admission::OutboxError> {
        Ok(toolkit_db::outbox::Wake::empty())
    }
}

fn worker(db: &Provider) -> DBProvider<WorkerError> {
    DBProvider::new(db.db())
}

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
    let mut doc = schema(gts_id, "holder");
    doc["properties"] = json!({ "target": { "$ref": format!("gts://{target}") } });
    doc
}

async fn submit(
    db: &Provider,
    key: &str,
    kind: OperationKind,
    dry_run: bool,
    candidates: Vec<Candidate>,
) -> Result<Uuid, AcceptanceError> {
    let provider: DBProvider<AcceptanceError> = DBProvider::new(db.db());
    let policy = RegistrationPolicy::default();
    let config = TypesRegistryConfig::default();
    let dispatch: Arc<dyn OperationDispatch> = Arc::new(NoDispatch);
    accept(
        &stores(),
        &provider,
        &allow_all(),
        &AcceptanceContext {
            policy: &policy,
            config: &config,
            metrics: &common::metrics(),
        },
        &dispatch,
        &SubmitRequest {
            idempotency_key: Some(key.to_owned()),
            kind,
            dry_run,
            candidates,
        },
        NOW,
    )
    .await
    .map(|accepted| accepted.operation_id)
}

async fn run(db: &Provider, operation_id: Uuid) -> OperationOutcome {
    run_operation(
        &stores(),
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

async fn pass(
    db: &Provider,
    key: &str,
    kind: OperationKind,
    dry_run: bool,
    candidate: Candidate,
) -> ItemOutcome {
    let op = submit(db, key, kind, dry_run, vec![candidate])
        .await
        .expect("accepted");
    run(db, op).await.items.remove(0)
}

async fn entity_of(db: &Provider, gts_id: &str) -> Option<EntityRow> {
    let provider = worker(db);
    let conn = provider.conn().expect("conn");
    EntityRepo::find_by_gts_id(&conn, &allow_all(), gts_id)
        .await
        .expect("read")
}

async fn entity_write_sequence(db: &Provider) -> i64 {
    let conn = db.conn().expect("conn");
    CoordinationStateRepo::entity_write_sequence(&conn, &allow_all())
        .await
        .expect("the migration seeds the state row")
}

/// Commit one registration so the revision and deletion cases have a subject.
async fn seed(db: &Provider) {
    let item = pass(
        db,
        "seed",
        OperationKind::Registration,
        false,
        creation(SUBJECT, schema(SUBJECT, "first")),
    )
    .await;
    assert_eq!(item.status, OperationItemStatus::Succeeded, "{item:?}");
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

/// The checks ran and the candidate would be admitted — and nothing exists.
#[tokio::test]
async fn a_dry_run_creation_succeeds_and_writes_no_entity() {
    let db = test_db().await;

    let item = pass(
        &db,
        "dry",
        OperationKind::Registration,
        true,
        creation(FRESH, schema(FRESH, "hypothetical")),
    )
    .await;

    assert_eq!(item.status, OperationItemStatus::Succeeded, "{item:?}");
    assert_eq!(
        item.resource_version, None,
        "a dry run moved no version, so naming one would name a version that does not exist",
    );
    assert_eq!(item.revision_no, None);
    assert!(
        entity_of(&db, FRESH).await.is_none(),
        "a dry run leaves no entity behind",
    );
}

/// A dry run is not a way past a check: the same refusal a commit would earn.
#[tokio::test]
async fn a_dry_run_creation_of_an_existing_identifier_is_refused() {
    let db = test_db().await;
    seed(&db).await;

    let item = pass(
        &db,
        "dry",
        OperationKind::Registration,
        true,
        creation(SUBJECT, schema(SUBJECT, "again")),
    )
    .await;

    assert_eq!(item.status, OperationItemStatus::Failed);
    assert_eq!(
        item.failure.as_ref().expect("failure").reason,
        AdmissionFailureReason::AlreadyExists,
    );
}

#[tokio::test]
async fn a_dry_run_revision_moves_no_version() {
    let db = test_db().await;
    seed(&db).await;

    let item = pass(
        &db,
        "dry",
        OperationKind::Registration,
        true,
        revision(SUBJECT, schema(SUBJECT, "second"), 1),
    )
    .await;

    assert_eq!(item.status, OperationItemStatus::Succeeded, "{item:?}");
    assert_eq!(item.resource_version, None);
    let row = entity_of(&db, SUBJECT).await.expect("still there");
    assert_eq!(row.resource_version, 1, "the committed version stood still");
}

/// The one dry-run outcome that **does** report a version: `unchanged` names the
/// version that did not move, and it exists whether or not this pass wrote.
#[tokio::test]
async fn a_dry_run_unchanged_reports_the_existing_version() {
    let db = test_db().await;
    seed(&db).await;

    let item = pass(
        &db,
        "dry",
        OperationKind::Registration,
        true,
        revision(SUBJECT, schema(SUBJECT, "first"), 1),
    )
    .await;

    assert_eq!(
        item.status,
        OperationItemStatus::Unchanged,
        "the authored content equals the current revision: {item:?}",
    );
    assert_eq!(
        item.resource_version,
        Some(1),
        "the version that did not move is a fact about committed state",
    );
    assert_eq!(item.revision_no, None);
}

// ---------------------------------------------------------------------------
// Deletion
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_dry_run_deletion_succeeds_and_deletes_nothing() {
    let db = test_db().await;
    seed(&db).await;

    let item = pass(
        &db,
        "dry",
        OperationKind::Deletion,
        true,
        removal(SUBJECT, 1),
    )
    .await;

    assert_eq!(item.status, OperationItemStatus::Succeeded, "{item:?}");
    assert_eq!(item.resource_version, None);
    let row = entity_of(&db, SUBJECT).await.expect("still there");
    assert_eq!(row.lifecycle_status, LifecycleStatus::Active);
    assert_eq!(row.resource_version, 1);
}

/// The dependant check is one of the checks a dry run exists to run.
#[tokio::test]
async fn a_dry_run_deletion_reports_the_refusal_a_commit_would_earn() {
    let db = test_db().await;
    seed(&db).await;
    let holder = pass(
        &db,
        "holder",
        OperationKind::Registration,
        false,
        creation(HOLDER, referencing(HOLDER, SUBJECT)),
    )
    .await;
    assert_eq!(holder.status, OperationItemStatus::Succeeded, "{holder:?}");

    let item = pass(
        &db,
        "dry",
        OperationKind::Deletion,
        true,
        removal(SUBJECT, 1),
    )
    .await;

    assert_eq!(item.status, OperationItemStatus::Failed);
    assert_eq!(
        item.failure.as_ref().expect("failure").reason,
        AdmissionFailureReason::HasRegisteredDependents,
    );
}

// ---------------------------------------------------------------------------
// What a dry run still does
// ---------------------------------------------------------------------------

/// A dry run must leave the write-order sequence unchanged: claiming it would
/// serialize real writers behind the whole prediction.
#[tokio::test]
async fn a_dry_run_leaves_the_entity_write_sequence_where_it_found_it() {
    let db = test_db().await;
    let before = entity_write_sequence(&db).await;

    let item = pass(
        &db,
        "dry",
        OperationKind::Registration,
        true,
        creation(FRESH, schema(FRESH, "hypothetical")),
    )
    .await;
    assert_eq!(item.status, OperationItemStatus::Succeeded, "{item:?}");

    assert_eq!(
        entity_write_sequence(&db).await,
        before,
        "a dry run must not advance the write path's serialization point",
    );
}

/// And a committing pass in the same database still advances it, so the test
/// above is about the dry run rather than about a claim nothing ever makes.
#[tokio::test]
async fn a_committing_pass_still_advances_the_entity_write_sequence() {
    let db = test_db().await;
    let before = entity_write_sequence(&db).await;

    pass(
        &db,
        "commit",
        OperationKind::Registration,
        false,
        creation(FRESH, schema(FRESH, "real")),
    )
    .await;

    assert_eq!(entity_write_sequence(&db).await - before, 1);
}

// The mode is part of the request fingerprint, so one key cannot serve both.
// Covered by `operation_idempotency_test::a_dry_run_and_a_commit_cannot_share_one_idempotency_key`,
// which makes the same assertion and also checks what was dispatched.

/// A dry run is still an operation, so a redelivered pass reports its stored
/// outcome and does not re-run the checks.
#[tokio::test]
async fn a_second_pass_over_a_dry_run_is_a_no_op() {
    let db = test_db().await;
    let op = submit(
        &db,
        "dry",
        OperationKind::Registration,
        true,
        vec![creation(FRESH, schema(FRESH, "hypothetical"))],
    )
    .await
    .expect("accepted");

    let first = run(&db, op).await;
    let second = run(&db, op).await;

    assert!(!first.already_terminal);
    assert!(second.already_terminal);
    assert_eq!(first.items[0].status, OperationItemStatus::Succeeded);
    assert_eq!(second.items[0].status, OperationItemStatus::Succeeded);
    assert!(entity_of(&db, FRESH).await.is_none());
}

/// A dry run waives nothing of its own, and it does not exempt the candidate
/// from the force gate either — it runs it. With the deployment **permitting**
/// force, a forced cross-minor dry run is therefore accepted and reaches the
/// waived verdict, exactly as the committing pass would, and still writes
/// nothing. The disallowed half of this pair lives in `acceptance_tests`.
#[tokio::test]
async fn a_forced_dry_run_is_waived_where_the_deployment_permits_force() {
    let db = test_db().await;
    let v2_0 = gts_id!("cf.core.dry.minor.v2.0~");
    let v2_1 = gts_id!("cf.core.dry.minor.v2.1~");

    // An **open** schema: adding a property to it is incompatible, so the later
    // minor below needs the waiver rather than merely being allowed one.
    let open = |gts_id: &str, extra: bool| {
        let mut properties = json!({ "a": { "type": "string" } });
        if extra {
            properties["b"] = json!({ "type": "string" });
        }
        json!({
            "$id": format!("gts://{gts_id}"),
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
            "properties": properties,
        })
    };

    let seeded = pass(
        &db,
        "minor-0",
        OperationKind::Registration,
        false,
        creation(v2_0, open(v2_0, false)),
    )
    .await;
    assert_eq!(seeded.status, OperationItemStatus::Succeeded, "{seeded:?}");

    // Without the waiver the same candidate is refused; with it, admitted.
    let unforced = pass(
        &db,
        "minor-1-plain",
        OperationKind::Registration,
        true,
        creation(v2_1, open(v2_1, true)),
    )
    .await;
    assert_eq!(
        unforced.failure.as_ref().map(|f| &f.reason),
        Some(&AdmissionFailureReason::IncompatibleWithBaseline),
        "the dry run must reach the comparison and fail it: {unforced:?}",
    );

    let forced = forced_dry_run(&db, "minor-1-forced", v2_1, open(v2_1, true)).await;

    assert_eq!(
        forced.status,
        OperationItemStatus::Succeeded,
        "the waiver applies in a dry run exactly as it does in a commit: {forced:?}",
    );
    assert_eq!(forced.resource_version, None);
    assert!(
        entity_of(&db, v2_1).await.is_none(),
        "and the waived dry run still wrote nothing",
    );
}

/// One forced dry-run pass under a deployment that permits force.
async fn forced_dry_run(db: &Provider, key: &str, gts_id: &str, content: Value) -> ItemOutcome {
    let config = TypesRegistryConfig {
        allow_compatibility_force: true,
        ..TypesRegistryConfig::default()
    };
    let provider: DBProvider<AcceptanceError> = DBProvider::new(db.db());
    let dispatch: Arc<dyn OperationDispatch> = Arc::new(NoDispatch);
    let operation_id = accept(
        &stores(),
        &provider,
        &allow_all(),
        &AcceptanceContext {
            policy: &RegistrationPolicy::default(),
            config: &config,
            metrics: &common::metrics(),
        },
        &dispatch,
        &SubmitRequest {
            idempotency_key: Some(key.to_owned()),
            kind: OperationKind::Registration,
            dry_run: true,
            candidates: vec![Candidate {
                gts_id: gts_id.to_owned(),
                content: Some(content),
                expected_resource_version: None,
                force: true,
            }],
        },
        NOW,
    )
    .await
    .expect("a forced dry run is accepted where the deployment permits force")
    .operation_id;

    run_operation(
        &stores(),
        &worker(db),
        &allow_all(),
        Tuning {
            limits: &common::limits(),
            worker: &common::worker_settings(),
            metrics: &common::metrics(),
            allow_compatibility_force: true,
        },
        operation_id,
        LATER,
    )
    .await
    .expect("the worker itself must not fail")
    .items
    .remove(0)
}
