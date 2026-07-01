//! Repository for the `retention_rules` table.

use sea_orm::{ColumnTrait, Condition, EntityTrait, QueryFilter, Set};
use time::OffsetDateTime;
use toolkit_db::secure::{DBRunner, SecureDeleteExt, SecureEntityExt, secure_insert};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::policy::{RetentionRuleBody, RetentionScope, StoredRetentionRule};
use crate::infra::storage::db::db_err;
use crate::infra::storage::entity::retention_rule::{ActiveModel, Column, Entity, Model};

/// Parameters for inserting a new retention rule.
pub struct InsertRetentionRule<'a> {
    pub tenant_id: Uuid,
    pub retention_scope: &'a RetentionScope,
    pub scope_target_id: Option<Uuid>,
    pub body: &'a RetentionRuleBody,
    pub now: OffsetDateTime,
}

/// Repository over the `retention_rules` table.
#[derive(Clone, Default)]
pub struct RetentionRuleRepo;

impl RetentionRuleRepo {
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// List all retention rules for a tenant (all scopes).
    pub async fn list_for_tenant<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
    ) -> Result<Vec<StoredRetentionRule>, DomainError> {
        let rows = Entity::find()
            .filter(Column::TenantId.eq(tenant_id))
            .secure()
            .scope_with(scope)
            .all(conn)
            .await
            .map_err(db_err)?;

        rows.into_iter().map(map_model).collect()
    }

    /// Fetch a single retention rule by `rule_id`.
    pub async fn get<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        rule_id: Uuid,
    ) -> Result<Option<StoredRetentionRule>, DomainError> {
        let model = Entity::find()
            .filter(Column::RuleId.eq(rule_id))
            .secure()
            .scope_with(scope)
            .one(conn)
            .await
            .map_err(db_err)?;

        model.map(map_model).transpose()
    }

    /// Insert a new retention rule. Returns the assigned `rule_id`.
    pub async fn insert<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        params: InsertRetentionRule<'_>,
    ) -> Result<Uuid, DomainError> {
        let rule_id = Uuid::now_v7();
        let body_json = serde_json::to_string(params.body)
            .map_err(|e| DomainError::database(format!("retention body serialization: {e}")))?;

        let am = ActiveModel {
            rule_id: Set(rule_id),
            tenant_id: Set(params.tenant_id),
            scope: Set(params.retention_scope.as_str().to_owned()),
            scope_target_id: Set(params.scope_target_id),
            body: Set(body_json),
            created_at: Set(params.now),
        };
        secure_insert::<Entity>(am, scope, conn)
            .await
            .map_err(db_err)?;
        Ok(rule_id)
    }

    /// List retention rules for a specific file (`scope = 'file'`), across all
    /// tenants — for the retention sweep engine.
    ///
    /// @cpt-cf-file-storage-fr-retention-policies
    pub async fn list_by_file_scope<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        file_id: Uuid,
    ) -> Result<Vec<StoredRetentionRule>, DomainError> {
        let rows = Entity::find()
            .filter(
                Condition::all()
                    .add(Column::Scope.eq("file"))
                    .add(Column::ScopeTargetId.eq(file_id)),
            )
            .secure()
            .scope_with(scope)
            .all(conn)
            .await
            .map_err(db_err)?;

        rows.into_iter().map(map_model).collect()
    }

    /// List every retention rule across all tenants and scopes — for the
    /// retention sweep engine.
    ///
    /// @cpt-cf-file-storage-fr-retention-policies
    pub async fn list_all<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
    ) -> Result<Vec<StoredRetentionRule>, DomainError> {
        let rows = Entity::find()
            .secure()
            .scope_with(scope)
            .all(conn)
            .await
            .map_err(db_err)?;

        rows.into_iter().map(map_model).collect()
    }

    /// Delete a retention rule by `rule_id`. Returns `true` if a row was removed.
    pub async fn delete<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        rule_id: Uuid,
    ) -> Result<bool, DomainError> {
        let res = Entity::delete_many()
            .filter(Column::RuleId.eq(rule_id))
            .secure()
            .scope_with(scope)
            .exec(conn)
            .await
            .map_err(db_err)?;
        Ok(res.rows_affected > 0)
    }
}

#[allow(clippy::needless_pass_by_value)]
fn map_model(m: Model) -> Result<StoredRetentionRule, DomainError> {
    let scope = RetentionScope::parse(&m.scope).ok_or_else(|| {
        DomainError::database(format!("invalid retention scope in DB: {}", m.scope))
    })?;
    let body: RetentionRuleBody = serde_json::from_str(&m.body)
        .map_err(|e| DomainError::database(format!("retention body deserialization: {e}")))?;
    Ok(StoredRetentionRule {
        rule_id: m.rule_id,
        tenant_id: m.tenant_id,
        scope,
        scope_target_id: m.scope_target_id,
        body,
        created_at: m.created_at,
    })
}
