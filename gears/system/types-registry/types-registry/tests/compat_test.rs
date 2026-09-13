//! Compatibility and provenance through direct admission-worker calls (T17, ADR-0003).
//! Baseline selection is covered in `src/domain/compat/baseline_tests.rs`.
//!
//! The matrix adds the same optional property at three object levels: closed
//! (compatible), open (incompatible), and partial (unknown), per GTS 0.13 §4.5.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::HashMap;
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
use types_registry::domain::admission::acceptance::{AcceptanceContext, AcceptanceError, accept};
use types_registry::domain::admission::fingerprint::canonical_text;
use types_registry::domain::admission::worker::{
    OperationOutcome, Tuning, WorkerError, run_operation,
};
use types_registry::domain::admission::{
    AdmissionFailureReason, Candidate, OperationDispatch, SubmitRequest,
};
use types_registry::domain::enums as domain_enums;
use types_registry::domain::policy::RegistrationPolicy;
use types_registry::infra::storage::entity::{
    entity, instance_revision, operation_item, type_schema_revision,
};

mod common;
use common::{allow_all, stores, test_db};

const NOW: OffsetDateTime = datetime!(2026-09-08 09:15:30 UTC);
const LATER: OffsetDateTime = datetime!(2026-09-08 10:20:40 UTC);

const SUBJECT: &str = gts_id!("cf.core.compat.thing.v1~");
const UNSTABLE: &str = gts_id!("cf.core.compat.thing.v0~");
const V2_0: &str = gts_id!("cf.core.compat.minor.v2.0~");
const V2_1: &str = gts_id!("cf.core.compat.minor.v2.1~");
const V2_2: &str = gts_id!("cf.core.compat.minor.v2.2~");
const INSTANCE: &str = gts_id!("cf.core.compat.thing.v1~cf.core.compat.first.v1");

struct NoDispatch;

#[async_trait::async_trait]
impl OperationDispatch for NoDispatch {
    async fn enqueue(&self, _tx: &DbTx<'_>, _operation_id: Uuid) -> anyhow::Result<()> {
        Ok(())
    }
}

/// Object-level content model: the compatibility matrix's independent variable.
#[derive(Clone, Copy)]
enum Level {
    /// `$=Closed`: every unnamed property was already refused.
    Closed,
    /// `$=Open`: every unnamed property was already accepted, under any value.
    Open,
    /// `$=Partial`: pattern-constrained names make the addition unprovable.
    Partial,
}

impl Level {
    /// The keywords that put the root level in this model.
    fn keywords(self) -> Value {
        match self {
            Self::Closed => json!({ "additionalProperties": false }),
            Self::Open => json!({}),
            Self::Partial => json!({
                "patternProperties": { "^b": { "type": "string" } },
                "additionalProperties": false,
            }),
        }
    }
}

/// One object level in the named content model, carrying `properties`.
fn document(gts_id: &str, level: Level, properties: &Value) -> Value {
    let mut doc = json!({
        "$id": format!("gts://{gts_id}"),
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "properties": properties.clone(),
    });
    let Value::Object(keywords) = level.keywords() else {
        unreachable!("keywords() returns an object");
    };
    for (key, value) in keywords {
        doc[key] = value;
    }
    doc
}

/// The baseline of every matrix row: one named property.
fn one(gts_id: &str, level: Level) -> Value {
    document(gts_id, level, &json!({ "a": { "type": "string" } }))
}

/// The candidate of every matrix row: the same document with one optional property
/// added. The edit is held constant so that only the content model varies.
fn two(gts_id: &str, level: Level) -> Value {
    document(
        gts_id,
        level,
        &json!({ "a": { "type": "string" }, "b": { "type": "string" } }),
    )
}

/// Candidate waiver request and deployment authorization used by acceptance.
#[derive(Clone, Copy, Default)]
struct Waiver {
    requested: bool,
    permitted: bool,
}

impl Waiver {
    const NONE: Self = Self {
        requested: false,
        permitted: false,
    };
    /// Requested and enabled: the only combination that reaches the worker.
    const GRANTED: Self = Self {
        requested: true,
        permitted: true,
    };
}

async fn submit(
    db: &Arc<DBProvider<DbError>>,
    key: &str,
    gts_id: &str,
    content: Value,
    expected_resource_version: Option<i64>,
) -> Uuid {
    submit_with(
        db,
        key,
        gts_id,
        content,
        expected_resource_version,
        Waiver::NONE,
    )
    .await
    .expect("accepted")
}

