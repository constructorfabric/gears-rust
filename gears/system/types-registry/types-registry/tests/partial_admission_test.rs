//! Dependency-aware partial admission (T19): dependency order, downstream-only
//! blocking, independent progress and cycle refusal before writes.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;

use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
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
use types_registry::domain::admission::worker::{
    ItemOutcome, OperationOutcome, Tuning, WorkerError, run_operation,
};
use types_registry::domain::admission::{Candidate, OperationDispatch, SubmitRequest};
use types_registry::domain::enums as domain_enums;
use types_registry::domain::enums::OperationItemStatus;
use types_registry::domain::policy::RegistrationPolicy;
use types_registry::domain::ports::{CurrentTypeSchemaRow, EntityRow};
use types_registry::infra::storage::entity::dependency;
use types_registry::infra::storage::repo::{EntityRepo, TypeSchemaRepo};

mod common;
use common::{allow_all, stores, test_db};

const NOW: OffsetDateTime = datetime!(2026-09-11 09:15:30 UTC);
const LATER: OffsetDateTime = datetime!(2026-09-11 10:20:40 UTC);

const BASE: &str = gts_id!("cf.core.batch.base.v1~");
const DERIVED: &str = gts_id!("cf.core.batch.base.v1~cf.core.batch.leaf.v1~");
const REFERRER: &str = gts_id!("cf.core.batch.referrer.v1~");
const MIDDLE: &str = gts_id!("cf.core.batch.middle.v1~");
const STANDALONE: &str = gts_id!("cf.core.batch.standalone.v1~");
const BROKEN: &str = gts_id!("cf.core.batch.broken.v1~");
const ABSENT: &str = gts_id!("cf.core.batch.absent.v1~");
const V1_0: &str = gts_id!("cf.core.batch.minor.v1.0~");
const V1_1: &str = gts_id!("cf.core.batch.minor.v1.1~");

type Provider = Arc<DBProvider<DbError>>;

struct NoDispatch;

#[async_trait::async_trait]
impl OperationDispatch for NoDispatch {
    async fn enqueue(&self, _tx: &DbTx<'_>, _operation_id: Uuid) -> anyhow::Result<()> {
        Ok(())
    }
}

fn worker(db: &Provider) -> DBProvider<WorkerError> {
    DBProvider::new(db.db())
}

fn schema(id: &str) -> Value {
    json!({
        "$id": format!("gts://{id}"),
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "properties": { "name": { "type": "string" } },
    })
}

/// A schema carrying a marker annotation, so the document a reference resolved
/// to is visible in the referrer's inlined artifacts.
fn schema_titled(id: &str, marker: &str) -> Value {
    json!({
        "$id": format!("gts://{id}"),
        "$schema": "http://json-schema.org/draft-07/schema#",
        "title": marker,
        "type": "object",
        "properties": { "name": { "type": "string" } },
    })
}

/// The same shape with its properties removed — a narrowing revision, refused
/// against its own current definition.
fn schema_without_properties(id: &str) -> Value {
    json!({
        "$id": format!("gts://{id}"),
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "properties": {},
        "additionalProperties": false,
    })
}

/// A schema whose `allOf` inlines `target`, so the reference must resolve before
/// the candidate has any effective form at all.
fn refs(id: &str, target: &str) -> Value {
    json!({
        "$id": format!("gts://{id}"),
        "$schema": "http://json-schema.org/draft-07/schema#",
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

fn revise(gts_id: &str, content: Value, expected: i64) -> Candidate {
    Candidate {
        gts_id: gts_id.to_owned(),
        content: Some(content),
        expected_resource_version: Some(expected),
        force: false,
    }
}

async fn submit(db: &Provider, key: &str, candidates: Vec<Candidate>) -> Uuid {
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
            idempotency_key: key.to_owned(),
            kind: domain_enums::OperationKind::Registration,
            dry_run: false,
            candidates,
        },
        NOW,
    )
    .await
    .expect("the batch is accepted")
    .operation_id
}

async fn admit_batch(db: &Provider, key: &str, candidates: Vec<Candidate>) -> OperationOutcome {
    let operation_id = submit(db, key, candidates).await;
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

fn item<'a>(outcome: &'a OperationOutcome, gts_id: &str) -> &'a ItemOutcome {
    outcome
        .items
        .iter()
        .find(|item| item.gts_id == gts_id)
        .unwrap_or_else(|| panic!("the operation owes {gts_id} an outcome"))
}

#[track_caller]
fn succeeded(outcome: &OperationOutcome, gts_id: &str) {
    let item = item(outcome, gts_id);
    assert_eq!(
        item.status,
        OperationItemStatus::Succeeded,
        "{gts_id} must be admitted, got {:?}",
        item.failure,
    );
}

