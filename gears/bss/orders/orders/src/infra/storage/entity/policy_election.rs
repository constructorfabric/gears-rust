//! Storage row; never a public SDK model.
use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
/// Deployment-promoted policy election at platform or seller scope.
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "bss_orders__policy_election")]
#[secure(no_tenant, resource_col = "policy_id", no_owner, no_type)]
pub struct Model {
    /// Policy identity.
    #[sea_orm(primary_key, auto_increment = false)]
    pub policy_id: Uuid,
    /// `tolerate_authorization_failure` or `acceptance_required`.
    pub election: String,
    /// `platform` or `seller`.
    pub scope: String,
    /// Seller tenant for seller scope; NULL for platform scope.
    pub scope_id: Option<Uuid>,
    /// Whether the election is made.
    pub elected: bool,
    /// Change identity of the promotion that set the row.
    pub elected_by: String,
    /// Instant of that promotion.
    pub elected_at: TimeDateTimeWithTimeZone,
}
/// No `SeaORM` relations are declared for this table.
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}
impl ActiveModelBehavior for ActiveModel {}
