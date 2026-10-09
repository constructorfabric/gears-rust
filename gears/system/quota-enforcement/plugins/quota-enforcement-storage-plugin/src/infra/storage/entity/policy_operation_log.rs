//! Platform policy persistence, with no synthetic tenant ownership.
use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "qe_policy_operation_log")]
#[secure(no_tenant, no_resource, no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: String,
    pub policy_id: String,
    pub version: i64,
    pub operation: String,
    pub actor: String,
    pub comment: Option<String>,
    pub occurred_at: String,
}
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}
impl ActiveModelBehavior for ActiveModel {}
