use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

/// One named setting row. Scoped exactly like `settings`: the tenant is the
/// tenant column and the user is the resource, so one `AccessScope` covers both
/// tables.
///
/// `value` is the JSON value serialized to text. Every backend stores it the
/// same way, and nothing queries into it.
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "named_settings")]
#[secure(tenant_col = "tenant_id", resource_col = "user_id", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub tenant_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub user_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub key: String,
    pub value: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
