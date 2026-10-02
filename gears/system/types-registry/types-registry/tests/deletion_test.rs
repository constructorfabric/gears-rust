//! Deletion safety (T20, SPEC §8.1 step 4, DESIGN §3.7).
//! Tombstones remain readable and usable as baselines; live direct dependants
//! block deletion under the admission write-order claim.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;
use types_registry::domain::ports::ListFilter;

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
use types_registry::domain::enums as domain_enums;
use types_registry::domain::enums::{LifecycleStatus, OperationItemStatus, OperationKind};
use types_registry::domain::policy::RegistrationPolicy;
use types_registry::domain::ports::EntityRow;
use types_registry::domain::ports::OperationItemRow;
use types_registry::infra::storage::repo::{
    CoordinationStateRepo, EntityRepo, OperationRepo, PageRequest,
};

mod common;
use common::{allow_all, stores, test_db};

const NOW: OffsetDateTime = datetime!(2026-09-11 09:15:30 UTC);
const LATER: OffsetDateTime = datetime!(2026-09-11 10:20:40 UTC);

const TARGET: &str = gts_id!("cf.core.del.target.v1~");
const OTHER: &str = gts_id!("cf.core.del.other.v1~");
const HOLDER: &str = gts_id!("cf.core.del.holder.v1~");
const MIDDLE: &str = gts_id!("cf.core.del.middle.v1~");
const ABSENT: &str = gts_id!("cf.core.del.absent.v1~");

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

fn schema(gts_id: &str) -> Value {
    json!({
        "$id": format!("gts://{gts_id}"),
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "properties": { "name": { "type": "string" } },
    })
}

/// Equivalent holder schemas differing only in `$ref` versus `x-gts-ref`.
/// Both use `type: "string"`, as required by `x-gts-ref`.
fn holder(gts_id: &str, keyword: &str, value: &Value) -> Value {
    let mut doc = schema(gts_id);
    doc["properties"] = json!({ "target": { "type": "string", keyword: value } });
    doc
}

/// A real dependency edge: the schema inlines the target.
fn referencing(gts_id: &str, target: &str) -> Value {
    holder(gts_id, "$ref", &json!(format!("gts://{target}")))
}

/// The same shape with `x-gts-ref`, which is an instance-value constraint and
/// creates **no** dependency edge (T18).
fn constraining(gts_id: &str, pattern: &str) -> Value {
    holder(gts_id, "x-gts-ref", &json!(pattern))
}

async fn submit(
    db: &Provider,
    key: &str,
    kind: OperationKind,
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
            dry_run: false,
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

/// Register one schema and assert it landed.
async fn register(db: &Provider, key: &str, gts_id: &str, content: Value) {
    let op = submit(
        db,
        key,
        OperationKind::Registration,
        vec![Candidate {
            gts_id: gts_id.to_owned(),
            content: Some(content),
            expected_resource_version: None,
            force: false,
        }],
    )
    .await
    .expect("accepted");
    let outcome = run(db, op).await;
    assert_eq!(
        outcome.items[0].status,
        OperationItemStatus::Succeeded,
        "{gts_id} must register: {:?}",
        outcome.items[0].failure,
    );
}

async fn delete(db: &Provider, key: &str, gts_id: &str, expected: i64) -> ItemOutcome {
    let op = submit(
        db,
        key,
        OperationKind::Deletion,
        vec![Candidate {
            gts_id: gts_id.to_owned(),
            content: None,
            expected_resource_version: Some(expected),
            force: false,
        }],
    )
    .await
    .expect("a well-formed deletion is accepted");
    run(db, op).await.items.remove(0)
}

async fn entity_of(db: &Provider, gts_id: &str) -> Option<EntityRow> {
    let provider = worker(db);
    let conn = provider.conn().expect("conn");
    EntityRepo::find_by_gts_id(&conn, &allow_all(), gts_id)
        .await
        .expect("read")
}

async fn items_of(db: &Provider, operation_id: Uuid) -> Vec<OperationItemRow> {
    let provider = worker(db);
    let conn = provider.conn().expect("conn");
    OperationRepo::find_items(&conn, &allow_all(), operation_id)
        .await
        .expect("read the operation items")
}

async fn listed_ids(db: &Provider) -> Vec<String> {
    let provider = worker(db);
    let conn = provider.conn().expect("conn");
    EntityRepo::list_page(
        &conn,
        &allow_all(),
        &ListFilter::default(),
        PageRequest::first(100),
    )
    .await
    .expect("list")
    .items
    .into_iter()
    .map(|row| row.gts_id)
    .collect()
}

async fn entity_write_sequence(db: &Provider) -> i64 {
    let conn = db.conn().expect("conn");
    CoordinationStateRepo::entity_write_sequence(&conn, &allow_all())
        .await
        .expect("the migration seeds the state row")
}

fn item<'a>(outcome: &'a OperationOutcome, gts_id: &str) -> &'a ItemOutcome {
    outcome
        .items
        .iter()
        .find(|item| item.gts_id == gts_id)
        .unwrap_or_else(|| panic!("the operation owes {gts_id} an outcome"))
}

