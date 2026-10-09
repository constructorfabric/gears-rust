//! Repository for the `policies` table (per-tenant / per-user policy store).
//!
//! Tenant-scoped via `SecureORM` (`toolkit_db::secure`).

use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, Set};
use time::OffsetDateTime;
use toolkit_db::secure::{DBRunner, SecureDeleteExt, SecureEntityExt, secure_insert};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::policy::{PolicyBody, PolicyScope, StoredPolicy};
use crate::infra::storage::entity::policy::{ActiveModel, Column, Entity, Model};

/// Repository over the `policies` table.
#[derive(Clone, Default)]
pub struct PolicyRepo;

impl PolicyRepo {
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Fetch the policy row for the given `(tenant_id, scope, scope_owner_id)`.
    /// Returns `None` when no matching row exists.
    pub async fn get<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        policy_scope: &PolicyScope,
        scope_owner_id: Option<Uuid>,
    ) -> Result<Option<StoredPolicy>, DomainError> {
        let mut q = Entity::find()
            .filter(Column::TenantId.eq(tenant_id))
            .filter(Column::Scope.eq(policy_scope.as_str()));

        q = match scope_owner_id {
            Some(id) => q.filter(Column::ScopeOwnerId.eq(id)),
            None => q.filter(Column::ScopeOwnerId.is_null()),
        };

        let model = q
            .secure()
            .scope_with(scope)
            .one(conn)
            .await
            .map_err(DomainError::from)?;

        model.map(map_model).transpose()
    }

    /// Insert or replace the policy row for the given scope.
    ///
    /// Deletes any existing row for the scope, then inserts. `Store::upsert_policy`
    /// runs this in a transaction so the pair is atomic; the partial unique indexes
    /// make the loser of two concurrent first-time upserts fail with a constraint
    /// violation instead of duplicating the row.
    #[allow(clippy::too_many_arguments)]
    pub async fn upsert<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        policy_scope: &PolicyScope,
        scope_owner_id: Option<Uuid>,
        body: &PolicyBody,
        now: OffsetDateTime,
    ) -> Result<Uuid, DomainError> {
        let mut del = Entity::delete_many()
            .filter(Column::TenantId.eq(tenant_id))
            .filter(Column::Scope.eq(policy_scope.as_str()));
        del = match scope_owner_id {
            Some(id) => del.filter(Column::ScopeOwnerId.eq(id)),
            None => del.filter(Column::ScopeOwnerId.is_null()),
        };
        del.secure()
            .scope_with(scope)
            .exec(conn)
            .await
            .map_err(DomainError::from)?;

        let policy_id = Uuid::now_v7();
        let body_json = serde_json::to_value(body)
            .map_err(|e| DomainError::database(format!("policy body serialization: {e}")))?;

        let am = ActiveModel {
            policy_id: Set(policy_id),
            tenant_id: Set(tenant_id),
            scope: Set(policy_scope.as_str().to_owned()),
            scope_owner_id: Set(scope_owner_id),
            body: Set(body_json),
            created_at: Set(now),
            updated_at: Set(now),
        };
        secure_insert::<Entity>(am, scope, conn)
            .await
            .map_err(DomainError::from)?;
        Ok(policy_id)
    }
}

#[allow(clippy::needless_pass_by_value)]
fn map_model(m: Model) -> Result<StoredPolicy, DomainError> {
    let scope = PolicyScope::parse(&m.scope)
        .ok_or_else(|| DomainError::database(format!("invalid policy scope in DB: {}", m.scope)))?;
    let body: PolicyBody = serde_json::from_value(m.body)
        .map_err(|e| DomainError::database(format!("policy body deserialization: {e}")))?;
    Ok(StoredPolicy {
        policy_id: m.policy_id,
        tenant_id: m.tenant_id,
        scope,
        scope_owner_id: m.scope_owner_id,
        body,
        created_at: m.created_at,
        updated_at: m.updated_at,
    })
}
