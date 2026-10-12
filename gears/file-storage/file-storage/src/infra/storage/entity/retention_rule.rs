//! `SeaORM` entity for the `retention_rules` table (per-tenant / per-user / per-file).
//!
//! `tenant_id` is the tenant boundary; see `scope_target_id` for the target.

use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "retention_rules")]
#[secure(tenant_col = "tenant_id", resource_col = "rule_id", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub rule_id: Uuid,
    pub tenant_id: Uuid,
    /// `"tenant"`, `"user"`, or `"file"`.
    pub scope: String,
    /// NULL for tenant scope, `user_id` for user scope, `file_id` for file scope.
    pub scope_target_id: Option<Uuid>,
    /// Retention rule body serialized as JSON (see `RetentionRuleBody`).
    ///
    /// Stored as `jsonb` on `Postgres` and `TEXT` on `SQLite`, matching the DDL.
    #[sea_orm(column_type = "Json")]
    pub body: Json,
    pub created_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Postgres DDL declares `jsonb`; a `Text` column type would fail there
    /// while `SQLite` tests still pass.
    #[test]
    fn body_column_is_json_typed() {
        assert_eq!(Column::Body.def().get_column_type(), &ColumnType::Json);
    }
}