#[track_caller]
fn refused(item: &ItemOutcome, reason: &AdmissionFailureReason) {
    assert_eq!(item.status, OperationItemStatus::Failed, "{item:?}");
    assert_eq!(
        &item
            .failure
            .as_ref()
            .expect("a failed item carries one")
            .reason,
        reason,
        "{item:?}",
    );
}

// ---------------------------------------------------------------------------
// The happy path
// ---------------------------------------------------------------------------

#[tokio::test]
async fn deleting_an_active_entity_tombstones_it_and_moves_its_version() {
    let db = test_db().await;
    register(&db, "reg", TARGET, schema(TARGET)).await;

    let item = delete(&db, "del", TARGET, 1).await;

    assert_eq!(item.status, OperationItemStatus::Succeeded);
    assert_eq!(item.resource_version, Some(2));
    assert_eq!(
        item.revision_no, None,
        "a deletion allocates no revision (ADR-0005)",
    );
    let row = entity_of(&db, TARGET).await.expect("the row survives");
    assert_eq!(row.lifecycle_status, LifecycleStatus::Deleted);
    assert_eq!(row.resource_version, 2);
}

/// A tombstone is still exact-readable — it remains a compatibility baseline
/// until purge — and absent from any list.
#[tokio::test]
async fn a_deleted_entity_is_exact_readable_and_absent_from_lists() {
    let db = test_db().await;
    register(&db, "reg", TARGET, schema(TARGET)).await;
    register(&db, "reg-other", OTHER, schema(OTHER)).await;
    delete(&db, "del", TARGET, 1).await;

    assert!(
        entity_of(&db, TARGET).await.is_some(),
        "an exact read still finds the tombstone",
    );
    assert_eq!(
        listed_ids(&db).await,
        vec![OTHER.to_owned()],
        "a list shows only what is still active",
    );
}

/// Deletion is a write against the entity row, and it must take its place in the
/// same total order admission takes — or that order stops being total.
#[tokio::test]
async fn a_deletion_claims_the_entity_write_order_row_exactly_once() {
    let db = test_db().await;
    register(&db, "reg", TARGET, schema(TARGET)).await;

    let before = entity_write_sequence(&db).await;
    delete(&db, "del", TARGET, 1).await;
    assert_eq!(
        entity_write_sequence(&db).await - before,
        1,
        "a deletion claims the row exactly once",
    );
}

// ---------------------------------------------------------------------------
// Dependants
// ---------------------------------------------------------------------------

/// The count is reported; the identities are not. Naming them would leak what a
/// caller may not be entitled to read.
#[tokio::test]
async fn a_live_direct_dependant_refuses_the_deletion_and_reports_a_count() {
    let db = test_db().await;
    register(&db, "reg", TARGET, schema(TARGET)).await;
    register(&db, "reg-holder", HOLDER, referencing(HOLDER, TARGET)).await;

    let item = delete(&db, "del", TARGET, 1).await;

    refused(&item, &AdmissionFailureReason::HasRegisteredDependents);
    let message = &item.failure.as_ref().expect("failure").message;
    assert!(
        message.contains('1'),
        "the refusal reports how many dependants block it: {message}",
    );
    assert!(!message.contains(HOLDER), "and never which ones: {message}");
    assert_eq!(
        entity_of(&db, TARGET).await.expect("row").lifecycle_status,
        LifecycleStatus::Active,
        "a refused deletion leaves the entity untouched",
    );
}

