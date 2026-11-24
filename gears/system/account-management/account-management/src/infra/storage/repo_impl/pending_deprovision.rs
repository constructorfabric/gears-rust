//! `SeaORM` adapter for [`PendingDeprovisionRepo`] over the
//! `pending_user_deprovisions` table.
//!
//! Every statement runs under `AccessScope::allow_all()`: the entity is
//! `no_tenant` (AM's own bookkeeping, see its module docs), and the caller —
//! `UserService::delete_user` — has already authorized the request before
//! it touches the record.

use std::sync::Arc;

use async_trait::async_trait;
use sea_orm::{ActiveValue, ColumnTrait, Condition, DbErr, EntityTrait, QueryFilter};
use toolkit_db::secure::{
    ScopeError, SecureDeleteExt, SecureEntityExt, SecureInsertExt, SecureOnConflict,
};
use toolkit_security::AccessScope;
use uuid::Uuid;

use super::AmDbProvider;
use super::helpers::map_scope_err;
use crate::domain::error::DomainError;
use crate::domain::user::pending_deprovision::PendingDeprovisionRepo;
use crate::infra::storage::entity::pending_user_deprovisions;

/// `SeaORM` repository adapter for [`PendingDeprovisionRepo`].
pub struct PendingDeprovisionRepoImpl {
    db: Arc<AmDbProvider>,
}

impl PendingDeprovisionRepoImpl {
    #[must_use]
    pub fn new(db: Arc<AmDbProvider>) -> Self {
        Self { db }
    }
}

/// `tenant_id = $1 AND user_id = $2` — the primary key.
fn key(tenant_id: Uuid, user_id: Uuid) -> Condition {
    Condition::all()
        .add(pending_user_deprovisions::Column::TenantId.eq(tenant_id))
        .add(pending_user_deprovisions::Column::UserId.eq(user_id))
}

#[async_trait]
impl PendingDeprovisionRepo for PendingDeprovisionRepoImpl {
    async fn record(&self, tenant_id: Uuid, user_id: Uuid) -> Result<(), DomainError> {
        let conn = self.db.conn()?;
        let mut on_conflict = SecureOnConflict::<pending_user_deprovisions::Entity>::columns([
            pending_user_deprovisions::Column::TenantId,
            pending_user_deprovisions::Column::UserId,
        ]);
        on_conflict.inner_mut().do_nothing();
        let row = pending_user_deprovisions::ActiveModel {
            tenant_id: ActiveValue::Set(tenant_id),
            user_id: ActiveValue::Set(user_id),
            // Left to the column default so the timestamp is the
            // database's clock, like every other `created_at` in AM.
            created_at: ActiveValue::NotSet,
        };
        let result = pending_user_deprovisions::Entity::insert(row)
            .secure()
            .scope_unchecked(&AccessScope::allow_all())
            .map_err(map_scope_err)?
            .on_conflict(on_conflict)
            .exec(&conn)
            .await;
        match result {
            // `RecordNotInserted` is the `DO NOTHING` arm: the deletion was
            // already recorded by an earlier attempt, which is the goal.
            Ok(_) | Err(ScopeError::Db(DbErr::RecordNotInserted)) => Ok(()),
            Err(err) => Err(map_scope_err(err)),
        }
    }

    async fn is_recorded(&self, tenant_id: Uuid, user_id: Uuid) -> Result<bool, DomainError> {
        let conn = self.db.conn()?;
        let row = pending_user_deprovisions::Entity::find()
            .filter(key(tenant_id, user_id))
            .secure()
            .scope_with(&AccessScope::allow_all())
            .one(&conn)
            .await
            .map_err(map_scope_err)?;
        Ok(row.is_some())
    }

    async fn clear(&self, tenant_id: Uuid, user_id: Uuid) -> Result<(), DomainError> {
        let conn = self.db.conn()?;
        pending_user_deprovisions::Entity::delete_many()
            .filter(key(tenant_id, user_id))
            .secure()
            .scope_with(&AccessScope::allow_all())
            .exec(&conn)
            .await
            .map_err(map_scope_err)?;
        Ok(())
    }
}
