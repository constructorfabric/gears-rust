//! Quarantine and dialect-pin worker tests (ADR-0015, ADR-0014).
//! Refusals must report the expected reason and write no entity. Register v0
//! targets first to distinguish quarantine from resolution failure.
//! Pure rule tests live in `src/domain/compat/derivation_tests.rs`.

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
use types_registry::domain::admission::acceptance::{AcceptanceContext, AcceptanceError, accept};
use types_registry::domain::admission::worker::{
    OperationOutcome, Tuning, WorkerError, run_operation,
};
use types_registry::domain::admission::{
    AdmissionFailureReason, Candidate, OperationDispatch, SubmitRequest,
};
use types_registry::domain::enums as domain_enums;
use types_registry::domain::policy::RegistrationPolicy;
use types_registry::infra::storage::entity::entity;

mod common;
use common::{allow_all, stores, test_db};

const NOW: OffsetDateTime = datetime!(2026-09-08 09:15:30 UTC);
const LATER: OffsetDateTime = datetime!(2026-09-08 10:20:40 UTC);

/// The unstable entity every refusal below is measured against.
const UNSTABLE: &str = gts_id!("cf.core.quar.draft.v0~");
/// A stable entity of the same shape, for the one-way half of the rule.
const STABLE: &str = gts_id!("cf.core.quar.published.v1~");
/// A stable leaf derived from the unstable entity — the quarantined base case.
const DERIVED_FROM_UNSTABLE: &str = gts_id!("cf.core.quar.draft.v0~cf.core.quar.leaf.v1~");
/// A stable leaf derived from the stable entity — the same shape, admissible.
const DERIVED_FROM_STABLE: &str = gts_id!("cf.core.quar.published.v1~cf.core.quar.leaf.v1~");
/// An unstable leaf derived from the stable entity: weaker on stronger.
const UNSTABLE_LEAF: &str = gts_id!("cf.core.quar.published.v1~cf.core.quar.sketch.v0~");
/// A standalone stable schema, used as the `$ref`-ing and `x-gts-ref`-ing subject.
const REFERRER: &str = gts_id!("cf.core.quar.referrer.v1~");
const CONSTRAINER: &str = gts_id!("cf.core.quar.constrainer.v1~");
/// A registered Instance whose conforming type is the unstable entity.
const INSTANCE_OF_UNSTABLE: &str = gts_id!("cf.core.quar.draft.v0~cf.core.quar.first.v1");
/// The same Instance shape against the stable type.
const INSTANCE_OF_STABLE: &str = gts_id!("cf.core.quar.published.v1~cf.core.quar.first.v1");

/// A major-only entity, for the intra-entity half of the dialect pin.
const REVISED: &str = gts_id!("cf.core.quar.revised.v1~");
/// A minor-bearing family, for the cross-minor half: the pin is on the major.
const M3_0: &str = gts_id!("cf.core.quar.minor.v3.0~");
const M3_1: &str = gts_id!("cf.core.quar.minor.v3.1~");

const DRAFT_07: &str = "http://json-schema.org/draft-07/schema#";
/// A dialect outside the P0 admissible set. Only ever written straight into a
/// stored row — acceptance refuses it on the wire (ADR-0014, SPEC §8.1 step 5).
const DRAFT_2020: &str = "https://json-schema.org/draft/2020-12/schema";

struct NoDispatch;

#[async_trait::async_trait]
impl OperationDispatch for NoDispatch {
    async fn enqueue(&self, _tx: &DbTx<'_>, _operation_id: Uuid) -> anyhow::Result<()> {
        Ok(())
    }
}

/// A plain open object schema, valid on its own and as a derivation base.
fn schema(gts_id: &str) -> Value {
    json!({
        "$id": format!("gts://{gts_id}"),
        "$schema": DRAFT_07,
        "type": "object",
        "properties": { "name": { "type": "string" } },
    })
}

/// The same schema with `properties` replaced, so a `$ref` or an `x-gts-ref` is
/// the only difference from the admissible baseline shape.
fn schema_with(gts_id: &str, properties: Value) -> Value {
    let mut doc = schema(gts_id);
    doc["properties"] = properties;
    doc
}

fn referencing(gts_id: &str, target: &str) -> Value {
    schema_with(
        gts_id,
        json!({ "target": { "$ref": format!("gts://{target}") } }),
    )
}

fn constraining(gts_id: &str, pattern: &str) -> Value {
    schema_with(
        gts_id,
        json!({ "target": { "type": "string", "x-gts-ref": pattern } }),
    )
}

async fn admit(
    db: &Arc<DBProvider<DbError>>,
    key: &str,
    gts_id: &str,
    content: Value,
) -> OperationOutcome {
    admit_at(db, key, gts_id, content, None).await
}

