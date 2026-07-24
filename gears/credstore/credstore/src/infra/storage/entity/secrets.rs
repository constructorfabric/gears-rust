//! `SeaORM` entity for the `credstore_secrets` table.
//!
//! Tenant-scoped (`tenant_col = "tenant_id"`, `resource_col = "id"`).
//! Sharing and status columns are stored as `SMALLINT` at the DB level
//! and mapped to typed enums in the repository layer.

use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db::secure::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "credstore_secrets")]
#[secure(tenant_col = "tenant_id", resource_col = "id", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub reference: String,
    /// Sharing mode: 1=Private, 2=Tenant, 3=Shared.
    pub sharing: i16,
    pub owner_id: Uuid,
    /// Status: 1=Provisioning, 2=Active, 3=Deprovisioning.
    pub status: i16,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
    /// Monotonic version for optimistic locking; seeded at 1 on insert,
    /// bumped by `touch`.
    pub version: i64,
    /// Deterministic v5 UUID of the secret's GTS type id (resolved to the
    /// type id + traits via the types-registry in the domain layer).
    pub secret_type_uuid: Uuid,
    /// Expiry instant for expirable types.
    pub expires_at: Option<OffsetDateTime>,
    /// Value-fingerprint fence: `HMAC-SHA256(fence_key, value)` of the value
    /// this row's metadata was written for. NULL only on out-of-band seeded
    /// rows (backfilled on first read / reaper sweep). Never leaves the
    /// gear.
    pub value_fp: Option<Vec<u8>>,
    /// Id of the fence key `value_fp` was computed under (keyring
    /// groundwork; NULL exactly when `value_fp` is NULL).
    pub fp_key_id: Option<i16>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
