// Created: 2026-09-07 by Constructor Tech
//! `tenant_permissions`: one sparse restriction per `(declaration, tenant)`.

use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

/// One restriction row.
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "tenant_permissions")]
#[secure(tenant_col = "tenant_id", resource_col = "id", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub declaration_id: Uuid,
    pub tenant_id: Uuid,
    pub access: String,
    pub set_by: String,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::declaration::Entity",
        from = "Column::DeclarationId",
        to = "super::declaration::Column::Id",
        on_delete = "Cascade"
    )]
    Declaration,
}

impl Related<super::declaration::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Declaration.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
