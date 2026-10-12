//! Deletion by Registry Reference: identity, replay and resolution under the
//! write claim (TR-DEL-107, TR-DEL-108, TR-DEL-305).

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;

use serde_json::{Value, json};
use time::OffsetDateTime;
use time::macros::datetime;
use toolkit_db::{DBProvider, DbError};
use toolkit_gts::gts_id;
use uuid::Uuid;

use types_registry::config::TypesRegistryConfig;
use types_registry::domain::admission::acceptance::{
    AcceptanceContext, AcceptanceError, accept, accept_deletion,
};
use types_registry::domain::admission::fingerprint::{
    FingerprintCandidate, FingerprintInput, P0_PRINCIPAL_ID, idempotency_scope_hash,
    request_fingerprint,
};
use types_registry::domain::admission::worker::{
    OperationOutcome, Tuning, WorkerError, run_operation,
};
use types_registry::domain::admission::{
    Accepted, AdmissionFailureReason, Candidate, DeleteRequest, DeleteTarget, OperationDispatch,
    Precondition, SubmitRequest,
};
use types_registry::domain::enums::LifecycleStatus;
use types_registry::domain::enums::{OperationItemStatus, OperationKind, OwnershipScope, Plane};
use types_registry::domain::key::EntityKey;
use types_registry::domain::policy::RegistrationPolicy;
use types_registry::domain::ports::{NewOperation, NewOperationItem, OperationItemRow};
use types_registry::infra::storage::repo::{EntityRepo, OperationRepo};

mod common;
use common::{allow_all, stores, test_db};

const NOW: OffsetDateTime = datetime!(2026-09-29 09:00:00 UTC);
const TARGET: &str = gts_id!("cf.core.delkey.target.v1~");
const OTHER: &str = gts_id!("cf.core.delkey.other.v1~");

type Provider = Arc<DBProvider<DbError>>;

fn uuid_of(gts_id: &str) -> Uuid {
    gts::GtsId::try_new(gts_id).expect("valid").to_uuid()
}

fn schema(gts_id: &str) -> Value {
    json!({
        "$id": format!("gts://{gts_id}"),
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
    })
}

fn dispatch() -> Arc<dyn OperationDispatch> {
    Arc::new(common::NoDispatch)
}

async fn register(db: &Provider, key: &str, gts_id: &str) {
    register_with(db, key, gts_id, schema(gts_id)).await;
}

async fn register_with(db: &Provider, key: &str, gts_id: &str, content: Value) {
    let provider: DBProvider<AcceptanceError> = DBProvider::new(db.db());
    let accepted = accept(
        &stores(),
        &provider,
        &allow_all(),
        &AcceptanceContext {
            policy: &RegistrationPolicy::default(),
            config: &TypesRegistryConfig::default(),
            metrics: &common::metrics(),
        },
        &dispatch(),
        &SubmitRequest {
            idempotency_key: Some(key.to_owned()),
            dry_run: false,
            candidates: vec![Candidate {
                gts_id: gts_id.to_owned(),
                content: Some(content),
                expected_resource_version: None,
                force: false,
            }],
        },
        NOW,
    )
    .await
    .expect("registration accepted");
    let outcome = run(db, accepted.operation_id).await;
    assert_eq!(outcome.items[0].status, OperationItemStatus::Succeeded);
}

async fn delete(
    db: &Provider,
    key: &str,
    dry_run: bool,
    targets: &[(EntityKey, i64)],
) -> Result<Accepted, AcceptanceError> {
    let provider: DBProvider<AcceptanceError> = DBProvider::new(db.db());
    accept_deletion(
        &stores(),
        &provider,
        &allow_all(),
        &AcceptanceContext {
            policy: &RegistrationPolicy::default(),
            config: &TypesRegistryConfig::default(),
            metrics: &common::metrics(),
        },
        &dispatch(),
        &DeleteRequest {
            idempotency_key: Some(key.to_owned()),
            dry_run,
            targets: targets
                .iter()
                .map(|(key, version)| DeleteTarget {
                    key: key.clone(),
                    expected_resource_version: Some(*version),
                })
                .collect(),
        },
        NOW,
    )
    .await
}

async fn run(db: &Provider, operation_id: Uuid) -> OperationOutcome {
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
        NOW,
    )
    .await
    .expect("the worker itself must not fail")
}

async fn items_of(db: &Provider, operation_id: Uuid) -> Vec<OperationItemRow> {
    let conn = db.conn().expect("conn");
    OperationRepo::find_items(&conn, &allow_all(), operation_id)
        .await
        .expect("read items")
}

