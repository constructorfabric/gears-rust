//! Platform policy persistence, with no synthetic tenant ownership.
use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "qe_policy_versions")]
#[secure(no_tenant, no_resource, no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub policy_id: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub version: i64,
    pub state: String,
    pub payload: String,
}
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::policy::Entity",
        from = "Column::PolicyId",
        to = "super::policy::Column::Id"
    )]
    Policy,
}
impl ActiveModelBehavior for ActiveModel {}
