//! The `entity` repository: exact reads, first admission, the two
//! compare-and-swap writes, and the keyset discovery page.

use gts::GtsId;
use sea_orm::sea_query::{Alias, Expr, JoinType};
use sea_orm::{
    ActiveValue::Set, ColumnTrait, Condition, DbErr, EntityTrait, ExprTrait, Order, QueryFilter,
    QueryOrder, QueryTrait,
};
use time::OffsetDateTime;
use toolkit_db::secure::{
    AccessScope, DBRunner, ScopeError, SecureEntityExt, SecureInsertExt, SecureUpdateExt,
    secure_insert_many,
};
use uuid::Uuid;

use super::segment_filter::{self, NameFilter, PatternPlan, SegmentFilter};
use super::{IN_CHUNK, conflict_do_nothing};
use crate::domain::enums::EntityKind;
use crate::domain::ports::{EntityPage, EntityRow, ListFilter, NewEntity, PageRequest};
use crate::infra::storage::entity::enums::{EntityKind as StoredKind, LifecycleStatus};
use crate::infra::storage::entity::{entity, entity_gts_segment};

/// One stored row as the domain names it.
///
/// The mapper sits beside the repository that produces it rather than on the
/// entity: `entity/` is a DDL mirror, and a mirror that also knows the domain's
/// row shape is no longer only a mirror. `credstore`'s `entity_to_model` sits in
/// the same position.
fn row(m: entity::Model) -> EntityRow {
    EntityRow {
        id: m.id,
        gts_uuid: m.gts_uuid,
        gts_id: m.gts_id,
        entity_kind: m.entity_kind.into(),
        family_id: m.family_id,
        ownership_scope: m.ownership_scope.into(),
        owner_tenant_id: m.owner_tenant_id,
        owning_gear: m.owning_gear,
        lifecycle_status: m.lifecycle_status.into(),
        resource_version: m.resource_version,
        deleted_at: m.deleted_at,
        created_at: m.created_at,
        updated_at: m.updated_at,
    }
}

pub struct EntityRepo;

impl EntityRepo {
    /// Exact read by GTS Identifier. Tombstones are returned: a DELETED entity
    /// stays reverse-resolvable until purge.
    ///
    /// # Errors
    /// Propagates scope validation and database query failures.
    pub async fn find_by_gts_id(
        runner: &impl DBRunner,
        scope: &AccessScope,
        gts_id: &str,
    ) -> Result<Option<EntityRow>, ScopeError> {
        Ok(entity::Entity::find()
            .filter(entity::Column::GtsId.eq(gts_id))
            .secure()
            .scope_with(scope)
            .one(runner)
            .await?
            .map(row))
    }

    /// The entity kind of any one member of a family, or `None` for an empty
    /// family.
    ///
    /// One row suffices because a family holds a single kind — the invariant this
    /// read enforces. Ordered by `id` so the answer is the founding member's, which
    /// keeps a refusal message stable across backends.
    ///
    /// # Errors
    /// Propagates the scoped query's failure.
    pub async fn kind_in_family(
        runner: &impl DBRunner,
        scope: &AccessScope,
        family_id: i64,
    ) -> Result<Option<EntityKind>, ScopeError> {
        Ok(entity::Entity::find()
            .filter(entity::Column::FamilyId.eq(family_id))
            .order_by_asc(entity::Column::Id)
            .secure()
            .scope_with(scope)
            .one(runner)
            .await?
            .map(|m| m.entity_kind.into()))
    }

    /// Batch exact read, chunked to stay inside every backend's parameter limit.
    /// Identifiers with no row are simply absent from the result.
    ///
    /// # Errors
    /// Propagates scope validation and database query failures from any chunk.
    pub async fn find_by_gts_ids(
        runner: &impl DBRunner,
        scope: &AccessScope,
        gts_ids: &[String],
    ) -> Result<Vec<EntityRow>, ScopeError> {
        let mut out = Vec::new();
        for chunk in gts_ids.chunks(IN_CHUNK) {
            out.extend(
                entity::Entity::find()
                    .filter(entity::Column::GtsId.is_in(chunk.iter().map(String::as_str)))
                    .secure()
                    .scope_with(scope)
                    .all(runner)
                    .await?
                    .into_iter()
                    .map(row),
            );
        }
        Ok(out)
    }

    /// Exact read by Registry Reference. The UUID derives from the identifier
    /// (`GtsId::to_uuid`), so this is the same entity by its other key — which is
    /// why `GET /entities/{entity_key}` accepts either.
    ///
    /// # Errors
    /// Propagates the scoped query's failure.
    pub async fn find_by_gts_uuid(
        runner: &impl DBRunner,
        scope: &AccessScope,
        gts_uuid: Uuid,
    ) -> Result<Option<EntityRow>, ScopeError> {
        Ok(entity::Entity::find()
            .filter(entity::Column::GtsUuid.eq(gts_uuid))
            .secure()
            .scope_with(scope)
            .one(runner)
            .await?
            .map(row))
    }