fn gts(gts_id: &str) -> EntityKey {
    EntityKey::GtsId(gts_id.to_owned())
}

#[tokio::test]
async fn an_absent_identifier_and_its_registry_reference_are_one_duplicate() {
    let db = test_db().await;
    let refused = delete(
        &db,
        "dup",
        false,
        &[(gts(TARGET), 1), (EntityKey::Uuid(uuid_of(TARGET)), 1)],
    )
    .await;
    assert!(
        matches!(
            refused,
            Err(AcceptanceError::DuplicateTarget {
                first_index: 0,
                second_index: 1
            })
        ),
        "{refused:?}"
    );
}

#[tokio::test]
async fn an_unknown_reference_fails_its_item_and_keeps_the_reference() {
    let db = test_db().await;
    register(&db, "reg", TARGET).await;
    let unknown = Uuid::new_v4();

    let accepted = delete(
        &db,
        "del",
        false,
        &[(EntityKey::Uuid(unknown), 1), (gts(TARGET), 1)],
    )
    .await
    .expect("an unknown reference is accepted");
    let outcome = run(&db, accepted.operation_id).await;

    assert_eq!(outcome.items[0].key, EntityKey::Uuid(unknown));
    assert_eq!(outcome.items[0].status, OperationItemStatus::Failed);
    assert_eq!(
        outcome.items[0].failure.as_ref().map(|f| &f.reason),
        Some(&AdmissionFailureReason::PreconditionFailed)
    );
    assert_eq!(outcome.items[1].key, gts(TARGET));
    assert_eq!(outcome.items[1].status, OperationItemStatus::Succeeded);
    assert_eq!(outcome.items[1].resource_version, Some(2));
    assert_eq!(
        items_of(&db, accepted.operation_id).await[0].key,
        EntityKey::Uuid(unknown)
    );
}

/// The entity appears between acceptance and execution; the worker decides
/// under its claim, so the deletion is about the entity that exists then.
#[tokio::test]
async fn a_reference_registered_before_execution_is_deleted() {
    let db = test_db().await;
    let accepted = delete(&db, "del", false, &[(EntityKey::Uuid(uuid_of(TARGET)), 1)])
        .await
        .expect("accepted");
    register(&db, "reg", TARGET).await;

    let outcome = run(&db, accepted.operation_id).await;
    assert_eq!(outcome.items[0].key, EntityKey::Uuid(uuid_of(TARGET)));
    assert_eq!(outcome.items[0].status, OperationItemStatus::Succeeded);
    assert_eq!(outcome.items[0].resource_version, Some(2));
}

/// A refusal recorded while the reference named nothing is replayed after the
/// entity appears, and a new key decides afresh.
#[tokio::test]
async fn a_replay_after_the_reference_resolves_returns_the_original_refusal() {
    let db = test_db().await;
    let reference = EntityKey::Uuid(uuid_of(TARGET));
    let first = delete(&db, "del", false, &[(reference.clone(), 1)])
        .await
        .expect("accepted");
    run(&db, first.operation_id).await;
    register(&db, "reg", TARGET).await;

    let replay = delete(&db, "del", false, &[(reference.clone(), 1)])
        .await
        .expect("the same request replays");
    assert!(replay.replayed);
    assert_eq!(replay.operation_id, first.operation_id);
    let items = items_of(&db, first.operation_id).await;
    assert_eq!(items[0].key, reference);
    assert_eq!(items[0].status, OperationItemStatus::Failed);

    let fresh = delete(&db, "del-2", false, &[(reference.clone(), 1)])
        .await
        .expect("accepted");
    let outcome = run(&db, fresh.operation_id).await;
    assert_eq!(outcome.items[0].key, reference);
    assert_eq!(outcome.items[0].status, OperationItemStatus::Succeeded);
}

#[tokio::test]
async fn either_spelling_replays_and_a_changed_precondition_conflicts() {
    let db = test_db().await;
    register(&db, "reg", TARGET).await;
    let first = delete(&db, "del", false, &[(gts(TARGET), 1)])
        .await
        .expect("accepted");
    run(&db, first.operation_id).await;

    let by_reference = delete(&db, "del", false, &[(EntityKey::Uuid(uuid_of(TARGET)), 1)])
        .await
        .expect("the other spelling replays");
    assert!(by_reference.replayed);
    assert_eq!(by_reference.operation_id, first.operation_id);

    for (dry_run, version) in [(false, 2), (true, 1)] {
        assert!(matches!(
            delete(&db, "del", dry_run, &[(gts(TARGET), version)]).await,
            Err(AcceptanceError::FingerprintConflict { operation_id }) if operation_id == first.operation_id
        ));
    }
}