async fn admit_at(
    db: &Arc<DBProvider<DbError>>,
    key: &str,
    gts_id: &str,
    content: Value,
    expected_resource_version: Option<i64>,
) -> OperationOutcome {
    let provider: DBProvider<AcceptanceError> = DBProvider::new(db.db());
    let policy = RegistrationPolicy::default();
    let config = TypesRegistryConfig::default();
    let dispatch: Arc<dyn OperationDispatch> = Arc::new(NoDispatch);
    let operation_id = accept(
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
            candidates: vec![Candidate {
                gts_id: gts_id.to_owned(),
                content: Some(content),
                expected_resource_version,
                force: false,
            }],
        },
        NOW,
    )
    .await
    .expect("a quarantined candidate is accepted and refused by the worker, not at acceptance")
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
    .expect("the worker itself must not fail")
}

fn succeeded(outcome: &OperationOutcome) {
    assert_eq!(
        outcome.items[0].status,
        domain_enums::OperationItemStatus::Succeeded,
        "{:?}",
        outcome.items[0].failure,
    );
}

fn refused_with(outcome: &OperationOutcome, reason: &AdmissionFailureReason) {
    let item = &outcome.items[0];
    assert_eq!(
        (item.status, item.failure.as_ref().map(|f| f.reason.clone())),
        (
            domain_enums::OperationItemStatus::Failed,
            Some(reason.clone())
        ),
        "{:?}",
        item.failure,
    );
    let message = item
        .failure
        .as_ref()
        .map(|f| f.message.clone())
        .unwrap_or_default();
    assert!(
        message.contains(UNSTABLE),
        "a quarantine refusal must name the offending target, got '{message}'",
    );
}

/// Every registered entity's identifier, so a refusal can be shown to have
/// written nothing rather than merely to have been reported.
async fn registered(db: &Arc<DBProvider<DbError>>) -> Vec<String> {
    let conn = db.conn().expect("conn");
    let mut ids: Vec<String> = entity::Entity::find()
        .secure()
        .scope_with(&allow_all())
        .all(&conn)
        .await
        .expect("entities")
        .into_iter()
        .map(|row| row.gts_id)
        .collect();
    ids.sort();
    ids
}

// ---------------------------------------------------------------------------
// The three quarantined edges
// ---------------------------------------------------------------------------

/// A stable derived identifier must not promise substitutability against an unstable base.
#[tokio::test]
async fn a_stable_schema_deriving_from_a_major_zero_base_is_refused() {
    let db = test_db().await;
    succeeded(&admit(&db, "base", UNSTABLE, schema(UNSTABLE)).await);

    refused_with(
        &admit(
            &db,
            "derived",
            DERIVED_FROM_UNSTABLE,
            schema(DERIVED_FROM_UNSTABLE),
        )
        .await,
        &AdmissionFailureReason::StableDerivesFromMajorZero,
    );
    assert_eq!(
        registered(&db).await,
        vec![UNSTABLE.to_owned()],
        "the refused candidate wrote nothing",
    );
}

/// Floating `$ref`s must not expose a stable schema to unstable target changes.
#[tokio::test]
async fn a_stable_schema_referencing_a_major_zero_target_is_refused() {
    let db = test_db().await;
    succeeded(&admit(&db, "target", UNSTABLE, schema(UNSTABLE)).await);

    refused_with(
        &admit(&db, "referrer", REFERRER, referencing(REFERRER, UNSTABLE)).await,
        &AdmissionFailureReason::StableRefsMajorZero,
    );
    assert_eq!(registered(&db).await, vec![UNSTABLE.to_owned()]);
}

/// The Instance tail is stable; its preceding conforming-type segment is major 0.
#[tokio::test]
async fn a_registered_instance_of_a_major_zero_schema_is_refused() {
    let db = test_db().await;
    succeeded(&admit(&db, "type", UNSTABLE, schema(UNSTABLE)).await);

    refused_with(
        &admit(
            &db,
            "instance",
            INSTANCE_OF_UNSTABLE,
            json!({ "name": "anything" }),
        )
        .await,
        &AdmissionFailureReason::InstanceOfMajorZero,
    );
    assert_eq!(registered(&db).await, vec![UNSTABLE.to_owned()]);
}

// ---------------------------------------------------------------------------
// What the rule deliberately does not reach
// ---------------------------------------------------------------------------

/// `x-gts-ref` creates no dependency, for exact IDs or patterns. Leave the target
/// unregistered to verify that the store is not consulted.
#[tokio::test]
async fn a_stable_schema_whose_x_gts_ref_names_a_major_zero_entity_is_admitted() {
    let db = test_db().await;

    for (key, id, pattern) in [
        ("exact", REFERRER, UNSTABLE.to_owned()),
        ("pattern", CONSTRAINER, format!("{UNSTABLE}*")),
    ] {
        succeeded(&admit(&db, key, id, constraining(id, &pattern)).await);
    }
    assert_eq!(
        registered(&db).await,
        vec![CONSTRAINER.to_owned(), REFERRER.to_owned()],
        "both stable schemas are admitted despite naming a major-0 entity",
    );
}