/// Only **direct** dependants block. A transitive one is separated from the
/// target by an entity that is itself still resolvable.
#[tokio::test]
async fn a_transitive_dependant_does_not_block() {
    let db = test_db().await;
    register(&db, "reg", TARGET, schema(TARGET)).await;
    register(&db, "reg-middle", MIDDLE, referencing(MIDDLE, TARGET)).await;
    register(&db, "reg-holder", HOLDER, referencing(HOLDER, MIDDLE)).await;

    // `MIDDLE` is a direct dependant of `TARGET`, so delete it first.
    assert_eq!(
        delete(&db, "del-middle", MIDDLE, 1).await.status,
        OperationItemStatus::Failed,
        "HOLDER depends on MIDDLE directly, so MIDDLE is blocked",
    );
    // `HOLDER` depends on `TARGET` only transitively, through `MIDDLE`.
    let item = delete(&db, "del-target", TARGET, 1).await;
    refused(&item, &AdmissionFailureReason::HasRegisteredDependents);
    // `MIDDLE` alone explains this refusal. Asserting the *count* is what
    // separates the two readings: a blocking rule that walked the closure would
    // report both `MIDDLE` and `HOLDER`, and `1` would fail.
    let message = &item.failure.as_ref().expect("failure").message;
    assert!(
        message.contains("has 1 live direct registered dependants"),
        "only the direct dependant is counted, not the transitive one: {message}",
    );
}

/// `x-gts-ref` is an instance-value constraint and creates no edge (T18), so
/// there is no registered dependant to find. The paired `$ref` case above is
/// otherwise identical, which is what makes this the distinction and not a
/// coincidence.
#[tokio::test]
async fn an_x_gts_ref_holder_does_not_block_and_stays_readable() {
    let db = test_db().await;
    register(&db, "reg", TARGET, schema(TARGET)).await;
    register(&db, "reg-holder", HOLDER, constraining(HOLDER, TARGET)).await;

    let item = delete(&db, "del", TARGET, 1).await;

    assert_eq!(
        item.status,
        OperationItemStatus::Succeeded,
        "the keyword names an entity without depending on it: {:?}",
        item.failure,
    );
    assert!(
        entity_of(&db, HOLDER).await.is_some(),
        "and the holder is unaffected",
    );
}

// ---------------------------------------------------------------------------
// Preconditions and lifecycle
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_stale_precondition_is_refused() {
    let db = test_db().await;
    register(&db, "reg", TARGET, schema(TARGET)).await;

    let item = delete(&db, "del", TARGET, 99).await;

    refused(&item, &AdmissionFailureReason::PreconditionFailed);
    assert_eq!(
        entity_of(&db, TARGET).await.expect("row").lifecycle_status,
        LifecycleStatus::Active,
    );
}

#[tokio::test]
async fn deleting_an_identifier_the_registry_does_not_hold_is_refused() {
    let db = test_db().await;
    let item = delete(&db, "del", ABSENT, 1).await;
    refused(&item, &AdmissionFailureReason::PreconditionFailed);
    assert!(entity_of(&db, ABSENT).await.is_none());
}

/// A tombstone is refused for **being** a tombstone, before its version is
/// looked at: a second deletion must never read as "retry with a newer version".
#[tokio::test]
async fn deleting_a_tombstone_is_refused_as_not_active() {
    let db = test_db().await;
    register(&db, "reg", TARGET, schema(TARGET)).await;
    delete(&db, "del", TARGET, 1).await;

    let item = delete(&db, "del-again", TARGET, 2).await;

    refused(&item, &AdmissionFailureReason::NotActive);
    assert_eq!(
        entity_of(&db, TARGET).await.expect("row").resource_version,
        2,
        "the refused second deletion moved nothing",
    );
}

// ---------------------------------------------------------------------------
// Redelivery
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_second_pass_over_a_completed_deletion_is_a_no_op() {
    let db = test_db().await;
    register(&db, "reg", TARGET, schema(TARGET)).await;
    let op = submit(
        &db,
        "del",
        OperationKind::Deletion,
        vec![Candidate {
            gts_id: TARGET.to_owned(),
            content: None,
            expected_resource_version: Some(1),
            force: false,
        }],
    )
    .await
    .expect("accepted");

    let first = run(&db, op).await;
    let second = run(&db, op).await;

    assert!(!first.already_terminal);
    assert!(second.already_terminal);
    assert_eq!(first.items[0].status, OperationItemStatus::Succeeded);
    assert_eq!(second.items[0].status, OperationItemStatus::Succeeded);
    assert_eq!(
        entity_of(&db, TARGET).await.expect("row").resource_version,
        2,
        "the redelivered pass wrote nothing",
    );
}

