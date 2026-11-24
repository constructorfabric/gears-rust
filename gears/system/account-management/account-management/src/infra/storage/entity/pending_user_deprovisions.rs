//! `pending_user_deprovisions` `SeaORM` entity — a user deletion that has
//! started but not yet finished (see
//! `migrations::m0009_create_pending_user_deprovisions` for the lifecycle).
//!
//! Schema (per dialect):
//!
//! * `tenant_id`  `UUID` / `TEXT`        PRIMARY KEY (with `user_id`).
//! * `user_id`    `UUID` / `TEXT`        PRIMARY KEY (with `tenant_id`).
//! * `created_at` `TIMESTAMPTZ` / `TEXT` NOT NULL DEFAULT now — forensic.
//!
//! `Scopable(no_tenant, ...)`: the row is AM's own bookkeeping for an
//! operation the caller was already authorized for, read and written only
//! by `UserService::delete_user` under its system scope. It is never
//! surfaced through the SDK.

use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "pending_user_deprovisions")]
#[secure(no_tenant, no_resource, no_owner, no_type)]
pub struct Model {
    /// Tenant named in the DELETE request; the user was confirmed to
    /// belong to it before the row was written.
    #[sea_orm(primary_key, auto_increment = false)]
    pub tenant_id: Uuid,
    /// The user being deleted.
    #[sea_orm(primary_key, auto_increment = false)]
    pub user_id: Uuid,
    /// When the deletion started. Written by the database default.
    pub created_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
