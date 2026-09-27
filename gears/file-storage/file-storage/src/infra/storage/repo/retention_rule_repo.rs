//! Repository for the `retention_rules` table.

use sea_orm::sea_query::Query;
use sea_orm::{ColumnTrait, Condition, EntityTrait, QueryFilter, QueryOrder, QuerySelect, Set};
use toolkit_db::secure::{DBRunner, SecureDeleteExt, SecureEntityExt, secure_insert};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::policy::{RetentionRuleBody, RetentionScope, StoredRetentionRule};
use crate::infra::storage::db::db_err;
use crate::infra::storage::entity::file::{Column as FileColumn, Entity as FileEntity};
use crate::infra::storage::entity::retention_rule::{ActiveModel, Column, Entity, Model};

use super::{InsertRetentionRule, RetentionRuleListParams};

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

    /// List retention rules for a tenant, forward-only keyset-paginated
    ///, with the non-admin visibility filter
    /// applied in SQL -- see [`RetentionRuleListParams`]'s field docs and
    /// `PolicyStore::list_retention_rules_page`'s doc comment for the exact
    /// semantics this reproduces (1:1) from the old filter-after-fetch
    /// application logic.
    ///
    /// Ordered `(created_at, rule_id)` descending -- same tie-breaker
    /// reasoning as `FileRepo::list_page`/`VersionRepo::list_by_file_page`.
    /// `after`, when `Some`, restricts the result to rows strictly past that
    /// position in this same descending order.
    pub async fn list_page<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        params: RetentionRuleListParams<'_>,
    ) -> Result<Vec<StoredRetentionRule>, DomainError> {
        let mut filter = Condition::all().add(Column::TenantId.eq(params.tenant_id));
        if !params.admin {
            // `EXISTS`-free `IN` subquery over the caller's own files --
            // mirrors `FileRepo::delete_if_orphan`/`list_versionless_orphan_files`'s
            // correlated-subquery style, here uncorrelated (own files are a
            // small, tenant+owner-scoped set independent of the outer row).
            let own_files = Query::select()
                .column(FileColumn::FileId)
                .from(FileEntity)
                .and_where(FileColumn::TenantId.eq(params.tenant_id))
                .and_where(FileColumn::OwnerKind.eq(params.subject_kind))
                .and_where(FileColumn::OwnerId.eq(params.subject_id))
                .to_owned();
            let visible = Condition::any()
                .add(Column::Scope.eq(RetentionScope::Tenant.as_str()))
                .add(
                    Condition::all()
                        .add(Column::Scope.eq(RetentionScope::User.as_str()))
                        .add(Column::ScopeTargetId.eq(params.subject_id)),
                )
                .add(
                    Condition::all()
                        .add(Column::Scope.eq(RetentionScope::File.as_str()))
                        .add(Column::ScopeTargetId.in_subquery(own_files)),
                );
            filter = filter.add(visible);
        }
        if let Some(seek) = params.after {
            filter = filter.add(super::tuple_lt(
                (Entity, Column::CreatedAt),
                (Entity, Column::RuleId),
                seek.created_at,
                seek.id,
            ));
        }

        let rows = Entity::find()
            .filter(filter)
            .order_by_desc(Column::CreatedAt)
            .order_by_desc(Column::RuleId)
            .limit(params.limit)
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
        let body_json = serde_json::to_value(params.body)
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

    /// Delete every `File`-scope retention rule targeting a specific file
    /// (`scope = 'file' AND scope_target_id = file_id`), in the SAME
    /// transaction that removes the `files` row itself -- see
    /// `Store::delete_file_collecting_versions`/`delete_orphan_file_with_event`/
    /// `delete_version_or_whole_file`'s callers. `retention_rules` has no FK
    /// from `scope_target_id` to `files.file_id` (it is a polymorphic column,
    /// also holding `user_id` for `User`-scope rows), so nothing else removes
    /// these rows once their target file is gone.
    ///
    /// Returns the number of rows removed (`0` when the file had no
    /// `File`-scope rule).
    pub async fn delete_file_scope_rules<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        file_id: Uuid,
    ) -> Result<u64, DomainError> {
        let res = Entity::delete_many()
            .filter(
                Condition::all()
                    .add(Column::Scope.eq("file"))
                    .add(Column::ScopeTargetId.eq(file_id)),
            )
            .secure()
            .scope_with(scope)
            .exec(conn)
            .await
            .map_err(db_err)?;
        Ok(res.rows_affected)
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
    let body: RetentionRuleBody = serde_json::from_value(m.body)
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