/// A dry run and a commit under one key are two different requests, because the
/// mode is part of the fingerprint.
#[tokio::test]
async fn the_same_key_for_a_dry_run_and_a_commit_is_a_conflict_not_a_replay() {
    let db = test_db().await;
    register(&db, "reg", TARGET, schema(TARGET)).await;

    let provider: DBProvider<AcceptanceError> = DBProvider::new(db.db());
    let policy = RegistrationPolicy::default();
    let config = TypesRegistryConfig::default();
    let dispatch: Arc<dyn OperationDispatch> = Arc::new(NoDispatch);
    let request = |dry_run: bool| SubmitRequest {
        idempotency_key: Some("one-key".to_owned()),
        kind: domain_enums::OperationKind::Deletion,
        dry_run,
        candidates: vec![Candidate {
            gts_id: TARGET.to_owned(),
            content: None,
            expected_resource_version: Some(1),
            force: false,
        }],
    };
    let context = AcceptanceContext {
        policy: &policy,
        config: &config,
        metrics: &common::metrics(),
    };

    accept(
        &stores(),
        &provider,
        &allow_all(),
        &context,
        &dispatch,
        &request(true),
        NOW,
    )
    .await
    .expect("the dry run is accepted");

    let conflict = accept(
        &stores(),
        &provider,
        &allow_all(),
        &context,
        &dispatch,
        &request(false),
        NOW,
    )
    .await;

    assert!(
        matches!(conflict, Err(AcceptanceError::FingerprintConflict { .. })),
        "a commit is not a replay of the dry run that preceded it: {conflict:?}",
    );
}

// ---------------------------------------------------------------------------
// Batch order (T20)
// ---------------------------------------------------------------------------

async fn delete_batch(db: &Provider, key: &str, targets: &[(&str, i64)]) -> OperationOutcome {
    let candidates = targets
        .iter()
        .map(|(gts_id, expected)| Candidate {
            gts_id: (*gts_id).to_owned(),
            content: None,
            expected_resource_version: Some(*expected),
            force: false,
        })
        .collect();
    let op = submit(db, key, OperationKind::Deletion, candidates)
        .await
        .expect("accepted");
    run(db, op).await
}

#[track_caller]
fn all_succeeded(outcome: &OperationOutcome) {
    for item in &outcome.items {
        assert_eq!(
            item.status,
            OperationItemStatus::Succeeded,
            "{} must be deleted: {:?}",
            item.gts_id,
            item.failure,
        );
    }
}

/// A deletion batch orders by the **reverse** relation: the dependant goes
/// first, or the target is refused for a dependant the same batch was about to
/// remove. Submitted target-first, which is the order that fails without it.
#[tokio::test]
async fn a_batch_deletes_a_dependant_before_its_target() {
    let db = test_db().await;
    register(&db, "reg", TARGET, schema(TARGET)).await;
    register(&db, "reg-holder", HOLDER, referencing(HOLDER, TARGET)).await;

    let outcome = delete_batch(&db, "del", &[(TARGET, 1), (HOLDER, 1)]).await;

    all_succeeded(&outcome);
    assert_eq!(
        entity_of(&db, TARGET).await.expect("row").lifecycle_status,
        LifecycleStatus::Deleted,
    );
    assert_eq!(
        entity_of(&db, HOLDER).await.expect("row").lifecycle_status,
        LifecycleStatus::Deleted,
    );
}

/// The order is transitive, and the edges come from `dependency` rather than
/// from any document — a deletion submits none.
#[tokio::test]
async fn a_batch_deletes_a_chain_from_its_far_end() {
    let db = test_db().await;
    register(&db, "reg", TARGET, schema(TARGET)).await;
    register(&db, "reg-middle", MIDDLE, referencing(MIDDLE, TARGET)).await;
    register(&db, "reg-holder", HOLDER, referencing(HOLDER, MIDDLE)).await;

    // Submitted in exactly the wrong order.
    let outcome = delete_batch(&db, "del", &[(TARGET, 1), (MIDDLE, 1), (HOLDER, 1)]).await;

    all_succeeded(&outcome);
    for gts_id in [TARGET, MIDDLE, HOLDER] {
        assert_eq!(
            entity_of(&db, gts_id).await.expect("row").lifecycle_status,
            LifecycleStatus::Deleted,
            "{gts_id} must be deleted",
        );
    }
}

