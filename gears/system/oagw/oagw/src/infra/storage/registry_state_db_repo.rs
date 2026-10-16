//! `DbRegistryStateStore`: `RegistryStateStore` on `oagw_registry_state`.
//!
//! Every method is one statement on the seed row (`id = 1`): a read of the
//! generation and hash, or a conditional `UPDATE` whose affected-row count
//! is the answer. Lease deadlines are computed by the database clock, never
//! bound from the pod's: `PostgreSQL` `now()`, `MySQL` `NOW(6)` (UTC, as
//! the driver sets the session), `SQLite` `datetime('now')`, the same
//! expressions as the outbox's leases. On `SQLite` a comparison wraps both
//! sides in `datetime()`, so the stored text and "now" compare as times.

use std::time::Duration;

use async_trait::async_trait;
use sea_orm::sea_query::{Condition, Expr, Func};
use sea_orm::{ColumnTrait, DbBackend, EntityTrait, FromQueryResult, QueryFilter, QuerySelect};
use time::OffsetDateTime;
use toolkit_db::DBProvider;
use toolkit_db::secure::{AccessScope, SecureEntityExt, SecureUpdateExt};
use uuid::Uuid;

use super::entity::registry_state::{Column, Entity, ROW_ID};
use super::error::internal;
use crate::domain::repo::{RegistryState, RegistryStateStore, RepositoryError};

/// The columns the gate reads; the timestamps are never decoded.
#[derive(FromQueryResult)]
struct StateRow {
    generation: Option<i64>,
    content_hash: Option<String>,
}

/// Database-backed registry-state store.
pub(crate) struct DbRegistryStateStore {
    db: DBProvider<RepositoryError>,
}

impl DbRegistryStateStore {
    /// Create a store over the given database provider.
    pub(crate) fn new(db: DBProvider<RepositoryError>) -> Self {
        Self { db }
    }

    fn backend(&self) -> DbBackend {
        self.db.db().backend()
    }
}

/// "Now" on the database clock. A constant of this code, so it is written
/// into the statement.
fn now(backend: DbBackend) -> Expr {
    Expr::cust(match backend {
        DbBackend::Postgres => "now()",
        DbBackend::MySql => "NOW(6)",
        _ => "datetime('now')",
    })
}

/// "Now plus `ttl`" on the database clock. The whole seconds are an integer
/// this code computes, so they are written into the statement.
fn deadline(backend: DbBackend, ttl: Duration) -> Expr {
    let secs = seconds(ttl);
    Expr::cust(match backend {
        DbBackend::Postgres => format!("now() + ({secs} * INTERVAL '1 second')"),
        DbBackend::MySql => format!("DATE_ADD(NOW(6), INTERVAL {secs} SECOND)"),
        _ => format!("datetime('now', '+{secs} seconds')"),
    })
}

/// "The lease has lapsed" on the database clock. The column comes from the
/// entity; `SQLite` compares both sides as `datetime()`.
fn lease_expired(backend: DbBackend) -> Expr {
    // Scoped: `ExprTrait` would shadow `Ord::max` on integers in this module.
    use sea_orm::sea_query::ExprTrait;

    let expires_at = Expr::col(Column::LeaseExpiresAt);
    if matches!(backend, DbBackend::Postgres | DbBackend::MySql) {
        return expires_at.lt(now(backend));
    }
    Expr::expr(Func::cust("datetime").arg(expires_at)).lt(now(backend))
}

/// Whole seconds, at least one.
fn seconds(ttl: Duration) -> i64 {
    i64::try_from(ttl.as_secs().max(1)).unwrap_or(i64::MAX)
}

fn generation_value(generation: u64) -> Result<i64, RepositoryError> {
    i64::try_from(generation).map_err(|_| {
        RepositoryError::Internal(format!("registry generation {generation} exceeds BIGINT"))
    })
}

fn seed_missing() -> RepositoryError {
    RepositoryError::Internal(
        "oagw_registry_state has no seed row (id = 1); were the OAGW migrations applied?".into(),
    )
}

