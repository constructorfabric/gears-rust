//! The `type_schema` / `type_schema_revision` repository: the immutable authored
//! revisions and the current-state row that points at one of them.

use sea_orm::{ActiveValue::Set, ColumnTrait, Condition, EntityTrait, QueryFilter};
use toolkit_db::secure::{AccessScope, DBRunner, ScopeError, SecureEntityExt, secure_insert};

use super::IN_CHUNK;
use crate::domain::ports::{
    CurrentDocument, CurrentTypeSchemaRow, NewCurrentTypeSchema, NewRevision,
};
use crate::infra::storage::entity::{type_schema, type_schema_revision};

/// Chunk size for the exact-pair disjunction in
/// [`TypeSchemaRepo::current_documents`]. Half of [`super::IN_CHUNK`] — written
/// out rather than derived, because `clippy::integer_division` denies the
/// expression — because each pair contributes two bound parameters rather than
/// one.
const PAIR_CHUNK: usize = 100;

/// One current-state row as the domain names it. See `entity_repo::row` for why
/// the mapper sits beside the repository rather than on the entity.
fn current_row(m: type_schema::Model) -> CurrentTypeSchemaRow {
    CurrentTypeSchemaRow {
        entity_id: m.entity_id,
        revision_no: m.revision_no,
        resolved_schema: m.resolved_schema,
        effective_traits: m.effective_traits,
        effective_traits_schema: m.effective_traits_schema,
        resolution_fingerprint: m.resolution_fingerprint,
        created_at: m.created_at,
        updated_at: m.updated_at,
    }
}

pub struct TypeSchemaRepo;

impl TypeSchemaRepo {
    /// Authored documents of the given entities' current revisions.
    ///
    /// Two reads rather than a join: no relations are declared on these entities
    /// (T3), and the current pointer is what selects the revision. First
    /// `type_schema` for the `(entity_id, revision_no)` pairs, then exactly those
    /// pairs from `type_schema_revision`.
    ///
    /// The second read is a disjunction of **exact pairs**, not
    /// `entity_id IN (…)`. The revision table is history, so a plain `IN` would
    /// return every revision ever admitted for these entities and leave the caller
    /// to pick the current one — reading, on a long-lived entity, arbitrarily more
    /// than the closure needs.
    ///
    /// An entity with no `type_schema` row is simply **absent** from the result
    /// rather than an error. That is the honest shape here: a registered Instance
    /// has no row in this table by construction — its current pointer lives in
    /// `instance` (T10) — and only the caller knows whether an absence is a fault.
    ///
    /// # Errors
    /// Propagates the scoped query's failure.
    pub async fn current_documents(
        runner: &impl DBRunner,
        scope: &AccessScope,
        entity_ids: &[i64],
    ) -> Result<Vec<CurrentDocument>, ScopeError> {
        let mut pointers: Vec<(i64, i32)> = Vec::with_capacity(entity_ids.len());
        for chunk in entity_ids.chunks(IN_CHUNK) {
            let rows = type_schema::Entity::find()
                .filter(type_schema::Column::EntityId.is_in(chunk.iter().copied()))
                .secure()
                .scope_with(scope)
                .all(runner)
                .await?;
            pointers.extend(rows.into_iter().map(|r| (r.entity_id, r.revision_no)));
        }

        let mut out = Vec::with_capacity(pointers.len());
        for chunk in pointers.chunks(PAIR_CHUNK) {
            let mut pairs = Condition::any();
            for (entity_id, revision_no) in chunk {
                pairs = pairs.add(
                    Condition::all()
                        .add(type_schema_revision::Column::EntityId.eq(*entity_id))
                        .add(type_schema_revision::Column::RevisionNo.eq(*revision_no)),
                );
            }
            let rows = type_schema_revision::Entity::find()
                .filter(pairs)
                .secure()
                .scope_with(scope)
                .all(runner)
                .await?;
            out.extend(rows.into_iter().map(|r| CurrentDocument {
                entity_id: r.entity_id,
                revision_no: r.revision_no,
                raw_schema: r.raw_schema,
            }));
        }
        Ok(out)
    }

    /// The current-state row of one entity: the revision pointer and D3's
    /// materialized artifacts. This is what a read returns without recomputing
    /// anything.
    ///
    /// # Errors
    /// Propagates the scoped query's failure.
    pub async fn find_current(
        runner: &impl DBRunner,
        scope: &AccessScope,
        entity_id: i64,
    ) -> Result<Option<CurrentTypeSchemaRow>, ScopeError> {
        Ok(type_schema::Entity::find()
            .filter(type_schema::Column::EntityId.eq(entity_id))
            .secure()
            .scope_with(scope)
            .one(runner)
            .await?
            .map(current_row))
    }

    /// Insert one immutable authored revision.
    ///
    /// # Errors
    /// Propagates the insert's failure.
    pub async fn insert_revision(
        runner: &impl DBRunner,
        scope: &AccessScope,
        new: NewRevision,
    ) -> Result<(), ScopeError> {
        let am = type_schema_revision::ActiveModel {
            entity_id: Set(new.entity_id),
            revision_no: Set(new.revision_no),
            raw_schema: Set(new.raw_schema),
            content_hash: Set(new.content_hash),
            gts_spec_version: Set(new.gts_spec_version),
            gts_impl_version: Set(new.gts_impl_version),
            compat_forced: Set(new.compat_forced),
            operation_item_id: Set(new.operation_item_id),
            created_at: Set(new.now),
            updated_at: Set(new.now),
        };
        secure_insert::<type_schema_revision::Entity>(am, scope, runner).await?;
        Ok(())
    }

    /// Insert the current-state row for a first admission.
    ///
    /// Insert, not upsert: moving an existing pointer is a *revision*, which
    /// carries its own preconditions and belongs to T11. A caller that reached
    /// here for an entity that already has a current row would get a primary-key
    /// violation, which is the honest outcome — the recheck that should have
    /// prevented it is missing, and a silent overwrite would hide that.
    ///
    /// # Errors
    /// Propagates the insert's failure.
    pub async fn insert_current(
        runner: &impl DBRunner,
        scope: &AccessScope,
        new: NewCurrentTypeSchema,
    ) -> Result<(), ScopeError> {
        let am = type_schema::ActiveModel {
            entity_id: Set(new.entity_id),
            revision_no: Set(new.revision_no),
            resolved_schema: Set(new.resolved_schema),
            effective_traits: Set(new.effective_traits),
            effective_traits_schema: Set(new.effective_traits_schema),
            resolution_fingerprint: Set(new.resolution_fingerprint),
            created_at: Set(new.now),
            updated_at: Set(new.now),
        };
        secure_insert::<type_schema::Entity>(am, scope, runner).await?;
        Ok(())
    }
}