async fn submit_with(
    db: &Arc<DBProvider<DbError>>,
    key: &str,
    gts_id: &str,
    content: Value,
    expected_resource_version: Option<i64>,
    waiver: Waiver,
) -> Result<Uuid, AcceptanceError> {
    let provider: DBProvider<AcceptanceError> = DBProvider::new(db.db());
    let policy = RegistrationPolicy::default();
    let config = TypesRegistryConfig {
        allow_compatibility_force: waiver.permitted,
        ..Default::default()
    };
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
            candidates: vec![Candidate {
                gts_id: gts_id.to_owned(),
                content: Some(content),
                expected_resource_version,
                force: waiver.requested,
            }],
        },
        NOW,
    )
    .await
    .map(|accepted| accepted.operation_id)
}

async fn admit(
    db: &Arc<DBProvider<DbError>>,
    key: &str,
    gts_id: &str,
    content: Value,
    expected_resource_version: Option<i64>,
) -> OperationOutcome {
    let op = submit(db, key, gts_id, content, expected_resource_version).await;
    run(db, op).await
}

async fn admit_forced(
    db: &Arc<DBProvider<DbError>>,
    key: &str,
    gts_id: &str,
    content: Value,
    expected_resource_version: Option<i64>,
) -> OperationOutcome {
    let op = submit_with(
        db,
        key,
        gts_id,
        content,
        expected_resource_version,
        Waiver::GRANTED,
    )
    .await
    .expect("a later minor with force permitted is accepted");
    // Keep the acceptance-time deployment setting for this worker pass.
    run_with(db, op, Waiver::GRANTED.permitted).await
}

async fn run(db: &Arc<DBProvider<DbError>>, op: Uuid) -> OperationOutcome {
    run_with(db, op, Waiver::NONE.permitted).await
}

async fn run_with(
    db: &Arc<DBProvider<DbError>>,
    op: Uuid,
    allow_compatibility_force: bool,
) -> OperationOutcome {
    run_operation(
        &stores(),
        &DBProvider::<WorkerError>::new(db.db()),
        &allow_all(),
        Tuning {
            limits: &common::limits(),
            worker: &common::worker_settings(),
            metrics: &common::metrics(),
            allow_compatibility_force,
        },
        op,
        LATER,
    )
    .await
    .expect("the worker itself must not fail")
}

/// Stored `(gts_id, revision_no, compat_forced)` tuples, sorted by identifier.
/// Include identity so a waiver on the wrong revision cannot satisfy the assertion.
async fn revisions(db: &Arc<DBProvider<DbError>>) -> Vec<(String, i32, bool)> {
    let conn = db.conn().expect("conn");
    let entities: HashMap<i64, String> = entity::Entity::find()
        .secure()
        .scope_with(&allow_all())
        .all(&conn)
        .await
        .expect("entities")
        .into_iter()
        .map(|e| (e.id, e.gts_id))
        .collect();
    let mut rows: Vec<(String, i32, bool)> = type_schema_revision::Entity::find()
        .secure()
        .scope_with(&allow_all())
        .all(&conn)
        .await
        .expect("revisions")
        .into_iter()
        .map(|r| {
            let gts_id = entities
                .get(&r.entity_id)
                .unwrap_or_else(|| panic!("revision {} has no entity row", r.entity_id))
                .clone();
            (gts_id, r.revision_no, r.compat_forced)
        })
        .collect();
    rows.sort_unstable();
    rows
}

/// The item's status, and the machine reason when it was refused.
fn outcome_of(
    outcome: &OperationOutcome,
) -> (
    domain_enums::OperationItemStatus,
    Option<AdmissionFailureReason>,
) {
    let item = &outcome.items[0];
    (item.status, item.failure.as_ref().map(|f| f.reason.clone()))
}

fn succeeded(outcome: &OperationOutcome) {
    assert_eq!(
        outcome_of(outcome),
        (domain_enums::OperationItemStatus::Succeeded, None),
        "{:?}",
        outcome.items[0].failure,
    );
}

fn refused_with(outcome: &OperationOutcome, reason: AdmissionFailureReason) {
    assert_eq!(
        outcome_of(outcome),
        (domain_enums::OperationItemStatus::Failed, Some(reason)),
        "{:?}",
        outcome.items[0].failure,
    );
}

