//! Storage row; never a public SDK model.
use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
/// Per-audit-tenant roll-up checkpoint over the committed order audit chains.
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "bss_orders__audit_checkpoint")]
#[secure(tenant_col = "audit_tenant_id", no_resource, no_owner, no_type)]
pub struct Model {
    /// Immutable audit namespace whose committed order chains are summarized.
    #[sea_orm(primary_key, auto_increment = false)]
    pub audit_tenant_id: Uuid,
    /// Positive per-tenant checkpoint counter, starting at 1.
    #[sea_orm(primary_key, auto_increment = false)]
    pub checkpoint_sequence: i64,
    /// Roll-up encoding version (currently 1), separate from the audit row encoding.
    pub format_version: i16,
    /// Snapshot capture instant; not a transition commit watermark.
    pub captured_at: TimeDateTimeWithTimeZone,
    /// Number of order heads in this snapshot.
    pub member_count: i64,
    /// 32-byte digest of the preceding checkpoint, or the tenant genesis value.
    pub prev_checkpoint_hash: Vec<u8>,
    /// 32-byte digest over this header and its sorted members.
    pub checkpoint_hash: Vec<u8>,
}
/// No `SeaORM` relations are declared for this table.
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}
impl ActiveModelBehavior for ActiveModel {}