#[track_caller]
fn failed_with(outcome: &OperationOutcome, gts_id: &str, reason: &AdmissionFailureReason) {
    let item = item(outcome, gts_id);
    assert_eq!(
        item.status,
        OperationItemStatus::Failed,
        "{gts_id} must fail"
    );
    assert_eq!(
        &item
            .failure
            .as_ref()
            .expect("a failed item carries one")
            .reason,
        reason,
        "{gts_id} must fail for the named reason",
    );
}

async fn entity_of(db: &Provider, gts_id: &str) -> Option<EntityRow> {
    let provider = worker(db);
    let conn = provider.conn().expect("conn");
    EntityRepo::find_by_gts_id(&conn, &allow_all(), gts_id)
        .await
        .expect("read")
}

/// No `#[track_caller]`: it is a no-op on an async function, so the assertion
/// names the identifier instead of relying on the caller's line.
async fn no_entity(db: &Provider, gts_id: &str) {
    assert!(
        entity_of(db, gts_id).await.is_none(),
        "{gts_id} must have written no entity row",
    );
}

async fn current(db: &Provider, gts_id: &str) -> CurrentTypeSchemaRow {
    let entity_id = entity_of(db, gts_id)
        .await
        .unwrap_or_else(|| panic!("{gts_id} has no entity row"))
        .id;
    let provider = worker(db);
    let conn = provider.conn().expect("conn");
    TypeSchemaRepo::find_current(&conn, &allow_all(), entity_id)
        .await
        .expect("read")
        .unwrap_or_else(|| panic!("{gts_id} must have a current row"))
}

/// The identifiers this entity's stored outgoing edges point at.
async fn outgoing_targets(db: &Provider, gts_id: &str) -> Vec<String> {
    let scope = allow_all();
    let from = entity_of(db, gts_id)
        .await
        .unwrap_or_else(|| panic!("{gts_id} has no entity row"))
        .id;
    let provider = worker(db);
    let conn = provider.conn().expect("conn");
    let rows = dependency::Entity::find()
        .filter(dependency::Column::FromEntityId.eq(from))
        .secure()
        .scope_with(&scope)
        .all(&conn)
        .await
        .expect("read edges");
    let ids: Vec<i64> = rows.iter().map(|row| row.to_entity_id).collect();
    let mut targets: Vec<String> = EntityRepo::find_by_ids(&conn, &scope, &ids)
        .await
        .expect("read edge targets")
        .into_iter()
        .map(|row| row.gts_id)
        .collect();
    targets.sort();
    targets
}

// ---------------------------------------------------------------------------
// Ordering
// ---------------------------------------------------------------------------

/// The whole point of the task: the batch is processed in dependency order, not
/// in submission order. Listed dependent-first, so `item_no` order refuses it.
#[tokio::test]
async fn a_ref_target_submitted_after_its_dependent_is_still_admitted_first() {
    let db = test_db().await;
    let outcome = admit_batch(
        &db,
        "k-order",
        vec![
            create(REFERRER, refs(REFERRER, BASE)),
            create(BASE, schema_titled(BASE, "in-batch")),
        ],
    )
    .await;

    succeeded(&outcome, BASE);
    succeeded(&outcome, REFERRER);
    assert_eq!(
        outgoing_targets(&db, REFERRER).await,
        vec![BASE.to_owned()],
        "the in-batch reference is a stored edge like any other",
    );
    let resolved = current(&db, REFERRER).await.resolved_schema;
    assert!(
        resolved.contains("in-batch"),
        "the referrer inlines the candidate submitted beside it, got {resolved}",
    );
}

/// A derived Type Schema and its base in one batch, submitted derived-first.
#[tokio::test]
async fn a_derivation_base_submitted_after_its_derived_type_is_admitted_first() {
    let db = test_db().await;
    let outcome = admit_batch(
        &db,
        "k-derive",
        vec![create(DERIVED, schema(DERIVED)), create(BASE, schema(BASE))],
    )
    .await;

    succeeded(&outcome, BASE);
    succeeded(&outcome, DERIVED);
}

/// The implicit `vM.(n-1)~ -> vM.n~` edge orders the batch and is **not** stored:
/// a predecessor edge in `dependency` would forbid deleting `v1.0~` while
/// `v1.1~` exists.
#[tokio::test]
async fn a_minor_pair_is_ordered_by_an_edge_that_is_never_stored() {
    let db = test_db().await;
    let outcome = admit_batch(
        &db,
        "k-minor",
        vec![create(V1_1, schema(V1_1)), create(V1_0, schema(V1_0))],
    )
    .await;

    succeeded(&outcome, V1_0);
    succeeded(&outcome, V1_1);
    assert!(
        outgoing_targets(&db, V1_1).await.is_empty(),
        "the predecessor edge is an ordering edge only",
    );
}