impl DbRegistryStateStore {
    /// Run `update` on the seed row and return how many rows it changed.
    async fn update(
        &self,
        update: sea_orm::UpdateMany<Entity>,
        condition: Condition,
    ) -> Result<u64, RepositoryError> {
        let conn = self.db.conn()?;
        let result = update
            .filter(Condition::all().add(Column::Id.eq(ROW_ID)).add(condition))
            .secure()
            .scope_with(&AccessScope::allow_all())
            .exec(&conn)
            .await
            .map_err(internal)?;
        Ok(result.rows_affected)
    }
}

#[async_trait]
impl RegistryStateStore for DbRegistryStateStore {
    async fn read(&self) -> Result<RegistryState, RepositoryError> {
        let conn = self.db.conn()?;
        let row = Entity::find()
            .filter(Column::Id.eq(ROW_ID))
            .secure()
            .scope_with(&AccessScope::allow_all())
            .project_all(&conn, |q| {
                q.select_only()
                    .column(Column::Generation)
                    .column(Column::ContentHash)
                    .into_model::<StateRow>()
            })
            .await
            .map_err(internal)?
            .into_iter()
            .next()
            .ok_or_else(seed_missing)?;
        let generation = row
            .generation
            .map(|g| {
                u64::try_from(g).map_err(|_| {
                    RepositoryError::Internal(format!("stored registry generation {g} is negative"))
                })
            })
            .transpose()?;
        Ok(RegistryState {
            generation,
            content_hash: row.content_hash,
        })
    }

    async fn claim(&self, generation: u64) -> Result<bool, RepositoryError> {
        let g = generation_value(generation)?;
        let changed = self
            .update(
                Entity::update_many().col_expr(Column::Generation, Expr::value(g)),
                Condition::any()
                    .add(Column::Generation.is_null())
                    .add(Column::Generation.lt(g)),
            )
            .await?;
        Ok(changed == 1)
    }

    async fn try_acquire(
        &self,
        owner: Uuid,
        ttl: Duration,
        generation: Option<u64>,
    ) -> Result<bool, RepositoryError> {
        let backend = self.backend();
        // The run may stop half-way: forget the last clean run's hash, so a
        // pod with that content does not take the rows as up to date.
        let mut update = Entity::update_many()
            .col_expr(Column::LeaseOwner, Expr::value(owner))
            .col_expr(Column::LeaseExpiresAt, deadline(backend, ttl))
            .col_expr(Column::ContentHash, Expr::value(Option::<String>::None))
            .col_expr(
                Column::AppliedAt,
                Expr::value(Option::<OffsetDateTime>::None),
            );
        let mut condition = Condition::all().add(
            Condition::any()
                .add(Column::LeaseOwner.is_null())
                .add(lease_expired(backend))
                .add(Column::LeaseOwner.eq(owner)),
        );
        if let Some(generation) = generation {
            let g = generation_value(generation)?;
            update = update.col_expr(Column::Generation, Expr::value(g));
            condition = condition.add(
                Condition::any()
                    .add(Column::Generation.is_null())
                    .add(Column::Generation.lte(g)),
            );
        }
        Ok(self.update(update, condition).await? == 1)
    }

    async fn renew(&self, owner: Uuid, ttl: Duration) -> Result<bool, RepositoryError> {
        let backend = self.backend();
        let changed = self
            .update(
                Entity::update_many().col_expr(Column::LeaseExpiresAt, deadline(backend, ttl)),
                Condition::all().add(Column::LeaseOwner.eq(owner)),
            )
            .await?;
        Ok(changed == 1)
    }

    async fn finish(
        &self,
        owner: Uuid,
        content_hash: Option<&str>,
    ) -> Result<bool, RepositoryError> {
        let backend = self.backend();
        let mut update = Entity::update_many()
            .col_expr(Column::LeaseOwner, Expr::value(Option::<Uuid>::None))
            .col_expr(
                Column::LeaseExpiresAt,
                Expr::value(Option::<OffsetDateTime>::None),
            );
        if let Some(hash) = content_hash {
            update = update
                .col_expr(Column::ContentHash, Expr::value(hash.to_owned()))
                .col_expr(Column::AppliedAt, now(backend));
        }
        let changed = self
            .update(update, Condition::all().add(Column::LeaseOwner.eq(owner)))
            .await?;
        Ok(changed == 1)
    }
}
