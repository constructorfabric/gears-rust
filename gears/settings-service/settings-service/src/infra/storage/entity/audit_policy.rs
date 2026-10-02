// Created: 2026-09-25 by Virtuozzo International GmbH
//! The `settings_audit_policy` table.

use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db_macros::Scopable;

/// The configured audit retention, as the database's trigger reads it.
///
/// One row, platform-wide: the retention is the deployment's, not a tenant's.
/// The gear writes it before every retention pass, and the trigger refuses
/// deleting a record younger than the greater of it and the platform minimum.
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "settings_audit_policy")]
#[secure(unrestricted)]
pub struct Model {
    /// Always `1`: there is one policy.
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: i16,
    /// The configured retention, in days; never below the platform minimum.
    pub retention_days: i32,
    /// When the gear last wrote it.
    pub updated_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
