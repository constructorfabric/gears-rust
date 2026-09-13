//! The `entity` repository: exact reads, first admission, the two
//! compare-and-swap writes, and the keyset discovery page.
//!
//! The SQL prefilter behind [`EntityRepo::list_page`] lives here too — see the
//! module header of [`super`] for why it only ever narrows, and never decides.

use gts::{GtsId, GtsIdPattern};
use sea_orm::sea_query::Expr;
use sea_orm::{
    ActiveValue::Set, ColumnTrait, Condition, DbErr, EntityTrait, Order, QueryFilter, QueryOrder,
};
use time::OffsetDateTime;
use toolkit_db::secure::{
    AccessScope, DBRunner, ScopeError, SecureEntityExt, SecureInsertExt, SecureUpdateExt,
};
use uuid::Uuid;

use super::{IN_CHUNK, conflict_do_nothing};
use crate::domain::enums::EntityKind;
use crate::domain::ports::{EntityRow, NewEntity};
use crate::infra::storage::entity::entity;
use crate::infra::storage::entity::enums::LifecycleStatus;

/// Rows the SQL prefilter may read in one [`EntityRepo::list_page`] call.
///
/// ponytail: a very selective pattern over a large table could otherwise scan the
/// whole range inside a single call while returning one page. The budget bounds
/// one call's work; the caller pages again, and `has_more` says so. Upgrade path
/// if paging a sparse pattern becomes hot: a `gts_id`-prefix index per pattern
/// shape, which is an index change rather than a protocol change.
const SCAN_BUDGET: u64 = 2048;

/// Rows the SQL prefilter reads per round trip when a pattern may reject some of
/// them. Without a pattern the batch is the page's own remainder instead — see
/// [`EntityRepo::list_page`] — because then nothing can be rejected and reading
/// ahead would be pure waste.
const SCAN_BATCH: u64 = 256;

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

/// One keyset page request: resume after a stored `gts_id`, not at an offset.
#[derive(Clone, Debug)]
pub struct PageRequest {
    /// Exclusive lower bound. `None` starts at the beginning.
    pub after: Option<String>,
    pub limit: u32,
}

impl PageRequest {
    #[must_use]
    pub fn first(limit: u32) -> Self {
        Self { after: None, limit }
    }

    #[must_use]
    pub fn after(after: String, limit: u32) -> Self {
        Self {
            after: Some(after),
            limit,
        }
    }
}

/// One page of a keyset traversal.
#[derive(Clone, Debug)]
pub struct EntityPage {
    pub items: Vec<EntityRow>,
    /// The last `gts_id` the SQL prefilter **consumed**, which is what the next
    /// request resumes after. It is not necessarily the last item returned: rows
    /// the pattern rejected were still consumed, and skipping them again would
    /// re-scan them on every page.
    pub next_after: Option<String>,
    /// `true` when the scan stopped on the page limit or the scan budget rather
    /// than on exhausting the range. It may over-report — stopping exactly on the
    /// last row of a range looks the same as stopping early — which is the safe
    /// direction: the caller asks once more and gets an empty page.
    pub has_more: bool,
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