// ---------------------------------------------------------------------------
// The matrix: one edit, three content models
// ---------------------------------------------------------------------------

/// A closed level already refused every unnamed property, so naming one takes
/// nothing away: every instance the baseline accepted is still accepted.
#[tokio::test]
async fn an_optional_property_added_at_a_closed_level_is_compatible() {
    let db = test_db().await;
    succeeded(&admit(&db, "one", SUBJECT, one(SUBJECT, Level::Closed), None).await);
    succeeded(&admit(&db, "two", SUBJECT, two(SUBJECT, Level::Closed), Some(1)).await);
}

/// An open level already accepted arbitrary values under that name, so constraining
/// it to a string rejects instances the baseline accepted.
#[tokio::test]
async fn the_same_addition_at_an_open_level_is_incompatible() {
    let db = test_db().await;
    succeeded(&admit(&db, "one", SUBJECT, one(SUBJECT, Level::Open), None).await);
    let op = submit(&db, "two", SUBJECT, two(SUBJECT, Level::Open), Some(1)).await;
    let outcome = run(&db, op).await;
    refused_with(&outcome, AdmissionFailureReason::IncompatibleWithBaseline);
    let failure = outcome.items[0].failure.as_ref().unwrap();
    assert!(failure.message.contains("PropertyAdded at $"));

    let conn = db.conn().expect("conn");
    let stored = operation_item::Entity::find()
        .filter(operation_item::Column::OperationId.eq(op))
        .secure()
        .scope_with(&allow_all())
        .one(&conn)
        .await
        .unwrap()
        .unwrap();
    let payload: Value = serde_json::from_str(stored.error_payload.as_deref().unwrap()).unwrap();
    assert_eq!(
        payload,
        json!({
            "reason": "incompatible_with_baseline",
            "message": failure.message,
        })
    );
    assert_eq!(
        run(&db, op).await.items[0].failure,
        outcome.items[0].failure
    );
}

/// Restating a baseline must change all of its retained revisions, while a
/// different schema referring to it keeps its exact authored JSON.
#[tokio::test]
async fn restating_stored_revisions_does_not_rewrite_referrers() {
    let db = test_db().await;
    let mut first = one(SUBJECT, Level::Closed);
    let mut second = two(SUBJECT, Level::Closed);
    succeeded(&admit(&db, "one", SUBJECT, first.clone(), None).await);
    succeeded(&admit(&db, "two", SUBJECT, second.clone(), Some(1)).await);
    let referring = document(
        V2_0,
        Level::Closed,
        &json!({
            "a": { "$ref": format!("gts://{SUBJECT}") },
        }),
    );
    succeeded(&admit(&db, "referring", V2_0, referring.clone(), None).await);
    let dialect = "https://json-schema.org/draft/2020-12/schema";
    common::restate_stored_dialect(&db, SUBJECT, dialect).await;
    let conn = db.conn().expect("conn");
    let stored: Vec<Value> = type_schema_revision::Entity::find()
        .secure()
        .scope_with(&allow_all())
        .all(&conn)
        .await
        .unwrap()
        .into_iter()
        .map(|row| serde_json::from_str(&row.raw_schema).unwrap())
        .collect();
    first["$schema"] = json!(dialect);
    second["$schema"] = json!(dialect);
    assert_eq!(stored.len(), 3);
    assert!(stored.contains(&first));
    assert!(stored.contains(&second));
    assert!(
        stored.contains(&referring),
        "the referring schema must not change"
    );
}

/// A partially open level is reported as such rather than guessed into either
/// category, so the relation is undecidable — and P0 refuses it under **its own**
/// reason (`principle-fail-closed`, SPEC §16.12).
#[tokio::test]
async fn the_same_addition_at_a_partial_level_is_undecidable_and_refused_separately() {
    let db = test_db().await;
    succeeded(&admit(&db, "one", SUBJECT, one(SUBJECT, Level::Partial), None).await);
    let outcome = admit(&db, "two", SUBJECT, two(SUBJECT, Level::Partial), Some(1)).await;
    refused_with(&outcome, AdmissionFailureReason::CompatibilityUndecidable);
    assert!(
        outcome.items[0]
            .failure
            .as_ref()
            .unwrap()
            .message
            .contains("NotProvable at $")
    );
}