/// A dependant left **outside** the batch still refuses the target: the order
/// only decides what this batch does first, never what it is allowed to strand.
#[tokio::test]
async fn a_dependant_outside_the_batch_still_refuses_the_target() {
    let db = test_db().await;
    register(&db, "reg", TARGET, schema(TARGET)).await;
    register(&db, "reg-holder", HOLDER, referencing(HOLDER, TARGET)).await;
    register(&db, "reg-other", OTHER, schema(OTHER)).await;

    let outcome = delete_batch(&db, "del", &[(TARGET, 1), (OTHER, 1)]).await;

    refused(
        item(&outcome, TARGET),
        &AdmissionFailureReason::HasRegisteredDependents,
    );
    assert_eq!(
        item(&outcome, OTHER).status,
        OperationItemStatus::Succeeded,
        "the unrelated deletion still commits",
    );
}

/// Reported in submission order, worked in dependency order — the same split
/// the registration path makes.
#[tokio::test]
async fn a_deletion_batch_reports_its_outcomes_in_submission_order() {
    let db = test_db().await;
    register(&db, "reg", TARGET, schema(TARGET)).await;
    register(&db, "reg-holder", HOLDER, referencing(HOLDER, TARGET)).await;

    let outcome = delete_batch(&db, "del", &[(TARGET, 1), (HOLDER, 1)]).await;

    let reported: Vec<&str> = outcome.items.iter().map(|i| i.gts_id.as_str()).collect();
    assert_eq!(reported, vec![TARGET, HOLDER]);
}

// ---------------------------------------------------------------------------
// The two arms a real race and a corrupt row reach
// ---------------------------------------------------------------------------

