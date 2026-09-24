//! Batch-level dry-run regression and lifecycle tests (T20 follow-up).
//!
//! Run identical batches against equally seeded databases and assert both expected
//! outcomes and parity. Normalize only predicted-success revision/version fields
//! (ADR-0012); compare statuses and refusal reasons exactly.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;

use sea_orm::EntityTrait;
use serde_json::{Value, json};
use time::OffsetDateTime;
use time::macros::datetime;
use toolkit_db::secure::SecureEntityExt;
use toolkit_db::{DBProvider, DbError, DbTx};
use toolkit_gts::gts_id;
use uuid::Uuid;

use types_registry::config::TypesRegistryConfig;
use types_registry::domain::admission::AdmissionFailureReason;
use types_registry::domain::admission::acceptance::{AcceptanceContext, AcceptanceError, accept};
use types_registry::domain::admission::worker::{ItemOutcome, Tuning, WorkerError, run_operation};
use types_registry::domain::admission::{Candidate, OperationDispatch, SubmitRequest};
use types_registry::domain::enums::{OperationItemStatus, OperationKind};
use types_registry::domain::policy::RegistrationPolicy;
use types_registry::infra::storage::entity::{
    coordination_state, dependency, entity, instance, instance_revision, type_schema,
    type_schema_revision, version_family,
};

mod common;
use common::{TestStores, allow_all, stores, test_db};

const NOW: OffsetDateTime = datetime!(2026-09-13 09:15:30 UTC);
const LATER: OffsetDateTime = datetime!(2026-09-13 10:20:40 UTC);

/// A new base and the referrer that consumes it, both created by one batch.
const BASE: &str = gts_id!("cf.core.dryb.base.v1~");
const REFERRER: &str = gts_id!("cf.core.dryb.holder.v1~");

/// Revise a concrete type to abstract, then submit an Instance of it.
/// `x-gts-abstract` passes JSON Schema compatibility while forbidding direct
/// Instances. Ordinary narrowing would fail compatibility; major-0 Instances
/// are forbidden by ADR-0015.
const NARROWING: &str = gts_id!("cf.core.dryb.narrow.v1~");
const SAMPLE: &str = gts_id!("cf.core.dryb.narrow.v1~cf.core.dryb.sample.v1");

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

// ---------------------------------------------------------------------------
// Documents
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

/// A Type Schema whose only property is a `$ref` at `target`.
fn referencing(gts_id: &str, target: &str) -> Value {
    let mut doc = schema(gts_id, "holder");
    doc["properties"] = json!({ "target": { "$ref": format!("gts://{target}") } });
    doc
}

/// The narrowing fixture: concrete at revision 1, abstract at revision 2, so an
/// Instance of it is valid under exactly one of them.
fn abstractable(gts_id: &str, is_abstract: bool) -> Value {
    json!({
        "$id": format!("gts://{gts_id}"),
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "x-gts-abstract": is_abstract,
    })
}

// ---------------------------------------------------------------------------
// Driving the public service
// ---------------------------------------------------------------------------