    /// Batch exact read by Registry Reference, chunked like
    /// [`Self::find_by_gts_ids`]. References with no row are simply absent, which
    /// is what lets the caller report the first unresolved target in request order.
    ///
    /// # Errors
    /// Propagates scope validation and database query failures from any chunk.
    pub async fn find_by_gts_uuids(
        runner: &impl DBRunner,
        scope: &AccessScope,
        gts_uuids: &[Uuid],
    ) -> Result<Vec<EntityRow>, ScopeError> {
        let mut out = Vec::new();
        for chunk in gts_uuids.chunks(IN_CHUNK) {
            out.extend(
                entity::Entity::find()
                    .filter(entity::Column::GtsUuid.is_in(chunk.iter().copied()))
                    .secure()
                    .scope_with(scope)
                    .all(runner)
                    .await?
                    .into_iter()
                    .map(row),
            );
        }
        Ok(out)
    }

    /// Batch read by surrogate id, chunked. Used by the closure walk, which
    /// discovers ids rather than identifiers.
    ///
    /// # Errors
    /// Propagates scope validation and database query failures from any chunk.
    pub async fn find_by_ids(
        runner: &impl DBRunner,
        scope: &AccessScope,
        ids: &[i64],
    ) -> Result<Vec<EntityRow>, ScopeError> {
        let mut out = Vec::new();
        for chunk in ids.chunks(IN_CHUNK) {
            out.extend(
                entity::Entity::find()
                    .filter(entity::Column::Id.is_in(chunk.iter().copied()))
                    .secure()
                    .scope_with(scope)
                    .all(runner)
                    .await?
                    .into_iter()
                    .map(row),
            );
        }
        Ok(out)
    }

