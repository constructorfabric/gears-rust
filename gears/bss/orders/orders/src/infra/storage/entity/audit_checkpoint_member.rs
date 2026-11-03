//! Storage row; never a public SDK model.
use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
/// One order's committed audit-chain head captured by an audit checkpoint.
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "bss_orders__audit_checkpoint_member")]
#[secure(tenant_col = "audit_tenant_id", no_resource, no_owner, no_type)]
pub struct Model {
    /// Audit namespace of the owning checkpoint.
    #[sea_orm(primary_key, auto_increment = false)]
    pub audit_tenant_id: Uuid,
    /// Sequence of the owning checkpoint.
    #[sea_orm(primary_key, auto_increment = false)]
    pub checkpoint_sequence: i64,
    /// Order whose chain head is captured; deliberately no FK to the live order.
    #[sea_orm(primary_key, auto_increment = false)]
    pub order_id: Uuid,
    /// Committed head audit sequence observed in the snapshot.
    pub audit_sequence: i64,
    /// Exact 32-byte audit entry digest at that sequence.
    pub entry_hash: Vec<u8>,
}
/// No `SeaORM` relations; the checkpoint header link is a deferred SQL foreign key.
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}
impl ActiveModelBehavior for ActiveModel {}
