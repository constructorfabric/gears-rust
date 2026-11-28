//! Tenant-scoped storage for `pricing_plan_quota_rule`: one quota rule of a plan revision
//! (D-541, D-546).
use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "pricing_plan_quota_rule")]
#[secure(tenant_col = "tenant_id", resource_col = "rule_id", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub revision_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub rule_id: Uuid,
    pub tenant_id: Uuid,
    pub item_id: Uuid,
    pub definition_id: Uuid,
    /// The definition version as decimal text (1 to 20 characters).
    pub definition_version: String,
    /// The definition's digest, 64 lowercase hex characters.
    pub definition_digest: String,
    pub dimension_value: Option<String>,
    pub logical_binding_key: String,
    pub subject_selector: String,
    /// The scope selection, a JSON object of strings, as text.
    pub scope_selection_json: String,
    /// A canonical decimal integer in `unit_ref`.
    pub amount: String,
    pub unit_ref: String,
    pub basis: String,
    /// The definition's identity-bearing content without text or availability (D-542), as text.
    pub evidence_json: String,
    /// The availability the rule saw at its save (D-542), as text.
    pub availability_json: String,
    pub created_at: TimeDateTimeWithTimeZone,
}
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}
impl ActiveModelBehavior for ActiveModel {}