fn worker(db: &Provider) -> DBProvider<WorkerError> {
    DBProvider::new(db.db())
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
            idempotency_key: Some(key.to_owned()),
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

/// Accept and admit one batch, returning its per-candidate outcomes in
/// submission order.
async fn run_batch(
    db: &Provider,
    key: &str,
    kind: OperationKind,
    dry_run: bool,
    candidates: Vec<Candidate>,
) -> Vec<ItemOutcome> {
    let operation_id = submit(db, key, kind, dry_run, candidates).await;
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
    .items
}

// ---------------------------------------------------------------------------
// Assertions
// ---------------------------------------------------------------------------

/// The part of an outcome a dry run is supposed to predict exactly: which
/// candidate, what happened to it, and — when it failed — why.
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

fn expect(gts_id: &str, status: OperationItemStatus) -> Verdict {
    Verdict {
        gts_id: gts_id.to_owned(),
        status,
        reason: None,
    }
}

fn expect_failure(gts_id: &str, reason: AdmissionFailureReason) -> Verdict {
    Verdict {
        gts_id: gts_id.to_owned(),
        status: OperationItemStatus::Failed,
        reason: Some(reason),
    }
}

/// Dump all entity-state columns: family, entity, both revision/current tables,
/// edges and the write-order coordination row. `Debug` avoids omitting columns.
/// Pair final-state equality with [`EntityWriteSpy`] to reject write attempts too.
#[derive(Debug, PartialEq, Eq)]
struct EntityState {
    families: Vec<String>,
    entities: Vec<String>,
    schema_revisions: Vec<String>,
    current_schemas: Vec<String>,
    instance_revisions: Vec<String>,
    current_instances: Vec<String>,
    edges: Vec<String>,
    coordination: Vec<String>,
}

/// Read one table whole, as sorted `Debug` lines.
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
    EntityState {
        families: dump!(&conn, &scope, version_family::Entity),
        entities: dump!(&conn, &scope, entity::Entity),
        schema_revisions: dump!(&conn, &scope, type_schema_revision::Entity),
        current_schemas: dump!(&conn, &scope, type_schema::Entity),
        instance_revisions: dump!(&conn, &scope, instance_revision::Entity),
        current_instances: dump!(&conn, &scope, instance::Entity),
        edges: dump!(&conn, &scope, dependency::Entity),
        coordination: dump!(&conn, &scope, coordination_state::Entity),
    }
}

