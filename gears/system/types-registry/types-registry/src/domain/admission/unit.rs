//! One admission unit: evaluate a candidate against its transient store, then
//! commit it (SPEC §8.1, worker steps 3 and 4).
//!
//! The split between the two halves is the point of this module. Evaluation is
//! everything expensive and everything fallible for content reasons — building the
//! store, resolving references, composing effective traits, meta-compiling the
//! schema — and it happens with **no transaction open**. The transaction that
//! follows contains only the rechecks that must hold at commit time and the
//! writes, so a slow validation never holds a row lock and a failed validation
//! never opened one.
//!
//! P0 scope of this file is T8's: one acyclic, reference-free candidate per unit.
//! In-batch reference resolution and SCC ordering are T19, compatibility against a
//! baseline is T17, dependency edges are T13, and the revision-vector guard is
//! T15. Each of those adds a step to `evaluate` or to `commit` without moving the
//! boundary between them.

use std::sync::Arc;

use gts::{GTS_IMPLEMENTATION_VERSION, GTS_SPECIFICATION_VERSION, GtsId};
use serde_json::Value;
use time::OffsetDateTime;
use toolkit_db::secure::AccessScope;
use toolkit_db::{DBProvider, DbTx};
use uuid::Uuid;

use super::fingerprint::canonical_text;
use crate::domain::artifacts::{MaterializedArtifacts, content_hash, materialize};
use crate::domain::enums::{EntityKind, OwnershipScope};
use crate::domain::family::{FamilyKey, family_key};
use crate::domain::gts_store::{UnitDocument, UnitStore, load_unit_store};
use crate::domain::ports::{
    NewCurrentInstance, NewCurrentTypeSchema, NewEntity, NewInstanceRevision, NewRevision, Stores,
    snapshot_read,
};

use super::worker::{ItemFailure, WorkerError};

/// The owning gear recorded on a P0 admission.
///
/// ponytail: caller-declared attribution that MUST NOT authorize (`database.sql`).
/// P0 has one writer — types-registry itself, seeding — so the constant is honest
/// here. T24 replaces it with T22's `owning_gear` from the inventory record, which
/// is what strikes ceiling C3.
pub const P0_OWNING_GEAR: &str = "types-registry";

/// The kind-specific half of an evaluation, and why `EvaluatedUnit` has no
/// `entity_kind` field.
///
/// Variants rather than two `Option`s beside a `kind` discriminant: with `Option`s a
/// commit path reading the wrong one sees `None` at runtime, and a `kind` that
/// disagrees with its payload is representable. Here the kind *is* the variant.
#[derive(Clone, Debug)]
pub enum EvaluatedOutcome {
    /// D3's artifacts, materialized at admission so the read path recomputes
    /// nothing.
    TypeSchema { artifacts: MaterializedArtifacts },
    /// The Type Schema revision this value was validated against. Recorded rather
    /// than re-derived: the schema's current revision may move afterwards, and this
    /// is the record of which rules the value passed.
    Instance {
        type_schema_entity_id: i64,
        type_schema_revision_no: i32,
    },
}

impl EvaluatedOutcome {
    /// Derived, never passed: the identifier's `~` chose the branch, the branch chose
    /// this variant. Supplying it independently is how an entity row and its revision
    /// table come to disagree.
    #[must_use]
    pub const fn entity_kind(&self) -> EntityKind {
        match self {
            Self::TypeSchema { .. } => EntityKind::TypeSchema,
            Self::Instance { .. } => EntityKind::Instance,
        }
    }
}

/// What evaluation produced, and what the commit needs. Owned, because it crosses
/// into a transaction closure that may not borrow anything shorter-lived than
/// `'static`.
#[derive(Clone, Debug)]
pub struct EvaluatedUnit {
    pub gts_id: String,
    pub gts_uuid: Uuid,
    pub family_key: FamilyKey,
    pub canonical_body: String,
    pub content_hash: Vec<u8>,
    pub outcome: EvaluatedOutcome,
    pub operation_item_id: i64,
}

/// The commit's result for one item.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommittedUnit {
    pub gts_uuid: Uuid,
    pub revision_no: i32,
    pub resource_version: i64,
}