/// The two adverse verdicts must never arrive under one code: an incompatible
/// candidate is a design decision to revisit, an undecidable one is a schema to
/// simplify, and a shared reason makes them one number (SPEC §16.12).
#[tokio::test]
async fn incompatible_and_undecidable_are_recorded_under_different_reasons() {
    let db = test_db().await;
    succeeded(&admit(&db, "open-1", SUBJECT, one(SUBJECT, Level::Open), None).await);
    let incompatible = admit(&db, "open-2", SUBJECT, two(SUBJECT, Level::Open), Some(1)).await;

    let db2 = test_db().await;
    succeeded(&admit(&db2, "part-1", SUBJECT, one(SUBJECT, Level::Partial), None).await);
    let undecidable = admit(
        &db2,
        "part-2",
        SUBJECT,
        two(SUBJECT, Level::Partial),
        Some(1),
    )
    .await;

    let (_, incompatible_reason) = outcome_of(&incompatible);
    let (_, undecidable_reason) = outcome_of(&undecidable);
    assert!(incompatible_reason.is_some() && undecidable_reason.is_some());
    assert_ne!(incompatible_reason, undecidable_reason);
}

// ---------------------------------------------------------------------------
// Which baseline, and when there is none
// ---------------------------------------------------------------------------

/// A first admission has nothing before it, so an open level is no obstacle: there
/// is no baseline and therefore no verdict.
#[tokio::test]
async fn a_first_admission_is_compared_against_nothing() {
    let db = test_db().await;
    succeeded(&admit(&db, "first", SUBJECT, two(SUBJECT, Level::Open), None).await);
}

/// ADR-0015: major 0 enforces no mode, so the revision an open level would refuse
/// on a stable major is admitted here — no baseline, no verdict.
#[tokio::test]
async fn a_major_zero_revision_is_admitted_without_a_verdict() {
    let db = test_db().await;
    succeeded(&admit(&db, "one", UNSTABLE, one(UNSTABLE, Level::Open), None).await);
    succeeded(&admit(&db, "two", UNSTABLE, two(UNSTABLE, Level::Open), Some(1)).await);
}

/// Contiguity names the baseline in the identifier: `v2.2~` is compared against
/// `v2.1~` — a **different entity**, and its current definition.
#[tokio::test]
async fn a_later_minor_is_refused_against_its_preceding_minor() {
    let db = test_db().await;
    succeeded(&admit(&db, "m0", V2_0, one(V2_0, Level::Open), None).await);
    succeeded(&admit(&db, "m1", V2_1, one(V2_1, Level::Open), None).await);
    // Adding `b` at the open level `v2.1~` published is incompatible, and the
    // candidate is a *creation* of a new entity — so the refusal can only have come
    // from the cross-minor baseline.
    refused_with(
        &admit(&db, "m2", V2_2, two(V2_2, Level::Open), None).await,
        AdmissionFailureReason::IncompatibleWithBaseline,
    );
}

/// The same shape, compatible: the cross-minor edge is a real check that admits as
/// well as refuses.
#[tokio::test]
async fn a_compatible_later_minor_is_admitted_against_its_predecessor() {
    let db = test_db().await;
    succeeded(&admit(&db, "m0", V2_0, one(V2_0, Level::Closed), None).await);
    succeeded(&admit(&db, "m1", V2_1, one(V2_1, Level::Closed), None).await);
    succeeded(&admit(&db, "m2", V2_2, two(V2_2, Level::Closed), None).await);
}

/// `vM.0~` opens its major, so it has no predecessor and no comparison — which is
/// why an open level is admissible there and refused one minor later.
#[tokio::test]
async fn the_first_minor_of_a_major_is_compared_against_nothing() {
    let db = test_db().await;
    succeeded(&admit(&db, "m0", V2_0, two(V2_0, Level::Open), None).await);
}

// ---------------------------------------------------------------------------
// `force`: one waived cross-minor check, recorded on the revision
// ---------------------------------------------------------------------------

