//! The upstream row lock that orders the writes hanging off an upstream:
//! route creates and updates (for the overlap check) and the removal of a
//! dropped registry upstream (for its in-use check). Each runs in one READ
//! COMMITTED transaction that first locks the upstream row, so its later
//! reads see what a writer before it committed.

use sea_orm::sea_query::Expr;
use sea_orm::{ColumnTrait, Condition, DbBackend, EntityTrait, QueryFilter, QuerySelect};
use toolkit_db::secure::{
    AccessScope, DBRunner, ScopeError, SecureEntityExt, SecureUpdateExt, TxConfig, TxIsolationLevel,
};
use uuid::Uuid;

use super::entity::upstream;
use super::mapper::managed_by_text;
use crate::domain::model::ManagedBy;

/// READ COMMITTED where the backend has isolation levels, so a read after
/// the row lock sees a row committed meanwhile (`MySQL` defaults to
/// REPEATABLE READ); `SQLite` serializes writers and takes no level.
pub(super) fn read_committed_for(backend: DbBackend) -> TxConfig {
    if backend == DbBackend::Sqlite {
        return TxConfig::default();
    }
    TxConfig {
        isolation: Some(TxIsolationLevel::ReadCommitted),
        access_mode: None,
    }
}

/// The unique index the route foreign key references (`(tenant_id, id)`).
#[cfg(feature = "mysql")]
const UPSTREAM_TENANT_ID_INDEX: &str = "uq_oagw_upstream_tenant_id";

/// Lock the tenant's upstream `id` until the transaction ends, as the
/// transaction's first statement. `owner` limits the lock to a row of that
/// owner. `false` when there is no such row, and nothing is locked.
///
/// - `PostgreSQL`: a locking read (`FOR UPDATE`), which conflicts with the
///   key-share lock a route insert's foreign key check takes.
/// - `MySQL`: a locking read by primary key, then one through the index the
///   route foreign key references. `InnoDB`'s foreign key check share-locks
///   the parent's record in that index, and a primary key read locks only
///   the clustered record, so without the second read a removal would miss
///   an uncommitted route, then cascade it away. The primary key comes
///   first because an upstream update or delete locks the clustered record
///   before that index record (its own foreign key checks, the delete mark):
///   one order for every writer, so they wait instead of deadlocking.
/// - `SQLite`: a write (`UPDATE … SET enabled = enabled`, which changes
///   nothing). `SQLite` takes no row lock, and a transaction that reads
///   before it writes fails its first write as busy at once when another
///   writer is active, without waiting for the busy timeout; starting with
///   a write takes the database's write lock under the busy timeout.
pub(super) async fn lock_upstream(
    runner: &impl DBRunner,
    backend: DbBackend,
    tenant_id: Uuid,
    id: Uuid,
    owner: Option<ManagedBy>,
) -> Result<bool, ScopeError> {
    let scope = AccessScope::for_tenant(tenant_id);
    let mut row = Condition::all()
        .add(upstream::Column::TenantId.eq(tenant_id))
        .add(upstream::Column::Id.eq(id));
    if let Some(owner) = owner {
        row = row.add(upstream::Column::ManagedBy.eq(managed_by_text(owner)));
    }
    if backend == DbBackend::Sqlite {
        let touched = upstream::Entity::update_many()
            .secure()
            .col_expr(
                upstream::Column::Enabled,
                Expr::col(upstream::Column::Enabled),
            )
            .filter(row)
            .scope_with(&scope)
            .exec(runner)
            .await?;
        return Ok(touched.rows_affected > 0);
    }
    #[cfg(feature = "mysql")]
    if backend == DbBackend::MySql {
        for index in ["PRIMARY", UPSTREAM_TENANT_ID_INDEX] {
            if !locking_read(runner, &scope, row.clone(), Some(index)).await? {
                return Ok(false);
            }
        }
        return Ok(true);
    }
    locking_read(runner, &scope, row, None).await
}

/// `SELECT … FOR UPDATE` of the upstream `row`, through `index` if given.
async fn locking_read(
    runner: &impl DBRunner,
    scope: &AccessScope,
    row: Condition,
    index: Option<&str>,
) -> Result<bool, ScopeError> {
    #[cfg_attr(
        not(feature = "mysql"),
        expect(unused_mut, reason = "only the MySQL build adds an index hint")
    )]
    let mut select = upstream::Entity::find().filter(row).lock_exclusive();
    #[cfg(feature = "mysql")]
    if let Some(index) = index {
        use sea_orm::QueryTrait;
        use sea_orm::sea_query::Alias;
        use sea_orm::sea_query::extension::mysql::{IndexHintScope, MySqlSelectStatementExt};

        QueryTrait::query(&mut select).force_index(Alias::new(index), IndexHintScope::All);
    }
    #[cfg(not(feature = "mysql"))]
    let _ = index;
    Ok(select
        .secure()
        .scope_with(scope)
        .one(runner)
        .await?
        .is_some())
}

/// Holds the next transaction of a repository open after its last
/// statement, for the race tests.
#[cfg(all(test, feature = "integration"))]
#[derive(Clone, Default)]
pub(crate) struct Hold(std::sync::Arc<parking_lot::Mutex<Option<HoldBeforeCommit>>>);

#[cfg(all(test, feature = "integration"))]
impl Hold {
    /// Hold the next transaction that reaches [`Self::wait`].
    pub(crate) fn set(&self, hold: HoldBeforeCommit) {
        *self.0.lock() = Some(hold);
    }

    /// Inside a transaction, after its last statement: if a hold is set,
    /// signal `locked` and wait for `release`.
    pub(crate) async fn wait(&self) {
        let held = self.0.lock().take();
        if let Some(held) = held {
            let _ = held.locked.send(());
            let _ = held.release.await;
        }
    }
}

/// The two ends of a [`Hold`].
#[cfg(all(test, feature = "integration"))]
pub(crate) struct HoldBeforeCommit {
    /// Fired once the transaction has run its statements.
    pub(crate) locked: tokio::sync::oneshot::Sender<()>,
    /// The transaction commits once this fires (or its sender drops).
    pub(crate) release: tokio::sync::oneshot::Receiver<()>,
}