/// Run one operation through a substituted set of ports.
async fn run_with(
    db: &Provider,
    stores: &Arc<dyn types_registry::domain::ports::Stores>,
    operation_id: Uuid,
) -> OperationOutcome {
    run_operation(
        stores,
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

/// Both of `mark_deleted`'s preconditions live in the statement's `WHERE`, so
/// matching no row means the entity moved after this transaction read it.
/// Refused rather than reported as done — and refused as `precondition_failed`,
/// because that is what a caller with a stale version should retry against.
#[tokio::test]
async fn a_deletion_whose_write_matches_no_row_is_refused_not_reported_as_done() {
    let db = test_db().await;
    register(&db, "reg", TARGET, schema(TARGET)).await;
    let entity_id = entity_of(&db, TARGET).await.expect("row").id;

    let op = submit(
        &db,
        "del",
        OperationKind::Deletion,
        vec![Candidate {
            gts_id: TARGET.to_owned(),
            content: None,
            expected_resource_version: Some(1),
            force: false,
        }],
    )
    .await
    .expect("accepted");
    let hooked: Arc<dyn types_registry::domain::ports::Stores> =
        common::TestStores::deletion_miss(entity_id);
    let outcome = run_with(&db, &hooked, op).await;

    refused(
        &outcome.items[0],
        &AdmissionFailureReason::PreconditionFailed,
    );
    assert!(
        outcome.items[0]
            .failure
            .as_ref()
            .expect("failure")
            .message
            .contains("moved while"),
        "the message must say the row moved, not that the version was wrong: {:?}",
        outcome.items[0].failure,
    );
    assert_eq!(
        entity_of(&db, TARGET).await.expect("row").lifecycle_status,
        LifecycleStatus::Active,
        "nothing was written",
    );
}

#[tokio::test]
async fn deletion_rolls_back_when_its_item_outcome_cannot_be_written() {
    let db = test_db().await;
    register(&db, "reg", TARGET, schema(TARGET)).await;

    let op = submit(
        &db,
        "del-atomic",
        OperationKind::Deletion,
        vec![Candidate {
            gts_id: TARGET.to_owned(),
            content: None,
            expected_resource_version: Some(1),
            force: false,
        }],
    )
    .await
    .expect("accepted");
    let hooked: Arc<dyn types_registry::domain::ports::Stores> =
        common::TestStores::failing_item_success();

    let result = run_operation(
        &hooked,
        &worker(&db),
        &allow_all(),
        Tuning {
            limits: &common::limits(),
            worker: &common::worker_settings(),
            metrics: &common::metrics(),
            allow_compatibility_force: false,
        },
        op,
        LATER,
    )
    .await;
    assert!(
        matches!(result, Err(WorkerError::Storage(_))),
        "the injected item write must surface as the storage failure it is: {result:?}",
    );
    assert_eq!(
        entity_of(&db, TARGET).await.expect("row").lifecycle_status,
        LifecycleStatus::Active,
        "the entity mutation must roll back with the item outcome",
    );
    let items = items_of(&db, op).await;
    assert_eq!(items.len(), 1);
    assert_eq!(
        items[0].status,
        OperationItemStatus::Pending,
        "a rolled-back commit must leave the item for the next delivery",
    );
}

#[tokio::test]
async fn deletion_rolls_back_tombstone_when_item_cas_loses() {
    let db = test_db().await;
    register(&db, "reg", TARGET, schema(TARGET)).await;

    let op = submit(
        &db,
        "del-cas-miss",
        OperationKind::Deletion,
        vec![Candidate {
            gts_id: TARGET.to_owned(),
            content: None,
            expected_resource_version: Some(1),
            force: false,
        }],
    )
    .await
    .expect("accepted");

    let pending_items = items_of(&db, op).await;
    assert_eq!(pending_items[0].status, OperationItemStatus::Pending);
    let item_id = pending_items[0].id;

    let prior_payload = r#"{"reason":"prior_pass_terminated"}"#.to_owned();
    let conn = db.conn().expect("conn");
    OperationRepo::mark_item_failed(&conn, &allow_all(), item_id, prior_payload.clone(), NOW)
        .await
        .expect("terminalize item as Failed before this pass runs");

    let hooked: Arc<dyn types_registry::domain::ports::Stores> =
        common::TestStores::with_stale_snapshot(pending_items);

    let outcome = run_with(&db, &hooked, op).await;

    assert_eq!(
        entity_of(&db, TARGET).await.expect("row").lifecycle_status,
        LifecycleStatus::Active,
        "tombstone must roll back when the item CAS loses (pre-fix: entity would be Tombstoned)",
    );
    let items = items_of(&db, op).await;
    assert_eq!(items.len(), 1);
    assert_eq!(
        items[0].status,
        OperationItemStatus::Failed,
        "the prior pass's Failed outcome must be preserved verbatim",
    );
    assert_eq!(
        items[0].error_payload.as_deref(),
        Some(prior_payload.as_str()),
        "the stored payload must be the prior pass's reason, not a new one from this pass",
    );
    assert_eq!(
        outcome.items[0].status,
        OperationItemStatus::Failed,
        "run_operation must surface the stored Failed outcome, not a fresh write",
    );
}

/// Acceptance refuses a deletion with no `expected_resource_version`, so a
/// stored item in that shape disagrees with the rules that admitted it. The
/// worker still owes it an outcome, and answers `precondition_failed` rather
/// than deleting at whatever version it finds.
#[tokio::test]
async fn a_stored_deletion_item_with_no_version_is_refused_rather_than_obeyed() {
    let db = test_db().await;
    register(&db, "reg", TARGET, schema(TARGET)).await;

    // `0` is storage's spelling of must-not-exist, which no deletion can mean.
    let op = {
        let conn = db.conn().expect("conn");
        common::seed_pending_deletion_item(&conn, TARGET, 0, false, NOW)
            .await
            .0
    };

    let outcome = run(&db, op).await;

    refused(
        &outcome.items[0],
        &AdmissionFailureReason::PreconditionFailed,
    );
    assert!(
        outcome.items[0]
            .failure
            .as_ref()
            .expect("failure")
            .message
            .contains("no expected_resource_version"),
        "{:?}",
        outcome.items[0].failure,
    );
    assert_eq!(
        entity_of(&db, TARGET).await.expect("row").lifecycle_status,
        LifecycleStatus::Active,
        "a contradictory stored item deletes nothing",
    );
}

/// The dry-run twin of the refusal above. `predict_deletion` carries its own copy
/// of the rule, so the committing test does not cover it: nothing but this case
/// would notice the prediction path answering something else, or deleting.
#[tokio::test]
async fn a_dry_run_over_a_stored_deletion_item_with_no_version_is_refused_too() {
    let db = test_db().await;
    register(&db, "reg", TARGET, schema(TARGET)).await;

    let op = {
        let conn = db.conn().expect("conn");
        common::seed_pending_deletion_item(&conn, TARGET, 0, true, NOW)
            .await
            .0
    };

    let outcome = run(&db, op).await;

    refused(
        &outcome.items[0],
        &AdmissionFailureReason::PreconditionFailed,
    );
    assert!(
        outcome.items[0]
            .failure
            .as_ref()
            .expect("failure")
            .message
            .contains("no expected_resource_version"),
        "{:?}",
        outcome.items[0].failure,
    );
    assert_eq!(
        entity_of(&db, TARGET).await.expect("row").lifecycle_status,
        LifecycleStatus::Active,
        "and a dry run writes nothing either way",
    );
}
