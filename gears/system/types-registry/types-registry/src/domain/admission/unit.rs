//! One admission unit: evaluate a candidate against its transient store, then
//! commit it (SPEC §8.1, worker steps 3 and 4).
//!
//! Evaluation — building the store, resolving references, composing effective
//! traits, meta-compiling the schema — runs with **no transaction open**, so a slow
//! validation never holds a row lock and a failed one never opened one. The
//! transaction that follows holds only the commit-time rechecks and the writes.
//!
//! P0 scope is one acyclic, reference-free candidate per unit; later phases add
//! steps to `evaluate` or `commit` without moving that boundary.
//!
//! [`commit_creation`] requires the identifier **absent**, [`commit_revision`]
//! requires it present at a named `resource_version`. They share evaluation and
//! nothing else — one function branching on an `Option<i64>` would make each half's
//! writes reachable under the other's precondition.

use std::sync::Arc;

use gts::{GTS_IMPLEMENTATION_VERSION, GTS_SPECIFICATION_VERSION, GtsId};
use serde_json::Value;
use time::OffsetDateTime;
use toolkit_db::secure::AccessScope;
use toolkit_db::{DBProvider, DbTx};
use toolkit_macros::domain_model;
use uuid::Uuid;

use super::errors::{ItemFailure, WorkerError};
use super::fingerprint::canonical_text;
use crate::domain::artifacts::{MaterializedArtifacts, content_hash, materialize};
use crate::domain::enums::{EntityKind, LifecycleStatus, OwnershipScope};
use crate::domain::family::{FamilyKey, admits_new_member, family_key};
use crate::domain::gts_store::{UnitDocument, UnitStore, load_unit_store};
use crate::domain::ports::{
    NewCurrentInstance, NewCurrentTypeSchema, NewEntity, NewInstanceRevision, NewRevision, Stores,
    snapshot_read,
};

/// The owning gear recorded on a P0 admission.
///
/// ponytail: ceiling C3 — caller-declared attribution that MUST NOT authorize
/// (`database.sql`). Honest while P0 has one writer, the registry seeding itself.
/// Upgrade: the inventory record's own `owning_gear`.
pub const P0_OWNING_GEAR: &str = "types-registry";

/// The kind-specific half of an evaluation. The kind *is* the variant, so
/// `EvaluatedUnit` needs no `entity_kind` field and no payload can disagree with
/// one.
#[domain_model]
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
    /// Derived, never passed: the identifier's `~` chose the variant. Supplying the
    /// kind alongside it is how an entity row and its revision table come to disagree.
    #[must_use]
    pub const fn entity_kind(&self) -> EntityKind {
        match self {
            Self::TypeSchema { .. } => EntityKind::TypeSchema,
            Self::Instance { .. } => EntityKind::Instance,
        }
    }
}

/// What evaluation produced, and what the commit needs. Owned, because it crosses
/// into a transaction closure that borrows nothing shorter-lived than `'static`.
#[domain_model]
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

/// The commit's result for one item that wrote a revision.
#[domain_model]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommittedUnit {
    pub gts_uuid: Uuid,
    pub revision_no: i32,
    pub resource_version: i64,
}

/// What committing a *revision* produced. The outcome is the variant: an
/// `unchanged` candidate allocates no revision number (ADR-0005), and
/// [`commit_creation`] cannot reach that outcome at all.
#[domain_model]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RevisionCommit {
    /// A new immutable revision, the current pointer moved onto it, and
    /// `resource_version` advanced.
    Admitted(CommittedUnit),
    /// The authored content already equalled the current revision: no revision, no
    /// version move. `resource_version` is the one that did not move.
    Unchanged {
        gts_uuid: Uuid,
        resource_version: i64,
    },
}