#[tokio::test]
async fn a_dry_run_predicts_each_reference_as_the_commit_would() {
    let db = test_db().await;
    register(&db, "reg", TARGET).await;
    let known = EntityKey::Uuid(uuid_of(TARGET));
    let unknown = EntityKey::Uuid(Uuid::new_v4());
    let accepted = delete(
        &db,
        "dry",
        true,
        &[(known.clone(), 1), (unknown.clone(), 1)],
    )
    .await
    .expect("accepted");
    let outcome = run(&db, accepted.operation_id).await;

    assert_eq!(outcome.items[0].key, known);
    assert_eq!(outcome.items[0].status, OperationItemStatus::Succeeded);
    assert_eq!(outcome.items[1].key, unknown);
    assert_eq!(outcome.items[1].status, OperationItemStatus::Failed);
    // A prediction writes nothing: the entity it would delete is untouched.
    let conn = db.conn().expect("conn");
    let row = EntityRepo::find_by_gts_id(&conn, &allow_all(), TARGET)
        .await
        .expect("read")
        .expect("row");
    assert_eq!(row.lifecycle_status, LifecycleStatus::Active);
    assert_eq!(row.resource_version, 1);
}

/// A schema that inlines `target`, which is a real dependency edge.
fn holding(gts_id: &str, target: &str) -> Value {
    let mut doc = schema(gts_id);
    doc["properties"] = json!({ "target": { "$ref": format!("gts://{target}") } });
    doc
}

/// Deletion ordering resolves a Registry Reference to its identifier, so a pair
/// named by either kind of key deletes the dependant first. Without that, the
/// target would take no edge and be refused for a dependant the same batch was
/// about to remove.
#[tokio::test]
async fn a_batch_orders_a_dependant_before_its_target_named_by_reference() {
    const HOLDER: &str = gts_id!("cf.core.delkey.holder.v1~");
    for (index, targets) in [
        [(EntityKey::Uuid(uuid_of(TARGET)), 1), (gts(HOLDER), 1)],
        [(gts(TARGET), 1), (EntityKey::Uuid(uuid_of(HOLDER)), 1)],
    ]
    .into_iter()
    .enumerate()
    {
        let db = test_db().await;
        register(&db, "reg", TARGET).await;
        register_with(&db, "reg-holder", HOLDER, holding(HOLDER, TARGET)).await;
        let accepted = delete(&db, &format!("del-{index}"), false, &targets)
            .await
            .expect("accepted");
        let outcome = run(&db, accepted.operation_id).await;
        for item in &outcome.items {
            assert_eq!(
                item.status,
                OperationItemStatus::Succeeded,
                "{targets:?}: {:?}",
                item.failure
            );
        }
    }
}

/// Redelivery rebuilds a finished item from its row rather than re-running it; a
/// deletion by reference must come back exactly as the first pass reported it.
#[tokio::test]
async fn a_redelivered_deletion_by_reference_reports_its_first_outcome() {
    let db = test_db().await;
    register(&db, "reg", TARGET).await;
    let reference = EntityKey::Uuid(uuid_of(TARGET));
    let accepted = delete(&db, "del", false, &[(reference.clone(), 1)])
        .await
        .expect("accepted");
    let first = run(&db, accepted.operation_id).await;
    assert!(!first.already_terminal);
    assert_eq!(first.items[0].status, OperationItemStatus::Succeeded);

    let again = run(&db, accepted.operation_id).await;
    assert!(again.already_terminal);
    assert_eq!(again.items, first.items);
    assert_eq!(again.items[0].key, reference);
    assert_eq!(again.items[0].gts_uuid, Some(uuid_of(TARGET)));
    assert_eq!(again.items[0].resource_version, Some(2));
}

// ---------------------------------------------------------------------------
// Deletions accepted before Registry-Reference fingerprints
// ---------------------------------------------------------------------------

