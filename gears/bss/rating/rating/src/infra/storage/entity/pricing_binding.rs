//! `bss_rating__pricing_binding` (DESIGN §3.7 `cpt-cf-bss-rating-dbtable-pricing-binding`): one
//! row per distinct answer, content-addressed; insert-only.
use sea_orm::entity::prelude::*;
use serde_json::Value as JsonValue;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "bss_rating__pricing_binding")]
#[secure(tenant_col = "tenant_id", no_resource, no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub tenant_id: Uuid,
    /// Pricing's `selected_bindings_digest` of the covered cells.
    #[sea_orm(primary_key, auto_increment = false)]
    pub binding_digest: String,
    pub plan_id: Uuid,
    pub revision_id: Uuid,
    /// The cells, as the core reads them.
    pub cells: JsonValue,
    pub first_fetched_at: TimeDateTimeWithTimeZone,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
