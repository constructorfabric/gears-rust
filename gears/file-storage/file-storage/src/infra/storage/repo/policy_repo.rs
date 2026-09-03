//! Repository for the `policies` table (per-tenant / per-user policy store).
//!
//! Uses `SecureORM` (`toolkit_db::secure`) for tenant-scoped access, consistent
//! with the other repositories in this gear.

use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, Set};
use time::OffsetDateTime;
use toolkit_db::secure::{DBRunner, SecureDeleteExt, SecureEntityExt, secure_insert};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::policy::{PolicyBody, PolicyScope, StoredPolicy};
use crate::infra::storage::db::{conflict_on_unique_violation, db_err};
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
            .map_err(db_err)?;

        model.map(map_model).transpose()
    }

    /// Insert or replace the policy row for the given scope.
    ///
    /// Since there is at most one policy per `(tenant_id, scope, scope_owner_id)`,
    /// this first deletes any existing row for that combination, then inserts the
    /// new one.
    ///
    /// Callers (`Store::upsert_policy`) run this inside an explicit DB
    /// transaction so the delete+insert pair is atomic, and the
    /// `policies_user_scope_unique_idx` / `policies_tenant_scope_unique_idx`
    /// partial unique indexes (migration `m20260706_000003`) act as a
    /// backstop against the remaining no-existing-row race between two
    /// concurrent first-time upserts for the same scope — the losing
    /// writer's insert fails with a constraint violation rather than
    /// silently duplicating the row.
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
        // Delete any existing row for this scope before inserting.
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
            .map_err(db_err)?;

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
        // Two concurrent first-time upserts for the same
        // `(tenant_id, scope, scope_owner_id)` both see nothing to delete
        // above and both reach this insert; the partial unique indexes
        // (`policies_tenant_scope_unique_idx` / `policies_user_scope_unique_idx`,
        // migration `m20260706_000003_policies_unique_scope.rs`) are what
        // stops the loser from creating a second row. Classify that loss as
        // a conflict rather than an opaque `db_err` 500, so the losing
        // caller gets a 409 it can react to (re-fetch via `get` and, if it
        // still wants its own values, retry the upsert against the row that
        // won).
        secure_insert::<Entity>(am, scope, conn)
            .await
            .map_err(|e| {
                conflict_on_unique_violation(
                    e,
                    "a policy for this scope was just created by a concurrent request",
                )
            })?;
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