/// Store a deletion the way the previous acceptance did: every key resolved to
/// its identifier, digested by the registration-format fingerprint.
async fn seed_legacy_deletion(db: &Provider, key: &str, targets: &[(&str, i64)]) -> Uuid {
    let candidates: Vec<FingerprintCandidate<'_>> = targets
        .iter()
        .map(|(gts_id, version)| FingerprintCandidate {
            gts_id,
            canonical_body: "null",
            precondition: Precondition::Version(*version),
            force: false,
        })
        .collect();
    let fingerprint = request_fingerprint(&FingerprintInput {
        kind: OperationKind::Deletion,
        dry_run: false,
        plane: Plane::Platform,
        tenant_id: None,
        principal_id: P0_PRINCIPAL_ID,
        ownership_scope: OwnershipScope::Global,
        candidates: &candidates,
    });
    let conn = db.conn().expect("conn");
    let parent = OperationRepo::insert(
        &conn,
        &allow_all(),
        NewOperation {
            id: Uuid::new_v4(),
            kind: OperationKind::Deletion,
            dry_run: false,
            plane: Plane::Platform,
            tenant_id: None,
            principal_id: P0_PRINCIPAL_ID,
            idempotency_key: key.to_owned(),
            idempotency_scope_hash: idempotency_scope_hash(Plane::Platform, None, P0_PRINCIPAL_ID),
            request_fingerprint: fingerprint,
            now: NOW,
        },
    )
    .await
    .expect("seed operation");
    let items: Vec<NewOperationItem> = targets
        .iter()
        .enumerate()
        .map(|(item_no, (gts_id, version))| NewOperationItem {
            item_no: i32::try_from(item_no).expect("small"),
            key: gts(gts_id),
            precondition: Precondition::Version(*version),
            compat_forced: false,
            request_payload: "null".to_owned(),
        })
        .collect();
    OperationRepo::insert_items(&conn, &allow_all(), &parent, &items)
        .await
        .expect("seed items");
    parent.id
}

#[tokio::test]
async fn a_legacy_deletion_replays_under_either_spelling() {
    let db = test_db().await;
    let legacy = seed_legacy_deletion(&db, "legacy", &[(TARGET, 1), (OTHER, 3)]).await;

    for request in [
        [(gts(TARGET), 1), (gts(OTHER), 3)],
        [(EntityKey::Uuid(uuid_of(TARGET)), 1), (gts(OTHER), 3)],
        [(gts(TARGET), 1), (EntityKey::Uuid(uuid_of(OTHER)), 3)],
    ] {
        let replay = delete(&db, "legacy", false, &request)
            .await
            .expect("a legacy deletion replays");
        assert!(replay.replayed);
        assert_eq!(replay.operation_id, legacy);
    }
}

#[tokio::test]
async fn a_changed_request_under_a_legacy_key_conflicts() {
    let db = test_db().await;
    let legacy = seed_legacy_deletion(&db, "legacy", &[(TARGET, 1), (OTHER, 3)]).await;

    let changed: [&[(EntityKey, i64)]; 5] = [
        &[(gts(TARGET), 1), (gts(OTHER), 4)],
        &[(gts(OTHER), 3), (gts(TARGET), 1)],
        &[(gts(TARGET), 1)],
        // A reference is substituted only where it names the stored identifier.
        &[(EntityKey::Uuid(uuid_of(OTHER)), 1), (gts(TARGET), 3)],
        &[(EntityKey::Uuid(Uuid::new_v4()), 1), (gts(OTHER), 3)],
    ];
    for request in changed {
        assert!(matches!(
            delete(&db, "legacy", false, request).await,
            Err(AcceptanceError::FingerprintConflict { operation_id }) if operation_id == legacy
        ));
    }
    assert!(matches!(
        delete(&db, "legacy", true, &[(gts(TARGET), 1), (gts(OTHER), 3)]).await,
        Err(AcceptanceError::FingerprintConflict { .. })
    ));

    // A registration under a legacy deletion's key conflicts. Its payload differs
    // too, so the kind guard itself is pinned by `only_a_deletion_takes_the_legacy_fallback`.
    let provider: DBProvider<AcceptanceError> = DBProvider::new(db.db());
    let registration = accept(
        &stores(),
        &provider,
        &allow_all(),
        &AcceptanceContext {
            policy: &RegistrationPolicy::default(),
            config: &TypesRegistryConfig::default(),
            metrics: &common::metrics(),
        },
        &dispatch(),
        &SubmitRequest {
            idempotency_key: Some("legacy".to_owned()),
            dry_run: false,
            candidates: vec![Candidate {
                gts_id: TARGET.to_owned(),
                content: Some(schema(TARGET)),
                expected_resource_version: None,
                force: false,
            }],
        },
        NOW,
    )
    .await;
    assert!(matches!(
        registration,
        Err(AcceptanceError::FingerprintConflict { operation_id }) if operation_id == legacy
    ));
}