/// Unstable types may derive from stable bases.
#[tokio::test]
async fn an_unstable_schema_may_derive_from_and_reference_a_stable_one() {
    let db = test_db().await;
    succeeded(&admit(&db, "base", STABLE, schema(STABLE)).await);

    succeeded(&admit(&db, "leaf", UNSTABLE_LEAF, schema(UNSTABLE_LEAF)).await);
    succeeded(
        &admit(
            &db,
            "unstable-referrer",
            UNSTABLE,
            referencing(UNSTABLE, STABLE),
        )
        .await,
    );
}

/// The same three shapes against a stable target, so the tests above are shown to
/// fail on the target's major and not on the shape they used to express it.
#[tokio::test]
async fn the_same_three_shapes_against_a_stable_target_are_admitted() {
    let db = test_db().await;
    succeeded(&admit(&db, "base", STABLE, schema(STABLE)).await);

    succeeded(
        &admit(
            &db,
            "derived",
            DERIVED_FROM_STABLE,
            schema(DERIVED_FROM_STABLE),
        )
        .await,
    );
    succeeded(&admit(&db, "referrer", REFERRER, referencing(REFERRER, STABLE)).await);
    succeeded(
        &admit(
            &db,
            "instance",
            INSTANCE_OF_STABLE,
            json!({ "name": "anything" }),
        )
        .await,
    );
}

// ---------------------------------------------------------------------------
// The dialect pin (ADR-0014)
// ---------------------------------------------------------------------------

/// Restate the stored baseline dialect to exercise drift despite P0's single
/// admissible dialect; see `common::restate_stored_dialect`.
#[tokio::test]
async fn a_revision_may_not_change_the_dialect_its_major_was_admitted_under() {
    let db = test_db().await;
    succeeded(&admit(&db, "first", REVISED, schema(REVISED)).await);
    common::restate_stored_dialect(&db, REVISED, DRAFT_2020).await;

    let outcome = admit_at(&db, "second", REVISED, schema(REVISED), Some(1)).await;
    refused_for_dialect(&outcome, DRAFT_2020, DRAFT_07);
}

/// New minors inherit the major's dialect pin; only a new major can change it.
#[tokio::test]
async fn a_new_minor_may_not_change_the_dialect_of_its_major() {
    let db = test_db().await;
    succeeded(&admit(&db, "m3-0", M3_0, schema(M3_0)).await);
    common::restate_stored_dialect(&db, M3_0, DRAFT_2020).await;

    let outcome = admit(&db, "m3-1", M3_1, schema(M3_1)).await;
    refused_for_dialect(&outcome, DRAFT_2020, DRAFT_07);
}

/// The pin accepts equivalent Draft-07 spellings, but `compare_documents`
/// returns `Unknown`, yielding `compatibility_undecidable`. Keep this distinct
/// from `dialect_changed`; see gts-rust#120, linked in `compat::dialect_pin`.
#[tokio::test]
async fn a_respelled_dialect_is_refused_downstream_and_not_by_the_pin() {
    let db = test_db().await;
    succeeded(&admit(&db, "first", REVISED, schema(REVISED)).await);
    common::restate_stored_dialect(&db, REVISED, "http://json-schema.org/draft-07/schema").await;

    let outcome = admit_at(&db, "second", REVISED, schema(REVISED), Some(1)).await;
    let item = &outcome.items[0];
    assert_eq!(
        (item.status, item.failure.as_ref().map(|f| f.reason.clone())),
        (
            domain_enums::OperationItemStatus::Failed,
            Some(AdmissionFailureReason::CompatibilityUndecidable)
        ),
        "the respelling is gts-rust's refusal, not the pin's: {:?}",
        item.failure,
    );
}

/// Dialect drift names both dialects and prevents comparison.
fn refused_for_dialect(outcome: &OperationOutcome, pinned: &str, declared: &str) {
    let item = &outcome.items[0];
    assert_eq!(
        (item.status, item.failure.as_ref().map(|f| f.reason.clone())),
        (
            domain_enums::OperationItemStatus::Failed,
            Some(AdmissionFailureReason::DialectChanged)
        ),
        "{:?}",
        item.failure,
    );
    let message = item
        .failure
        .as_ref()
        .map(|f| f.message.clone())
        .unwrap_or_default();
    assert!(
        message.contains(pinned) && message.contains(declared),
        "the refusal must name the pinned dialect and the declared one, got '{message}'",
    );
}
