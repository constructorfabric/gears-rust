//! `SeaORM` entity for `bss.payer_pii_map` (the per-payer PII reference +
//! erasure tombstone, Slice 6 Phase 3 Group 3A, architecture §4.5 / AC #22).
//!
//! One row per `(tenant_id, payer_tenant_id)`. `pii_ref` is an opaque pointer
//! into the external PII store (never the PII itself); `erased` is the GDPR
//! right-to-erasure tombstone the [`crate::infra::pii::ErasureService`] flips in
//! place. UNLIKE the audit / metadata-change tables this row IS mutated (the
//! tombstone), so there is no append-only trigger on its table.

use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "payer_pii_map")]
#[secure(tenant_col = "tenant_id", no_resource, no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub tenant_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub payer_tenant_id: Uuid,
    pub pii_ref: String,
    pub erased: bool,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