/// Evaluate one candidate. **No transaction is open here.**
///
/// Builds the unit's transient store from the database (D2), asks `gts-rust` to
/// validate the candidate, and materializes D3's artifacts. The store is created
/// inside this call and dropped when it returns: nothing is retained on a worker,
/// a service or the gear, and there is no post-commit rebuild step — the next
/// invocation reads the database again.
///
/// # Errors
/// [`WorkerError`] for an infrastructure failure, which the outbox handler must
/// retry. A content failure is an [`ItemFailure`] in the `Ok(Err(..))` position:
/// it is an *outcome* of this operation, not a fault of the worker, and retrying
/// it would produce the same answer forever.
pub async fn evaluate(
    stores: &Arc<dyn Stores>,
    db: &DBProvider<WorkerError>,
    scope: &AccessScope,
    gts_id: &str,
    canonical_body: &str,
    operation_item_id: i64,
) -> Result<Result<EvaluatedUnit, ItemFailure>, WorkerError> {
    let id = match GtsId::try_new(gts_id) {
        Ok(id) => id,
        // Acceptance already refused a non-canonical identifier, so reaching here
        // means the stored row disagrees with the rules that admitted it.
        Err(e) => {
            return Ok(Err(ItemFailure::new(
                "invalid_identifier",
                format!("stored identifier '{gts_id}' does not parse: {e}"),
            )));
        }
    };
    let content: Value = match serde_json::from_str(canonical_body) {
        Ok(content) => content,
        Err(e) => {
            return Ok(Err(ItemFailure::new(
                "invalid_document",
                format!("stored request payload is not valid JSON: {e}"),
            )));
        }
    };

    // The store's reads are one snapshot, and the transaction ends with the load:
    // everything after this line — reference resolution, trait composition, the
    // meta-schema compile — runs with **no transaction open**, which is the split
    // this module's header is about.
    let candidates = vec![UnitDocument {
        gts_id: id.id().to_owned(),
        content,
    }];
    // The conforming type's `(entity_id, revision_no)` is read in the same snapshot as
    // the store: the recorded revision must be the one that validated the value.
    let conforming_type = (!id.is_type()).then(|| id.get_type_id()).flatten();
    let (store, schema_pair) = {
        let stores = Arc::clone(stores);
        let scope = scope.clone();
        let conforming_type = conforming_type.clone();
        db.transaction_with_config(snapshot_read(&db.db()), move |tx| {
            Box::pin(async move {
                let store = load_unit_store(stores.as_ref(), tx, &scope, candidates)
                    .await
                    .map_err(WorkerError::StoreBuild)?;
                let pair = match conforming_type {
                    Some(type_id) => {
                        let entity = stores.find_by_gts_id(tx, &scope, &type_id).await?;
                        match entity {
                            Some(row) => stores
                                .find_current_schema(tx, &scope, row.id)
                                .await?
                                .map(|current| (row.id, current.revision_no)),
                            None => None,
                        }
                    }
                    None => None,
                };
                Ok((store, pair))
            })
        })
        .await?
    };

    let canonical_body = canonical_body.to_owned();
    tokio::task::spawn_blocking(move || {
        evaluate_loaded(
            store,
            &id,
            conforming_type,
            schema_pair,
            canonical_body,
            operation_item_id,
        )
    })
    .await
    .map_err(WorkerError::EvaluationTask)?
}

