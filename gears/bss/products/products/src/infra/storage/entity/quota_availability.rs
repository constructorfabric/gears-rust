//! Tenant-scoped storage model for `products_quota_availability` (P-D-271, P-D-274): one
//! availability observation of a definition version; the current one is the highest revision. The
//! table's triggers refuse every `UPDATE` and `DELETE`; this entity carries no write besides the
//! insert.
use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "products_quota_availability")]
#[secure(
    tenant_col = "tenant_id",
    resource_col = "definition_id",
    no_owner,
    no_type
)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub tenant_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub definition_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub version: i64,
    #[sea_orm(primary_key, auto_increment = false)]
    pub revision: i64,
    /// `enabled`, `disabled` or `withdrawn`.
    pub state: String,
    /// Null exactly when the state is `enabled`.
    pub reason_code: Option<String>,
    pub qualification_id: Option<Uuid>,
    pub qualification_version: Option<i64>,
    pub qualification_digest: Option<String>,
    pub observed_at: TimeDateTimeWithTimeZone,
    pub decided_by: Uuid,
}
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}
impl ActiveModelBehavior for ActiveModel {}
