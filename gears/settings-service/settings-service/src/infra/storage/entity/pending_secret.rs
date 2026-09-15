// Created: 2026-09-15 by Constructor Tech
//! The `pending_secrets` table.

use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

/// A secret staged ahead of the batch, waiting to be claimed or swept.
///
/// Tenant-scoped like `setting_values`: the row belongs to the tenant whose
/// value it will become. The entry it names is already in the Credential
/// Store; the row is what ties the caller's token to it, and it exists only
/// between the stage and the batch — or the sweep.
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "pending_secrets")]
#[secure(tenant_col = "tenant_id", resource_col = "id", no_owner, no_type)]
pub struct Model {
    /// The token the caller holds.
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    /// The declaration the value is for; `ON DELETE CASCADE`.
    pub declaration_id: Uuid,
    /// The scope the value is for, as a tenant id.
    pub tenant_id: Uuid,
    /// The subject that staged it.
    pub subject_id: String,
    /// The store reference the batch adopts.
    pub secret_ref: String,
    /// When it was staged.
    pub created_at: OffsetDateTime,
    /// When it stops being claimable.
    pub expires_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