// ---------------------------------------------------------------------------
// Partial admission
// ---------------------------------------------------------------------------

/// One failure does not fail the batch. Nothing connects these two candidates,
/// so the independent one commits.
#[tokio::test]
async fn an_independent_branch_commits_while_another_candidate_fails() {
    let db = test_db().await;
    let outcome = admit_batch(
        &db,
        "k-partial",
        vec![
            create(BROKEN, refs(BROKEN, ABSENT)),
            create(STANDALONE, schema(STANDALONE)),
        ],
    )
    .await;

    failed_with(
        &outcome,
        BROKEN,
        &AdmissionFailureReason::DependencyNotFound,
    );
    succeeded(&outcome, STANDALONE);
    no_entity(&db, BROKEN).await;
}

#[tokio::test]
async fn a_dependent_of_a_failed_candidate_is_blocked_by_dependency() {
    let db = test_db().await;
    let outcome = admit_batch(
        &db,
        "k-blocked",
        vec![
            create(BROKEN, refs(BROKEN, ABSENT)),
            create(REFERRER, refs(REFERRER, BROKEN)),
        ],
    )
    .await;

    failed_with(
        &outcome,
        BROKEN,
        &AdmissionFailureReason::DependencyNotFound,
    );
    failed_with(
        &outcome,
        REFERRER,
        &AdmissionFailureReason::BlockedByDependency,
    );
    no_entity(&db, REFERRER).await;
}

/// Blocking is transitive without being computed transitively: a blocked
/// candidate is itself failed, so its own dependents find a failed blocker.
#[tokio::test]
async fn blocking_reaches_the_whole_downstream_of_one_failure() {
    let db = test_db().await;
    let outcome = admit_batch(
        &db,
        "k-chain",
        vec![
            create(REFERRER, refs(REFERRER, MIDDLE)),
            create(MIDDLE, refs(MIDDLE, BROKEN)),
            create(BROKEN, refs(BROKEN, ABSENT)),
        ],
    )
    .await;

    failed_with(
        &outcome,
        BROKEN,
        &AdmissionFailureReason::DependencyNotFound,
    );
    failed_with(
        &outcome,
        MIDDLE,
        &AdmissionFailureReason::BlockedByDependency,
    );
    failed_with(
        &outcome,
        REFERRER,
        &AdmissionFailureReason::BlockedByDependency,
    );
    no_entity(&db, MIDDLE).await;
    no_entity(&db, REFERRER).await;
}

/// A failed lower minor is a different operator problem from a failed selected
/// dependency, so it carries a different reason.
#[tokio::test]
async fn a_later_minor_whose_predecessor_failed_is_blocked_by_predecessor() {
    let db = test_db().await;
    let outcome = admit_batch(
        &db,
        "k-pred",
        vec![create(V1_0, refs(V1_0, ABSENT)), create(V1_1, schema(V1_1))],
    )
    .await;

    failed_with(&outcome, V1_0, &AdmissionFailureReason::DependencyNotFound);
    failed_with(
        &outcome,
        V1_1,
        &AdmissionFailureReason::BlockedByPredecessor,
    );
    no_entity(&db, V1_1).await;
}

// ---------------------------------------------------------------------------
// The candidate overlay
// ---------------------------------------------------------------------------

/// A refused in-batch revision must block its referrer even when the stored
/// revision is resolvable. Artifact equality cannot prove this: dependent
/// refresh may produce identical artifacts under either ordering.
#[tokio::test]
async fn an_in_batch_reference_never_resolves_against_the_committed_revision() {
    let db = test_db().await;
    succeeded(
        &admit_batch(&db, "k-seed", vec![create(BASE, schema(BASE))]).await,
        BASE,
    );

    let outcome = admit_batch(
        &db,
        "k-overlay",
        vec![
            create(REFERRER, refs(REFERRER, BASE)),
            // Dropping a property narrows the accepted value set, so the
            // revision is refused against its own current definition.
            revise(BASE, schema_without_properties(BASE), 1),
        ],
    )
    .await;

    failed_with(
        &outcome,
        BASE,
        &AdmissionFailureReason::IncompatibleWithBaseline,
    );
    failed_with(
        &outcome,
        REFERRER,
        &AdmissionFailureReason::BlockedByDependency,
    );
    no_entity(&db, REFERRER).await;
}

// ---------------------------------------------------------------------------
// Cycles
// ---------------------------------------------------------------------------