/// Run the CPU-heavy `gts-rust` validation and artifact materialization away from
/// the async executor. All database reads have completed before this function is
/// scheduled, so the blocking task owns a closed, in-memory unit store.
fn evaluate_loaded(
    mut store: UnitStore,
    id: &GtsId,
    conforming_type: Option<String>,
    schema_pair: Option<(i64, i32)>,
    canonical_body: String,
    operation_item_id: i64,
) -> Result<Result<EvaluatedUnit, ItemFailure>, WorkerError> {
    let outcome = if id.is_type() {
        let resolved = match store.store_mut().validate_schema(id.id()) {
            Ok(resolved) => resolved,
            Err(e) => {
                return Ok(Err(ItemFailure::new("invalid_schema", e.to_string())));
            }
        };
        EvaluatedOutcome::TypeSchema {
            artifacts: materialize(&resolved),
        }
    } else {
        // `Some` for every parsed Instance identifier: `get_type_id()` is `None` only
        // for a single segment, which `try_new` above already refused.
        let Some(type_id) = conforming_type else {
            return Ok(Err(ItemFailure::new(
                "invalid_identifier",
                format!("instance '{}' has no conforming type", id.id()),
            )));
        };
        // Checked before validation, so the failure names the cause:
        // `validate_instance` would report a missing schema as a content fault.
        let Some((type_schema_entity_id, type_schema_revision_no)) = schema_pair else {
            return Err(WorkerError::ConformingTypeAbsent {
                gts_id: id.id().to_owned(),
                type_id,
            });
        };
        if let Err(e) = store.store_mut().validate_instance(id.id()) {
            return Ok(Err(ItemFailure::new("invalid_value", e.to_string())));
        }
        EvaluatedOutcome::Instance {
            type_schema_entity_id,
            type_schema_revision_no,
        }
    };

    let content_hash = content_hash(&canonical_body);
    Ok(Ok(EvaluatedUnit {
        gts_id: id.id().to_owned(),
        // Derived by `gts-rust`, never locally: the Registry Reference is a
        // deterministic UUIDv5 over the identifier and its namespace, and
        // reproducing that derivation here would be a second implementation of a
        // GTS rule (`constraint-gts-implementation`).
        gts_uuid: id.to_uuid(),
        family_key: family_key(id),
        canonical_body,
        content_hash,
        outcome,
        operation_item_id,
    }))
}

