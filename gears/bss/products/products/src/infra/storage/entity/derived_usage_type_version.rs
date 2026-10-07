//! Tenant-scoped storage model for `products_derived_usage_type_version` (P-D-231): one immutable
//! declaration per version, with the digest stored at insert. The table's triggers refuse every
//! `UPDATE` and `DELETE`; this entity carries no write besides the insert.
use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "products_derived_usage_type_version")]
#[secure(tenant_col = "tenant_id", resource_col = "type_id", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub tenant_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub type_id: Uuid,
    /// 1, 2, …: the `<n>` of the meter id.
    #[sea_orm(primary_key, auto_increment = false)]
    pub version: i64,
    /// The declaration as the doors serve it.
    pub declaration_json: Json,
    /// The SHA-256 of the SDK's canonical bytes, 64 lowercase hex digits.
    pub digest: String,
    pub created_by: Uuid,
    pub created_at: TimeDateTimeWithTimeZone,
}
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::derived_usage_type::Entity",
        from = "Column::TypeId",
        to = "super::derived_usage_type::Column::Id"
    )]
    Type,
}
impl ActiveModelBehavior for ActiveModel {}