/// ADR-0004's waiver, doing the one thing it exists to do: the candidate refused
/// in `a_later_minor_is_refused_against_its_preceding_minor` is admitted, and the
/// revision says so.
#[tokio::test]
async fn force_waives_the_cross_minor_check_and_is_recorded_on_the_revision() {
    let db = test_db().await;
    succeeded(&admit(&db, "m0", V2_0, one(V2_0, Level::Open), None).await);
    succeeded(&admit(&db, "m1", V2_1, one(V2_1, Level::Open), None).await);
    succeeded(&admit_forced(&db, "m2", V2_2, two(V2_2, Level::Open), None).await);

    assert_eq!(
        revisions(&db).await,
        vec![
            (V2_0.to_owned(), 1, false),
            (V2_1.to_owned(), 1, false),
            (V2_2.to_owned(), 1, true),
        ],
        "only the forced candidate's revision carries the waiver; the two unforced \
         ones must not be tainted by it",
    );
}

/// An authorized waiver remains recorded even for a compatible verdict.
/// ADR-0003 withdraws the whole-history guarantee for any forced step.
#[tokio::test]
async fn a_forced_candidate_that_needed_no_waiver_still_records_the_flag() {
    let db = test_db().await;
    succeeded(&admit(&db, "m0", V2_0, one(V2_0, Level::Closed), None).await);
    succeeded(&admit(&db, "m1", V2_1, one(V2_1, Level::Closed), None).await);
    // Compatible on its own merits — the addition is at a closed level.
    succeeded(&admit_forced(&db, "m2", V2_2, two(V2_2, Level::Closed), None).await);

    assert_eq!(
        revisions(&db).await,
        vec![
            (V2_0.to_owned(), 1, false),
            (V2_1.to_owned(), 1, false),
            (V2_2.to_owned(), 1, true),
        ],
    );
}

/// An authorized cross-minor waiver also covers `Unknown` (ADR-0004).
#[tokio::test]
async fn force_waives_an_undecidable_cross_minor_verdict() {
    let db = test_db().await;
    succeeded(&admit(&db, "m0", V2_0, one(V2_0, Level::Partial), None).await);
    succeeded(&admit(&db, "m1", V2_1, one(V2_1, Level::Partial), None).await);
    refused_with(
        &admit(&db, "m2-unforced", V2_2, two(V2_2, Level::Partial), None).await,
        AdmissionFailureReason::CompatibilityUndecidable,
    );

    let db2 = test_db().await;
    succeeded(&admit(&db2, "m0", V2_0, one(V2_0, Level::Partial), None).await);
    succeeded(&admit(&db2, "m1", V2_1, one(V2_1, Level::Partial), None).await);
    succeeded(&admit_forced(&db2, "m2", V2_2, two(V2_2, Level::Partial), None).await);
    assert_eq!(
        revisions(&db2).await,
        vec![
            (V2_0.to_owned(), 1, false),
            (V2_1.to_owned(), 1, false),
            (V2_2.to_owned(), 1, true),
        ],
    );
}

/// Disabling the deployment flag after acceptance clears the stored waiver.
/// The worker then refuses this candidate under its ordinary incompatible verdict.
#[tokio::test]
async fn a_stored_waiver_stops_waiving_once_the_deployment_switch_is_off() {
    let db = test_db().await;
    succeeded(&admit(&db, "m0", V2_0, one(V2_0, Level::Open), None).await);
    succeeded(&admit(&db, "m1", V2_1, one(V2_1, Level::Open), None).await);

    // Accepted while the deployment permitted the waiver.
    let op = submit_with(
        &db,
        "m2",
        V2_2,
        two(V2_2, Level::Open),
        None,
        Waiver::GRANTED,
    )
    .await
    .expect("force is accepted while the deployment permits it");
    // Run with it off, as a pass after the operator flipped the switch would.
    refused_with(
        &run_with(&db, op, false).await,
        AdmissionFailureReason::IncompatibleWithBaseline,
    );
    assert_eq!(
        revisions(&db).await,
        vec![(V2_0.to_owned(), 1, false), (V2_1.to_owned(), 1, false)],
        "the un-waived candidate wrote no revision",
    );
}

/// Inject a dangling baseline `$ref` to test `baseline_unresolvable`.
/// Normal admission cannot create this state; it produces no verdict.
#[tokio::test]
async fn an_unresolvable_baseline_is_refused_rather_than_admitted() {
    let db = test_db().await;
    succeeded(&admit(&db, "m0", V2_0, one(V2_0, Level::Closed), None).await);
    succeeded(&admit(&db, "m1", V2_1, one(V2_1, Level::Closed), None).await);
    // The predecessor now points at an entity that was never registered.
    common::graft_stored_ref(&db, V2_1, gts_id!("cf.core.compat.absent.v1~")).await;

    refused_with(
        &admit(&db, "m2", V2_2, two(V2_2, Level::Closed), None).await,
        AdmissionFailureReason::BaselineUnresolvable,
    );
    assert_eq!(
        revisions(&db).await,
        vec![(V2_0.to_owned(), 1, false), (V2_1.to_owned(), 1, false)],
        "a candidate whose baseline could not be resolved wrote nothing",
    );
}

