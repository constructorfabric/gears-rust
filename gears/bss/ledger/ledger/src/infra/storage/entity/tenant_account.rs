//! `SeaORM` entity for `bss.ledger_tenant_account` (chart of accounts).

use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "ledger_tenant_account")]
#[secure(
    tenant_col = "tenant_id",
    resource_col = "account_id",
    no_owner,
    no_type
)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub account_id: Uuid,
    pub tenant_id: Uuid,
    pub legal_entity_id: Uuid,
    pub account_class: String,
    pub currency: String,
    pub revenue_stream: Option<String>,
    pub normal_side: String,
    pub may_go_negative: bool,
    pub lifecycle_state: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
