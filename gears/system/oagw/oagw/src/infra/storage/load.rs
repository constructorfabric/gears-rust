//! Batched, tenant-scoped child loads shared by both repositories, and the
//! single-row update they both run.
//!
//! A child table is read with one query for all parents, `tenant_id IN
//! (<parents' tenants>) AND <parent>_id IN (<parent ids>)`, never with a join,
//! so a parent without child rows is never dropped. The parents' tenants are
//! always within the calling method's tenants. Inputs past the backend's bind
//! limit are split into several queries; correctness is unaffected.

use std::collections::{BTreeSet, HashMap};

use sea_orm::{ColumnTrait, DbBackend, EntityTrait, QueryFilter};
use toolkit_db::secure::{
    AccessScope, DBRunner, ScopableEntity, ScopeError, Scoped, SecureEntityExt, SecureUpdateMany,
    TxAccessMode, TxConfig, TxIsolationLevel, max_bind_params_for,
};
use uuid::Uuid;

use super::error::internal;
use crate::domain::repo::RepositoryError;

/// Read-only REPEATABLE READ, for a management read whose parent and child
/// queries must see one snapshot. Under the default READ COMMITTED a
/// concurrent write could tear the aggregate, or a concurrent delete could
/// leave a route row without its match row.
pub(super) fn snapshot() -> TxConfig {
    TxConfig {
        isolation: Some(TxIsolationLevel::RepeatableRead),
        access_mode: Some(TxAccessMode::ReadOnly),
    }
}

/// A parent row: `(id, tenant_id)`.
pub(super) type Parent = (Uuid, Uuid);

/// Most values one `IN (..)` list may bind on `runner`'s backend, leaving
/// `reserved` parameters for the rest of the statement.
pub(super) fn max_in_list(runner: &impl DBRunner, reserved: usize) -> usize {
    max_bind_params_for(runner).saturating_sub(reserved).max(1)
}

/// Rows of child entity `E` whose `parent_col` is one of `parents`. No query
/// when `parents` is empty.
pub(super) async fn children<E>(
    runner: &impl DBRunner,
    parent_col: E::Column,
    parents: &[Parent],
) -> Result<Vec<E::Model>, RepositoryError>
where
    E: ScopableEntity + EntityTrait,
    E::Column: ColumnTrait + Copy,
{
    let mut rows = Vec::new();
    // Each chunk binds at most one tenant per parent plus the parent IDs.
    for chunk in parents.chunks(max_in_list(runner, 0).div_euclid(2).max(1)) {
        let tenants: BTreeSet<Uuid> = chunk.iter().map(|(_, tenant)| *tenant).collect();
        rows.extend(
            E::find()
                .filter(parent_col.is_in(chunk.iter().map(|(id, _)| *id)))
                .secure()
                .scope_with(&AccessScope::for_tenants(tenants.into_iter().collect()))
                .all(runner)
                .await
                .map_err(internal)?,
        );
    }
    Ok(rows)
}

/// Group rows by parent ID, keeping their order.
pub(super) fn by_parent<M>(rows: Vec<M>, parent: impl Fn(&M) -> Uuid) -> HashMap<Uuid, Vec<M>> {
    let mut grouped: HashMap<Uuid, Vec<M>> = HashMap::new();
    for row in rows {
        grouped.entry(parent(&row)).or_default().push(row);
    }
    grouped
}

/// Run `update`, which may match only row `id` in `scope`, and return that row
/// as written, or `None` when it matched nothing. `MySQL` has no
/// `UPDATE … RETURNING`, so there the row is read back in the same
/// transaction; the other backends return it from the update itself.
pub(super) async fn update_one<E>(
    runner: &impl DBRunner,
    backend: DbBackend,
    update: SecureUpdateMany<E, Scoped>,
    id_col: E::Column,
    id: Uuid,
    scope: &AccessScope,
) -> Result<Option<E::Model>, ScopeError>
where
    E: ScopableEntity + EntityTrait,
    E::Column: ColumnTrait + Copy,
{
    if backend != DbBackend::MySql {
        return Ok(update.exec_with_returning(runner).await?.into_iter().next());
    }
    update.exec(runner).await?;
    E::find()
        .filter(id_col.eq(id))
        .secure()
        .scope_with(scope)
        .one(runner)
        .await
}