/// Evaluate one candidate. **No transaction is open here.**
///
/// Builds the unit's transient store from the database (D2), asks `gts-rust` to
/// validate the candidate, and materializes D3's artifacts. The store is dropped
/// when this returns: nothing is retained anywhere, and the next invocation reads
/// the database again.
///
/// # Errors
/// [`WorkerError`] for an infrastructure failure, which the outbox handler must
/// retry. A content failure is an [`ItemFailure`] in the `Ok(Err(..))` position: an
/// *outcome*, not a fault, and retrying it would answer the same forever.
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
    // everything after it runs with no transaction open (see the module header).
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

    // Created here if absent: ownership must be fixed before the first member, and
    // the family and that member commit together.
    //
    // `uq_tr_version_family_key` decides which of two concurrent admissions founds
    // the family, and nothing more — the rules below are check-then-act reads. What
    // serializes *those* is the advisory lock `admission::worker` holds on the
    // family key across the whole of this transaction.
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

    // The three family rules — kind, minor shape, minor contiguity — in one call,
    // asked of a **new member** only: a revision adds nobody to the family and is
    // not gated. See `domain::family::rules`.
    //
    // Re-parsed rather than carried on `EvaluatedUnit`, which would hold two
    // spellings of one fact; the parse already succeeded in `evaluate`, so the
    // failure arm exists only because the type says it can.
    let id = match GtsId::try_new(&unit.gts_id) {
        Ok(id) => id,
        Err(e) => {
            return Ok(Err(ItemFailure::new(
                "invalid_identifier",
                format!("stored identifier '{}' does not parse: {e}", unit.gts_id),
            )));
        }
    };
    if let Some(refusal) = admits_new_member(
        stores,
        tx,
        scope,
        &id,
        &family,
        unit.outcome.entity_kind(),
        created,
    )
    .await?
    {
        return Ok(Err(ItemFailure::new(refusal.reason(), refusal.to_string())));
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
                // A **projection** of the family row, never a second reading of the
                // request: the entity's owner columns are a copy kept for SecureORM
                // scoping and join-free visibility checks. Family ownership is
                // write-once, so this is the only writer of either column.
                ownership_scope: family.ownership_scope,
                owner_tenant_id: family.owner_tenant_id,
                owning_gear: Some(P0_OWNING_GEAR.to_owned()),
                now,
            },
        )
        .await?;
    // The same question as the check above, asked at the moment the unique key
    // answers it. `None` rather than a raised violation, so the loser's transaction
    // stays usable (`repo::conflict_do_nothing`).
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
    // transaction back rather than be discarded: an overlapping pass already
    // recorded an outcome, and committing would leave an entity and a revision
    // behind an item that says otherwise. Everything written above goes with the
    // rollback — which is why the check belongs at the end of the transaction.
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

/// The current revision's number, authored content and digest, whichever kind the
/// candidate is.
///
/// The **authored** content, never the effective artifacts: those move when a
/// dependency moves while the authored document stands still, so including them
/// would report a revision for a document nobody edited.
///
/// TODO(toolkit): a genuine edit fails the `content_hash` prefilter and never
/// compares the bytes, so the body travels for nothing. Fetching it only on a hash
/// match needs a column projection `SecureSelect` does not expose — no
/// `select_only` / `into_tuple`.
///
/// # Errors
/// [`WorkerError::CurrentStateMissing`] when the entity row has no current-state
/// row of its kind — corruption, since one transaction writes both (D3).
async fn read_current_content(
    stores: &dyn Stores,
    tx: &DbTx<'_>,
    scope: &AccessScope,
    unit: &EvaluatedUnit,
    entity_id: i64,
) -> Result<(i32, String, Vec<u8>), WorkerError> {
    let missing = || WorkerError::CurrentStateMissing {
        gts_id: unit.gts_id.clone(),
        entity_id,
    };
    Ok(match &unit.outcome {
        EvaluatedOutcome::TypeSchema { .. } => {
            let current = stores
                .current_documents(tx, scope, &[entity_id])
                .await?
                .pop()
                .ok_or_else(missing)?;
            (
                current.revision_no,
                current.raw_schema,
                current.content_hash,
            )
        }
        EvaluatedOutcome::Instance { .. } => {
            let current = stores
                .current_values(tx, scope, &[entity_id])
                .await?
                .pop()
                .ok_or_else(missing)?;
            (
                current.revision_no,
                current.canonical_value,
                current.content_hash,
            )
        }
    })
}