/// Only the predecessor references the extra closure target. The candidate
/// is deliberately incompatible, so dropping the baseline root would wrongly
/// admit it and fail the assertion.
#[tokio::test]
async fn a_baseline_carrying_a_ref_the_candidate_drops_is_compared_not_skipped() {
    let db = test_db().await;
    let leaf = gts_id!("cf.core.compat.leaf.v1~");
    succeeded(&admit(&db, "leaf", leaf, one(leaf, Level::Closed), None).await);

    // `v2.0~` and `v2.1~` share the referencing shape, so each is admissible against
    // the one before it.
    let referencing = |gts_id: &str| {
        document(
            gts_id,
            Level::Open,
            &json!({ "a": { "$ref": format!("gts://{leaf}") } }),
        )
    };
    succeeded(&admit(&db, "m0", V2_0, referencing(V2_0), None).await);
    succeeded(&admit(&db, "m1", V2_1, referencing(V2_1), None).await);

    // The candidate names no reference of its own, and names a new property at an
    // open level — which the baseline had already accepted under any value.
    let widened = document(
        V2_2,
        Level::Open,
        &json!({ "a": { "type": "object" }, "b": { "type": "string" } }),
    );
    refused_with(
        &admit(&db, "m2", V2_2, widened, None).await,
        AdmissionFailureReason::IncompatibleWithBaseline,
    );
    assert_eq!(
        revisions(&db).await,
        vec![
            (leaf.to_owned(), 1, false),
            (V2_0.to_owned(), 1, false),
            (V2_1.to_owned(), 1, false),
        ],
        "the refused candidate wrote no revision",
    );
}

/// The disabled deployment gate refuses submission before any operation is created.
#[tokio::test]
async fn force_the_deployment_has_not_enabled_is_refused_before_any_operation_exists() {
    let db = test_db().await;
    succeeded(&admit(&db, "m0", V2_0, one(V2_0, Level::Open), None).await);
    succeeded(&admit(&db, "m1", V2_1, one(V2_1, Level::Open), None).await);

    let refusal = submit_with(
        &db,
        "m2",
        V2_2,
        two(V2_2, Level::Open),
        None,
        Waiver {
            requested: true,
            permitted: false,
        },
    )
    .await;
    assert!(
        matches!(refusal, Err(AcceptanceError::ForceNotPermitted { .. })),
        "{refusal:?}",
    );
    assert_eq!(
        revisions(&db).await,
        vec![(V2_0.to_owned(), 1, false), (V2_1.to_owned(), 1, false)],
        "the refused submission wrote nothing",
    );
}

/// Seed an intra-entity waiver that acceptance would reject. The worker must
/// clear it despite the enabled deployment flag, including in metrics and provenance.
#[tokio::test]
async fn a_stored_waiver_cannot_reach_the_intra_entity_edge() {
    let db = test_db().await;
    succeeded(&admit(&db, "v1", SUBJECT, one(SUBJECT, Level::Open), None).await);

    // A revision of the same identifier — baseline `CurrentRevision`, never waivable
    // — carrying `force` and an edit that is incompatible at an open level.
    let payload = canonical_text(&two(SUBJECT, Level::Open));
    let (operation_id, _item) = {
        let conn = db.conn().expect("conn");
        common::seed_pending_revision_item_with(&conn, SUBJECT, 1, &payload, true, NOW).await
    };

    // The switch is *on*, so the only thing that can refuse this is the baseline.
    refused_with(
        &run_with(&db, operation_id, true).await,
        AdmissionFailureReason::IncompatibleWithBaseline,
    );
    assert_eq!(
        revisions(&db).await,
        vec![(SUBJECT.to_owned(), 1, false)],
        "no second revision, and the first is not retroactively marked waived",
    );
}