    /// First admission of an entity: active, `resource_version = 1`, no tombstone,
    /// with its parsed segments. `None` means a concurrent writer already holds
    /// this `gts_id` or `gts_uuid`.
    ///
    /// # Errors
    /// Refuses an identifier with no stored segment shape; propagates scope
    /// validation and database failures other than the absorbed uniqueness race.
    pub async fn insert(
        runner: &impl DBRunner,
        scope: &AccessScope,
        new: NewEntity,
    ) -> Result<Option<EntityRow>, ScopeError> {
        let gts_id = new.gts_id.clone();
        let segments = stored_segments(&gts_id)?;
        let chain_depth = i16::try_from(segments.len())
            .map_err(|_| invalid_identifier(&gts_id, "too many segments"))?;
        let am = entity::ActiveModel {
            gts_uuid: Set(new.gts_uuid),
            gts_id: Set(new.gts_id),
            entity_kind: Set(new.entity_kind.into()),
            chain_depth: Set(chain_depth),
            family_id: Set(new.family_id),
            ownership_scope: Set(new.ownership_scope.into()),
            owner_tenant_id: Set(new.owner_tenant_id),
            owning_gear: Set(new.owning_gear),
            lifecycle_status: Set(LifecycleStatus::Active),
            resource_version: Set(1),
            deleted_at: Set(None),
            created_at: Set(new.now),
            updated_at: Set(new.now),
            ..Default::default()
        };
        // `exec`, not `exec_with_returning`: only `exec` spells a swallowed conflict
        // as `RecordNotInserted` on every backend — see `VersionFamilyRepo`.
        match entity::Entity::insert(am.clone())
            .secure()
            .scope_with_model(scope, &am)?
            .on_conflict_raw(conflict_do_nothing(entity::Column::Id))
            .exec(runner)
            .await
        {
            Ok(_) => {
                let inserted = Self::find_by_gts_id(runner, scope, &gts_id).await?;
                if let Some(entity) = &inserted {
                    let rows = segments
                        .into_iter()
                        .map(|segment| entity_gts_segment::ActiveModel {
                            entity_id: Set(entity.id),
                            segment_no: Set(segment.segment_no),
                            segment_name: Set(segment.segment_name),
                            major: Set(segment.major),
                            minor: Set(segment.minor),
                            is_type: Set(segment.is_type),
                        })
                        .collect();
                    secure_insert_many::<entity_gts_segment::Entity>(rows, scope, runner).await?;
                }
                Ok(inserted)
            }
            // `uq_tr_entity_gts_id` or `uq_tr_entity_gts_uuid` already holds this
            // identifier — a concurrent admission of the same candidate. Absorbed
            // rather than raised because this runs inside the commit transaction
            // (see `conflict_do_nothing`). The caller turns `None` into the item's
            // `already_exists` outcome, the same answer the pre-insert existence
            // check gives when the winner committed a moment earlier.
            Err(ScopeError::Db(DbErr::RecordNotInserted)) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Advance `resource_version` if and only if the row is **active** and still
    /// at `expected`.
    ///
    /// One statement: the precondition is in the `WHERE`, so there is no window
    /// between the check and the write, and the affected-row count is the success
    /// signal. A stale precondition is `Ok(None)` rather than an error — it is an
    /// ordinary concurrent-writer outcome the caller turns into `412`, not a fault.
    /// Success returns the exact value written by this statement.
    ///
    /// `lifecycle_status = ACTIVE` is in the same `WHERE` for the reason the version
    /// is: [`Self::mark_deleted`] can commit between the caller's read and this
    /// statement, and a revision that moved a tombstone's current state would
    /// resurrect a withdrawn entity. The caller refuses a tombstone it can see, so a
    /// deliberate attempt gets a message; this clause closes the race it cannot.
    ///
    /// # Errors
    /// Propagates scope validation and database update failures.
    pub async fn compare_and_swap_version(
        runner: &impl DBRunner,
        scope: &AccessScope,
        entity_id: i64,
        expected_resource_version: i64,
        now: OffsetDateTime,
    ) -> Result<Option<i64>, ScopeError> {
        let next_resource_version = expected_resource_version.checked_add(1).ok_or_else(|| {
            ScopeError::Db(DbErr::Custom(
                "resource_version cannot advance past i64::MAX".to_owned(),
            ))
        })?;
        let result = entity::Entity::update_many()
            .secure()
            .col_expr(
                entity::Column::ResourceVersion,
                Expr::value(next_resource_version),
            )
            .col_expr(entity::Column::UpdatedAt, Expr::value(now))
            .filter(
                Condition::all()
                    .add(entity::Column::Id.eq(entity_id))
                    .add(entity::Column::ResourceVersion.eq(expected_resource_version))
                    .add(entity::Column::LifecycleStatus.eq(LifecycleStatus::Active)),
            )
            .scope_with(scope)
            .exec(runner)
            .await?;
        Ok((result.rows_affected == 1).then_some(next_resource_version))
    }

    /// Turn an active entity into a tombstone under the same compare-and-swap.
    ///
    /// `lifecycle_status` and `deleted_at` move together because
    /// `ck_tr_entity_lifecycle` constrains the pair; the `WHERE` also requires the
    /// row to be active, so a second deletion reports failure instead of moving
    /// `deleted_at`. As with [`Self::compare_and_swap_version`], success returns
    /// the exact version written and `None` reports a lost race.
    ///
    /// # Errors
    /// Propagates scope validation and database update failures.
    pub async fn mark_deleted(
        runner: &impl DBRunner,
        scope: &AccessScope,
        entity_id: i64,
        expected_resource_version: i64,
        now: OffsetDateTime,
    ) -> Result<Option<i64>, ScopeError> {
        let next_resource_version = expected_resource_version.checked_add(1).ok_or_else(|| {
            ScopeError::Db(DbErr::Custom(
                "resource_version cannot advance past i64::MAX".to_owned(),
            ))
        })?;
        let result = entity::Entity::update_many()
            .secure()
            .col_expr(
                entity::Column::ResourceVersion,
                Expr::value(next_resource_version),
            )
            .col_expr(
                entity::Column::LifecycleStatus,
                Expr::value(LifecycleStatus::Deleted),
            )
            .col_expr(entity::Column::DeletedAt, Expr::value(now))
            .col_expr(entity::Column::UpdatedAt, Expr::value(now))
            .filter(
                Condition::all()
                    .add(entity::Column::Id.eq(entity_id))
                    .add(entity::Column::ResourceVersion.eq(expected_resource_version))
                    .add(entity::Column::LifecycleStatus.eq(LifecycleStatus::Active)),
            )
            .scope_with(scope)
            .exec(runner)
            .await?;
        Ok((result.rows_affected == 1).then_some(next_resource_version))
    }

    /// One keyset page of entities matching every `filter` field, in `gts_id`
    /// order after `request.after`.
    ///
    /// One statement decides the page: the pattern compiles to one inner join per
    /// constrained segment, and `LIMIT limit + 1` tells whether more remain. The
    /// keyset boundary is a stored `gts_id`, so a row inserted mid-traversal
    /// sorts either ahead of the cursor or behind it.
    ///
    /// # Errors
    /// Propagates scope validation and database query failures.
    pub async fn list_page(
        runner: &impl DBRunner,
        scope: &AccessScope,
        filter: &ListFilter,
        request: PageRequest,
    ) -> Result<EntityPage, ScopeError> {
        // `ExprTrait` shadows the inherent `max` on integers.
        let limit = std::cmp::max(request.limit, 1) as usize;
        let segments = match filter.pattern.as_ref().map(segment_filter::compile) {
            None => Vec::new(),
            Some(Ok(PatternPlan::Segments(segments))) => segments,
            Some(Ok(PatternPlan::Never)) => return Ok(EntityPage::empty()),
            Some(Err(_)) => {
                return Err(ScopeError::Invalid(
                    "discovery cannot compile a GTS pattern segment shape",
                ));
            }
        };

        let mut condition = Condition::all();
        if let Some(status) = filter.lifecycle.status() {
            condition =
                condition.add(entity::Column::LifecycleStatus.eq(LifecycleStatus::from(status)));
        }
        if let Some(kind) = filter.kind {
            condition = condition.add(entity::Column::EntityKind.eq(StoredKind::from(kind)));
        }
        if let Some(max) = filter.max_chain_depth {
            // Equality lets `idx_tr_entity_depth` return depth-1 rows in `gts_id` order.
            let max = i16::from(max.get());
            condition = condition.add(if max == 1 {
                entity::Column::ChainDepth.eq(max)
            } else {
                entity::Column::ChainDepth.lte(max)
            });
        }
        if let Some((lower, upper)) = segment_filter::id_range(&segments) {
            condition = condition.add(entity::Column::GtsId.gte(lower));
            if let Some(upper) = upper {
                condition = condition.add(entity::Column::GtsId.lt(upper));
            }
        }
        if let Some(after) = &request.after {
            condition = condition.add(entity::Column::GtsId.gt(after.as_str()));
        }

        let mut items: Vec<EntityRow> = entity::Entity::find()
            .filter(condition)
            .secure()
            .scope_with(scope)
            .order_by(entity::Column::GtsId, Order::Asc)
            .limit(limit as u64 + 1)
            .project_all(runner, |mut query| {
                for segment in &segments {
                    join_segment(&mut query, segment);
                }
                query.into_model::<entity::Model>()
            })
            .await?
            .into_iter()
            .map(row)
            .collect();

        let next_after = if items.len() > limit {
            items.truncate(limit);
            items.last().map(|row| row.gts_id.clone())
        } else {
            None
        };
        Ok(EntityPage { items, next_after })
    }
}

/// Parse `gts_id` into the rows it is stored as.
fn stored_segments(gts_id: &str) -> Result<Vec<segment_filter::SegmentRow>, ScopeError> {
    let id = GtsId::try_new(gts_id).map_err(|e| invalid_identifier(gts_id, &e.to_string()))?;
    // The rows describe `id.id()`, so it must be the stored spelling too.
    if id.id() != gts_id {
        return Err(invalid_identifier(gts_id, "not in canonical form"));
    }
    segment_filter::segment_rows(&id).map_err(|e| invalid_identifier(gts_id, &e.to_string()))
}

fn invalid_identifier(gts_id: &str, reason: &str) -> ScopeError {
    ScopeError::Db(DbErr::Custom(format!(
        "types_registry cannot store `{gts_id}`: {reason}"
    )))
}

/// Inner-join the segment `filter` constrains. The primary key allows one row per
/// alias, so the join never duplicates an entity.
fn join_segment(query: &mut sea_orm::Select<entity::Entity>, filter: &SegmentFilter) {
    use entity_gts_segment::Column;

    let alias = Alias::new(format!("seg{}", filter.segment_no));
    let col = |column: Column| Expr::col((alias.clone(), column));
    let mut on = Condition::all()
        .add(col(Column::EntityId).equals((entity::Entity, entity::Column::Id)))
        .add(col(Column::SegmentNo).eq(filter.segment_no));
    match &filter.name {
        Some(NameFilter::Exact(name)) => on = on.add(col(Column::SegmentName).eq(name.as_str())),
        Some(NameFilter::Prefix(prefix)) => {
            // A byte range, not `LIKE`: `_` is a GTS token character.
            on = on.add(col(Column::SegmentName).gte(prefix.as_str()));
            if let Some(upper) = segment_filter::upper_bound(prefix) {
                on = on.add(col(Column::SegmentName).lt(upper));
            }
        }
        None => {}
    }
    if let Some(major) = filter.major {
        on = on.add(col(Column::Major).eq(major));
    }
    if let Some(minor) = filter.minor {
        on = on.add(col(Column::Minor).eq(minor));
    }
    if let Some(is_type) = filter.is_type {
        on = on.add(col(Column::IsType).eq(is_type));
    }
    QueryTrait::query(query).join_as(JoinType::InnerJoin, entity_gts_segment::Entity, alias, on);
}