/// Commit one evaluated unit: family, entity, revision, current-state projection,
/// and the item outcome.
///
/// The precondition recheck is inside the transaction because that is the only
/// place it means anything: a creation requires the identifier **absent**, and
/// between evaluation and here another admission may have created it.
///
/// # Errors
/// [`WorkerError`] for an infrastructure failure; a lost precondition race is an
/// [`ItemFailure`] in the `Ok(Err(..))` position.
pub async fn commit_creation(
    stores: &dyn Stores,
    tx: &DbTx<'_>,
    scope: &AccessScope,
    unit: &EvaluatedUnit,
    now: OffsetDateTime,
) -> Result<Result<CommittedUnit, ItemFailure>, WorkerError> {
    if stores
        .find_by_gts_id(tx, scope, &unit.gts_id)
        .await?
        .is_some()
    {
        return Ok(Err(ItemFailure::new(
            "already_exists",
            format!(
                "'{}' already exists; a creation requires the identifier to be absent",
                unit.gts_id
            ),
        )));
    }

    // The family row is the serialization point for the shape and contiguity rules
    // (T12). Created here if absent: ownership must be fixed before the first
    // member, and the family and that member commit together.
    let (family, created) = stores
        .create_or_get(
            tx,
            scope,
            &unit.family_key,
            OwnershipScope::Global,
            None,
            now,
        )
        .await?;

    // One kind per family (T12's kind rule). `family_key` normalizes the trailing `~`
    // away, so `…thing.v1~` and `…thing.v1` share a key; T12's shape and contiguity
    // rules assume a family holds versions of one logical entity.
    //
    // Only for a family that already existed — a fresh one has this candidate as its
    // founding member. Under the family lock, so a concurrent first member of the
    // other kind cannot slip in. Not `already_exists`: the identifier is free.
    if !created {
        let candidate_kind = unit.outcome.entity_kind();
        if let Some(existing) = stores.kind_in_family(tx, scope, family.id).await?
            && existing != candidate_kind
        {
            return Ok(Err(ItemFailure::new(
                "family_kind_conflict",
                format!(
                    "'{}' is a {candidate_kind:?}, but version family '{}' already holds \
                     {existing:?} members; a family holds one kind",
                    unit.gts_id, unit.family_key
                ),
            )));
        }
    }

    let inserted = stores
        .insert_entity(
            tx,
            scope,
            NewEntity {
                gts_uuid: unit.gts_uuid,
                gts_id: unit.gts_id.clone(),
                entity_kind: unit.outcome.entity_kind(),
                family_id: family.id,
                ownership_scope: OwnershipScope::Global,
                owner_tenant_id: None,
                owning_gear: Some(P0_OWNING_GEAR.to_owned()),
                now,
            },
        )
        .await?;
    // The existence check above and this one are the same question asked at two
    // moments: a creation that lost the race by microseconds sees no row and then
    // loses the unique key. Both answers are `already_exists`, and neither aborts
    // the transaction — which is what returning `None` instead of raising the
    // violation buys (`repo::conflict_do_nothing`).
    let Some(entity) = inserted else {
        return Ok(Err(ItemFailure::new(
            "already_exists",
            format!(
                "'{}' was created concurrently; a creation requires the identifier to be absent",
                unit.gts_id
            ),
        )));
    };

    let revision_no = 1;
    match &unit.outcome {
        EvaluatedOutcome::TypeSchema { artifacts } => {
            stores
                .insert_schema_revision(
                    tx,
                    scope,
                    NewRevision {
                        entity_id: entity.id,
                        revision_no,
                        raw_schema: unit.canonical_body.clone(),
                        content_hash: unit.content_hash.clone(),
                        // Recorded for *every* revision, including one with no
                        // compatibility comparison at all: it identifies the engine,
                        // and that cannot be reconstructed later (ADR-0003).
                        gts_spec_version: GTS_SPECIFICATION_VERSION.to_owned(),
                        gts_impl_version: GTS_IMPLEMENTATION_VERSION.to_owned(),
                        compat_forced: false,
                        operation_item_id: unit.operation_item_id,
                        now,
                    },
                )
                .await?;

            stores
                .insert_current_schema(
                    tx,
                    scope,
                    NewCurrentTypeSchema {
                        entity_id: entity.id,
                        revision_no,
                        resolved_schema: artifacts.resolved_schema.clone(),
                        effective_traits: artifacts.effective_traits.clone(),
                        effective_traits_schema: artifacts.effective_traits_schema.clone(),
                        resolution_fingerprint: artifacts.resolution_fingerprint.clone(),
                        now,
                    },
                )
                .await?;
        }
        EvaluatedOutcome::Instance {
            type_schema_entity_id,
            type_schema_revision_no,
        } => {
            stores
                .insert_instance_revision(
                    tx,
                    scope,
                    NewInstanceRevision {
                        entity_id: entity.id,
                        revision_no,
                        canonical_value: unit.canonical_body.clone(),
                        content_hash: unit.content_hash.clone(),
                        // From evaluation's snapshot, not a fresh lookup: re-reading
                        // could pin a revision that landed after validation.
                        type_schema_entity_id: *type_schema_entity_id,
                        type_schema_revision_no: *type_schema_revision_no,
                        gts_spec_version: GTS_SPECIFICATION_VERSION.to_owned(),
                        gts_impl_version: GTS_IMPLEMENTATION_VERSION.to_owned(),
                        operation_item_id: unit.operation_item_id,
                        now,
                    },
                )
                .await?;

            stores
                .insert_current_instance(
                    tx,
                    scope,
                    NewCurrentInstance {
                        entity_id: entity.id,
                        revision_no,
                        now,
                    },
                )
                .await?;
        }
    }

    // The write is a CAS on the item's status, and its `false` must roll this
    // transaction back rather than be discarded: an overlapping pass has already
    // recorded an outcome for this item, and committing here would leave an entity
    // and a revision behind an item that says otherwise. Everything written above
    // goes with the rollback, which is why the check belongs at the end of the
    // transaction rather than before it.
    if !stores
        .mark_item_succeeded(
            tx,
            scope,
            unit.operation_item_id,
            revision_no,
            entity.resource_version,
            now,
        )
        .await?
    {
        return Err(WorkerError::ItemAlreadyTerminal {
            item_id: unit.operation_item_id,
        });
    }

    Ok(Ok(CommittedUnit {
        gts_uuid: unit.gts_uuid,
        revision_no,
        resource_version: entity.resource_version,
    }))
}

/// Canonicalize a document the way acceptance did, for a caller that has a `Value`
/// rather than the stored text. Exposed so the seeding path (T24) and tests share
/// one canonical form with the acceptance path.
#[must_use]
pub fn canonical_body(content: &Value) -> String {
    canonical_text(content)
}
