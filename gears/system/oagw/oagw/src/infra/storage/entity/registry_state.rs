//! `oagw_registry_state`: the installation-wide row that gates the registry
//! reconcile (generation, content hash, lease). Always `id = 1`.

use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

/// The single row's key.
pub(crate) const ROW_ID: i16 = 1;

/// The registry state row.
#[allow(unknown_lints, de0309_must_have_domain_model)]
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "oagw_registry_state")]
#[secure(unrestricted)]
pub struct Model {
    /// Always [`ROW_ID`].
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: i16,
    /// Highest deployment generation that claimed the registry.
    pub generation: Option<i64>,
    /// Hex SHA-256 of the registry content of the last clean reconcile.
    pub content_hash: Option<String>,
    /// When `content_hash` was recorded (database clock).
    pub applied_at: Option<OffsetDateTime>,
    /// Process holding the reconcile lease.
    pub lease_owner: Option<Uuid>,
    /// When the lease lapses (database clock).
    pub lease_expires_at: Option<OffsetDateTime>,
}

/// Relations of the `oagw_registry_state` entity (none).
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
