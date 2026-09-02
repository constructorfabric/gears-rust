//! The `instance` / `instance_revision` repository: the immutable authored values
//! and the current-revision pointer.
//!
//! Shaped like [`super::type_schema_repo`] and deliberately not shared with it: the
//! reads look alike but the rows do not, and factoring them together would make both
//! halves optional on both sides.

use sea_orm::sea_query::Expr;
use sea_orm::{ActiveValue::Set, ColumnTrait, Condition, EntityTrait, QueryFilter};
use toolkit_db::secure::{
    AccessScope, DBRunner, ScopeError, SecureEntityExt, SecureUpdateExt, secure_insert,
};

use super::IN_CHUNK;
use crate::domain::ports::{
    CurrentInstanceRow, CurrentInstanceValue, NewCurrentInstance, NewInstanceRevision,
};
use crate::infra::storage::entity::{instance, instance_revision};

/// Half of [`super::IN_CHUNK`]: each pair binds two parameters rather than one.
const PAIR_CHUNK: usize = IN_CHUNK.checked_div(2).expect("pair width is non-zero");

/// One current-state row as the domain names it. By reference, unlike
/// `type_schema_repo`'s counterpart: every field is `Copy`.
fn current_row(m: &instance::Model) -> CurrentInstanceRow {
    CurrentInstanceRow {
        entity_id: m.entity_id,
        revision_no: m.revision_no,
        created_at: m.created_at,
        updated_at: m.updated_at,
    }
}

pub struct InstanceRepo;

impl InstanceRepo {
    /// Authored values of the given entities' current revisions, each with the
    /// Type Schema revision it was validated against.
    ///
    /// Two reads and an exact-pair disjunction, for the reasons
    /// [`super::type_schema_repo::TypeSchemaRepo::current_documents`] states. An
    /// entity with no `instance` row is absent rather than an error: a Type Schema
    /// has none by construction.
    ///
    /// # Errors
    /// Propagates the scoped query's failure.
    pub async fn current_values(
        runner: &impl DBRunner,
        scope: &AccessScope,
        entity_ids: &[i64],
    ) -> Result<Vec<CurrentInstanceValue>, ScopeError> {
        let mut pointers: Vec<(i64, i32)> = Vec::with_capacity(entity_ids.len());
        for chunk in entity_ids.chunks(IN_CHUNK) {
            let rows = instance::Entity::find()
                .filter(instance::Column::EntityId.is_in(chunk.iter().copied()))
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
                        .add(instance_revision::Column::EntityId.eq(*entity_id))
                        .add(instance_revision::Column::RevisionNo.eq(*revision_no)),
                );
            }
            let rows = instance_revision::Entity::find()
                .filter(pairs)
                .secure()
                .scope_with(scope)
                .all(runner)
                .await?;
            out.extend(rows.into_iter().map(|r| CurrentInstanceValue {
                entity_id: r.entity_id,
                revision_no: r.revision_no,
                canonical_value: r.canonical_value,
                content_hash: r.content_hash,
                type_schema_entity_id: r.type_schema_entity_id,
                type_schema_revision_no: r.type_schema_revision_no,
            }));
        }
        Ok(out)
    }

    /// The current-revision pointer of one Instance.
    ///
    /// No artifact, because there is none — see
    /// [`crate::infra::storage::entity::instance`]. For the value ask
    /// [`Self::current_values`].
    ///
    /// # Errors
    /// Propagates the scoped query's failure.
    pub async fn find_current(
        runner: &impl DBRunner,
        scope: &AccessScope,
        entity_id: i64,
    ) -> Result<Option<CurrentInstanceRow>, ScopeError> {
        Ok(instance::Entity::find()
            .filter(instance::Column::EntityId.eq(entity_id))
            .secure()
            .scope_with(scope)
            .one(runner)
            .await?
            .as_ref()
            .map(current_row))
    }

    /// Insert one immutable authored revision.
    ///
    /// The schema-revision pair is written once and never recomputed: the schema's
    /// current revision may move afterwards.
    ///
    /// # Errors
    /// Propagates the insert's failure.
    pub async fn insert_revision(
        runner: &impl DBRunner,
        scope: &AccessScope,
        new: NewInstanceRevision,
    ) -> Result<(), ScopeError> {
        let am = instance_revision::ActiveModel {
            entity_id: Set(new.entity_id),
            revision_no: Set(new.revision_no),
            canonical_value: Set(new.canonical_value),
            content_hash: Set(new.content_hash),
            type_schema_entity_id: Set(new.type_schema_entity_id),
            type_schema_revision_no: Set(new.type_schema_revision_no),
            gts_spec_version: Set(new.gts_spec_version),
            gts_impl_version: Set(new.gts_impl_version),
            operation_item_id: Set(new.operation_item_id),
            created_at: Set(new.now),
            updated_at: Set(new.now),
        };
        secure_insert::<instance_revision::Entity>(am, scope, runner).await?;
        Ok(())
    }

    /// Insert the current-revision pointer for a first admission.
    ///
    /// Insert, not upsert — see
    /// [`super::type_schema_repo::TypeSchemaRepo::insert_current`].
    ///
    /// # Errors
    /// Propagates the insert's failure.
    pub async fn insert_current(
        runner: &impl DBRunner,
        scope: &AccessScope,
        new: NewCurrentInstance,
    ) -> Result<(), ScopeError> {
        let am = instance::ActiveModel {
            entity_id: Set(new.entity_id),
            revision_no: Set(new.revision_no),
            created_at: Set(new.now),
            updated_at: Set(new.now),
        };
        secure_insert::<instance::Entity>(am, scope, runner).await?;
        Ok(())
    }

    /// Move the current-revision pointer onto a newly admitted revision.
    ///
    /// Shorter than its Type Schema counterpart because there is nothing else on
    /// the row: an Instance has no artifact to re-materialize.
    ///
    /// `Ok(false)` means the entity has no current row — see
    /// [`super::type_schema_repo::TypeSchemaRepo::update_current`].
    ///
    /// # Errors
    /// Propagates the update's failure.
    pub async fn update_current(
        runner: &impl DBRunner,
        scope: &AccessScope,
        new: NewCurrentInstance,
    ) -> Result<bool, ScopeError> {
        let result = instance::Entity::update_many()
            .secure()
            .col_expr(instance::Column::RevisionNo, Expr::value(new.revision_no))
            .col_expr(instance::Column::UpdatedAt, Expr::value(new.now))
            .filter(Condition::all().add(instance::Column::EntityId.eq(new.entity_id)))
            .scope_with(scope)
            .exec(runner)
            .await?;
        Ok(result.rows_affected == 1)
    }
}
