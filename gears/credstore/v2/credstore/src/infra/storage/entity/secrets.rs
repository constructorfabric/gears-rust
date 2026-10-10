// Updated: 2026-10-06 by Constructor Tech
//! `SeaORM` entity for the `credstore_secrets` table (ADR-0006: immutable
//! value versions).
//!
//! Tenant-scoped (`tenant_col = "tenant_id"`, `resource_col = "id"`), plus
//! the PDP row properties (`secret_type` -> `secret_type_uuid`, `reference` ->
//! `reference`, ADR-0010): a PDP constraint on either compiles to a predicate
//! on that column, so row-scoped authorization is applied in SQL.
//! Sharing, status, and fallback columns are stored as `SMALLINT` at the DB
//! level and mapped to typed enums in the repository layer.

use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db::secure::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "credstore_secrets")]
#[secure(
    tenant_col = "tenant_id",
    resource_col = "id",
    no_owner,
    no_type,
    pep_prop(secret_type = "secret_type_uuid"),
    pep_prop(reference = "reference")
)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub reference: String,
    /// Sharing mode: 1=Private, 2=Tenant, 3=Shared.
    pub sharing: i16,
    pub owner_id: Uuid,
    /// Status: 2=Active, 4=Declared. Codes 1 (provisioning) and 3
    /// (deprovisioning) are retired by ADR-0006 and reserved, never
    /// reassigned.
    pub status: i16,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
    /// Monotonic version for optimistic locking; seeded at 1 on insert,
    /// bumped by every successful value switch or metadata update.
    pub version: i64,
    /// Deterministic v5 UUID of the secret's GTS type id (resolved to the
    /// type id + traits via the types-registry in the domain layer).
    pub secret_type_uuid: Uuid,
    /// Expiry instant for expirable types.
    pub expires_at: Option<OffsetDateTime>,
    /// The value version the provider returned from `put` for the secret
    /// this row currently serves (opaque, never parsed or compared by the
    /// gear; not the row's own `version`). `NULL` iff `status = Declared`
    /// (`credstore_secrets_value_version_check`). The store key is
    /// `(tenant_id, id)`.
    pub value_version: Option<String>,
    /// Suppression fallback: 1=Inherit (default), 2=None. Only consulted for
    /// a `Declared` row's resolution competition (ADR-0004); always 1 in
    /// Phase 1.
    pub fallback: i16,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