/// The overlay is what makes a cycle reachable: each candidate sees the other,
/// so nothing before the ordering has refused the pair.
#[tokio::test]
async fn a_ref_cycle_between_two_candidates_refuses_both() {
    let db = test_db().await;
    let outcome = admit_batch(
        &db,
        "k-cycle",
        vec![
            create(BASE, refs(BASE, REFERRER)),
            create(REFERRER, refs(REFERRER, BASE)),
        ],
    )
    .await;

    failed_with(&outcome, BASE, &AdmissionFailureReason::InvalidSchema);
    failed_with(&outcome, REFERRER, &AdmissionFailureReason::InvalidSchema);
    no_entity(&db, BASE).await;
    no_entity(&db, REFERRER).await;
}

/// A `$ref`-only cycle check would order this batch and admit it: derivation
/// supplies the return edge.
#[tokio::test]
async fn a_cycle_mixing_a_ref_with_derivation_refuses_both() {
    let db = test_db().await;
    let outcome = admit_batch(
        &db,
        "k-mixed",
        vec![
            create(BASE, refs(BASE, DERIVED)),
            create(DERIVED, schema(DERIVED)),
        ],
    )
    .await;

    failed_with(&outcome, BASE, &AdmissionFailureReason::InvalidSchema);
    failed_with(&outcome, DERIVED, &AdmissionFailureReason::InvalidSchema);
    no_entity(&db, BASE).await;
    no_entity(&db, DERIVED).await;
}

#[tokio::test]
async fn a_self_referential_ref_refuses_its_candidate() {
    let db = test_db().await;
    let outcome = admit_batch(&db, "k-self", vec![create(BASE, refs(BASE, BASE))]).await;

    failed_with(&outcome, BASE, &AdmissionFailureReason::InvalidSchema);
    no_entity(&db, BASE).await;
}

/// A cycle closed by a revision leaves the revised entity exactly as it was: no
/// new revision, no moved version, and no new outgoing edge.
#[tokio::test]
async fn a_cycle_closed_by_a_revision_leaves_the_committed_entity_untouched() {
    let db = test_db().await;
    succeeded(
        &admit_batch(&db, "k-seed", vec![create(BASE, schema(BASE))]).await,
        BASE,
    );
    let before = current(&db, BASE).await;
    let before_version = entity_of(&db, BASE).await.expect("seeded").resource_version;

    let outcome = admit_batch(
        &db,
        "k-revise-cycle",
        vec![
            revise(BASE, refs(BASE, REFERRER), 1),
            create(REFERRER, refs(REFERRER, BASE)),
        ],
    )
    .await;

    failed_with(&outcome, BASE, &AdmissionFailureReason::InvalidSchema);
    failed_with(&outcome, REFERRER, &AdmissionFailureReason::InvalidSchema);
    no_entity(&db, REFERRER).await;

    let after = current(&db, BASE).await;
    assert_eq!(after.revision_no, before.revision_no);
    assert_eq!(after.resolved_schema, before.resolved_schema);
    assert_eq!(
        entity_of(&db, BASE)
            .await
            .expect("still there")
            .resource_version,
        before_version,
    );
    assert!(
        outgoing_targets(&db, BASE).await.is_empty(),
        "a refused revision writes no outgoing edge",
    );
}

// ---------------------------------------------------------------------------
// Redelivery
// ---------------------------------------------------------------------------

/// A second pass over a batch that partially committed reports the stored
/// outcomes and writes nothing — the property at-least-once delivery makes
/// load-bearing (T21).
#[tokio::test]
async fn a_second_pass_over_a_partially_committed_batch_is_a_no_op() {
    let db = test_db().await;
    let operation_id = submit(
        &db,
        "k-redeliver",
        vec![
            create(BROKEN, refs(BROKEN, ABSENT)),
            create(REFERRER, refs(REFERRER, BROKEN)),
            create(STANDALONE, schema(STANDALONE)),
        ],
    )
    .await;
    let limits = common::limits();
    let worker_settings = common::worker_settings();
    let metrics = common::metrics();
    let tuning = Tuning {
        limits: &limits,
        worker: &worker_settings,
        metrics: &metrics,
        allow_compatibility_force: false,
    };
    let first = run_operation(
        &stores(),
        &worker(&db),
        &allow_all(),
        tuning,
        operation_id,
        LATER,
    )
    .await
    .expect("first pass");
    let second = run_operation(
        &stores(),
        &worker(&db),
        &allow_all(),
        tuning,
        operation_id,
        LATER,
    )
    .await
    .expect("second pass");

    assert!(!first.already_terminal);
    assert!(second.already_terminal);
    let statuses = |outcome: &OperationOutcome| {
        let mut pairs: Vec<(String, String)> = outcome
            .items
            .iter()
            .map(|item| (item.gts_id.clone(), format!("{:?}", item.status)))
            .collect();
        pairs.sort();
        pairs
    };
    assert_eq!(statuses(&first), statuses(&second));
    assert_eq!(current(&db, STANDALONE).await.revision_no, 1);
}
