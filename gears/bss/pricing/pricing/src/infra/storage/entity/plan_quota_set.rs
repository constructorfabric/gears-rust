//! Tenant-scoped storage for `pricing_plan_quota_set`: a revision's set row, with the digest its
//! rules were written with (D-541, D-546).
use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "pricing_plan_quota_set")]
#[secure(
    tenant_col = "tenant_id",
    resource_col = "revision_id",
    no_owner,
    no_type
)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub revision_id: Uuid,
    pub tenant_id: Uuid,
    /// The set digest as written, 64 lowercase hex characters; no read computes it again.
    pub quota_rules_digest: String,
    /// The number of rules, 1 to 200: a revision without rules has no set row.
    pub rule_count: i32,
    pub updated_by: Uuid,
    pub updated_at: TimeDateTimeWithTimeZone,
}
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}
impl ActiveModelBehavior for ActiveModel {}