/// Check the field contract a predicted outcome carries (ADR-0012): a dry-run
/// `succeeded` names no revision and no resulting resource version, while an
/// `unchanged` still names the version that did not move.
fn assert_predicted_fields(items: &[ItemOutcome]) {
    for item in items {
        match item.status {
            OperationItemStatus::Succeeded => {
                assert_eq!(
                    (item.revision_no, item.resource_version),
                    (None, None),
                    "a predicted success allocated no revision and moved no version: {item:?}",
                );
                assert!(
                    item.gts_uuid.is_some(),
                    "a terminal success still carries the Registry Reference: {item:?}",
                );
            }
            OperationItemStatus::Unchanged => {
                assert_eq!(item.revision_no, None, "{item:?}");
                assert!(
                    item.resource_version.is_some(),
                    "an unchanged candidate reports the version that did not move: {item:?}",
                );
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// A new base and its referrer, in one batch
// ---------------------------------------------------------------------------

/// Regression: a referrer must see the base admitted earlier in the batch.
/// The one-connection `SQLite` pool also exposes nested-transaction deadlocks;
/// the concurrent-writer test separately verifies snapshot coherence.
#[tokio::test]
async fn a_dry_run_admits_a_referrer_whose_base_the_same_batch_creates() {
    let candidates = || {
        vec![
            creation(BASE, schema(BASE, "base")),
            creation(REFERRER, referencing(REFERRER, BASE)),
        ]
    };

    let dry_db = test_db().await;
    let before = entity_state(&dry_db).await;
    let dry = run_batch(
        &dry_db,
        "batch",
        OperationKind::Registration,
        true,
        candidates(),
    )
    .await;

    let real_db = test_db().await;
    let real = run_batch(
        &real_db,
        "batch",
        OperationKind::Registration,
        false,
        candidates(),
    )
    .await;

    // The real batch is the comparator, so it is asserted first: a red dry-run
    // assertion is only evidence if the thing it is compared against is known
    // to be what the contract says.
    let expected = vec![
        expect(BASE, OperationItemStatus::Succeeded),
        expect(REFERRER, OperationItemStatus::Succeeded),
    ];
    assert_eq!(
        verdicts(&real),
        expected,
        "the committing batch admits the referrer against the base it just created",
    );
    assert_eq!(
        verdicts(&dry),
        expected,
        "the referrer resolves against the base this batch is admitting",
    );
    assert_predicted_fields(&dry);
    assert_eq!(
        entity_state(&dry_db).await,
        before,
        "a dry run leaves every entity-state table exactly as it found it",
    );
}

// ---------------------------------------------------------------------------
// A narrowing revision and an Instance that only the old schema admits
// ---------------------------------------------------------------------------

/// Seed the concrete Type Schema, committed for real, so the batch under test
/// starts from identical committed state in both modes.
async fn seed_narrowing(db: &Provider) {
    let items = run_batch(
        db,
        "seed-schema",
        OperationKind::Registration,
        false,
        vec![creation(NARROWING, abstractable(NARROWING, false))],
    )
    .await;
    assert_eq!(items[0].status, OperationItemStatus::Succeeded, "{items:?}");
}

/// Regression: an Instance must see the preceding abstract revision and fail.
/// Discarding that virtual revision would falsely admit it against the old
/// concrete schema.
#[tokio::test]
async fn a_dry_run_refuses_an_instance_the_batchs_own_abstract_revision_invalidates() {
    let candidates = || {
        vec![
            revision(NARROWING, abstractable(NARROWING, true), 1),
            creation(SAMPLE, json!({})),
        ]
    };

    let dry_db = test_db().await;
    seed_narrowing(&dry_db).await;
    let before = entity_state(&dry_db).await;
    let dry = run_batch(
        &dry_db,
        "batch",
        OperationKind::Registration,
        true,
        candidates(),
    )
    .await;

    let real_db = test_db().await;
    seed_narrowing(&real_db).await;
    let real = run_batch(
        &real_db,
        "batch",
        OperationKind::Registration,
        false,
        candidates(),
    )
    .await;

    let expected = vec![
        expect(NARROWING, OperationItemStatus::Succeeded),
        expect_failure(SAMPLE, AdmissionFailureReason::InvalidValue),
    ];
    assert_eq!(
        verdicts(&real),
        expected,
        "the committing batch refuses the value against the revision it just made current",
    );
    assert_eq!(
        verdicts(&dry),
        expected,
        "the value is judged against the revision this batch made current",
    );
    assert_predicted_fields(&dry);
    assert_eq!(
        entity_state(&dry_db).await,
        before,
        "a dry run leaves every entity-state table exactly as it found it",
    );
}

// ---------------------------------------------------------------------------
// No entity-state write, checked at the attempt
// ---------------------------------------------------------------------------

/// Run one batch through ports that refuse every entity-state write.
async fn run_batch_with(
    db: &Provider,
    ports: &Arc<dyn types_registry::domain::ports::Stores>,
    key: &str,
    dry_run: bool,
    candidates: Vec<Candidate>,
) -> Result<Vec<ItemOutcome>, WorkerError> {
    let operation_id = submit(db, key, OperationKind::Registration, dry_run, candidates).await;
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
    .map(|outcome| outcome.items)
}

/// The requirement stated as the storage adapter sees it: not "the tables are
/// unchanged afterwards", which write-then-rollback also satisfies, but "no
/// entity-state write was attempted at all". The claim on `entity_write_order`
/// counts as one: a prediction must not take the write path's serialization
/// point.
#[tokio::test]
async fn a_dry_run_batch_attempts_no_entity_state_write() {
    let db = test_db().await;
    let spy = TestStores::forbidding_entity_writes();
    let ports: Arc<dyn types_registry::domain::ports::Stores> = Arc::clone(&spy) as _;

    let items = run_batch_with(
        &db,
        &ports,
        "batch",
        true,
        vec![
            creation(BASE, schema(BASE, "base")),
            creation(REFERRER, referencing(REFERRER, BASE)),
        ],
    )
    .await
    .expect("a dry run reaches no refused write, so the pass itself must not fail");

    assert_eq!(
        verdicts(&items),
        vec![
            expect(BASE, OperationItemStatus::Succeeded),
            expect(REFERRER, OperationItemStatus::Succeeded),
        ],
        "the prediction is unchanged by ports that would refuse a write: {items:?}",
    );
    assert_eq!(
        spy.entity_write_attempts(),
        Vec::<&str>::new(),
        "a dry run issued an entity-state write or claimed the write order",
    );
}

/// The control: the same spy over a **committing** batch records attempts, so
/// the assertion above is about the dry run rather than about a spy that never
/// sees anything.
#[tokio::test]
async fn the_write_spy_sees_a_committing_batch() {
    let db = test_db().await;
    let spy = TestStores::forbidding_entity_writes();
    let ports: Arc<dyn types_registry::domain::ports::Stores> = Arc::clone(&spy) as _;

    // The refused write fails the pass; what matters is that the spy saw it.
    drop(
        run_batch_with(
            &db,
            &ports,
            "batch",
            false,
            vec![creation(BASE, schema(BASE, "base"))],
        )
        .await,
    );

    assert_eq!(
        spy.entity_write_attempts().first(),
        Some(&"claim_entity_write_order"),
        "a committing pass claims the write order as its first statement",
    );
}

// ---------------------------------------------------------------------------
// One snapshot, released before the outcomes are published
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Publication is atomic with completion
// ---------------------------------------------------------------------------

/// Fail completion and require publication to roll back all item writes.
/// Terminalization clears payloads, so atomic completion must preserve them
/// on failure for redelivery to predict the whole batch again.
#[tokio::test]
async fn a_failed_completion_publishes_nothing_and_the_redelivery_still_predicts_the_batch() {
    let db = test_db().await;
    let candidates = vec![
        creation(BASE, schema(BASE, "base")),
        creation(REFERRER, referencing(REFERRER, BASE)),
    ];
    let operation_id = submit(
        &db,
        "batch",
        OperationKind::Registration,
        true,
        candidates.clone(),
    )
    .await;

    let failing: Arc<dyn types_registry::domain::ports::Stores> =
        TestStores::failing_completion() as _;
    let refused = run_operation(
        &failing,
        &worker(&db),
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
    .await;
    assert!(
        refused.is_err(),
        "a publication whose completion fails must fail the pass: {refused:?}",
    );

    let stored = stored_items(&db, operation_id).await;
    assert!(
        stored
            .iter()
            .all(|item| item.status == OperationItemStatus::Pending
                && item.request_payload.is_some()),
        "the failed publication rolled every item write back with the completion: {stored:?}",
    );

    let items = run_batch(&db, "batch", OperationKind::Registration, true, candidates).await;
    assert_eq!(
        verdicts(&items),
        vec![
            expect(BASE, OperationItemStatus::Succeeded),
            expect(REFERRER, OperationItemStatus::Succeeded),
        ],
        "the redelivery predicts the whole batch again: {items:?}",
    );
}

/// The stored item rows of one operation, in submission order.
async fn stored_items(
    db: &Provider,
    operation_id: Uuid,
) -> Vec<types_registry::domain::ports::OperationItemRow> {
    use types_registry::domain::ports::OperationStore;
    let provider = worker(db);
    provider
        .transaction(move |tx| {
            Box::pin(async move {
                Ok(types_registry::infra::storage::Repos
                    .find_items(tx, &allow_all(), operation_id)
                    .await?)
            })
        })
        .await
        .expect("read the stored items")
}

// ---------------------------------------------------------------------------
// One coherent base, under a concurrent writer
// ---------------------------------------------------------------------------

/// Two independent subjects, so the pass can be held inside its snapshot after
/// reading the first and before reading the second.
const SUBJECT_ONE: &str = gts_id!("cf.core.dryb.subjone.v1~");
const SUBJECT_TWO: &str = gts_id!("cf.core.dryb.subjtwo.v1~");

/// Pause prediction between candidates, then commit a revision of the second
/// candidate's subject. The prediction must still see version 1 and admit it;
/// a fresh snapshot would see version 2 and fail `precondition_failed`.
///
/// `SQLite` WAL lets the writer commit while the snapshot is held. The backend
/// suite repeats this under PostgreSQL/MySQL `REPEATABLE READ`.
#[tokio::test]
async fn a_dry_run_predicts_the_batch_against_the_state_it_started_from() {
    let dir = common::TestDir::new("types-registry-dry-run-snapshot");
    let db = common::test_db_file_wal(&dir.path().join("registry.db")).await;

    for (key, gts_id) in [("seed-one", SUBJECT_ONE), ("seed-two", SUBJECT_TWO)] {
        let seeded = run_batch(
            &db,
            key,
            OperationKind::Registration,
            false,
            vec![creation(gts_id, schema(gts_id, "first"))],
        )
        .await;
        assert_eq!(
            seeded[0].status,
            OperationItemStatus::Succeeded,
            "{seeded:?}"
        );
    }

    let operation_id = submit(
        &db,
        "dry",
        OperationKind::Registration,
        true,
        vec![
            revision(SUBJECT_ONE, schema(SUBJECT_ONE, "predicted"), 1),
            revision(SUBJECT_TWO, schema(SUBJECT_TWO, "predicted"), 1),
        ],
    )
    .await;

    // Hold the pass inside its snapshot, after the first candidate's read.
    let (paused, reached, resume) = TestStores::pausing(common::PausePoint::CurrentDocuments);
    let ports: Arc<dyn types_registry::domain::ports::Stores> = paused;
    let pass_db = Arc::clone(&db);
    let pass = tokio::spawn(async move { run_batch_on(&pass_db, &ports, operation_id).await });
    reached
        .await
        .expect("the pass reached its first current-document read");

    // A second connection revises the second subject for real, past the version
    // that candidate names — and **commits**, entirely while the pass is still
    // holding its snapshot. That is the whole point: the change is durable in
    // the database before the second candidate is evaluated.
    let deadline = std::time::Duration::from_secs(30);
    let writer_op = submit(
        &db,
        "concurrent",
        OperationKind::Registration,
        false,
        vec![revision(SUBJECT_TWO, schema(SUBJECT_TWO, "committed"), 1)],
    )
    .await;
    let committed = tokio::time::timeout(deadline, run_batch_on(&db, &stores(), writer_op))
        .await
        .expect("the concurrent write must not block on the held snapshot")
        .expect("the concurrent write must not fail");
    assert_eq!(
        committed[0].status,
        OperationItemStatus::Succeeded,
        "the concurrent revision really did land, before the pass resumed: {committed:?}",
    );
    assert_eq!(
        entity_version(&db, SUBJECT_TWO).await,
        2,
        "and it is visible to a reader that starts after it",
    );

    resume.send(()).expect("resume the paused pass");
    let predicted = tokio::time::timeout(deadline, pass)
        .await
        .expect("the prediction must not deadlock against the concurrent writer")
        .expect("pass task")
        .expect("the pass must not fail");
    assert_eq!(
        verdicts(&predicted),
        vec![
            expect(SUBJECT_ONE, OperationItemStatus::Succeeded),
            expect(SUBJECT_TWO, OperationItemStatus::Succeeded),
        ],
        "the second candidate was predicted against the version its snapshot held, \
         not against the one committed underneath it: {predicted:?}",
    );
}

/// The committed `resource_version` of one entity, read in its own transaction.
async fn entity_version(db: &Provider, gts_id: &str) -> i64 {
    use types_registry::domain::ports::EntityStore;
    let provider = worker(db);
    let gts_id = gts_id.to_owned();
    provider
        .transaction(move |tx| {
            Box::pin(async move {
                Ok(types_registry::infra::storage::Repos
                    .find_by_gts_id(tx, &allow_all(), &gts_id)
                    .await?)
            })
        })
        .await
        .expect("read the entity")
        .expect("the entity is admitted")
        .resource_version
}

/// Admit one already-accepted operation through the given ports.
async fn run_batch_on(
    db: &Provider,
    ports: &Arc<dyn types_registry::domain::ports::Stores>,
    operation_id: Uuid,
) -> Result<Vec<ItemOutcome>, WorkerError> {
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
    .map(|outcome| outcome.items)
}

// ---------------------------------------------------------------------------
// Two passes over one dry run
// ---------------------------------------------------------------------------

/// Overlapping passes are safe the same way the committing path's are: each
/// predicts the whole batch, and the item compare-and-swap decides which one's
/// outcomes are recorded. The loser reports what the winner stored, so the two
/// callers see one answer rather than two predictions of it.
#[tokio::test]
async fn two_passes_over_one_dry_run_report_the_same_outcomes() {
    let dir = common::TestDir::new("types-registry-dry-run-overlap");
    let db = common::test_db_file_wal(&dir.path().join("registry.db")).await;
    let before = entity_state(&db).await;

    let operation_id = submit(
        &db,
        "batch",
        OperationKind::Registration,
        true,
        vec![
            creation(BASE, schema(BASE, "base")),
            creation(REFERRER, referencing(REFERRER, BASE)),
        ],
    )
    .await;

    let left_db = Arc::clone(&db);
    let right_db = Arc::clone(&db);
    let left = tokio::spawn(async move { run_batch_on(&left_db, &stores(), operation_id).await });
    let right = tokio::spawn(async move { run_batch_on(&right_db, &stores(), operation_id).await });

    let deadline = std::time::Duration::from_secs(30);
    let left = tokio::time::timeout(deadline, left)
        .await
        .expect("neither pass may deadlock")
        .expect("left task")
        .expect("left pass");
    let right = tokio::time::timeout(deadline, right)
        .await
        .expect("neither pass may deadlock")
        .expect("right task")
        .expect("right pass");

    let expected = vec![
        expect(BASE, OperationItemStatus::Succeeded),
        expect(REFERRER, OperationItemStatus::Succeeded),
    ];
    assert_eq!(verdicts(&left), expected, "{left:?}");
    assert_eq!(
        verdicts(&right),
        expected,
        "the losing pass reports what the winner stored: {right:?}",
    );
    assert_predicted_fields(&left);
    assert_eq!(
        entity_state(&db).await,
        before,
        "two predictions still wrote no entity state",
    );
}

/// Pause pass one on an empty snapshot, then register `BASE` and let pass two
/// publish `already_exists` plus a blocked referrer. Release pass one: although
/// it predicts two successes, losing publication CAS must return pass two's
/// stored failures. Different predictions make stale-result returns observable.
#[tokio::test]
async fn a_pass_that_loses_publication_reports_the_stored_outcomes() {
    let dir = common::TestDir::new("types-registry-dry-run-publication");
    let db = common::test_db_file_wal(&dir.path().join("registry.db")).await;

    let operation_id = submit(
        &db,
        "batch",
        OperationKind::Registration,
        true,
        vec![
            creation(BASE, schema(BASE, "base")),
            creation(REFERRER, referencing(REFERRER, BASE)),
        ],
    )
    .await;

    // Hold the first pass inside a snapshot of the registry as it is now:
    // empty, so both candidates are admissible.
    let (paused, reached, resume) = TestStores::pausing(common::PausePoint::CurrentDocuments);
    let ports: Arc<dyn types_registry::domain::ports::Stores> = paused;
    let loser_db = Arc::clone(&db);
    let loser = tokio::spawn(async move { run_batch_on(&loser_db, &ports, operation_id).await });
    reached
        .await
        .expect("the first pass reached its first current-document read");

    // Register `BASE` for real, underneath the held snapshot.
    let deadline = std::time::Duration::from_secs(30);
    let registered = tokio::time::timeout(
        deadline,
        run_batch(
            &db,
            "committed-base",
            OperationKind::Registration,
            false,
            vec![creation(BASE, schema(BASE, "committed"))],
        ),
    )
    .await
    .expect("the real registration must not block on the held snapshot");
    assert_eq!(
        registered[0].status,
        OperationItemStatus::Succeeded,
        "{registered:?}"
    );
    let after_writer = entity_state(&db).await;

    // The second pass sees it, and publishes the refusals it earns.
    let winner = tokio::time::timeout(deadline, run_batch_on(&db, &stores(), operation_id))
        .await
        .expect("the winning pass must not block on the held snapshot")
        .expect("the winning pass");
    let expected = vec![
        expect_failure(BASE, AdmissionFailureReason::AlreadyExists),
        expect_failure(REFERRER, AdmissionFailureReason::BlockedByDependency),
    ];
    assert_eq!(
        verdicts(&winner),
        expected,
        "the second pass predicted against a registry that already holds the base: {winner:?}",
    );

    resume.send(()).expect("resume the held pass");
    let loser = tokio::time::timeout(deadline, loser)
        .await
        .expect("the losing pass must not deadlock")
        .expect("loser task")
        .expect("the losing pass still returns an outcome");

    assert_eq!(
        verdicts(&loser),
        expected,
        "the losing pass predicted two admissions against its older snapshot; what it \
         owes the caller is the outcome that was recorded: {loser:?}",
    );
    assert_eq!(
        entity_state(&db).await,
        after_writer,
        "and neither dry-run pass wrote entity state of its own",
    );
}