/// Acceptance refuses forced revisions even when the deployment permits waivers.
#[tokio::test]
async fn force_cannot_push_an_incompatible_revision_through() {
    let db = test_db().await;
    succeeded(&admit(&db, "one", SUBJECT, one(SUBJECT, Level::Open), None).await);

    let refusal = submit_with(
        &db,
        "two",
        SUBJECT,
        two(SUBJECT, Level::Open),
        Some(1),
        Waiver::GRANTED,
    )
    .await;
    assert!(
        matches!(refusal, Err(AcceptanceError::ForceHasNothingToWaive { .. })),
        "{refusal:?}",
    );
    // And unforced it is refused on the verdict, so there is no route at all.
    refused_with(
        &admit(
            &db,
            "two-unforced",
            SUBJECT,
            two(SUBJECT, Level::Open),
            Some(1),
        )
        .await,
        AdmissionFailureReason::IncompatibleWithBaseline,
    );
    assert_eq!(revisions(&db).await, vec![(SUBJECT.to_owned(), 1, false)]);
}

// ---------------------------------------------------------------------------
// Provenance: which engine produced the verdict (ADR-0003)
// ---------------------------------------------------------------------------

/// Every stored revision's `(gts_spec_version, gts_impl_version)`.
async fn schema_provenance(db: &Arc<DBProvider<DbError>>) -> Vec<(String, String)> {
    let conn = db.conn().expect("conn");
    type_schema_revision::Entity::find()
        .secure()
        .scope_with(&allow_all())
        .all(&conn)
        .await
        .expect("revisions")
        .into_iter()
        .map(|r| (r.gts_spec_version, r.gts_impl_version))
        .collect()
}

/// The versions in force when this binary admits anything.
fn engine() -> (String, String) {
    (
        gts::GTS_SPECIFICATION_VERSION.to_owned(),
        gts::GTS_IMPLEMENTATION_VERSION.to_owned(),
    )
}

/// Content revisions record engine provenance, as initial admissions do (ADR-0003).
#[tokio::test]
async fn a_revision_records_the_engine_that_admitted_it() {
    let db = test_db().await;
    succeeded(&admit(&db, "one", SUBJECT, one(SUBJECT, Level::Closed), None).await);
    succeeded(&admit(&db, "two", SUBJECT, two(SUBJECT, Level::Closed), Some(1)).await);

    assert_eq!(
        schema_provenance(&db).await,
        vec![engine(), engine()],
        "both the creation and the revision carry it",
    );
}

/// Cross-minor provenance identifies the engine that produced the comparison verdict.
#[tokio::test]
async fn a_compared_candidate_records_the_rules_that_judged_it() {
    let db = test_db().await;
    succeeded(&admit(&db, "m0", V2_0, one(V2_0, Level::Closed), None).await);
    succeeded(&admit(&db, "m1", V2_1, one(V2_1, Level::Closed), None).await);
    succeeded(&admit(&db, "m2", V2_2, two(V2_2, Level::Closed), None).await);

    let provenance = schema_provenance(&db).await;
    assert_eq!(provenance.len(), 3);
    assert!(
        provenance.iter().all(|row| *row == engine()),
        "got {provenance:?}",
    );
}

/// Instance revisions record the validating engine; `force` does not apply.
#[tokio::test]
async fn an_instance_revision_records_the_engine_too() {
    let db = test_db().await;
    succeeded(&admit(&db, "type", SUBJECT, one(SUBJECT, Level::Open), None).await);
    succeeded(&admit(&db, "value", INSTANCE, json!({ "a": "first" }), None).await);

    let conn = db.conn().expect("conn");
    let rows: Vec<(String, String)> = instance_revision::Entity::find()
        .secure()
        .scope_with(&allow_all())
        .all(&conn)
        .await
        .expect("instance revisions")
        .into_iter()
        .map(|r| (r.gts_spec_version, r.gts_impl_version))
        .collect();
    assert_eq!(rows, vec![engine()]);
}

/// Major-0 revisions record admission-engine provenance without implying a comparison.
#[tokio::test]
async fn a_candidate_that_compared_nothing_still_records_the_engine() {
    let db = test_db().await;
    succeeded(&admit(&db, "one", UNSTABLE, one(UNSTABLE, Level::Open), None).await);
    succeeded(&admit(&db, "two", UNSTABLE, two(UNSTABLE, Level::Open), Some(1)).await);

    assert_eq!(schema_provenance(&db).await, vec![engine(), engine()]);
}