    /// First admission of an entity: active, `resource_version = 1`, no tombstone.
    /// `None` means a concurrent writer already holds this `gts_id` or `gts_uuid`.
    ///
    /// # Errors
    /// Propagates scope validation and database insert/read failures other than
    /// the deliberately absorbed uniqueness race.
    pub async fn insert(
        runner: &impl DBRunner,
        scope: &AccessScope,
        new: NewEntity,
    ) -> Result<Option<EntityRow>, ScopeError> {
        let gts_id = new.gts_id.clone();
        let am = entity::ActiveModel {
            gts_uuid: Set(new.gts_uuid),
            gts_id: Set(new.gts_id),
            entity_kind: Set(new.entity_kind.into()),
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
            Ok(_) => Self::find_by_gts_id(runner, scope, &gts_id).await,
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

    /// One keyset page of active entities, optionally filtered by a GTS pattern.
    ///
    /// The page boundary is a stored `gts_id`, so it cannot drift or duplicate the
    /// way an offset can: a row inserted mid-traversal either sorts ahead of the
    /// cursor and is seen, or behind it and is not.
    ///
    /// The whole match set is never loaded to be sliced in memory: SQL is asked for
    /// bounded batches within the prefix range, each row is tested against the
    /// pattern as it arrives, and the scan stops on the page limit or scan budget.
    ///
    /// # Errors
    /// Propagates scope validation and database query failures from any scan batch.
    pub async fn list_page(
        runner: &impl DBRunner,
        scope: &AccessScope,
        pattern: Option<&GtsIdPattern>,
        request: PageRequest,
    ) -> Result<EntityPage, ScopeError> {
        // `ExprTrait` is in scope for `Expr::col(..).add(..)`, and its blanket impl
        // shadows the inherent `max` on integers, so this names `Ord::max` outright.
        let limit = std::cmp::max(request.limit, 1) as usize;
        let range = pattern.and_then(prefilter_prefix).map(|prefix| {
            let upper = range_upper_bound(&prefix);
            (prefix, upper)
        });

        let mut items: Vec<EntityRow> = Vec::with_capacity(limit);
        let mut cursor = request.after;
        let mut consumed: Option<String> = None;
        let mut scanned: u64 = 0;
        let mut exhausted = false;

        'scan: while items.len() < limit && scanned < SCAN_BUDGET {
            // With no pattern every row SQL returns is a match, so reading past the
            // remainder would be discarded. With a pattern the batch must exceed the
            // remainder, or a page over a sparse match set costs one round trip per
            // matching row. `SCAN_BATCH` caps it either way: the remainder is
            // caller-supplied, and one round trip's memory must not be.
            let batch_size = if pattern.is_some() {
                SCAN_BATCH
            } else {
                std::cmp::min((limit - items.len()) as u64, SCAN_BATCH)
            };
            let mut condition =
                Condition::all().add(entity::Column::LifecycleStatus.eq(LifecycleStatus::Active));
            if let Some(after) = &cursor {
                condition = condition.add(entity::Column::GtsId.gt(after.as_str()));
            }
            if let Some((prefix, upper)) = &range {
                condition = condition.add(entity::Column::GtsId.gte(prefix.as_str()));
                if let Some(upper) = upper {
                    condition = condition.add(entity::Column::GtsId.lt(upper.as_str()));
                }
            }

            let batch = entity::Entity::find()
                .filter(condition)
                .secure()
                .scope_with(scope)
                .order_by(entity::Column::GtsId, Order::Asc)
                .limit(batch_size)
                .all(runner)
                .await?;

            let batch_len = batch.len() as u64;
            for model in batch {
                scanned += 1;
                consumed = Some(model.gts_id.clone());
                if matches_pattern(&model.gts_id, pattern) {
                    items.push(row(model));
                    if items.len() == limit {
                        break 'scan;
                    }
                }
            }
            cursor = consumed.clone();
            if batch_len < batch_size {
                exhausted = true;
                break;
            }
        }

        Ok(EntityPage {
            items,
            next_after: consumed,
            has_more: !exhausted,
        })
    }
}

/// Test a stored identifier against a pattern, treating an unparsable stored
/// identifier as a non-match.
///
/// Admission parses every candidate before it reaches the table, so that arm is
/// unreachable through the write path. It stays because the alternative is failing a
/// whole discovery page on one bad row.
fn matches_pattern(gts_id: &str, pattern: Option<&GtsIdPattern>) -> bool {
    let Some(pattern) = pattern else {
        return true;
    };
    GtsId::try_new(gts_id).is_ok_and(|id| id.matches_pattern(pattern))
}

/// The literal prefix a pattern's matches must all share, or `None` when the
/// pattern constrains nothing usable.
///
/// Cut at the first wildcard, then **drop the final segment**. The second step is
/// what makes the range safe rather than merely narrow: a pattern segment matches
/// with minor-version flexibility — `…type.v1~` also matches `…type.v1.0~`, whose
/// bytes do not start with `…type.v1~` — and dropping the final segment removes
/// exactly the part that flexibility can vary. The cost is some rows the pattern
/// then rejects, which is the right direction: over-admitting costs a comparison,
/// under-admitting loses a real match silently.
fn prefilter_prefix(pattern: &GtsIdPattern) -> Option<String> {
    let raw = pattern.pattern();
    let head = raw.split('*').next().unwrap_or_default();
    let without_boundary = head.trim_end_matches(['.', '~']);
    let cut = without_boundary.rfind(['.', '~']).map(|i| i + 1)?;
    Some(head[..cut].to_owned())
}

/// Exclusive upper bound of a byte-order prefix range: the prefix with its last
/// byte incremented.
///
/// Exact on every backend because the identifier columns carry binary collation,
/// and total because the GTS grammar is ASCII-only, so the last byte of a prefix
/// is always well below `0xFF`.
fn range_upper_bound(prefix: &str) -> Option<String> {
    let mut bytes = prefix.as_bytes().to_vec();
    let last = bytes.last_mut()?;
    if *last == u8::MAX {
        return None;
    }
    *last += 1;
    String::from_utf8(bytes).ok()
}

#[cfg(test)]
#[path = "entity_repo_test.rs"]
mod tests;