/// Commit one evaluated unit as a **revision** of an entity that already exists:
/// the `expected_resource_version` precondition, the immutable revision insert,
/// the current-state pointer move, and the item outcome.
///
/// # The order of the three statements is the concurrency design
///
/// 1. read the entity, refuse a tombstone, and compare `resource_version` to
///    `expected`, so a stale caller gets a message naming both versions rather
///    than a bare CAS failure;
/// 2. read the current revision's authored content, to decide `unchanged`;
/// 3. **re-ask the precondition** — as a compare-and-swap for a real revision, and
///    as a plain re-read for an `unchanged` one.
///
/// Step 3 is not redundant with step 1. The commit transaction runs at
/// `READ COMMITTED` ([`commit_write`](crate::domain::ports::commit_write)), so a
/// concurrent admission can commit between steps 1 and 2 and this pass would
/// otherwise answer `unchanged` against content that is no longer current. The
/// compare-and-swap closes that window by construction — its precondition is in the
/// `WHERE` — and the re-read closes it for `unchanged`, which writes nothing and so
/// has no `WHERE` to put it in. A re-read that still sees `expected` means the other
/// admission had not committed yet, so this pass genuinely came first.
///
/// ponytail: ceiling C6 (SPEC §9) — nothing authorizes this path. The registration
/// policy is asked of creations only, and P0 has no principal to check in its place,
/// so any caller that reaches the submit route can revise the authored content of
/// any entity the registry holds. Bounded by transport rather than by policy: the
/// mutation routes are internal-only (ceiling C8). Upgrade: the
/// identity-to-permission binding, then an owner check before this call.
///
/// # Errors
/// [`WorkerError`] for an infrastructure failure, including a corrupt row with no
/// current state. A lost or stale precondition, and a revision aimed at a
/// tombstone, are [`ItemFailure`]s in the `Ok(Err(..))` position — terminal, and
/// never rebased onto the current version.
pub async fn commit_revision(
    stores: &dyn Stores,
    tx: &DbTx<'_>,
    scope: &AccessScope,
    unit: &EvaluatedUnit,
    expected_resource_version: i64,
    now: OffsetDateTime,
) -> Result<Result<RevisionCommit, ItemFailure>, WorkerError> {
    let Some(entity) = stores.find_by_gts_id(tx, scope, &unit.gts_id).await? else {
        return Ok(Err(ItemFailure::new(
            "precondition_failed",
            format!(
                "'{}' does not exist; expected_resource_version {expected_resource_version} \
                 requires it to exist at that version",
                unit.gts_id
            ),
        )));
    };
    // Before the version, because a tombstone is not a stale version: reporting
    // `precondition_failed` would send the caller to retry against a row that will
    // never accept a revision. This is the only write path that reaches a `DELETED`
    // row — `find_by_gts_id` returns tombstones because the family rules need them.
    // The compare-and-swap below carries the same clause for the race this read
    // cannot see.
    if entity.lifecycle_status == LifecycleStatus::Deleted {
        return Ok(Err(ItemFailure::new(
            "entity_deleted",
            format!(
                "'{}' is deleted; a revision cannot be admitted onto a withdrawn entity",
                unit.gts_id
            ),
        )));
    }
    if entity.resource_version != expected_resource_version {
        return Ok(Err(stale_precondition(
            &unit.gts_id,
            expected_resource_version,
            entity.resource_version,
        )));
    }

    let (current_revision_no, current_body, current_hash) =
        read_current_content(stores, tx, scope, unit, entity.id).await?;

    // The hash is a prefilter and the bytes are the decision (ADR-0012): a digest
    // collision would otherwise silently swallow a real edit. Equality against an
    // *older* revision is deliberately not asked — that is an ordinary update which
    // allocates a new number rather than moving the pointer backwards (ADR-0005).
    if current_hash == unit.content_hash && current_body == unit.canonical_body {
        // `EntityVanished`, not `CurrentStateMissing`: the row that disappeared is
        // the *entity*, read twice in one transaction. Naming the current-state
        // tables would point an operator at the wrong half of the corruption.
        let still = stores
            .find_by_gts_id(tx, scope, &unit.gts_id)
            .await?
            .ok_or_else(|| WorkerError::EntityVanished {
                gts_id: unit.gts_id.clone(),
                entity_id: entity.id,
            })?;
        if still.resource_version != expected_resource_version {
            return Ok(Err(stale_precondition(
                &unit.gts_id,
                expected_resource_version,
                still.resource_version,
            )));
        }
        if !stores
            .mark_item_unchanged(
                tx,
                scope,
                unit.operation_item_id,
                still.resource_version,
                now,
            )
            .await?
        {
            return Err(WorkerError::ItemAlreadyTerminal {
                item_id: unit.operation_item_id,
            });
        }
        return Ok(Ok(RevisionCommit::Unchanged {
            gts_uuid: unit.gts_uuid,
            resource_version: still.resource_version,
        }));
    }

    if expected_resource_version == i64::MAX {
        return Err(WorkerError::ResourceVersionExhausted {
            gts_id: unit.gts_id.clone(),
        });
    }

    // One statement carrying the precondition, so there is no window between
    // checking the version and moving it. `None` is the lost race — the version
    // moved, or the entity was deleted, both of which the statement's `WHERE`
    // covers and neither of which it can tell apart.
    let Some(resource_version) = stores
        .compare_and_swap_version(tx, scope, entity.id, expected_resource_version, now)
        .await?
    else {
        return Ok(Err(ItemFailure::new(
            "precondition_failed",
            format!(
                "'{}' moved past resource_version {expected_resource_version}, or was deleted, \
                 while this revision was being admitted",
                unit.gts_id
            ),
        )));
    };
    let revision_no =
        current_revision_no
            .checked_add(1)
            .ok_or_else(|| WorkerError::RevisionNumberExhausted {
                gts_id: unit.gts_id.clone(),
            })?;

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
                        gts_spec_version: GTS_SPECIFICATION_VERSION.to_owned(),
                        gts_impl_version: GTS_IMPLEMENTATION_VERSION.to_owned(),
                        compat_forced: false,
                        operation_item_id: unit.operation_item_id,
                        now,
                    },
                )
                .await?;
            if !stores
                .update_current_schema(
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
                .await?
            {
                return Err(WorkerError::CurrentStateMissing {
                    gts_id: unit.gts_id.clone(),
                    entity_id: entity.id,
                });
            }
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
                        // Re-recorded per revision, not inherited: this value was
                        // validated against whatever the schema's current revision
                        // was at *this* evaluation.
                        type_schema_entity_id: *type_schema_entity_id,
                        type_schema_revision_no: *type_schema_revision_no,
                        gts_spec_version: GTS_SPECIFICATION_VERSION.to_owned(),
                        gts_impl_version: GTS_IMPLEMENTATION_VERSION.to_owned(),
                        operation_item_id: unit.operation_item_id,
                        now,
                    },
                )
                .await?;
            if !stores
                .update_current_instance(
                    tx,
                    scope,
                    NewCurrentInstance {
                        entity_id: entity.id,
                        revision_no,
                        now,
                    },
                )
                .await?
            {
                return Err(WorkerError::CurrentStateMissing {
                    gts_id: unit.gts_id.clone(),
                    entity_id: entity.id,
                });
            }
        }
    }

    // Last, and its `false` rolls everything above back — see `commit_creation`.
    if !stores
        .mark_item_succeeded(
            tx,
            scope,
            unit.operation_item_id,
            revision_no,
            resource_version,
            now,
        )
        .await?
    {
        return Err(WorkerError::ItemAlreadyTerminal {
            item_id: unit.operation_item_id,
        });
    }

    Ok(Ok(RevisionCommit::Admitted(CommittedUnit {
        gts_uuid: unit.gts_uuid,
        revision_no,
        resource_version,
    })))
}

/// The refusal for a precondition that was already wrong when read — the entry
/// check and the `unchanged` re-read. The lost compare-and-swap words it
/// differently on purpose, knowing only that the version *moved*, not what to; both
/// carry the same `reason`, so a client branching on it sees one outcome.
fn stale_precondition(gts_id: &str, expected: i64, found: i64) -> ItemFailure {
    ItemFailure::new(
        "precondition_failed",
        format!(
            "'{gts_id}' is at resource_version {found}, not the expected {expected}; a revision \
             is never rebased onto the current version"
        ),
    )
}

/// Canonicalize a document the way acceptance did, for a caller that has a `Value`
/// rather than the stored text. Exposed so the seeding path and tests share one
/// canonical form with the acceptance path.
#[must_use]
pub fn canonical_body(content: &Value) -> String {
    canonical_text(content)
}
